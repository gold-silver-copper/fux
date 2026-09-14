//! Generic, bounded input receipts. PTY writers report delivery; ECS never blocks on I/O.
use crate::ecs::components::Pane;
use crate::ecs::messages::{Effect, Inbound};
use crate::ecs::resources::{
    Clock, Deadlines, Ids, InputOperations, InputRecord, MAX_INPUT_OPERATIONS,
};
use crate::ecs::support::{Failure, effect};
use crate::proto::control::{
    CommandResult, ErrorCode, InputReceipt, InputState, MAX_INPUT_RETENTION_MS,
};
use bevy_ecs::prelude::*;

/// `retain_ms` is the caller's retention: zero is rejected, larger values are clamped to
/// [`MAX_INPUT_RETENTION_MS`], and the receipt's `expires_ms` reflects the clamp.
pub fn reserve(
    world: &mut World,
    workspace: Entity,
    pane: Entity,
    retain_ms: u64,
) -> Result<CommandResult, Failure> {
    if retain_ms == 0 {
        return Err(Failure::invalid("retain_ms must be nonzero"));
    }
    let retain_ms = retain_ms.min(MAX_INPUT_RETENTION_MS);
    expire(world);
    let component = world
        .get::<Pane>(pane)
        .ok_or_else(|| Failure::not_found("pane not found"))?;
    if !component.state.accepts_input() {
        return Err(Failure::conflict("pane is not accepting input"));
    }
    let mut receipt = InputReceipt {
        operation: 0,
        pane: component.id,
        state: InputState::Reserved,
        revision: component.terminal.revision(),
        input_sequence: component.input_sequence,
        expires_ms: world.resource::<Clock>().now_ms.saturating_add(retain_ms),
        bytes_written: 0,
        error: None,
    };
    let mut operations = world.resource_mut::<InputOperations>();
    if operations.records.len() >= MAX_INPUT_OPERATIONS {
        return Err(Failure::limit(
            "input receipt capacity reached; wait for expiry",
        ));
    }
    let operation = operations
        .next
        .checked_add(1)
        .ok_or_else(|| Failure::limit("input operation ids exhausted"))?;
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

pub fn status(world: &World, workspace: Entity, operation: u64) -> Result<CommandResult, Failure> {
    let record = record(world, workspace, operation)?;
    Ok(CommandResult::Input {
        receipt: record.receipt.clone(),
    })
}

/// Manager authority can read retained evidence after the originating route disappears.
/// This never rebinds a reservation or permits submission from another workspace.
pub fn manager_status(
    world: &World,
    instance: &str,
    pane: crate::ids::PaneId,
    operation: u64,
) -> Result<CommandResult, Failure> {
    if instance
        != world
            .resource::<crate::ecs::resources::ServerIdentity>()
            .instance_nonce
    {
        return Err(Failure::conflict("server instance changed"));
    }
    let record = world
        .resource::<InputOperations>()
        .records
        .get(&operation)
        .filter(|record| {
            record.receipt.pane == pane
                && record.receipt.expires_ms > world.resource::<Clock>().now_ms
        })
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::Expired,
                "input operation unavailable or expired; delivery outcome is unknown",
            )
        })?;
    Ok(CommandResult::Input {
        receipt: record.receipt.clone(),
    })
}

fn record(world: &World, workspace: Entity, operation: u64) -> Result<&InputRecord, Failure> {
    world
        .resource::<InputOperations>()
        .records
        .get(&operation)
        .filter(|record| {
            record.workspace == workspace
                && record.receipt.expires_ms > world.resource::<Clock>().now_ms
        })
        .ok_or_else(|| {
            Failure::new(
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
) -> Result<CommandResult, Failure> {
    let record = record(world, workspace, operation)?;
    if let Some(submitted) = &record.bytes {
        return if submitted == &bytes {
            Ok(CommandResult::Input {
                receipt: record.receipt.clone(),
            })
        } else {
            Err(Failure::conflict(
                "operation already submitted with different bytes",
            ))
        };
    }
    if record.receipt.state != InputState::Reserved {
        return Err(Failure::conflict(
            "reservation is no longer usable; reserve again explicitly",
        ));
    }
    if bytes.is_empty() {
        return Err(Failure::invalid("input must not be empty"));
    }
    let pane = record.receipt.pane;
    let sequence = record.receipt.input_sequence;
    let entity = world
        .resource::<Ids>()
        .pane(pane)
        .ok_or_else(|| Failure::not_found("pane no longer exists"))?;
    if crate::ecs::support::pane_workspace(world, entity) != Some(workspace) {
        return Err(Failure::conflict(
            "pane changed workspace; reserve on its current route",
        ));
    }
    let mut component = world
        .get_mut::<Pane>(entity)
        .ok_or_else(|| Failure::not_found("pane no longer exists"))?;
    if !component.state.accepts_input() || component.input_sequence != sequence {
        return Err(Failure::conflict(
            "pane stopped accepting input or another writer intervened; reserve again explicitly",
        ));
    }
    let next = sequence
        .checked_add(1)
        .ok_or_else(|| Failure::limit("input sequence exhausted"))?;
    component.input_sequence = next;
    let revision = component.terminal.revision();
    let mut operations = world.resource_mut::<InputOperations>();
    let record = operations
        .records
        .get_mut(&operation)
        .ok_or_else(|| Failure::new(ErrorCode::Expired, "operation expired"))?;
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

/// A plain (non-exclusive) system: it needs no command flush and, with nothing tracked, costs
/// one pass over the step's inbound batch.
pub fn apply_completions(
    inbound: Res<Messages<Inbound>>,
    clock: Res<Clock>,
    mut operations: ResMut<InputOperations>,
) {
    if operations.records.is_empty() {
        return;
    }
    let now = clock.now_ms;
    let completions = inbound
        .iter_current_update_messages()
        .filter_map(|message| match message {
            Inbound::InputCompleted {
                pane,
                operation,
                bytes_written,
                error,
            } => Some((*pane, *operation, *bytes_written, error.as_deref())),
            _ => None,
        });
    for (pane, operation, bytes_written, error) in completions {
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
                    .unwrap_or("incomplete PTY write")
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
