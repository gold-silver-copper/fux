//! `fux/input.{reserve,submit,status}` and `fux/pane.final`: the JSON-RPC face of
//! [`crate::input_ops`] and [`crate::finals`]. `final` failures carry the oracle's code word in
//! `data.reason` and one code each in `-32010..=-32014`.

use bevy_ecs::prelude::*;
use bevy_remote::{BrpError, BrpResult, error_codes};
use serde_json::json;

use super::methods::{Request, codes, error, handler, invalid, spec, to_value};
use super::schema::described;
use crate::finals::{self, FinalError};
use crate::input_ops::{self, InputError, Receipt};
use crate::model::{Ids, InputOperation, InputState, OperationOn, PaneId};

/// `fux/pane.final` failure codes, one per [`FinalError`].
pub mod final_codes {
    pub const PENDING: i16 = -32010;
    pub const CONFLICT: i16 = -32011;
    pub const EVICTED: i16 = -32012;
    pub const EXPIRED: i16 = -32013;
    pub const UNKNOWN: i16 = -32014;
}

described!(
    pub struct InputReserveParams {
        pub pane: u64,
        /// Milliseconds the receipt stays readable from the reservation; clamped to
        /// `MAX_INPUT_RETENTION_MS`, which `expires_ms` reflects.
        pub retain_ms: u64,
    }
);
described!(
    pub struct InputSubmitParams {
        pub operation: u64,
        /// Escape notation: `\n \r \t \e \\ \0 \xHH`, everything else literal.
        pub keys: String,
    }
);
described!(
    pub struct InputStatusParams {
        pub operation: u64,
    }
);
described!(
    /// The receipt every `input.*` method returns.
    pub struct InputReceipt {
        pub operation: u64,
        /// Absent once the pane exited and the receipt was detached.
        pub pane: Option<u64>,
        /// `reserved`, `submitted`, `uncertain` or `expired`.
        pub state: String,
        pub bytes_written: usize,
        /// The pane's terminal sequence when the bytes were submitted.
        pub seq: Option<u64>,
        pub expires_ms: u64,
    }
);
described!(
    pub struct PaneFinalParams {
        pub pane: u64,
    }
);
described!(
    pub struct PaneFinal {
        pub pane: u64,
        pub workspace: String,
        pub stream: String,
        pub exit_code: i32,
        pub title: String,
        pub last_seq: u64,
        pub exited_ms: u64,
        pub expires_ms: u64,
        pub screen: Vec<String>,
    }
);

fn input_error(e: InputError) -> BrpError {
    match e {
        InputError::NoSuchOperation(_) => error(codes::NOT_FOUND, e.to_string()),
        InputError::ZeroRetention | InputError::Empty => {
            error(error_codes::INVALID_PARAMS, e.to_string())
        }
        _ => invalid(e.to_string()),
    }
}

fn final_error(e: FinalError) -> BrpError {
    let code = match e {
        FinalError::Pending => final_codes::PENDING,
        FinalError::Conflict => final_codes::CONFLICT,
        FinalError::Evicted => final_codes::EVICTED,
        FinalError::Expired => final_codes::EXPIRED,
        FinalError::Unknown => final_codes::UNKNOWN,
    };
    BrpError {
        code,
        message: e.to_string(),
        data: Some(json!({ "reason": e.reason() })),
    }
}

fn receipt(world: &World, operation: Entity) -> BrpResult {
    let entity = world.entity(operation);
    let (Some(op), Some(receipt)) = (entity.get::<InputOperation>(), entity.get::<Receipt>())
    else {
        return Err(BrpError::internal("operation without receipt"));
    };
    let pane = entity
        .get::<OperationOn>()
        .and_then(|on| world.get::<PaneId>(on.0))
        .map(|id| id.0);
    let (state, bytes_written, seq) = match op.state {
        InputState::Reserved => ("reserved", 0, None),
        InputState::Submitted { seq, bytes } => ("submitted", bytes, Some(seq)),
        InputState::Uncertain => ("uncertain", 0, None),
        InputState::Expired => ("expired", 0, None),
    };
    to_value(InputReceipt {
        operation: op.id,
        pane,
        state: state.to_owned(),
        bytes_written,
        seq,
        expires_ms: receipt.expires_ms,
    })
}

/// An operation by public id inside the token's grant.
fn operation(req: &Request, world: &mut World, id: u64) -> Result<Entity, BrpError> {
    let operation = input_ops::find(world, id)
        .ok_or_else(|| error(codes::NOT_FOUND, format!("operation {id} not found")))?;
    let workspace = world
        .get::<Receipt>(operation)
        .map(|r| r.workspace.clone())
        .unwrap_or_default();
    req.cover_name(&workspace)?;
    Ok(operation)
}

fn input_reserve(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: InputReserveParams = req.parse()?;
    let pane = req.pane(world, params.pane)?;
    let operation = input_ops::reserve(world, pane, params.retain_ms).map_err(input_error)?;
    receipt(world, operation)
}

fn input_submit(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: InputSubmitParams = req.parse()?;
    if params.keys.len() > input_ops::MAX_INPUT_BYTES {
        return Err(invalid(format!(
            "at most {} bytes per request",
            input_ops::MAX_INPUT_BYTES
        )));
    }
    operation(&req, world, params.operation)?;
    let bytes = input_ops::parse_keys(&params.keys).map_err(|e| invalid(e.to_string()))?;
    let operation = input_ops::submit(world, params.operation, bytes).map_err(input_error)?;
    receipt(world, operation)
}

fn input_status(mut req: Request, world: &mut World) -> BrpResult {
    let params: InputStatusParams = req.parse()?;
    let operation = operation(&req, world, params.operation)?;
    receipt(world, operation)
}

fn pane_final(mut req: Request, world: &mut World) -> BrpResult {
    let params: PaneFinalParams = req.parse()?;
    let instance = req.instance().unwrap_or_default().to_owned();
    // A live pane is authorised through its workspace before `pending` reveals it exists.
    if world.resource::<Ids>().pane(PaneId(params.pane)).is_some() {
        req.pane(world, params.pane)?;
    }
    let record = finals::read(world, &instance, PaneId(params.pane)).map_err(final_error)?;
    req.cover_name(&record.workspace)?;
    to_value(PaneFinal {
        pane: record.pane.0,
        workspace: record.workspace.clone(),
        stream: record.stream.clone(),
        exit_code: record.exit_code,
        title: record.title.clone(),
        last_seq: record.last_seq,
        exited_ms: record.exited_ms,
        expires_ms: record.expires_ms,
        screen: record.screen.clone(),
    })
}

handler!(brp_input_reserve, input_reserve);
handler!(brp_input_submit, input_submit);
handler!(brp_input_status, input_status);
handler!(brp_pane_final, pane_final);

pub const METHODS: &[super::methods::MethodSpec] = &[
    spec!(
        "fux/input.reserve",
        brp_input_reserve,
        InputReserveParams,
        InputReceipt
    ),
    spec!(
        "fux/input.submit",
        brp_input_submit,
        InputSubmitParams,
        InputReceipt
    ),
    spec!(
        "fux/input.status",
        brp_input_status,
        InputStatusParams,
        InputReceipt
    ),
    spec!("fux/pane.final", brp_pane_final, PaneFinalParams, PaneFinal),
];
