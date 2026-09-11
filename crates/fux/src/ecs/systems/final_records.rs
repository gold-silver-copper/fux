//! Bounded terminal evidence, independent of the lifetime of pane and workspace entities.
//!
//! A `final` read distinguishes why a record is absent: `pending` (the pane is still live),
//! `conflict` (another server instance), `evicted` (the record cap dropped it before its
//! `expires_ms`), `expired` (its retention elapsed) and `unknown` (no record was ever made, or
//! the id has fallen off the bounded [`ForgottenFinals`] rings).
use crate::ecs::components::{Pane, PaneState};
use crate::ecs::resources::{
    Clock, Deadlines, FinalRecords, ForgottenFinals, Ids, MAX_FINAL_RECORDS, RetainedFinal,
    ServerIdentity,
};
use crate::proto::control::{CommandResult, ErrorCode, FinalRecord, Reply};
use bevy_ecs::prelude::*;

pub fn remember(world: &mut World, pane: Entity) {
    let now = world.resource::<Clock>().now_ms;
    let Some(mut component) = world.get_mut::<Pane>(pane) else {
        return;
    };
    if matches!(component.state, PaneState::Starting) {
        return;
    }
    let record = FinalRecord {
        pane: component.id,
        workspace: component.workspace_name.clone(),
        stream: component.workspace_stream,
        command: component.argv.clone(),
        cwd: component.cwd.clone(),
        exit_status: component.state.exit_code(),
        input_sequence: component.input_sequence,
        capture: component.terminal.capture_snapshot(0, false, 131_072, None),
    };
    let record = RetainedFinal {
        record,
        closed_ms: now,
        // The launcher's retention, clamped at creation; nothing here re-derives it.
        expires_ms: now.saturating_add(component.final_retain_ms),
    };
    // Expire first so capacity pressure only ever evicts a record that was still valid.
    expire(world);
    let mut records = world.resource_mut::<FinalRecords>();
    if records.0.len() >= MAX_FINAL_RECORDS {
        let oldest = records
            .0
            .values()
            .min_by_key(|retained| (retained.closed_ms, retained.record.pane))
            .map(|retained| retained.record.pane);
        if let Some(oldest) = oldest {
            records.0.remove(&oldest);
            world.resource_mut::<ForgottenFinals>().evicted(oldest);
        }
    }
    world
        .resource_mut::<FinalRecords>()
        .0
        .insert(record.record.pane, record);
    expire(world);
}

pub fn read(world: &World, instance: &str, pane: crate::ids::PaneId) -> Reply {
    if instance != world.resource::<ServerIdentity>().instance_nonce {
        return Reply::failed(
            0,
            ErrorCode::Conflict,
            "server instance changed; final evidence belongs to the previous server",
        );
    }
    if world.resource::<Ids>().pane(pane).is_some() {
        return Reply::failed(
            0,
            ErrorCode::Pending,
            "pane has not been released; use capture for current evidence",
        );
    }
    let now = world.resource::<Clock>().now_ms;
    if let Some(retained) = world.resource::<FinalRecords>().0.get(&pane) {
        if retained.expires_ms > now {
            return Reply::Completed {
                id: 0,
                result: CommandResult::Final {
                    record: Box::new(retained.record.clone()),
                },
            };
        }
        // Past its deadline but not yet swept: the outcome is the same as after the sweep.
        return expired();
    }
    let forgotten = world.resource::<ForgottenFinals>();
    if forgotten.evicted.contains(&pane) {
        return Reply::failed(
            0,
            ErrorCode::Evicted,
            "the server dropped the final record under load before its retention elapsed",
        );
    }
    if forgotten.expired.contains(&pane) {
        return expired();
    }
    Reply::failed(
        0,
        ErrorCode::Unknown,
        "no final record was retained for this pane on this server instance",
    )
}

fn expired() -> Reply {
    Reply::failed(
        0,
        ErrorCode::Expired,
        "final evidence expired; its retention elapsed",
    )
}

pub fn expire(world: &mut World) {
    let now = world.resource::<Clock>().now_ms;
    let expired: Vec<_> = world
        .resource::<FinalRecords>()
        .0
        .values()
        .filter(|retained| retained.expires_ms <= now)
        .map(|retained| retained.record.pane)
        .collect();
    if !expired.is_empty() {
        let mut records = world.resource_mut::<FinalRecords>();
        for pane in &expired {
            records.0.remove(pane);
        }
        let mut forgotten = world.resource_mut::<ForgottenFinals>();
        for pane in expired {
            forgotten.expired(pane);
        }
    }
    if let Some(next) = world
        .resource::<FinalRecords>()
        .0
        .values()
        .map(|retained| retained.expires_ms)
        .min()
    {
        world.resource_mut::<Deadlines>().propose(next);
    }
}
