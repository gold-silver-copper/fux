//! Generic, bounded input receipts. PTY writers report delivery; ECS never blocks on I/O.
use crate::ecs::components::Pane;
use crate::ecs::messages::{Effect, Inbound};
use crate::ecs::resources::{
    Clock, Deadlines, INPUT_RETENTION_MS, Ids, InputOperations, InputRecord, MAX_INPUT_OPERATIONS,
};
use crate::ecs::support::effect;
use crate::proto::control::{CommandResult, ErrorCode, InputReceipt, InputState, Reply};
use bevy_ecs::prelude::*;

fn failure(id: u64, code: ErrorCode, message: &str) -> Reply {
    Reply::failed(id, code, message)
}

pub fn reserve(
    world: &mut World,
    workspace: Entity,
    pane: Entity,
    id: u64,
) -> Result<CommandResult, Reply> {
    expire(world);
    let component = world
        .get::<Pane>(pane)
        .ok_or_else(|| failure(id, ErrorCode::NotFound, "pane not found"))?;
    if !component.state.accepts_input() {
        return Err(failure(
            id,
            ErrorCode::Conflict,
            "pane is not accepting input",
        ));
    }
    let mut receipt = InputReceipt {
        operation: 0,
        pane: component.id,
        state: InputState::Reserved,
        revision: component.terminal.revision(),
        input_sequence: component.input_sequence,
        expires_ms: world
            .resource::<Clock>()
            .now_ms
            .saturating_add(INPUT_RETENTION_MS),
        bytes_written: 0,
        error: None,
    };
    let mut operations = world.resource_mut::<InputOperations>();
    if operations.records.len() >= MAX_INPUT_OPERATIONS {
        return Err(failure(
            id,
            ErrorCode::Limit,
            "input receipt capacity reached; wait for expiry",
        ));
    }
    let operation = operations
        .next
        .checked_add(1)
        .ok_or_else(|| failure(id, ErrorCode::Limit, "input operation ids exhausted"))?;
    operations.next = operation;
    receipt.operation = operation;
    operations.records.insert(
        operation,
        InputRecord {
            workspace,
            receipt: receipt.clone(),
            bytes: None,
        },
    );
    world
        .resource_mut::<Deadlines>()
        .propose(receipt.expires_ms);
    Ok(CommandResult::Input { receipt })
}

pub fn status(
    world: &World,
    workspace: Entity,
    operation: u64,
    id: u64,
) -> Result<CommandResult, Reply> {
    let record = record(world, workspace, operation, id)?;
    Ok(CommandResult::Input {
        receipt: record.receipt.clone(),
    })
}

fn record(
    world: &World,
    workspace: Entity,
    operation: u64,
    id: u64,
) -> Result<&InputRecord, Reply> {
    world
        .resource::<InputOperations>()
        .records
        .get(&operation)
        .filter(|record| {
            record.workspace == workspace
                && record.receipt.expires_ms > world.resource::<Clock>().now_ms
        })
        .ok_or_else(|| {
            failure(
                id,
                ErrorCode::Expired,
                "input operation unavailable or expired; delivery outcome is unknown",
            )
        })
}

pub fn submit(
    world: &mut World,
    workspace: Entity,
    operation: u64,
    bytes: Vec<u8>,
    id: u64,
) -> Result<CommandResult, Reply> {
    let record = record(world, workspace, operation, id)?;
    if let Some(submitted) = &record.bytes {
        return if submitted == &bytes {
            Ok(CommandResult::Input {
                receipt: record.receipt.clone(),
            })
        } else {
            Err(failure(
                id,
                ErrorCode::Conflict,
                "operation already submitted with different bytes",
            ))
        };
    }
    if bytes.is_empty() {
        return Err(failure(
            id,
            ErrorCode::InvalidRequest,
            "input must not be empty",
        ));
    }
    let pane = record.receipt.pane;
    let sequence = record.receipt.input_sequence;
    let entity = world
        .resource::<Ids>()
        .pane(pane)
        .ok_or_else(|| failure(id, ErrorCode::NotFound, "pane no longer exists"))?;
    let mut component = world
        .get_mut::<Pane>(entity)
        .ok_or_else(|| failure(id, ErrorCode::NotFound, "pane no longer exists"))?;
    if !component.state.accepts_input() || component.input_sequence != sequence {
        return Err(failure(
            id,
            ErrorCode::Conflict,
            "pane stopped accepting input or another writer intervened; reserve again explicitly",
        ));
    }
    let next = sequence
        .checked_add(1)
        .ok_or_else(|| failure(id, ErrorCode::Limit, "input sequence exhausted"))?;
    component.input_sequence = next;
    let revision = component.terminal.revision();
    let mut operations = world.resource_mut::<InputOperations>();
    let record = operations
        .records
        .get_mut(&operation)
        .ok_or_else(|| failure(id, ErrorCode::Expired, "operation expired"))?;
    record.receipt.input_sequence = next;
    record.receipt.revision = revision;
    record.receipt.state = InputState::Queued;
    record.bytes = Some(bytes.clone());
    let receipt = record.receipt.clone();
    crate::ecs::support::event(
        world,
        workspace,
        crate::proto::control::Event::WorkspaceChanged { id: 0 },
    );
    effect(
        world,
        Effect::WriteTrackedInput {
            pane,
            operation,
            bytes,
        },
    );
    Ok(CommandResult::Input { receipt })
}

pub fn apply_completions(world: &mut World) {
    let completions: Vec<_> = world
        .resource::<Messages<Inbound>>()
        .iter_current_update_messages()
        .filter_map(|message| match message {
            Inbound::InputCompleted {
                pane,
                operation,
                bytes_written,
                error,
            } => Some((*pane, *operation, *bytes_written, error.clone())),
            _ => None,
        })
        .collect();
    let now = world.resource::<Clock>().now_ms;
    for (pane, operation, bytes_written, error) in completions {
        let mut operations = world.resource_mut::<InputOperations>();
        let Some(record) = operations.records.get_mut(&operation) else {
            continue;
        };
        if record.receipt.pane != pane
            || record.receipt.state != InputState::Queued
            || record.receipt.expires_ms <= now
        {
            continue;
        }
        let length = record.bytes.as_ref().map_or(0, Vec::len);
        record.receipt.bytes_written = bytes_written.min(length);
        record.receipt.state = if error.is_none() && bytes_written == length {
            InputState::Delivered
        } else {
            InputState::Failed
        };
        record.receipt.error = if record.receipt.state == InputState::Failed {
            Some(
                error
                    .unwrap_or_else(|| "incomplete PTY write".into())
                    .chars()
                    .take(256)
                    .collect(),
            )
        } else {
            None
        };
    }
}

pub fn expire(world: &mut World) {
    let now = world.resource::<Clock>().now_ms;
    world
        .resource_mut::<InputOperations>()
        .records
        .retain(|_, record| record.receipt.expires_ms > now);
    let next = world
        .resource::<InputOperations>()
        .records
        .values()
        .map(|record| record.receipt.expires_ms)
        .min();
    if let Some(next) = next {
        world.resource_mut::<Deadlines>().propose(next);
    }
}
