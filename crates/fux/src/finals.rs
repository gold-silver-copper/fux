//! Retention of [`FinalRecord`]s (oracle `docs/local-control-protocol.md` "Manager requests",
//! `final`): lifecycle spawns one record entity per exited pane; this module bounds them and
//! answers `fux/pane.final`.
//!
//! Bounds: at most [`MAX_FINAL_RECORDS`] records (capacity pressure evicts the oldest-closed
//! record early), at most [`MAX_FINAL_CAPTURE_BYTES`] of capture text each, retention clamped to
//! [`MAX_FINAL_RETENTION_MS`]. A read that finds no record distinguishes `evicted` from `expired`
//! from `unknown` through two rings of the most recent [`MAX_FORGOTTEN_FINAL_IDS`] pane ids each
//! ([`ForgottenFinals`]): an id is never reported evicted or expired unless a record was made
//! for it, and after enough later evictions or expiries its history is forgotten again.

use std::collections::VecDeque;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;

use crate::lifecycle::now_ms;
use crate::model::{FinalRecord, Ids, PaneId, Phase, ServerInstance};

/// Records retained at once, server-wide.
pub const MAX_FINAL_RECORDS: usize = 128;
/// Capture text per record; trailing lines are dropped whole.
pub const MAX_FINAL_CAPTURE_BYTES: usize = 128 * 1024;
/// Ceiling on a record's retention after its pane closed (four hours).
pub const MAX_FINAL_RETENTION_MS: u64 = 4 * 60 * 60 * 1000;
/// Pane ids each ring of [`ForgottenFinals`] remembers.
pub const MAX_FORGOTTEN_FINAL_IDS: usize = 1024;

/// Why a `final` read found no record; the JSON-RPC layer maps each to its code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalError {
    /// The pane is still live; `pane.capture` has current evidence.
    Pending,
    /// The instance nonce names another server incarnation.
    Conflict,
    /// The record existed and the cap dropped it before its retention elapsed.
    Evicted,
    /// The record existed and its retention elapsed.
    Expired,
    /// No record was retained for this id, or its history has been forgotten.
    Unknown,
}

impl FinalError {
    /// The oracle's error code word (`data.reason`).
    pub fn reason(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Conflict => "conflict",
            Self::Evicted => "evicted",
            Self::Expired => "expired",
            Self::Unknown => "unknown",
        }
    }
}

impl core::fmt::Display for FinalError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Pending => "pane has not been released; use capture for current evidence",
            Self::Conflict => {
                "server instance changed; final evidence belongs to the previous server"
            }
            Self::Evicted => {
                "the server dropped the final record under load before its retention elapsed"
            }
            Self::Expired => "final evidence expired; its retention elapsed",
            Self::Unknown => "no final record was retained for this pane on this server instance",
        })
    }
}

impl std::error::Error for FinalError {}

/// The two bounded rings of pane ids whose records were dropped: by the cap and by expiry.
#[derive(Resource, Debug, Default)]
pub struct ForgottenFinals {
    evicted: VecDeque<PaneId>,
    expired: VecDeque<PaneId>,
}

impl ForgottenFinals {
    fn push(ring: &mut VecDeque<PaneId>, pane: PaneId) {
        if ring.len() == MAX_FORGOTTEN_FINAL_IDS {
            ring.pop_front();
        }
        ring.push_back(pane);
    }

    pub fn evicted(&self, pane: PaneId) -> bool {
        self.evicted.contains(&pane)
    }

    pub fn expired(&self, pane: PaneId) -> bool {
        self.expired.contains(&pane)
    }
}

pub struct FinalsPlugin;

impl Plugin for FinalsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ForgottenFinals>()
            .add_systems(Update, maintain.after(Phase::Lifecycle));
    }
}

/// The record entity for a pane id, if any.
fn record_of(world: &mut World, pane: PaneId) -> Option<(Entity, u64)> {
    world
        .query::<(Entity, &FinalRecord)>()
        .iter(world)
        .find_map(|(e, r)| (r.pane == pane).then_some((e, r.expires_ms)))
}

/// The retained record for `pane`, or why there is none. `instance` is the caller's nonce.
pub fn read<'w>(
    world: &'w mut World,
    instance: &str,
    pane: PaneId,
) -> Result<&'w FinalRecord, FinalError> {
    if instance != world.resource::<ServerInstance>().nonce {
        return Err(FinalError::Conflict);
    }
    if world.resource::<Ids>().pane(pane).is_some() {
        return Err(FinalError::Pending);
    }
    let now = now_ms(world);
    if let Some((entity, expires_ms)) = record_of(world, pane) {
        // Past its deadline but not yet swept: the outcome is the same as after the sweep.
        if expires_ms <= now {
            return Err(FinalError::Expired);
        }
        return world.get::<FinalRecord>(entity).ok_or(FinalError::Unknown);
    }
    let forgotten = world.resource::<ForgottenFinals>();
    if forgotten.evicted(pane) {
        return Err(FinalError::Evicted);
    }
    if forgotten.expired(pane) {
        return Err(FinalError::Expired);
    }
    Err(FinalError::Unknown)
}

/// `Update`, after lifecycle: new records are bounded (capture bytes, retention ceiling), then
/// expired ones are swept into the expiry ring, then capacity pressure evicts the oldest-closed
/// record into the eviction ring, so pressure only ever drops a record that was still valid.
fn maintain(
    world: &mut World,
    added: &mut QueryState<&mut FinalRecord, Added<FinalRecord>>,
    records: &mut QueryState<(Entity, &FinalRecord)>,
) {
    for mut record in added.iter_mut(world) {
        bound(&mut record);
    }
    let now = now_ms(world);
    let expired: Vec<(Entity, PaneId)> = records
        .iter(world)
        .filter(|(_, r)| r.expires_ms <= now)
        .map(|(e, r)| (e, r.pane))
        .collect();
    for (entity, pane) in expired {
        world.despawn(entity);
        ForgottenFinals::push(&mut world.resource_mut::<ForgottenFinals>().expired, pane);
    }
    let mut count = records.iter(world).count();
    while count > MAX_FINAL_RECORDS {
        let Some((entity, pane)) = records
            .iter(world)
            .min_by_key(|(_, r)| (r.exited_ms, r.pane))
            .map(|(e, r)| (e, r.pane))
        else {
            break;
        };
        world.despawn(entity);
        ForgottenFinals::push(&mut world.resource_mut::<ForgottenFinals>().evicted, pane);
        count -= 1;
    }
}

/// Clamps a fresh record to the capture and retention ceilings.
fn bound(record: &mut FinalRecord) {
    let ceiling = record.exited_ms.saturating_add(MAX_FINAL_RETENTION_MS);
    if record.expires_ms > ceiling {
        record.expires_ms = ceiling;
    }
    let mut bytes = 0_usize;
    let keep = record
        .screen
        .iter()
        .take_while(|line| {
            bytes = bytes.saturating_add(line.len());
            bytes <= MAX_FINAL_CAPTURE_BYTES
        })
        .count();
    if keep < record.screen.len() {
        record.screen.truncate(keep);
    }
}
