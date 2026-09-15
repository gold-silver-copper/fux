//! The BRP method table (prompt 3.9). Every handler is a fux system `fn(In<Option<Value>>,
//! &mut World) -> BrpResult` that strips the `{ token, instance }` envelope, authorises the
//! token and its workspace grant, checks the instance nonce on mutations and the template
//! generation on template edits, and only then calls the typed transitions in `layout::ops` and
//! `lifecycle`. Built-in reads are wrapped the same way and restricted to projection components.

use bevy_ecs::prelude::*;
use bevy_ecs::relationship::RelationshipTarget;
use bevy_remote::builtin_methods::{
    self, BRP_GET_COMPONENTS_METHOD, BRP_LIST_COMPONENTS_METHOD, BRP_QUERY_METHOD,
    BRP_REGISTRY_SCHEMA_METHOD, BrpGetComponentsParams, BrpQueryParams, ComponentSelector,
    RPC_DISCOVER_METHOD,
};
use bevy_remote::{BrpError, BrpResult, error_codes};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use super::descriptor::Endpoint;
use super::projection::{ALLOWED_TYPE_PATHS, is_allowed_type_path};
use super::schema::described;
use super::token::{Capabilities, Capability, Grant, Tokens};
use super::watch::{self, open_events};
use crate::events::DiagnosticsSnapshot;
use crate::layout::{LayoutError, NodePatch, ops};
use crate::model::components::ExactTarget;
use crate::model::components::Open;
use crate::model::{
    Effect, Ids, LaunchAttribution, LayoutGeneration, Limits, NodeId, PaneId, PaneIn, PaneSize,
    PaneTemplate, PlacedIn, Places, Process, RootOf, RootOrder, ServerInstance, Showing,
    SplitDirection, Targets, TemplateRoot, Title, ViewedBy, Viewer, ViewerId, Viewing, Viewport,
    WorkspaceName,
};
use crate::terminal::Terminal;

/// fux application error codes (the JSON-RPC reserved range is `-32768..=-32000`; these sit
/// where Bevy leaves room, like Bevy's own `-2340x` codes).
pub mod codes {
    /// `generation` does not match the root's `LayoutGeneration`.
    pub const STALE_GENERATION: i16 = -32001;
    /// Missing/unknown token, or the token's grant does not cover the request.
    pub const UNAUTHORIZED: i16 = -32002;
    /// An id names nothing.
    pub const NOT_FOUND: i16 = -32003;
    /// The request is well-formed but not applicable (instance mismatch, layout refusal,
    /// limit reached, malformed keys).
    pub const INVALID: i16 = -32004;
}

/// An instant handler: answers in the update that dequeued the request.
pub type Instant = fn(In<Option<Value>>, &mut World) -> BrpResult;

/// How a method is served.
#[derive(Clone, Copy)]
pub enum Handler {
    Instant(Instant),
    /// A `+watch` stream; opened by `remote::watch`, polled every update.
    Watch(super::watch::Open),
}

/// One `fux/*` method: handler plus the typed shapes `fux/schema` and the fixtures pin.
pub struct MethodSpec {
    pub name: &'static str,
    pub handler: Handler,
    pub params: &'static str,
    pub params_fields: fn() -> Value,
    pub result: &'static str,
    pub result_fields: fn() -> Value,
    pub roundtrip_params: fn(Value) -> Result<Value, String>,
    pub roundtrip_result: fn(Value) -> Result<Value, String>,
}

/// Bound on `send_keys`/`send_bytes` payloads.
pub const MAX_KEY_BYTES: usize = 64 * 1024;
pub const MAX_ARGV_ENTRIES: usize = 256;
pub const MAX_ARG_BYTES: usize = 64 * 1024;
pub const MAX_ENV_ENTRIES: usize = 64;
pub const MAX_ENV_BYTES: usize = 16 * 1024;
/// Bound on `pane.capture` lines (screen plus scrollback).
pub const MAX_CAPTURE_LINES: usize = 10_000;

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

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

pub(super) fn not_found(what: &str, id: u64) -> BrpError {
    error(codes::NOT_FOUND, format!("{what} {id} not found"))
}

pub(super) fn invalid(message: impl Into<String>) -> BrpError {
    error(codes::INVALID, message)
}

pub(super) fn layout_error(e: LayoutError) -> BrpError {
    invalid(e.to_string())
}

pub(super) fn bevy_error(e: bevy_ecs::error::BevyError) -> BrpError {
    invalid(e.to_string())
}

pub(super) fn to_value<T: serde::Serialize>(value: T) -> BrpResult {
    serde_json::to_value(value).map_err(BrpError::internal)
}

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

    /// Whether the grant covers a workspace entity.
    pub fn cover(&self, world: &World, workspace: Entity) -> Result<(), BrpError> {
        let name = world
            .get::<WorkspaceName>(workspace)
            .map_or("", |n| n.0.as_str());
        self.cover_name(name)
    }

    /// Whether the grant covers a workspace by name (a record can outlive its workspace).
    pub fn cover_name(&self, name: &str) -> Result<(), BrpError> {
        if self.grant.covers(name) {
            Ok(())
        } else {
            Err(unauthorized("token is scoped to another workspace"))
        }
    }

    /// The grant's workspace scope: `None` covers every workspace.
    pub fn workspace_scope(&self) -> Option<&str> {
        self.grant.workspace.as_deref()
    }

    /// Workspace-wide operations (creating or listing workspaces) need an unscoped token.
    pub fn unscoped(&self) -> Result<(), BrpError> {
        if self.grant.workspace.is_none() {
            Ok(())
        } else {
            Err(unauthorized("token is scoped to one workspace"))
        }
    }

    pub fn workspace(&self, world: &World, name: &str) -> Result<Entity, BrpError> {
        let entity = world
            .resource::<Ids>()
            .workspace(name)
            .ok_or_else(|| error(codes::NOT_FOUND, format!("workspace `{name}` not found")))?;
        self.cover(world, entity)?;
        Ok(entity)
    }

    pub fn pane(&self, world: &World, id: u64) -> Result<Entity, BrpError> {
        let pane = world
            .resource::<Ids>()
            .pane(PaneId(id))
            .ok_or_else(|| not_found("pane", id))?;
        let workspace = world
            .get::<PaneIn>(pane)
            .ok_or_else(|| not_found("pane", id))?
            .0;
        self.cover(world, workspace)?;
        Ok(pane)
    }

    /// A template node (roots included) and its root.
    pub fn node(&self, world: &World, id: u64) -> Result<(Entity, Entity), BrpError> {
        let node = world
            .resource::<Ids>()
            .node(NodeId(id))
            .ok_or_else(|| not_found("node", id))?;
        let root = root_of(world, node).ok_or_else(|| not_found("node", id))?;
        let workspace = world
            .get::<RootOf>(root)
            .ok_or_else(|| not_found("node", id))?
            .0;
        self.cover(world, workspace)?;
        Ok((node, root))
    }

    pub fn root(&self, world: &World, id: u64) -> Result<Entity, BrpError> {
        let (node, root) = self.node(world, id)?;
        if node == root {
            Ok(root)
        } else {
            Err(invalid(format!("node {id} is not a root")))
        }
    }

    pub fn viewer(&self, world: &World, id: u64) -> Result<Entity, BrpError> {
        let viewer = world
            .resource::<Ids>()
            .viewer(ViewerId(id))
            .ok_or_else(|| not_found("viewer", id))?;
        let workspace = world
            .get::<Viewing>(viewer)
            .ok_or_else(|| not_found("viewer", id))?
            .0;
        self.cover(world, workspace)?;
        Ok(viewer)
    }
}

/// Walks `ChildOf` up to the template root.
pub fn root_of(world: &World, mut node: Entity) -> Option<Entity> {
    for _ in 0..=crate::model::MAX_DEPTH {
        if world.get::<TemplateRoot>(node).is_some() {
            return Some(node);
        }
        node = world.get::<ChildOf>(node)?.parent();
    }
    None
}

pub(super) fn generation(world: &World, root: Entity) -> u64 {
    world.get::<LayoutGeneration>(root).map_or(0, |g| g.0)
}

pub(super) fn check_generation(world: &World, root: Entity, expected: u64) -> Result<(), BrpError> {
    let current = generation(world, root);
    if current == expected {
        Ok(())
    } else {
        Err(BrpError {
            code: codes::STALE_GENERATION,
            message: format!("stale generation {expected}; the root is at {current}"),
            data: Some(json!({ "generation": current })),
        })
    }
}

pub(super) fn node_id(world: &World, node: Entity) -> Result<u64, BrpError> {
    world
        .get::<NodeId>(node)
        .map(|id| id.0)
        .ok_or_else(|| BrpError::internal("node without id"))
}

pub(super) fn pane_id(world: &World, pane: Entity) -> Result<u64, BrpError> {
    world
        .get::<PaneId>(pane)
        .map(|id| id.0)
        .ok_or_else(|| BrpError::internal("pane without id"))
}

fn effect(world: &mut World, effect: Effect) {
    world.resource_mut::<Messages<Effect>>().write(effect);
}

// ---------------------------------------------------------------------------------------------
// Typed shapes
// ---------------------------------------------------------------------------------------------

described!(
    pub struct NoParams {}
);
described!(
    pub struct Done {}
);

described!(
    pub struct LimitsInfo {
        pub panes_per_workspace: usize,
        pub nodes_per_workspace: usize,
        pub workspaces: usize,
        pub viewers: usize,
        pub scrollback_lines: usize,
        pub final_retain_ms: u64,
        pub output_pacing_ms: u64,
        pub tokens: usize,
        pub key_bytes: usize,
        pub capture_lines: usize,
    }
);
described!(
    /// `bevy_diagnostic` counters the server keeps (owner `events`): runner wake-ups since
    /// start, live panes, attached viewers, retained event entries.
    pub struct Diagnostics {
        pub wakeups: u64,
        pub panes_live: u32,
        pub viewers: u32,
        pub events_retained: u32,
    }
);

impl From<DiagnosticsSnapshot> for Diagnostics {
    fn from(snapshot: DiagnosticsSnapshot) -> Self {
        Self {
            wakeups: snapshot.wakeups,
            panes_live: snapshot.panes_live,
            viewers: snapshot.viewers,
            events_retained: snapshot.events_retained,
        }
    }
}

described!(
    pub struct ServerInfo {
        pub name: String,
        pub nonce: String,
        pub pid: u32,
        pub started_ms: u64,
        pub http: Endpoint,
        pub workspaces: usize,
        pub panes: usize,
        pub viewers: usize,
        pub tokens_minted: usize,
        /// Open `+watch` streams.
        pub watches: usize,
        pub limits: LimitsInfo,
        pub diagnostics: Diagnostics,
    }
);

described!(
    pub struct TokenMintParams {
        pub workspace: Option<String>,
        pub capabilities: Vec<Capability>,
    }
);
described!(
    pub struct TokenMinted {
        pub token: String,
        pub workspace: Option<String>,
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
    pub struct PaneEntry {
        pub id: u64,
        pub node: Option<u64>,
        pub state: String,
        pub pid: Option<u32>,
        pub exit_code: Option<i32>,
        pub title: String,
        pub rows: u16,
        pub cols: u16,
        pub seq: u64,
        pub argv: Vec<String>,
        pub cwd: Option<String>,
    }
);
described!(
    pub struct RootEntry {
        pub id: u64,
        pub name: String,
        pub generation: u64,
        pub panes: Vec<PaneEntry>,
    }
);
described!(
    pub struct WorkspaceEntry {
        pub name: String,
        pub open: bool,
        pub viewers: usize,
        pub roots: Vec<RootEntry>,
    }
);
described!(
    pub struct WorkspaceList {
        pub workspaces: Vec<WorkspaceEntry>,
    }
);

described!(
    /// Optional overrides on the configured default command.
    pub struct TemplateSpec {
        #[serde(default)]
        pub argv: Option<Vec<String>>,
        #[serde(default)]
        pub cwd: Option<String>,
        #[serde(default)]
        pub env: Option<Vec<(String, String)>>,
        #[serde(default)]
        pub stream: Option<String>,
    }
);

described!(
    pub struct WorkspaceNewParams {
        pub name: String,
        #[serde(default)]
        pub template: Option<TemplateSpec>,
    }
);
described!(
    pub struct WorkspaceCreated {
        pub name: String,
        pub root: u64,
        pub pane: u64,
    }
);
described!(
    pub struct WorkspaceKillParams {
        pub name: String,
    }
);

described!(
    pub struct RootNewParams {
        pub workspace: String,
        pub name: String,
        #[serde(default)]
        pub template: Option<TemplateSpec>,
    }
);
described!(
    pub struct RootCreated {
        pub root: u64,
        pub pane: u64,
        pub generation: u64,
    }
);
described!(
    pub struct RootCloseParams {
        pub root: u64,
    }
);
described!(
    pub struct RootClosed {
        pub panes: Vec<u64>,
    }
);
described!(
    pub struct RootRenameParams {
        pub root: u64,
        pub name: String,
    }
);
described!(
    pub struct RootOrderParams {
        pub workspace: String,
        pub order: Vec<u64>,
    }
);
described!(
    pub struct RootMoveParams {
        pub root: u64,
        pub workspace: String,
    }
);

described!(
    pub struct NodeSpawnParams {
        pub generation: u64,
        pub parent: u64,
        #[serde(default)]
        pub index: Option<usize>,
        #[serde(default)]
        pub patch: Option<NodePatch>,
        #[serde(default)]
        pub template: Option<TemplateSpec>,
    }
);
described!(
    pub struct NodeSpawned {
        pub node: u64,
        pub pane: Option<u64>,
        pub generation: u64,
    }
);
described!(
    pub struct Generation {
        pub generation: u64,
    }
);
described!(
    pub struct NodeDespawnParams {
        pub generation: u64,
        pub node: u64,
    }
);
described!(
    pub struct NodeReparentParams {
        pub generation: u64,
        pub node: u64,
        pub parent: u64,
        #[serde(default)]
        pub index: Option<usize>,
    }
);
described!(
    pub struct NodeReorderParams {
        pub generation: u64,
        pub node: u64,
        pub index: usize,
    }
);
described!(
    pub struct NodePatchParams {
        pub generation: u64,
        pub node: u64,
        pub patch: NodePatch,
    }
);

described!(
    pub struct ViewerEntry {
        pub id: u64,
        pub workspace: String,
        pub showing: Option<u64>,
        pub target: Option<u64>,
        pub rows: u16,
        pub cols: u16,
        pub exact: bool,
    }
);
described!(
    pub struct ViewerList {
        pub viewers: Vec<ViewerEntry>,
    }
);
described!(
    pub struct ViewerShowParams {
        pub viewer: u64,
        pub root: u64,
    }
);
described!(
    pub struct ViewerTargetParams {
        pub viewer: u64,
        pub pane: u64,
    }
);
described!(
    pub struct ViewerZoomParams {
        pub viewer: u64,
        pub node: u64,
    }
);
described!(
    pub struct ViewerUnzoomParams {
        pub viewer: u64,
    }
);
described!(
    pub struct ViewerScrollParams {
        pub viewer: u64,
        pub node: u64,
        pub rows: i32,
    }
);

described!(
    /// Exactly one of `parent` (append a leaf under a container) or `split` (beside a pane).
    pub struct PaneNewParams {
        #[serde(default)]
        pub generation: Option<u64>,
        #[serde(default)]
        pub parent: Option<u64>,
        #[serde(default)]
        pub index: Option<usize>,
        #[serde(default)]
        pub split: Option<u64>,
        #[serde(default)]
        pub direction: Option<SplitDirection>,
        #[serde(default)]
        pub template: Option<TemplateSpec>,
    }
);
described!(
    pub struct PaneCreated {
        pub pane: u64,
        pub node: u64,
        pub generation: u64,
    }
);
described!(
    pub struct PaneCloseParams {
        pub pane: u64,
    }
);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyNotation {
    /// Byte-exact with `\n \r \t \e \\ \0 \xHH`.
    #[default]
    Escapes,
    /// Space-separated key names (`Enter`, `C-c`, `M-x`, `Up`, `F5`, literal characters).
    Keys,
}

described!(
    pub struct PaneSendKeysParams {
        pub pane: u64,
        pub keys: String,
        #[serde(default)]
        pub notation: KeyNotation,
    }
);
described!(
    pub struct PaneSendBytesParams {
        pub pane: u64,
        pub bytes: Vec<u8>,
    }
);
described!(
    pub struct BytesWritten {
        pub bytes: usize,
    }
);
described!(
    pub struct PaneCaptureParams {
        pub pane: u64,
        /// Rows of history above the screen to include, newest last.
        #[serde(default)]
        pub scrollback: usize,
    }
);
described!(
    pub struct Capture {
        pub pane: u64,
        pub seq: u64,
        pub rows: u16,
        pub cols: u16,
        pub title: String,
        pub state: String,
        pub cursor: crate::wire::Cursor,
        pub lines: Vec<String>,
        pub truncated: bool,
    }
);

described!(
    pub struct SchemaTable {
        /// Fields every method accepts besides its own: `token` (required) and `instance`.
        pub envelope: Vec<String>,
        pub methods: Map<String, Value>,
    }
);

// ---------------------------------------------------------------------------------------------
// Handlers: server, tokens
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
    let watches = world.resource::<watch::Watches>().open_count();
    to_value(ServerInfo {
        name: instance.name.clone(),
        nonce: instance.nonce.clone(),
        pid: instance.pid,
        started_ms: instance.started_ms,
        http: Endpoint { host, port },
        workspaces: ids.workspaces.len(),
        panes: ids.panes.len(),
        viewers: ids.viewers.len(),
        tokens_minted: world.resource::<Tokens>().minted_count(),
        watches,
        limits: LimitsInfo {
            panes_per_workspace: limits.panes_per_workspace,
            nodes_per_workspace: limits.nodes_per_workspace,
            workspaces: limits.workspaces,
            viewers: limits.viewers,
            scrollback_lines: limits.scrollback_lines,
            final_retain_ms: limits.final_retain_ms,
            output_pacing_ms: limits.output_pacing_ms,
            tokens: limits.tokens,
            key_bytes: MAX_KEY_BYTES,
            capture_lines: MAX_CAPTURE_LINES,
        },
        diagnostics: Diagnostics::from(
            world
                .get_resource::<DiagnosticsSnapshot>()
                .copied()
                .unwrap_or_default(),
        ),
    })
}

fn token_mint(mut req: Request, world: &mut World) -> BrpResult {
    req.require(Capabilities::ADMIN)?;
    let params: TokenMintParams = req.parse()?;
    if let Some(name) = &params.workspace {
        // A narrowed token must stay within the minter's own grant.
        req.workspace(world, name)?;
    } else {
        req.unscoped()?;
    }
    let capabilities = params
        .capabilities
        .iter()
        .fold(Capabilities::default(), |acc, c| acc.union(c.mask()));
    if !req.grant.capabilities.contains(capabilities) {
        return Err(unauthorized(
            "cannot mint beyond the minting token's capabilities",
        ));
    }
    let token = crate::attach::random_hex256().map_err(BrpError::internal)?;
    world
        .resource_mut::<Tokens>()
        .mint(
            token.clone(),
            Grant {
                workspace: params.workspace.clone(),
                capabilities,
            },
        )
        .map_err(|e| invalid(e.to_string()))?;
    to_value(TokenMinted {
        token,
        workspace: params.workspace,
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

// ---------------------------------------------------------------------------------------------
// Handlers: workspaces and roots
// ---------------------------------------------------------------------------------------------

fn pane_entry(world: &World, pane: Entity) -> Option<PaneEntry> {
    let process = *world.get::<Process>(pane)?;
    let size = world.get::<PaneSize>(pane).copied().unwrap_or_default();
    let launch = world.get::<LaunchAttribution>(pane);
    Some(PaneEntry {
        id: world.get::<PaneId>(pane)?.0,
        node: world
            .get::<PlacedIn>(pane)
            .and_then(|p| p.iter().next())
            .and_then(|leaf| world.get::<NodeId>(leaf))
            .map(|id| id.0),
        state: process_name(process).to_owned(),
        pid: process.pid(),
        exit_code: match process {
            Process::Exited { code } => Some(code),
            _ => None,
        },
        title: world
            .get::<Title>(pane)
            .map_or_else(String::new, |t| t.0.clone()),
        rows: size.rows,
        cols: size.cols,
        seq: world.get::<Terminal>(pane).map_or(0, Terminal::seq),
        argv: launch.map(|l| l.argv.clone()).unwrap_or_default(),
        cwd: launch.and_then(|l| l.cwd.clone()),
    })
}

fn process_name(process: Process) -> &'static str {
    match process {
        Process::Starting => "starting",
        Process::Live { .. } => "live",
        Process::Eof { .. } => "eof",
        Process::Terminating { .. } => "terminating",
        Process::Exited { .. } => "exited",
    }
}

/// Panes placed in a root's subtree, document order.
fn root_panes(world: &World, root: Entity) -> Vec<PaneEntry> {
    let mut panes = Vec::new();
    crate::layout::instances::walk(world, root, &mut |node, _| {
        if let Some(entry) = world
            .get::<Places>(node)
            .and_then(|p| pane_entry(world, p.0))
        {
            panes.push(entry);
        }
    });
    panes
}

fn root_entry(world: &World, root: Entity) -> Option<RootEntry> {
    Some(RootEntry {
        id: world.get::<NodeId>(root)?.0,
        name: world
            .get::<Name>(root)
            .map_or_else(String::new, |n| n.as_str().to_owned()),
        generation: generation(world, root),
        panes: root_panes(world, root),
    })
}

fn workspace_entry(world: &World, workspace: Entity) -> Option<WorkspaceEntry> {
    Some(WorkspaceEntry {
        name: world.get::<WorkspaceName>(workspace)?.0.clone(),
        open: world.get::<Open>(workspace).is_some(),
        viewers: world.get::<ViewedBy>(workspace).map_or(0, |v| v.len()),
        roots: world
            .get::<RootOrder>(workspace)
            .map(|order| {
                order
                    .0
                    .iter()
                    .filter_map(|r| root_entry(world, *r))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn workspace_list(mut req: Request, world: &mut World) -> BrpResult {
    let NoParams {} = req.parse()?;
    let mut names: Vec<(WorkspaceName, Entity)> = world
        .resource::<Ids>()
        .workspaces
        .iter()
        .map(|(n, e)| (n.clone(), *e))
        .collect();
    names.sort_by(|a, b| a.0.0.cmp(&b.0.0));
    let workspaces = names
        .iter()
        .filter(|(name, _)| req.grant.covers(&name.0))
        .filter_map(|(_, e)| workspace_entry(world, *e))
        .collect();
    to_value(WorkspaceList { workspaces })
}

fn validate_template(template: &PaneTemplate) -> Result<(), BrpError> {
    if template.argv.len() > MAX_ARGV_ENTRIES {
        return Err(invalid(format!(
            "argv has more than {MAX_ARGV_ENTRIES} entries"
        )));
    }
    if template.argv.first().is_none_or(|first| first.is_empty()) {
        return Err(invalid("argv must start with a non-empty executable"));
    }
    if template
        .argv
        .iter()
        .any(|a| a.len() > MAX_ARG_BYTES || a.contains('\0'))
    {
        return Err(invalid("argv entries must be bounded and contain no NUL"));
    }
    if template.env.len() > MAX_ENV_ENTRIES {
        return Err(invalid(format!(
            "at most {MAX_ENV_ENTRIES} environment entries"
        )));
    }
    let mut total = 0usize;
    for (name, value) in &template.env {
        if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
            return Err(invalid(
                "environment names are non-empty without `=` or NUL; values carry no NUL",
            ));
        }
        total = total.saturating_add(name.len()).saturating_add(value.len());
    }
    if total > MAX_ENV_BYTES {
        return Err(invalid(format!(
            "environment exceeds {MAX_ENV_BYTES} bytes"
        )));
    }
    if template.cwd.as_deref().is_some_and(|c| c.contains('\0')) {
        return Err(invalid("cwd contains NUL"));
    }
    Ok(())
}

/// The configured default command with the request's overrides applied and validated.
fn resolve_template(world: &World, spec: Option<TemplateSpec>) -> Result<PaneTemplate, BrpError> {
    let mut template = crate::lifecycle::default_template(world);
    if let Some(spec) = spec {
        if let Some(argv) = spec.argv {
            template.argv = argv;
        }
        if let Some(cwd) = spec.cwd {
            template.cwd = Some(cwd);
        }
        if let Some(env) = spec.env {
            template.env = env;
        }
        if let Some(stream) = spec.stream {
            template.stream = stream;
        }
    }
    validate_template(&template)?;
    Ok(template)
}

fn leaf_node() -> bevy_ui::Node {
    bevy_ui::Node {
        flex_grow: 1.0,
        flex_shrink: 1.0,
        min_width: bevy_ui::Val::Px(crate::model::MIN_PANE_COLS.into()),
        min_height: bevy_ui::Val::Px(crate::model::MIN_PANE_ROWS.into()),
        ..Default::default()
    }
}

fn placed_pane(world: &World, leaf: Entity) -> Result<u64, BrpError> {
    let pane = world
        .get::<Places>(leaf)
        .ok_or_else(|| BrpError::internal("leaf places no pane"))?
        .0;
    pane_id(world, pane)
}

/// A root with one pane inside; the composition `workspace.new` and `root.new` share.
fn new_root_with_pane(
    world: &mut World,
    workspace: Entity,
    name: &str,
    template: PaneTemplate,
) -> Result<(Entity, Entity), BrpError> {
    let root = ops::new_root(world, workspace, name).map_err(layout_error)?;
    let leaf = match ops::spawn_node(world, root, None, leaf_node(), Some(template)) {
        Ok(leaf) => leaf,
        Err(e) => {
            let _ = ops::close_root(world, root);
            return Err(layout_error(e));
        }
    };
    Ok((root, leaf))
}

fn workspace_new(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    req.unscoped()?;
    let params: WorkspaceNewParams = req.parse()?;
    let template = resolve_template(world, params.template)?;
    if world.resource::<Ids>().workspaces.len() >= world.resource::<Limits>().workspaces {
        return Err(invalid("workspace limit reached"));
    }
    let workspace = ops::new_workspace(world, &params.name).map_err(layout_error)?;
    let (root, leaf) = match new_root_with_pane(world, workspace, "main", template) {
        Ok(created) => created,
        Err(e) => {
            let now = crate::lifecycle::now_ms(world);
            let _ = ops::retire_workspace(world, workspace, now);
            return Err(e);
        }
    };
    to_value(WorkspaceCreated {
        name: params.name,
        root: node_id(world, root)?,
        pane: placed_pane(world, leaf)?,
    })
}

fn workspace_kill(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: WorkspaceKillParams = req.parse()?;
    let workspace = req.workspace(world, &params.name)?;
    let now = crate::lifecycle::now_ms(world);
    ops::retire_workspace(world, workspace, now).map_err(layout_error)?;
    to_value(Done {})
}

fn root_new(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: RootNewParams = req.parse()?;
    let workspace = req.workspace(world, &params.workspace)?;
    let template = resolve_template(world, params.template)?;
    let (root, leaf) = new_root_with_pane(world, workspace, &params.name, template)?;
    to_value(RootCreated {
        root: node_id(world, root)?,
        pane: placed_pane(world, leaf)?,
        generation: generation(world, root),
    })
}

fn root_close(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: RootCloseParams = req.parse()?;
    let root = req.root(world, params.root)?;
    let panes: Vec<u64> = ops::close_root(world, root)
        .map_err(layout_error)?
        .into_iter()
        .filter_map(|p| world.get::<PaneId>(p).map(|id| id.0))
        .collect();
    to_value(RootClosed { panes })
}

fn root_rename(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: RootRenameParams = req.parse()?;
    let (node, _) = req.node(world, params.root)?;
    ops::rename(world, node, &params.name).map_err(layout_error)?;
    to_value(Done {})
}

fn root_order(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: RootOrderParams = req.parse()?;
    let workspace = req.workspace(world, &params.workspace)?;
    let order = params
        .order
        .iter()
        .map(|id| req.root(world, *id))
        .collect::<Result<Vec<_>, _>>()?;
    ops::order_roots(world, workspace, &order).map_err(layout_error)?;
    to_value(Done {})
}

fn root_move(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: RootMoveParams = req.parse()?;
    let root = req.root(world, params.root)?;
    let workspace = req.workspace(world, &params.workspace)?;
    ops::move_root(world, root, workspace).map_err(layout_error)?;
    to_value(Done {})
}

// ---------------------------------------------------------------------------------------------
// Handlers: template nodes
// ---------------------------------------------------------------------------------------------

fn node_spawn(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: NodeSpawnParams = req.parse()?;
    let (parent, root) = req.node(world, params.parent)?;
    check_generation(world, root, params.generation)?;
    let template = params
        .template
        .map(|spec| resolve_template(world, Some(spec)))
        .transpose()?;
    let node = ops::spawn_node(world, parent, params.index, leaf_node(), template)
        .map_err(layout_error)?;
    if let Some(patch) = &params.patch
        && let Err(e) = ops::patch_node(world, node, patch)
    {
        // The spawn is rolled back so a refused patch commits nothing.
        let _ = ops::despawn_node(world, node);
        return Err(layout_error(e));
    }
    to_value(NodeSpawned {
        node: node_id(world, node)?,
        pane: world
            .get::<Places>(node)
            .and_then(|p| world.get::<PaneId>(p.0))
            .map(|id| id.0),
        generation: generation(world, root),
    })
}

fn node_despawn(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: NodeDespawnParams = req.parse()?;
    let (node, root) = req.node(world, params.node)?;
    check_generation(world, root, params.generation)?;
    ops::despawn_node(world, node).map_err(layout_error)?;
    to_value(Generation {
        generation: generation(world, root),
    })
}

fn node_reparent(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: NodeReparentParams = req.parse()?;
    let (node, root) = req.node(world, params.node)?;
    let (parent, parent_root) = req.node(world, params.parent)?;
    check_generation(world, root, params.generation)?;
    if parent_root != root {
        check_generation(world, parent_root, params.generation)?;
    }
    ops::reparent_node(world, node, parent, params.index).map_err(layout_error)?;
    to_value(Generation {
        generation: generation(world, root),
    })
}

fn node_reorder(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: NodeReorderParams = req.parse()?;
    let (node, root) = req.node(world, params.node)?;
    check_generation(world, root, params.generation)?;
    ops::reorder_node(world, node, params.index).map_err(layout_error)?;
    to_value(Generation {
        generation: generation(world, root),
    })
}

fn node_patch(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: NodePatchParams = req.parse()?;
    let (node, root) = req.node(world, params.node)?;
    check_generation(world, root, params.generation)?;
    ops::patch_node(world, node, &params.patch).map_err(layout_error)?;
    to_value(Generation {
        generation: generation(world, root),
    })
}

// ---------------------------------------------------------------------------------------------
// Handlers: viewers
// ---------------------------------------------------------------------------------------------

fn viewer_entry(world: &World, viewer: Entity) -> Option<ViewerEntry> {
    let viewport = world
        .get::<Viewport>(viewer)
        .copied()
        .unwrap_or(Viewport { rows: 0, cols: 0 });
    Some(ViewerEntry {
        id: world.get::<ViewerId>(viewer)?.0,
        workspace: world
            .get::<Viewing>(viewer)
            .and_then(|v| world.get::<WorkspaceName>(v.0))
            .map_or_else(String::new, |n| n.0.clone()),
        showing: world
            .get::<Showing>(viewer)
            .and_then(|s| world.get::<NodeId>(s.0))
            .map(|id| id.0),
        target: world
            .get::<Targets>(viewer)
            .and_then(|t| world.get::<PaneId>(t.0))
            .map(|id| id.0),
        rows: viewport.rows,
        cols: viewport.cols,
        exact: world.get::<ExactTarget>(viewer).is_some(),
    })
}

fn viewer_list(mut req: Request, world: &mut World) -> BrpResult {
    let NoParams {} = req.parse()?;
    let mut ids: Vec<(ViewerId, Entity)> = world
        .resource::<Ids>()
        .viewers
        .iter()
        .map(|(id, e)| (*id, *e))
        .collect();
    ids.sort();
    let viewers = ids
        .iter()
        .filter(|(_, e)| world.get::<Viewer>(*e).is_some())
        .filter(|(_, e)| {
            world
                .get::<Viewing>(*e)
                .is_some_and(|v| req.cover(world, v.0).is_ok())
        })
        .filter_map(|(_, e)| viewer_entry(world, *e))
        .collect();
    to_value(ViewerList { viewers })
}

fn viewer_show(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: ViewerShowParams = req.parse()?;
    let viewer = req.viewer(world, params.viewer)?;
    let root = req.root(world, params.root)?;
    ops::show_root(world, viewer, root).map_err(layout_error)?;
    to_value(Done {})
}

fn viewer_target(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: ViewerTargetParams = req.parse()?;
    let viewer = req.viewer(world, params.viewer)?;
    let pane = req.pane(world, params.pane)?;
    ops::target(world, viewer, pane).map_err(layout_error)?;
    to_value(Done {})
}

fn viewer_zoom(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: ViewerZoomParams = req.parse()?;
    let viewer = req.viewer(world, params.viewer)?;
    let (node, _) = req.node(world, params.node)?;
    ops::zoom(world, viewer, node).map_err(layout_error)?;
    to_value(Done {})
}

fn viewer_unzoom(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: ViewerUnzoomParams = req.parse()?;
    let viewer = req.viewer(world, params.viewer)?;
    ops::unzoom(world, viewer).map_err(layout_error)?;
    to_value(Done {})
}

fn viewer_scroll(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: ViewerScrollParams = req.parse()?;
    let viewer = req.viewer(world, params.viewer)?;
    let (node, _) = req.node(world, params.node)?;
    ops::scroll(world, viewer, node, params.rows).map_err(layout_error)?;
    to_value(Done {})
}

// ---------------------------------------------------------------------------------------------
// Handlers: panes
// ---------------------------------------------------------------------------------------------

fn pane_new(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: PaneNewParams = req.parse()?;
    let template = resolve_template(world, params.template)?;
    let (leaf, root) = match (params.parent, params.split) {
        (Some(parent), None) => {
            let (parent, root) = req.node(world, parent)?;
            if let Some(expected) = params.generation {
                check_generation(world, root, expected)?;
            }
            let leaf = ops::spawn_node(world, parent, params.index, leaf_node(), Some(template))
                .map_err(layout_error)?;
            (leaf, root)
        }
        (None, Some(pane)) => {
            let pane = req.pane(world, pane)?;
            let placing = world
                .get::<PlacedIn>(pane)
                .and_then(|p| p.iter().next())
                .ok_or_else(|| invalid("pane is not placed in any root"))?;
            let root = root_of(world, placing).ok_or_else(|| invalid("pane is not in a root"))?;
            if let Some(expected) = params.generation {
                check_generation(world, root, expected)?;
            }
            let direction = params.direction.unwrap_or(SplitDirection::Right);
            let (leaf, _) = ops::split(world, pane, direction, template).map_err(layout_error)?;
            (leaf, root)
        }
        _ => return Err(invalid("exactly one of `parent` or `split` is required")),
    };
    to_value(PaneCreated {
        pane: placed_pane(world, leaf)?,
        node: node_id(world, leaf)?,
        generation: generation(world, root),
    })
}

fn pane_close(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: PaneCloseParams = req.parse()?;
    let pane = req.pane(world, params.pane)?;
    crate::lifecycle::close_pane(world, pane).map_err(bevy_error)?;
    to_value(Done {})
}

fn write_pane(world: &mut World, pane: Entity, bytes: Vec<u8>) -> Result<usize, BrpError> {
    if bytes.len() > MAX_KEY_BYTES {
        return Err(invalid(format!(
            "at most {MAX_KEY_BYTES} bytes per request"
        )));
    }
    let process = world
        .get::<Process>(pane)
        .copied()
        .ok_or_else(|| BrpError::internal("pane without process"))?;
    if !matches!(process, Process::Live { .. }) {
        return Err(invalid(format!(
            "pane is {}; input needs a live process",
            process_name(process)
        )));
    }
    let written = bytes.len();
    effect(world, Effect::WritePty { pane, bytes });
    Ok(written)
}

fn pane_send_keys(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: PaneSendKeysParams = req.parse()?;
    let pane = req.pane(world, params.pane)?;
    if params.keys.len() > MAX_KEY_BYTES {
        return Err(invalid(format!(
            "at most {MAX_KEY_BYTES} bytes per request"
        )));
    }
    let bytes = match params.notation {
        KeyNotation::Escapes => {
            crate::input_ops::parse_keys(&params.keys).map_err(|e| invalid(e.to_string()))?
        }
        KeyNotation::Keys => decode_key_names(&params.keys).map_err(invalid)?,
    };
    let bytes = write_pane(world, pane, bytes)?;
    to_value(BytesWritten { bytes })
}

fn pane_send_bytes(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: PaneSendBytesParams = req.parse()?;
    let pane = req.pane(world, params.pane)?;
    let bytes = write_pane(world, pane, params.bytes)?;
    to_value(BytesWritten { bytes })
}

fn pane_capture(mut req: Request, world: &mut World) -> BrpResult {
    let params: PaneCaptureParams = req.parse()?;
    let pane = req.pane(world, params.pane)?;
    let process = world
        .get::<Process>(pane)
        .copied()
        .unwrap_or(Process::Starting);
    // `scrollback_line` shifts the emulator's view and restores it, so it needs `&mut`; the
    // terminal's observable state is unchanged, hence no change tick.
    let mut terminal = world
        .get_mut::<Terminal>(pane)
        .ok_or_else(|| invalid("pane has no terminal yet"))?;
    let terminal = terminal.bypass_change_detection();
    let screen = terminal.screen_lines();
    let budget = MAX_CAPTURE_LINES.saturating_sub(screen.len());
    let scrollback = params.scrollback.min(budget);
    let truncated = scrollback < params.scrollback;
    let mut lines = Vec::with_capacity(scrollback + screen.len());
    // History rows are addressed by distance above the screen; emit oldest first.
    for offset in (1..=scrollback).rev() {
        if let Some(line) = terminal.scrollback_line(offset) {
            lines.push(line);
        }
    }
    lines.extend(screen);
    to_value(Capture {
        pane: params.pane,
        seq: terminal.seq(),
        rows: terminal.rows(),
        cols: terminal.cols(),
        title: terminal.title().unwrap_or_default().to_owned(),
        state: process_name(process).to_owned(),
        cursor: terminal.cursor(),
        lines,
        truncated,
    })
}

/// The xterm byte sequence for one named key (normal cursor mode).
fn named_key(name: &str) -> Option<&'static [u8]> {
    Some(match name {
        "Enter" | "Return" => b"\r",
        "Tab" => b"\t",
        "Escape" | "Esc" => b"\x1b",
        "Space" => b" ",
        "Backspace" | "BSpace" => b"\x7f",
        "Up" => b"\x1b[A",
        "Down" => b"\x1b[B",
        "Right" => b"\x1b[C",
        "Left" => b"\x1b[D",
        "Home" => b"\x1b[H",
        "End" => b"\x1b[F",
        "Insert" | "IC" => b"\x1b[2~",
        "Delete" | "DC" => b"\x1b[3~",
        "PageUp" | "PgUp" => b"\x1b[5~",
        "PageDown" | "PgDn" => b"\x1b[6~",
        "F1" => b"\x1bOP",
        "F2" => b"\x1bOQ",
        "F3" => b"\x1bOR",
        "F4" => b"\x1bOS",
        "F5" => b"\x1b[15~",
        "F6" => b"\x1b[17~",
        "F7" => b"\x1b[18~",
        "F8" => b"\x1b[19~",
        "F9" => b"\x1b[20~",
        "F10" => b"\x1b[21~",
        "F11" => b"\x1b[23~",
        "F12" => b"\x1b[24~",
        _ => return None,
    })
}

const MAX_KEY_MODIFIERS: usize = 8;

fn decode_token(token: &str, depth: usize, out: &mut Vec<u8>) -> Result<(), String> {
    if depth > MAX_KEY_MODIFIERS {
        return Err("too many key modifiers".to_owned());
    }
    if let Some(rest) = token.strip_prefix("C-") {
        let start = out.len();
        decode_token(rest, depth + 1, out)?;
        // Control applies to one ASCII byte; anything longer is undefined.
        let [byte] = out.get(start..).unwrap_or_default() else {
            return Err(format!("C- needs one key: {token}"));
        };
        let control = byte.to_ascii_uppercase().wrapping_sub(0x40) & 0x7f;
        out.truncate(start);
        out.push(control);
        return Ok(());
    }
    if let Some(rest) = token.strip_prefix("M-") {
        out.push(0x1b);
        return decode_token(rest, depth + 1, out);
    }
    if let Some(bytes) = named_key(token) {
        out.extend_from_slice(bytes);
        return Ok(());
    }
    let mut chars = token.chars();
    if let (Some(character), None) = (chars.next(), chars.next()) {
        let mut encoded = [0_u8; 4];
        out.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
        return Ok(());
    }
    Err(format!("unknown key `{token}`"))
}

/// Space-separated key tokens into the bytes a pane receives.
pub fn decode_key_names(input: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(input.len());
    for token in input.split_whitespace() {
        decode_token(token, 0, &mut out)?;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Handlers: schema and wrapped built-ins
// ---------------------------------------------------------------------------------------------

fn schema(mut req: Request, _world: &mut World) -> BrpResult {
    let NoParams {} = req.parse()?;
    to_value(schema_table())
}

/// Every `fux/*` method from every owner table.
pub fn all_specs() -> impl Iterator<Item = &'static MethodSpec> {
    TABLE
        .iter()
        .chain(super::scene_methods::METHODS)
        .chain(super::input_methods::METHODS)
        .chain(super::session_methods::METHODS)
        .chain(super::surface_methods::METHODS)
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
                "`{path}` is not a projection component; see fux::remote::projection"
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
    let params = match core::mem::take(&mut req.params) {
        Value::Object(fields) if fields.is_empty() => None,
        other => Some(other),
    };
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

/// `registry.schema` over the projection vocabulary only: the caller's filter is applied by
/// `export_registry_types` (so a caller can narrow), then the result is cut down to the
/// allowlisted projection components and the types their schemas reference (so a caller can
/// never widen to an authoritative component's shape).
fn registry_schema(mut req: Request, world: &mut World) -> BrpResult {
    let params = match core::mem::take(&mut req.params) {
        Value::Object(fields) if fields.is_empty() => None,
        other => Some(other),
    };
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

/// Type paths a schema refers to through `$ref`.
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
pub(super) use handler;

/// `spec!("fux/x.y", brp_handler, Params, Result)`: one instant [`MethodSpec`] row;
/// `spec!(watch "fux/x+watch", open_fn, Params, Item)`: a stream whose result shape is one
/// item.
macro_rules! spec {
    ($method:literal, $handler:ident, $params:ty, $result:ty) => {
        $crate::remote::methods::spec!(@row $method, $crate::remote::methods::Handler::Instant($handler), $params, $result)
    };
    (watch $method:expr, $open:ident, $params:ty, $result:ty) => {
        $crate::remote::methods::spec!(@row $method, $crate::remote::methods::Handler::Watch($open), $params, $result)
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
pub(super) use spec;

handler!(brp_server_info, server_info);
handler!(brp_token_mint, token_mint);
handler!(brp_token_revoke, token_revoke);
handler!(brp_workspace_list, workspace_list);
handler!(brp_workspace_new, workspace_new);
handler!(brp_workspace_kill, workspace_kill);
handler!(brp_root_new, root_new);
handler!(brp_root_close, root_close);
handler!(brp_root_rename, root_rename);
handler!(brp_root_order, root_order);
handler!(brp_root_move, root_move);
handler!(brp_node_spawn, node_spawn);
handler!(brp_node_despawn, node_despawn);
handler!(brp_node_reparent, node_reparent);
handler!(brp_node_reorder, node_reorder);
handler!(brp_node_patch, node_patch);
handler!(brp_viewer_list, viewer_list);
handler!(brp_viewer_show, viewer_show);
handler!(brp_viewer_target, viewer_target);
handler!(brp_viewer_zoom, viewer_zoom);
handler!(brp_viewer_unzoom, viewer_unzoom);
handler!(brp_viewer_scroll, viewer_scroll);
handler!(brp_pane_new, pane_new);
handler!(brp_pane_close, pane_close);
handler!(brp_pane_send_keys, pane_send_keys);
handler!(brp_pane_send_bytes, pane_send_bytes);
handler!(brp_pane_capture, pane_capture);
handler!(brp_schema, schema);
handler!(brp_world_query, world_query);
handler!(brp_world_get_components, world_get_components);
handler!(brp_world_list_components, world_list_components);
handler!(brp_registry_schema, registry_schema);
handler!(brp_rpc_discover, rpc_discover);

/// Every `fux/*` method with its typed shapes.
pub static TABLE: &[MethodSpec] = &[
    spec!("fux/server.info", brp_server_info, NoParams, ServerInfo),
    spec!(
        "fux/token.mint",
        brp_token_mint,
        TokenMintParams,
        TokenMinted
    ),
    spec!(
        "fux/token.revoke",
        brp_token_revoke,
        TokenRevokeParams,
        TokenRevoked
    ),
    spec!(
        "fux/workspace.list",
        brp_workspace_list,
        NoParams,
        WorkspaceList
    ),
    spec!(
        "fux/workspace.new",
        brp_workspace_new,
        WorkspaceNewParams,
        WorkspaceCreated
    ),
    spec!(
        "fux/workspace.kill",
        brp_workspace_kill,
        WorkspaceKillParams,
        Done
    ),
    spec!("fux/root.new", brp_root_new, RootNewParams, RootCreated),
    spec!(
        "fux/root.close",
        brp_root_close,
        RootCloseParams,
        RootClosed
    ),
    spec!("fux/root.rename", brp_root_rename, RootRenameParams, Done),
    spec!("fux/root.order", brp_root_order, RootOrderParams, Done),
    spec!("fux/root.move", brp_root_move, RootMoveParams, Done),
    spec!(
        "fux/node.spawn",
        brp_node_spawn,
        NodeSpawnParams,
        NodeSpawned
    ),
    spec!(
        "fux/node.despawn",
        brp_node_despawn,
        NodeDespawnParams,
        Generation
    ),
    spec!(
        "fux/node.reparent",
        brp_node_reparent,
        NodeReparentParams,
        Generation
    ),
    spec!(
        "fux/node.reorder",
        brp_node_reorder,
        NodeReorderParams,
        Generation
    ),
    spec!(
        "fux/node.patch",
        brp_node_patch,
        NodePatchParams,
        Generation
    ),
    spec!("fux/viewer.list", brp_viewer_list, NoParams, ViewerList),
    spec!("fux/viewer.show", brp_viewer_show, ViewerShowParams, Done),
    spec!(
        "fux/viewer.target",
        brp_viewer_target,
        ViewerTargetParams,
        Done
    ),
    spec!("fux/viewer.zoom", brp_viewer_zoom, ViewerZoomParams, Done),
    spec!(
        "fux/viewer.unzoom",
        brp_viewer_unzoom,
        ViewerUnzoomParams,
        Done
    ),
    spec!(
        "fux/viewer.scroll",
        brp_viewer_scroll,
        ViewerScrollParams,
        Done
    ),
    spec!("fux/pane.new", brp_pane_new, PaneNewParams, PaneCreated),
    spec!("fux/pane.close", brp_pane_close, PaneCloseParams, Done),
    spec!(
        "fux/pane.send_keys",
        brp_pane_send_keys,
        PaneSendKeysParams,
        BytesWritten
    ),
    spec!(
        "fux/pane.send_bytes",
        brp_pane_send_bytes,
        PaneSendBytesParams,
        BytesWritten
    ),
    spec!(
        "fux/pane.capture",
        brp_pane_capture,
        PaneCaptureParams,
        Capture
    ),
    spec!("fux/schema", brp_schema, NoParams, SchemaTable),
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

/// Every registered method name; the allowlist test compares `RemoteMethods::methods()` to it.
pub fn allowlist() -> Vec<&'static str> {
    all_specs()
        .map(|s| s.name)
        .chain(WRAPPED.iter().map(|(n, _)| *n))
        .chain(watch::WATCHED.iter().map(|(n, _)| *n))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_follow_xterm() {
        assert_eq!(decode_key_names("C-c Enter").unwrap(), b"\x03\r");
        assert_eq!(decode_key_names("M-x Up").unwrap(), b"\x1bx\x1b[A");
        assert_eq!(decode_key_names("é").unwrap(), "é".as_bytes());
        assert!(decode_key_names("C-Enter").is_ok());
        assert!(decode_key_names("C-Up").is_err());
        assert!(decode_key_names("Bogus").is_err());
    }

    #[test]
    fn table_names_are_unique_and_prefixed() {
        let names = allowlist();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
        assert!(TABLE.iter().all(|s| s.name.starts_with("fux/")));
    }
}
