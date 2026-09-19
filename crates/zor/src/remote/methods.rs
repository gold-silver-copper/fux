//! The BRP method table. Every handler is a system `fn(In<Option<Value>>, &mut World) ->
//! BrpResult` that strips the `{ token, instance }` envelope, authorises the token, checks the
//! instance nonce on mutations and only then calls typed transitions. Built-in reads are
//! wrapped the same way and restricted to projection components.

use bevy_ecs::prelude::*;
use bevy_remote::builtin_methods::{
    self, BRP_GET_COMPONENTS_METHOD, BRP_LIST_COMPONENTS_METHOD, BRP_QUERY_METHOD,
    BRP_REGISTRY_SCHEMA_METHOD, BrpGetComponentsParams, BrpQueryParams, ComponentSelector,
    RPC_DISCOVER_METHOD,
};
use bevy_remote::{BrpError, BrpResult, error_codes};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use super::Endpoint;
use super::projection::{ALLOWED_TYPE_PATHS, is_allowed_type_path};
use super::token::{Capabilities, Capability, Grant, Tokens};
use super::watch::{self, open_events};
use crate::model::{Ids, Limits, ServerInstance};

/// zor application error codes, in the range fux leaves free below its own `-3200x`.
pub mod codes {
    /// Missing/unknown token, or the token's grant does not cover the request.
    pub const UNAUTHORIZED: i16 = -32002;
    /// An id names nothing.
    pub const NOT_FOUND: i16 = -32003;
    /// The request is well-formed but not applicable (instance mismatch, limit reached, ...).
    pub const INVALID: i16 = -32004;
    /// The outcome of a mutation is unknown (a lost reply); reconcile before retrying.
    pub const UNCERTAIN: i16 = -32005;
}

/// An instant handler: answers in the update that dequeued the request.
pub type Instant = fn(In<Option<Value>>, &mut World) -> BrpResult;

#[derive(Clone, Copy)]
pub enum Handler {
    Instant(Instant),
    Watch(watch::Open),
}

/// One `zor/*` method: handler plus the typed shapes `zor/schema` reports.
pub struct MethodSpec {
    pub name: &'static str,
    pub handler: Handler,
    pub params: &'static str,
    pub params_fields: fn() -> Value,
    pub result: &'static str,
    pub result_fields: fn() -> Value,
    /// Deserialise-then-serialise through the typed shape (the fixture round trip).
    pub roundtrip_params: fn(Value) -> Result<Value, String>,
    pub roundtrip_result: fn(Value) -> Result<Value, String>,
}

pub(super) fn error(code: i16, message: impl Into<String>) -> BrpError {
    BrpError {
        code,
        message: message.into(),
        data: None,
    }
}

pub(super) fn unauthorized(message: impl Into<String>) -> BrpError {
    error(codes::UNAUTHORIZED, message)
}

pub(super) fn invalid(message: impl Into<String>) -> BrpError {
    error(codes::INVALID, message)
}

pub(super) fn to_value<T: serde::Serialize>(value: T) -> BrpResult {
    serde_json::to_value(value).map_err(BrpError::internal)
}

/// Defines a `deny_unknown_fields` serde struct and records its fields for `zor/schema`
/// (fux's `Described`).
macro_rules! described {
    ($(#[$m:meta])* pub struct $name:ident { $( $(#[$fm:meta])* pub $field:ident : $ty:ty ),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name { $( $(#[$fm])* pub $field: $ty, )* }
        impl $crate::remote::schema::Described for $name {
            const NAME: &'static str = stringify!($name);
            const FIELDS: &'static [(&'static str, &'static str)] =
                &[ $( (stringify!($field), stringify!($ty)) ),* ];
        }
    };
}
pub(super) use described;

// ---------------------------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------------------------

/// A request after the envelope was validated: `token` resolved to its grant, `instance`
/// remembered for mutations, the remaining fields left for the typed params.
pub struct Request {
    token: String,
    grant: Grant,
    instance: Option<String>,
    params: Value,
}

impl Request {
    /// Strips `token`/`instance` and authorises. Reads need `read`; every handler checks its
    /// own further capabilities.
    pub fn open(params: Option<Value>, world: &World) -> Result<Self, BrpError> {
        let mut fields = match params {
            Some(Value::Object(fields)) => fields,
            None => Map::new(),
            Some(_) => {
                return Err(error(
                    error_codes::INVALID_PARAMS,
                    "params must be an object",
                ));
            }
        };
        let Some(Value::String(token)) = fields.remove("token") else {
            return Err(unauthorized("missing token"));
        };
        let grant = world
            .resource::<Tokens>()
            .authorize(&token)
            .ok_or_else(|| unauthorized("unknown token"))?;
        let instance = match fields.remove("instance") {
            Some(Value::String(instance)) => Some(instance),
            Some(Value::Null) | None => None,
            Some(_) => {
                return Err(error(
                    error_codes::INVALID_PARAMS,
                    "instance must be a string",
                ));
            }
        };
        let request = Self {
            token,
            grant,
            instance,
            params: Value::Object(fields),
        };
        request.require(Capabilities::READ)?;
        Ok(request)
    }

    pub fn parse<P: DeserializeOwned>(&mut self) -> Result<P, BrpError> {
        builtin_methods::parse(core::mem::take(&mut self.params))
    }

    /// The raw params for wrapped built-ins that take their own shape.
    pub fn take_params(&mut self) -> Option<Value> {
        match core::mem::take(&mut self.params) {
            Value::Object(fields) if fields.is_empty() => None,
            other => Some(other),
        }
    }

    /// The presented token, for binding a stream to its revocation.
    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn require(&self, capabilities: Capabilities) -> Result<(), BrpError> {
        if self.grant.capabilities.contains(capabilities) {
            Ok(())
        } else {
            Err(unauthorized("token lacks the required capability"))
        }
    }

    /// A mutation needs `mutate` and the current instance nonce.
    pub fn mutation(&self, world: &World) -> Result<(), BrpError> {
        self.require(Capabilities::MUTATE)?;
        if world.resource::<crate::journal::Journal>().is_frozen() {
            return Err(invalid("journal is frozen; mutations are disabled until recovery"));
        }
        if world.get_resource::<bevy_state::prelude::State<crate::model::ServerMode>>()
            .is_some_and(|state| *state.get() == crate::model::ServerMode::ShuttingDown)
        {
            return Err(invalid("server is shutting down; mutations are disabled"));
        }
        let nonce = &world.resource::<ServerInstance>().nonce;
        match self.instance.as_deref() {
            Some(instance) if instance == nonce => Ok(()),
            Some(_) => Err(invalid(
                "instance mismatch: this is another server incarnation",
            )),
            None => Err(invalid("mutations must carry `instance`")),
        }
    }

    /// The `instance` nonce the request carried, if any.
    pub fn instance(&self) -> Option<&str> {
        self.instance.as_deref()
    }
}

// ---------------------------------------------------------------------------------------------
// Typed shapes
// ---------------------------------------------------------------------------------------------

described!(
    pub struct NoParams {}
);
described!(
    pub struct LimitsInfo {
        pub tasks: usize,
        pub attempts: usize,
        pub prompts: usize,
        pub checks: usize,
        pub groups: usize,
        pub journal_bytes: usize,
        pub tokens: usize,
        pub archive_after_ms: u64,
    }
);
described!(
    pub struct ServerInfo {
        pub name: String,
        pub nonce: String,
        pub pid: u32,
        pub started_ms: u64,
        pub http: Endpoint,
        pub tasks: usize,
        pub attempts: usize,
        pub checks: usize,
        pub machines: usize,
        pub tokens_minted: usize,
        pub watches: usize,
        pub events_retained: usize,
        pub journal_generation: u64,
        pub limits: LimitsInfo,
    }
);
described!(
    pub struct TokenMintParams {
        pub capabilities: Vec<Capability>,
    }
);
described!(
    pub struct TokenMinted {
        pub token: String,
        pub capabilities: Vec<Capability>,
    }
);
described!(
    pub struct TokenRevokeParams {
        pub revoke: String,
    }
);
described!(
    pub struct TokenRevoked {
        pub revoked: bool,
    }
);
described!(
    pub struct SchemaTable {
        pub envelope: Vec<String>,
        pub methods: Map<String, Value>,
    }
);

// ---------------------------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------------------------

fn server_info(mut req: Request, world: &mut World) -> BrpResult {
    let NoParams {} = req.parse()?;
    let instance = world.resource::<ServerInstance>();
    let limits = world.resource::<Limits>();
    let host = world
        .get_resource::<bevy_remote::http::HostAddress>()
        .map_or_else(|| "127.0.0.1".to_owned(), |h| h.0.to_string());
    let port = world
        .get_resource::<bevy_remote::http::HostPort>()
        .map_or(0, |p| p.0);
    let ids = world.resource::<Ids>();
    to_value(ServerInfo {
        name: instance.name.clone(),
        nonce: instance.nonce.clone(),
        pid: instance.pid,
        started_ms: instance.started_ms,
        http: Endpoint { host, port },
        tasks: ids.tasks.len(),
        attempts: ids.attempts.len(),
        checks: ids.checks.len(),
        machines: ids.machines.len(),
        tokens_minted: world.resource::<Tokens>().minted_count(),
        watches: world.resource::<watch::Watches>().open_count(),
        events_retained: world.resource::<super::events::EventLog>().retained(),
        journal_generation: world.resource::<crate::model::Generation>().0,
        limits: LimitsInfo {
            tasks: limits.tasks,
            attempts: limits.attempts,
            prompts: limits.prompts,
            checks: limits.checks,
            groups: limits.groups,
            journal_bytes: limits.journal_bytes,
            tokens: limits.tokens,
            archive_after_ms: limits.archive_after_ms,
        },
    })
}

fn token_mint(mut req: Request, world: &mut World) -> BrpResult {
    req.require(Capabilities::ADMIN)?;
    let params: TokenMintParams = req.parse()?;
    let capabilities = params
        .capabilities
        .iter()
        .fold(Capabilities::default(), |acc, c| acc.union(c.mask()));
    if !req.grant.capabilities.contains(capabilities) {
        return Err(unauthorized(
            "cannot mint beyond the minting token's capabilities",
        ));
    }
    let token = fux::attach::random_hex256().map_err(BrpError::internal)?;
    world
        .resource_mut::<Tokens>()
        .mint(
            token.clone(),
            Grant {
                workspace: None,
                capabilities,
            },
        )
        .map_err(|e| invalid(e.to_string()))?;
    to_value(TokenMinted {
        token,
        capabilities: capabilities.names(),
    })
}

fn token_revoke(mut req: Request, world: &mut World) -> BrpResult {
    req.require(Capabilities::ADMIN)?;
    let params: TokenRevokeParams = req.parse()?;
    let revoked = world.resource_mut::<Tokens>().revoke(&params.revoke);
    if revoked {
        watch::revoke(world, &params.revoke);
    }
    to_value(TokenRevoked { revoked })
}

fn schema(mut req: Request, _world: &mut World) -> BrpResult {
    let NoParams {} = req.parse()?;
    to_value(schema_table())
}

pub fn schema_table() -> SchemaTable {
    let mut methods = Map::new();
    for spec in all_specs() {
        methods.insert(
            spec.name.to_owned(),
            json!({
                "params": { "name": spec.params, "fields": (spec.params_fields)() },
                "result": { "name": spec.result, "fields": (spec.result_fields)() },
            }),
        );
    }
    SchemaTable {
        envelope: vec!["token".to_owned(), "instance".to_owned()],
        methods,
    }
}

pub(super) fn check_paths<'a>(paths: impl IntoIterator<Item = &'a String>) -> Result<(), BrpError> {
    for path in paths {
        if !is_allowed_type_path(path) {
            return Err(unauthorized(format!(
                "`{path}` is not a projection component; see zor::remote::projection"
            )));
        }
    }
    Ok(())
}

fn world_query(mut req: Request, world: &mut World) -> BrpResult {
    let params: BrpQueryParams = req.parse()?;
    check_paths(&params.data.components)?;
    match &params.data.option {
        ComponentSelector::All => {
            return Err(unauthorized(
                "`option: \"all\"` is refused; name projection components",
            ));
        }
        ComponentSelector::Paths(paths) => check_paths(paths)?,
    }
    check_paths(&params.data.has)?;
    check_paths(&params.filter.with)?;
    check_paths(&params.filter.without)?;
    if params.data.components.is_empty() && params.filter.with.is_empty() {
        return Err(unauthorized(
            "query must require at least one projection component",
        ));
    }
    let params = to_value(params)?;
    builtin_methods::process_remote_query_request(In(Some(params)), world)
}

fn world_get_components(mut req: Request, world: &mut World) -> BrpResult {
    let params: BrpGetComponentsParams = req.parse()?;
    check_paths(&params.components)?;
    let params = to_value(params)?;
    builtin_methods::process_remote_get_components_request(In(Some(params)), world)
}

fn world_list_components(mut req: Request, world: &mut World) -> BrpResult {
    let params = req.take_params();
    let listed = builtin_methods::process_remote_list_components_request(In(params), world)?;
    let Value::Array(names) = listed else {
        return Err(BrpError::internal("list_components returned a non-array"));
    };
    Ok(Value::Array(
        names
            .into_iter()
            .filter(|n| n.as_str().is_some_and(is_allowed_type_path))
            .collect(),
    ))
}

/// `registry.schema` over the projection vocabulary and the types their schemas reference.
fn registry_schema(mut req: Request, world: &mut World) -> BrpResult {
    let params = req.take_params();
    let Value::Object(mut exported) = builtin_methods::export_registry_types(In(params), world)?
    else {
        return Err(BrpError::internal("registry.schema returned a non-object"));
    };
    let mut kept = serde_json::Map::new();
    let mut pending: Vec<String> = ALLOWED_TYPE_PATHS.iter().map(|p| (*p).to_owned()).collect();
    while let Some(path) = pending.pop() {
        let Some(schema) = exported.remove(&path) else {
            continue;
        };
        collect_schema_refs(&schema, &mut pending);
        kept.insert(path, schema);
    }
    Ok(Value::Object(kept))
}

const SCHEMA_REF_PREFIX: &str = "#/$defs/";

fn collect_schema_refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                match value
                    .as_str()
                    .and_then(|s| s.strip_prefix(SCHEMA_REF_PREFIX))
                {
                    Some(path) if key == "$ref" => out.push(path.to_owned()),
                    _ => collect_schema_refs(value, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_schema_refs(item, out)),
        _ => {}
    }
}

fn rpc_discover(mut req: Request, world: &mut World) -> BrpResult {
    let NoParams {} = req.parse()?;
    builtin_methods::process_remote_list_methods_request(In(None), world)
}

// ---------------------------------------------------------------------------------------------
// Table
// ---------------------------------------------------------------------------------------------

/// `handler!(brp_name, inner)`: the `fn(In<Option<Value>>, &mut World)` system that opens the
/// envelope and calls `inner(Request, &mut World)`.
macro_rules! handler {
    ($name:ident, $inner:ident) => {
        fn $name(
            bevy_ecs::system::In(params): bevy_ecs::system::In<Option<serde_json::Value>>,
            world: &mut bevy_ecs::world::World,
        ) -> bevy_remote::BrpResult {
            let req = $crate::remote::methods::Request::open(params, world)?;
            $inner(req, world)
        }
    };
}

macro_rules! spec {
    ($method:literal, $handler:ident, $params:ty, $result:ty) => {
        spec!(@row $method, $crate::remote::methods::Handler::Instant($handler), $params, $result)
    };
    (watch $method:expr, $open:ident, $params:ty, $result:ty) => {
        spec!(@row $method, $crate::remote::methods::Handler::Watch($open), $params, $result)
    };
    (@row $method:expr, $handler:expr, $params:ty, $result:ty) => {
        $crate::remote::methods::MethodSpec {
            name: $method,
            handler: $handler,
            params: <$params as $crate::remote::schema::Described>::NAME,
            params_fields: $crate::remote::schema::fields_of::<$params>,
            result: <$result as $crate::remote::schema::Described>::NAME,
            result_fields: $crate::remote::schema::fields_of::<$result>,
            roundtrip_params: $crate::remote::schema::roundtrip::<$params>,
            roundtrip_result: $crate::remote::schema::roundtrip::<$result>,
        }
    };
}
pub(super) use {handler, spec};

handler!(brp_server_info, server_info);
handler!(brp_token_mint, token_mint);
handler!(brp_token_revoke, token_revoke);
handler!(brp_schema, schema);
handler!(brp_world_query, world_query);
handler!(brp_world_get_components, world_get_components);
handler!(brp_world_list_components, world_list_components);
handler!(brp_registry_schema, registry_schema);
handler!(brp_rpc_discover, rpc_discover);

/// Every `zor/*` method with its typed shapes.
pub static TABLE: &[MethodSpec] = &[
    spec!("zor/server.info", brp_server_info, NoParams, ServerInfo),
    spec!(
        "zor/token.mint",
        brp_token_mint,
        TokenMintParams,
        TokenMinted
    ),
    spec!(
        "zor/token.revoke",
        brp_token_revoke,
        TokenRevokeParams,
        TokenRevoked
    ),
    spec!("zor/schema", brp_schema, NoParams, SchemaTable),
    spec!(
        watch watch::EVENTS_WATCH_METHOD,
        open_events,
        watch::EventsWatchParams,
        watch::EventsWatchItem
    ),
];

/// Token-checked wrappers over Bevy's read-only built-ins, under their standard names.
pub static WRAPPED: &[(&str, Instant)] = &[
    (BRP_QUERY_METHOD, brp_world_query),
    (BRP_GET_COMPONENTS_METHOD, brp_world_get_components),
    (BRP_LIST_COMPONENTS_METHOD, brp_world_list_components),
    (BRP_REGISTRY_SCHEMA_METHOD, brp_registry_schema),
    (RPC_DISCOVER_METHOD, brp_rpc_discover),
];

/// Every `zor/*` method from every owner table.
pub fn all_specs() -> impl Iterator<Item = &'static MethodSpec> {
    TABLE
        .iter()
        .chain(super::task_methods::METHODS)
        .chain(super::resume_methods::METHODS)
        .chain(super::check_methods::METHODS)
        .chain(super::group_methods::METHODS)
        .chain(super::provider_methods::METHODS)
        .chain(super::plugin_methods::METHODS)
        .chain(super::machine_methods::METHODS)
        .chain(super::dashboard_methods::METHODS)
}

/// Every registered method name; the allowlist test compares `RemoteMethods::methods()` to it.
pub fn allowlist() -> Vec<&'static str> {
    all_specs()
        .map(|s| s.name)
        .chain(WRAPPED.iter().map(|(n, _)| *n))
        .chain(watch::WATCHED.iter().map(|(n, _)| *n))
        .collect()
}
