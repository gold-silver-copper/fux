//! Input receipts (`fux/input.{reserve,submit,status}`, oracle `docs/local-control-protocol.md`
//! "Identity, captures and tracked input"): a caller reserves an operation on a pane, submits
//! escaped keys once through it, and reads the receipt back until it expires.
//!
//! An operation is an entity carrying the model's [`InputOperation`] (public id and state) plus
//! this module's [`Receipt`] (scope and retention); [`OperationOn`] links it to its pane while
//! the pane lives. State machine: `Reserved → Submitted { seq, bytes } | Uncertain | Expired`.
//! Delivery is the runner draining `Effect::WritePty` after the update that submitted, so a pane
//! that is already `Exited` when the next update ingests leaves the write [`Uncertain`]; a pane
//! exit also detaches every receipt on it (the operation loses `OperationOn`) so the outcome stays
//! readable until retention elapses, as the oracle's receipt table did. Retention is the caller's
//! `retain_ms` clamped to [`MAX_INPUT_RETENTION_MS`]; an expired receipt is kept for one more
//! retention window as `Expired`, then despawned.

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;

use crate::model::{
    Effect, InputOperation, InputState, OperationOn, PaneIn, Process, WorkspaceName,
};
use crate::terminal::Terminal;

/// Receipts retained at once, server-wide; capacity pressure refuses new reservations.
pub const MAX_INPUT_OPERATIONS: usize = 128;
/// Ceiling on `retain_ms` (ten minutes); `expires_ms` reports the clamp.
pub const MAX_INPUT_RETENTION_MS: u64 = 600_000;
/// Bound on one submission's decoded bytes.
pub const MAX_INPUT_BYTES: usize = 64 * 1024;

/// Scope and retention of a receipt: the workspace name the reservation was made under (kept
/// as text so an authority check works after the workspace entity is gone), when the receipt
/// expires and the retention actually applied.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub workspace: String,
    pub expires_ms: u64,
    pub retain_ms: u64,
}

/// A submission whose `Effect::WritePty` has not been drained yet: present from the submit until
/// the next update's ingest settled the pane's fate.
#[derive(Component, Debug, Default)]
struct Unconfirmed;

/// What was submitted, so an identical resubmission returns the receipt without writing again
/// and a different one conflicts.
#[derive(Component, Debug)]
struct SubmittedBytes(Vec<u8>);

/// Monotonic public operation ids and the cached query over every operation, so
/// `fux/input.*` lookups and the sweep do not rebuild a `QueryState` per call.
#[derive(Resource)]
struct Operations {
    next: u64,
    all: QueryState<(Entity, &'static mut InputOperation, &'static Receipt)>,
}

impl FromWorld for Operations {
    fn from_world(world: &mut World) -> Self {
        Self {
            next: 0,
            all: world.query(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// A malformed key string in escape notation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    UnknownEscape(char),
    /// `\x` without two hexadecimal digits.
    BadHex,
    TrailingBackslash,
}

impl core::fmt::Display for KeyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownEscape(c) => write!(f, "unknown escape `\\{c}`"),
            Self::BadHex => write!(f, "`\\x` requires exactly two hexadecimal digits"),
            Self::TrailingBackslash => write!(f, "trailing backslash"),
        }
    }
}

impl std::error::Error for KeyError {}

/// Typed refusal; every variant leaves the World untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputError {
    NoSuchOperation(u64),
    /// Only a `Live` process accepts input.
    PaneNotLive(Process),
    /// The operation's pane exited and the receipt was detached.
    PaneGone,
    ZeroRetention,
    Capacity,
    Expired,
    /// The reservation is `Uncertain`: it cannot be submitted again.
    Unusable,
    /// Already submitted with different bytes.
    Conflict,
    Empty,
    TooLong {
        bytes: usize,
        max: usize,
    },
}

impl core::fmt::Display for InputError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoSuchOperation(id) => write!(f, "operation {id} not found"),
            Self::PaneNotLive(p) => write!(f, "pane is {p:?}; input needs a live process"),
            Self::PaneGone => write!(f, "the operation's pane exited; reserve again explicitly"),
            Self::ZeroRetention => write!(f, "retain_ms must be nonzero"),
            Self::Capacity => write!(
                f,
                "input receipt capacity of {MAX_INPUT_OPERATIONS} reached; wait for expiry"
            ),
            Self::Expired => write!(f, "operation expired"),
            Self::Unusable => write!(
                f,
                "reservation is no longer usable; reserve again explicitly"
            ),
            Self::Conflict => write!(f, "operation already submitted with different bytes"),
            Self::Empty => write!(f, "input must not be empty"),
            Self::TooLong { bytes, max } => write!(f, "{bytes} input bytes exceed {max}"),
        }
    }
}

impl std::error::Error for InputError {}

type R<T> = Result<T, InputError>;

// ---------------------------------------------------------------------------------------------
// Key notation
// ---------------------------------------------------------------------------------------------

/// Escape notation for pane input: `\n \r \t \e \\ \0 \xHH`, everything else literal UTF-8.
pub fn parse_keys(input: &str) -> Result<Vec<u8>, KeyError> {
    let mut out = Vec::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut encoded = [0_u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut encoded).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('e') => out.push(0x1b),
            Some('\\') => out.push(b'\\'),
            Some('0') => out.push(0),
            Some('x') => {
                let hi = chars.next().and_then(|c| c.to_digit(16));
                let lo = chars.next().and_then(|c| c.to_digit(16));
                let (Some(hi), Some(lo)) = (hi, lo) else {
                    return Err(KeyError::BadHex);
                };
                out.push(u8::try_from((hi << 4) | lo).unwrap_or(0));
            }
            Some(other) => return Err(KeyError::UnknownEscape(other)),
            None => return Err(KeyError::TrailingBackslash),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Transitions
// ---------------------------------------------------------------------------------------------

/// The operation entity behind a public id.
pub fn find(world: &mut World, id: u64) -> Option<Entity> {
    world.resource_scope(|world, mut ops: Mut<Operations>| {
        ops.all
            .iter(world)
            .find_map(|(e, op, _)| (op.id == id).then_some(e))
    })
}

/// Reserves an operation on `pane` for `retain_ms` (clamped to [`MAX_INPUT_RETENTION_MS`]).
/// Under the server-wide cap the oldest expired receipt makes room; with none, the reservation
/// is refused.
pub fn reserve(world: &mut World, pane: Entity, retain_ms: u64) -> R<Entity> {
    if retain_ms == 0 {
        return Err(InputError::ZeroRetention);
    }
    let retain_ms = retain_ms.min(MAX_INPUT_RETENTION_MS);
    let process = world
        .get::<Process>(pane)
        .copied()
        .unwrap_or(Process::Starting);
    if !matches!(process, Process::Live { .. }) {
        return Err(InputError::PaneNotLive(process));
    }
    let now = crate::lifecycle::now_ms(world);
    sweep(world, now);
    let victim = world.resource_scope(|world, mut ops: Mut<Operations>| {
        if ops.all.iter(world).count() < MAX_INPUT_OPERATIONS {
            return Ok(None);
        }
        ops.all
            .iter(world)
            .filter(|(_, op, _)| op.state == InputState::Expired)
            .min_by_key(|(_, _, r)| r.expires_ms)
            .map(|(e, _, _)| Some(e))
            .ok_or(InputError::Capacity)
    })?;
    if let Some(victim) = victim {
        world.despawn(victim);
    }
    let workspace = world
        .get::<PaneIn>(pane)
        .and_then(|p| world.get::<WorkspaceName>(p.0))
        .map(|n| n.0.clone())
        .unwrap_or_default();
    let mut ops = world.resource_mut::<Operations>();
    ops.next = ops.next.saturating_add(1);
    let id = ops.next;
    Ok(world
        .spawn((
            InputOperation {
                id,
                state: InputState::Reserved,
                reserved_ms: now,
            },
            Receipt {
                workspace,
                expires_ms: now.saturating_add(retain_ms),
                retain_ms,
            },
            OperationOn(pane),
        ))
        .id())
}

/// Submits `bytes` through a reserved operation: exactly one `Effect::WritePty`, the terminal
/// sequence at the moment of submission recorded. Resubmitting the same bytes returns without
/// writing; different bytes conflict. Returns the operation entity.
pub fn submit(world: &mut World, id: u64, bytes: Vec<u8>) -> R<Entity> {
    if bytes.is_empty() {
        return Err(InputError::Empty);
    }
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(InputError::TooLong {
            bytes: bytes.len(),
            max: MAX_INPUT_BYTES,
        });
    }
    let now = crate::lifecycle::now_ms(world);
    let operation = find(world, id).ok_or(InputError::NoSuchOperation(id))?;
    let entity = world.entity(operation);
    let (Some(op), Some(receipt)) = (entity.get::<InputOperation>(), entity.get::<Receipt>())
    else {
        return Err(InputError::NoSuchOperation(id));
    };
    if receipt.expires_ms <= now {
        return Err(InputError::Expired);
    }
    match op.state {
        InputState::Expired => return Err(InputError::Expired),
        InputState::Uncertain => return Err(InputError::Unusable),
        InputState::Submitted { .. } => {
            let same = entity
                .get::<SubmittedBytes>()
                .is_some_and(|submitted| submitted.0 == bytes);
            return if same {
                Ok(operation)
            } else {
                Err(InputError::Conflict)
            };
        }
        InputState::Reserved => {}
    }
    let pane = entity
        .get::<OperationOn>()
        .map(|on| on.0)
        .ok_or(InputError::PaneGone)?;
    let process = world
        .get::<Process>(pane)
        .copied()
        .unwrap_or(Process::Starting);
    if !matches!(process, Process::Live { .. }) {
        return Err(InputError::PaneNotLive(process));
    }
    let seq = world.get::<Terminal>(pane).map_or(0, Terminal::seq);
    let len = bytes.len();
    world
        .resource_mut::<Messages<Effect>>()
        .write(Effect::WritePty {
            pane,
            bytes: bytes.clone(),
        });
    let mut entity = world.entity_mut(operation);
    if let Some(mut op) = entity.get_mut::<InputOperation>() {
        op.state = InputState::Submitted { seq, bytes: len };
    }
    entity.insert((Unconfirmed, SubmittedBytes(bytes)));
    Ok(operation)
}

// ---------------------------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------------------------

pub struct InputOpsPlugin;

impl Plugin for InputOpsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Operations>().add_systems(
            First,
            (settle, expire).chain().after(crate::model::Phase::Ingest),
        );
    }
}

/// `First`, after ingest: every receipt still linked to a pane that is now `Exited` (or gone)
/// is detached; its reservation, or a submission whose write had not been drained before the
/// exit, becomes `Uncertain`. Writes drained before this update are confirmed by losing
/// [`Unconfirmed`].
fn settle(
    world: &mut World,
    linked: &mut QueryState<(Entity, &OperationOn, &InputOperation, Has<Unconfirmed>)>,
) {
    let pending: Vec<(Entity, Entity, bool, bool)> = linked
        .iter(world)
        .map(|(e, on, op, unconfirmed)| (e, on.0, op.state == InputState::Reserved, unconfirmed))
        .collect();
    for (operation, pane, reserved, unconfirmed) in pending {
        let exited = world
            .get_entity(pane)
            .ok()
            .and_then(|pane| pane.get::<Process>().copied())
            .is_none_or(|process| matches!(process, Process::Exited { .. }));
        let mut entity = world.entity_mut(operation);
        if exited {
            if (reserved || unconfirmed)
                && let Some(mut op) = entity.get_mut::<InputOperation>()
            {
                op.state = InputState::Uncertain;
            }
            entity.remove::<OperationOn>();
        }
        if unconfirmed {
            entity.remove::<Unconfirmed>();
        }
    }
}

/// `First`: receipts past `expires_ms` become `Expired`; one retention window later they are
/// despawned.
fn expire(world: &mut World) {
    let now = crate::lifecycle::now_ms(world);
    sweep(world, now);
}

fn sweep(world: &mut World, now: u64) {
    let forget: Vec<Entity> = world.resource_scope(|world, mut ops: Mut<Operations>| {
        let mut forget = Vec::new();
        for (entity, mut op, receipt) in ops.all.iter_mut(world) {
            if receipt.expires_ms > now {
                continue;
            }
            if receipt.expires_ms.saturating_add(receipt.retain_ms) <= now {
                forget.push(entity);
            } else if op.state != InputState::Expired {
                op.state = InputState::Expired;
            }
        }
        forget
    });
    for operation in forget {
        world.despawn(operation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_decode_exactly() {
        assert_eq!(
            parse_keys("a\\n\\x1b\\e\\\\\\0é").as_deref(),
            Ok(&[b'a', b'\n', 0x1b, 0x1b, b'\\', 0, 0xc3, 0xa9][..])
        );
        assert_eq!(parse_keys("\\q"), Err(KeyError::UnknownEscape('q')));
        assert_eq!(parse_keys("\\x1"), Err(KeyError::BadHex));
        assert_eq!(parse_keys("a\\"), Err(KeyError::TrailingBackslash));
    }
}
