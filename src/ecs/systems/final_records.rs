//! Bounded terminal evidence, independent of the lifetime of pane and workspace entities.
use crate::ecs::components::{Pane, PaneState, Workspace};
use crate::ecs::resources::{
    Clock, Deadlines, FINAL_RETENTION_MS, FinalRecords, Ids, MAX_FINAL_RECORDS, ServerInstance,
};
use crate::proto::control::{CommandResult, ErrorCode, FinalRecord, Reply};
use bevy_ecs::prelude::*;

pub fn remember(world: &mut World, pane: Entity) {
    let Some(workspace) = crate::ecs::support::pane_workspace(world, pane) else {
        return;
    };
    let Some((name, stream)) = world
        .get::<Workspace>(workspace)
        .map(|workspace| (workspace.name.clone(), workspace.events.cursor().stream))
    else {
        return;
    };
    let now = world.resource::<Clock>().now_ms;
    let Some(mut component) = world.get_mut::<Pane>(pane) else {
        return;
    };
    if matches!(component.state, PaneState::Starting) {
        return;
    }
    let record = FinalRecord {
        pane: component.id,
        workspace: name,
        stream,
        command: component.argv.clone(),
        cwd: component.cwd.clone(),
        exit_status: component.state.exit_code(),
        closed_ms: now,
        expires_ms: now.saturating_add(FINAL_RETENTION_MS),
        input_sequence: component.input_sequence,
        capture: component.terminal.capture_snapshot(0, false, 131_072, None),
    };
    let mut records = world.resource_mut::<FinalRecords>();
    if records.0.len() >= MAX_FINAL_RECORDS {
        let oldest = records
            .0
            .values()
            .min_by_key(|record| (record.closed_ms, record.pane))
            .map(|record| record.pane);
        if let Some(oldest) = oldest {
            records.0.remove(&oldest);
        }
    }
    records.0.insert(record.pane, record);
    expire(world);
}

pub fn read(world: &World, instance: &str, pane: crate::ids::PaneId) -> Reply {
    if instance != world.resource::<ServerInstance>().0 {
        return Reply::failed(
            0,
            ErrorCode::Conflict,
            "server instance changed; final evidence belongs to the previous server",
        );
    }
    if world.resource::<Ids>().pane(pane).is_some() {
        return Reply::failed(
            0,
            ErrorCode::Conflict,
            "pane has not been released; use capture for current evidence",
        );
    }
    match world
        .resource::<FinalRecords>()
        .0
        .get(&pane)
        .filter(|record| record.expires_ms > world.resource::<Clock>().now_ms)
    {
        Some(record) => Reply::Completed {
            id: 0,
            result: CommandResult::Final {
                record: Box::new(record.clone()),
            },
        },
        None => Reply::failed(
            0,
            ErrorCode::Expired,
            "final evidence unknown, evicted, or expired",
        ),
    }
}

pub fn expire(world: &mut World) {
    let now = world.resource::<Clock>().now_ms;
    world
        .resource_mut::<FinalRecords>()
        .0
        .retain(|_, record| record.expires_ms > now);
    if let Some(next) = world
        .resource::<FinalRecords>()
        .0
        .values()
        .map(|record| record.expires_ms)
        .min()
    {
        world.resource_mut::<Deadlines>().propose(next);
    }
}
