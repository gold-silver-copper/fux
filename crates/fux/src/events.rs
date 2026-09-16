//! Public lifecycle events and the retained event log (prompt 3.3, 3.9).
//!
//! Every lifecycle fact is an [`EntityEvent`] targeting the entity it is about, `#[entity_event(propagate)]`
//! so an observer may bubble it up `ChildOf`, and reflected with `#[reflect(Event)]` so
//! `world.observe+watch` can serialize it. Bodies carry public ids and counters only: the target
//! entity and the workspace entity that scopes the fact are ignored by reflection and serde.
//!
//! The events are *triggered* by the lifecycle after a transition commits (`lifecycle.rs` for
//! spawn and exit, `pty.rs` for output, titles and bells, the component-lifecycle observers below
//! for attach/detach/close, which see every despawn path); they never decide one. The one
//! consumer here is [`EventLog`]: a bounded per-workspace log feeding `fux/events+watch` with
//! explicit gaps, plus the [`DiagnosticsSnapshot`] `fux/server.info` reports.

use bevy_app::prelude::*;
use bevy_diagnostic::{Diagnostic, DiagnosticPath, Diagnostics, RegisterDiagnostic};
use bevy_ecs::entity::EntityHashMap;
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::event::PropagateEntityTrigger;
use bevy_ecs::lifecycle::{Add, Remove};
use bevy_ecs::prelude::*;
use bevy_ecs::query::Allow;
use bevy_log::{debug, warn};
use bevy_platform::collections::HashMap;
use bevy_reflect::prelude::*;
use serde::Serialize;
use serde_json::Value;

use crate::lifecycle::Clock;
use crate::model::*;
use crate::runner::StepCounter;
use crate::surface::SurfaceInputDrops;

/// Bytes of event bodies retained per workspace stream (the old protocol's 512 KiB).
pub const MAX_STREAM_BYTES: usize = 512 * 1024;
/// Reflected type paths of the public events, the `world.observe+watch` allowlist.
pub const EVENT_TYPE_PATHS: [&str; 11] = [
    "fux::events::PaneSpawned",
    "fux::events::PaneOutput",
    "fux::events::PaneTitleChanged",
    "fux::events::PaneExited",
    "fux::events::PaneClosed",
    "fux::events::RootEmptied",
    "fux::events::WorkspaceRetired",
    "fux::events::ViewerAttached",
    "fux::events::ViewerDetached",
    "fux::events::Bell",
    "fux::events::SurfaceInput",
];

/// Runner steps since start (`fux/runner/wakeups`).
pub const WAKEUPS: DiagnosticPath = DiagnosticPath::const_new("fux/runner/wakeups");
/// Panes with a running process.
pub const PANES_LIVE: DiagnosticPath = DiagnosticPath::const_new("fux/panes/live");
/// Attached viewers.
pub const VIEWERS: DiagnosticPath = DiagnosticPath::const_new("fux/viewers");
/// Entries retained by the event log across every stream.
pub const EVENTS_RETAINED: DiagnosticPath = DiagnosticPath::const_new("fux/events/retained");
/// `SurfaceInput` events dropped by the per-surface rate bound since start.
pub const SURFACE_INPUTS_DROPPED: DiagnosticPath =
    DiagnosticPath::const_new("fux/surface/inputs_dropped");

// ---------------------------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------------------------

/// What the log needs from an event beyond its body. The trigger bound is what the
/// `lifecycle_event!` derive produces (`#[entity_event(propagate)]` over `ChildOf`).
pub trait Logged:
    EntityEvent
    + Serialize
    + for<'a> Event<Trigger<'a> = PropagateEntityTrigger<false, Self, &'static ChildOf>>
{
    /// The `name` of the log entry: the bare type name.
    const NAME: &'static str;
    /// Whether the stream closes after this event (only `WorkspaceRetired`).
    const RETIRES: bool = false;
    /// The workspace entity whose stream retains the event.
    fn scope(&self) -> Entity;
}

macro_rules! lifecycle_event {
    ($(#[$m:meta])* $name:ident { $($(#[$fm:meta])* $field:ident : $ty:ty),* $(,)? } $(retires = $retires:literal)?) => {
        $(#[$m])*
        #[derive(EntityEvent, Reflect, Clone, Debug, PartialEq, Serialize)]
        #[entity_event(propagate)]
        #[reflect(Event, Clone, from_reflect = false)]
        pub struct $name {
            /// The entity the fact is about; not part of the body.
            #[reflect(ignore)]
            #[serde(skip)]
            pub entity: Entity,
            /// The workspace entity the fact belongs to (the log's stream key); not part of the
            /// body.
            #[reflect(ignore)]
            #[serde(skip)]
            pub scope: Entity,
            $($(#[$fm])* pub $field: $ty,)*
        }

        impl Logged for $name {
            const NAME: &'static str = stringify!($name);
            $(const RETIRES: bool = $retires;)?
            fn scope(&self) -> Entity {
                self.scope
            }
        }
    };
}

lifecycle_event!(
    /// A pane's process is running: the first `Process::Live` the lifecycle observed.
    PaneSpawned { pane: PaneId, pid: u32 }
);
lifecycle_event!(
    /// The pane's output sequence moved; paced to one per [`Limits::output_pacing_ms`] per
    /// pane and carrying the latest sequence.
    PaneOutput { pane: PaneId, seq: u64 }
);
lifecycle_event!(
    /// The pane's title changed; read it from the projection or a capture.
    PaneTitleChanged { pane: PaneId }
);
lifecycle_event!(
    /// The process exited; the pane's `FinalRecord` exists from this point.
    PaneExited { pane: PaneId, code: i32 }
);
lifecycle_event!(
    /// The pane entity is going away (its record outlives it).
    PaneClosed { pane: PaneId }
);
lifecycle_event!(
    /// A template root (a tab) closed because nothing is left in it.
    RootEmptied { root: NodeId }
);
lifecycle_event!(
    /// A workspace began retiring; its stream ends after the events its panes still produce.
    WorkspaceRetired { workspace: String }
    retires = true
);
lifecycle_event!(ViewerAttached { viewer: ViewerId });
lifecycle_event!(ViewerDetached { viewer: ViewerId });
lifecycle_event!(
    /// The terminal rang (BEL); one event per update however many rang.
    Bell { pane: PaneId }
);

/// What a viewer did on a surface node.
#[derive(Reflect, Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum SurfaceInputKind {
    Press,
    Release,
    Scroll { rows: i32 },
    /// Keys typed while the viewer's focus was on the surface leaf; the event's `bytes` carry
    /// them.
    Key,
}

lifecycle_event!(
    /// A viewer acted on a surface (prompt 3.13): a pointer press, release or wheel on `node`
    /// (the surface leaf itself or a node the provider streamed under it) at cell (`col`,
    /// `row`) of the leaf's content box, or keys typed while the viewer's focus was on the
    /// leaf. `bytes` is empty except for `Key`, where it holds at most
    /// [`crate::surface::MAX_INPUT_BYTES`]. At most [`crate::surface::MAX_INPUTS_PER_SECOND`]
    /// per surface; the excess is dropped and counted in `fux/server.info`.
    SurfaceInput {
        surface: NodeId,
        node: NodeId,
        viewer: ViewerId,
        kind: SurfaceInputKind,
        col: u16,
        row: u16,
        bytes: Vec<u8>,
    }
);

// ---------------------------------------------------------------------------------------------
// Retained log
// ---------------------------------------------------------------------------------------------

/// One retained event.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entry {
    /// Server-wide monotonic cursor (unique across workspaces, starts at 1).
    pub cursor: u64,
    pub workspace: String,
    /// [`Clock`] milliseconds when the event was logged.
    pub ms: u64,
    /// The event's bare type name (`PaneSpawned`, ...).
    pub name: &'static str,
    /// The serialized body: public ids and counters only.
    pub event: Value,
}

/// A cursor the log cannot serve losslessly: older than what is retained, from a replaced
/// workspace stream, or unknown to this server. `resume` is the smallest cursor now accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Gap {
    pub since: u64,
    pub resume: u64,
}

/// One workspace incarnation's events. `entries[start..]` are live; evicted rows are removed
/// lazily so a read is a plain slice.
#[derive(Debug)]
struct Stream {
    name: String,
    /// Smallest cursor `read_after` serves losslessly: the last evicted cursor, or the tail of
    /// the stream this one replaced.
    floor: u64,
    /// Latest cursor appended, or `floor` while empty.
    latest: u64,
    entries: Vec<Entry>,
    start: usize,
    bytes: usize,
    retired: bool,
}

impl Stream {
    fn live(&self) -> &[Entry] {
        self.entries.get(self.start..).unwrap_or(&[])
    }

    fn len(&self) -> usize {
        self.entries.len().saturating_sub(self.start)
    }
}

/// The retained per-workspace event log (prompt 3.9): bounded by entries and bytes per stream,
/// explicit [`Gap`]s once eviction happened, one server-wide cursor.
#[derive(Resource, Debug)]
pub struct EventLog {
    streams: EntityHashMap<Stream>,
    /// The current stream of each workspace name.
    by_name: HashMap<String, Entity>,
    /// The tail cursor of streams dropped whole (pruned or replaced), by name, so a read for
    /// a name without a live stream still reports the gap. Bounded at twice `max_retired`: a
    /// name whose tail aged out answers like an unknown name (nothing, not a gap).
    tombstones: HashMap<String, u64>,
    /// Next cursor to issue.
    next: u64,
    /// Highest cursor ever evicted or dropped, server-wide: the floor of unscoped reads.
    dropped_floor: u64,
    max_entries: usize,
    max_bytes: usize,
    max_retired: usize,
}

impl EventLog {
    pub fn new(limits: &Limits) -> Self {
        Self {
            streams: EntityHashMap::default(),
            by_name: HashMap::default(),
            tombstones: HashMap::default(),
            next: 1,
            dropped_floor: 0,
            max_entries: limits.event_log_entries.max(1),
            max_bytes: MAX_STREAM_BYTES,
            max_retired: limits.workspaces.max(1),
        }
    }

    /// Every retained entry of `workspace` after `cursor` (the last one the reader saw; the
    /// stream's own `cursor()` for "from now"). Unknown workspaces have nothing.
    pub fn read_after(&self, workspace: &str, cursor: u64) -> Result<&[Entry], Gap> {
        if cursor >= self.next {
            return Err(Gap {
                since: cursor,
                resume: self.latest(),
            });
        }
        let Some(stream) = self
            .by_name
            .get(workspace)
            .and_then(|e| self.streams.get(e))
        else {
            return match self.tombstones.get(workspace) {
                Some(&tail) if cursor < tail => Err(Gap {
                    since: cursor,
                    resume: tail,
                }),
                _ => Ok(&[]),
            };
        };
        if cursor < stream.floor {
            return Err(Gap {
                since: cursor,
                resume: stream.floor,
            });
        }
        let live = stream.live();
        let from = live.partition_point(|e| e.cursor <= cursor);
        Ok(live.get(from..).unwrap_or(&[]))
    }

    /// Every retained entry of every workspace after `cursor`, in cursor order, appended to
    /// `out`.
    pub fn read_any_after<'a>(&'a self, cursor: u64, out: &mut Vec<&'a Entry>) -> Result<(), Gap> {
        if cursor >= self.next {
            return Err(Gap {
                since: cursor,
                resume: self.latest(),
            });
        }
        if cursor < self.dropped_floor {
            return Err(Gap {
                since: cursor,
                resume: self.dropped_floor,
            });
        }
        let at = out.len();
        for stream in self.streams.values() {
            let live = stream.live();
            let from = live.partition_point(|e| e.cursor <= cursor);
            out.extend(live.get(from..).unwrap_or(&[]));
        }
        if let Some(tail) = out.get_mut(at..) {
            tail.sort_unstable_by_key(|e| e.cursor);
        }
        Ok(())
    }

    /// The latest cursor of `workspace`'s stream (its floor while empty; 0 when unknown).
    pub fn cursor(&self, workspace: &str) -> u64 {
        self.by_name
            .get(workspace)
            .and_then(|e| self.streams.get(e))
            .map_or(0, |s| s.latest)
    }

    /// The latest cursor issued server-wide.
    pub fn latest(&self) -> u64 {
        self.next - 1
    }

    /// Names with a live (possibly retiring) stream.
    pub fn workspaces(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }

    /// Entries retained across every stream.
    pub fn retained(&self) -> usize {
        self.streams.values().map(Stream::len).sum()
    }

    /// Appends one event to the stream of the workspace entity `scope`; `name_of` resolves the
    /// workspace name the first time a stream is opened.
    pub fn append<E: Logged>(
        &mut self,
        event: &E,
        ms: u64,
        name_of: impl FnOnce() -> Option<String>,
    ) {
        let scope = event.scope();
        if !self.streams.contains_key(&scope) {
            let Some(name) = name_of() else {
                debug!(
                    "{} for an unknown workspace entity {scope}; not retained",
                    E::NAME
                );
                return;
            };
            self.open(scope, name);
        }
        let body = match serde_json::to_value(event) {
            Ok(body) => body,
            Err(error) => {
                warn!("serialize {}: {error}", E::NAME);
                return;
            }
        };
        let cursor = self.next;
        self.next += 1;
        let Some(stream) = self.streams.get_mut(&scope) else {
            return;
        };
        let entry = Entry {
            cursor,
            workspace: stream.name.clone(),
            ms,
            name: E::NAME,
            event: body,
        };
        stream.bytes += weight(&entry);
        stream.latest = cursor;
        stream.entries.push(entry);
        while stream.len() > self.max_entries || (stream.bytes > self.max_bytes && stream.len() > 1)
        {
            let Some(evicted) = stream.entries.get(stream.start) else {
                break;
            };
            stream.bytes = stream.bytes.saturating_sub(weight(evicted));
            stream.floor = evicted.cursor;
            self.dropped_floor = self.dropped_floor.max(evicted.cursor);
            stream.start += 1;
        }
        if stream.start > 0 && stream.start >= stream.entries.len() / 2 {
            stream.entries.drain(..stream.start);
            stream.start = 0;
        }
        if E::RETIRES {
            stream.retired = true;
            self.prune_retired();
        }
    }

    /// Opens the stream of a workspace entity; a stream of the same name (the workspace was
    /// retired and recreated) is dropped whole, and its tail becomes the new stream's floor.
    fn open(&mut self, scope: Entity, name: String) {
        let mut floor = self.tombstones.remove(&name).unwrap_or(0);
        if let Some(old) = self.by_name.remove(&name)
            && let Some(old) = self.streams.remove(&old)
        {
            floor = floor.max(old.latest);
            self.dropped_floor = self.dropped_floor.max(old.latest);
        }
        self.by_name.insert(name.clone(), scope);
        self.streams.insert(
            scope,
            Stream {
                name,
                floor,
                latest: floor.max(self.next - 1),
                entries: Vec::new(),
                start: 0,
                bytes: 0,
                retired: false,
            },
        );
    }

    /// Keeps at most `max_retired` retired streams: the oldest are dropped whole into
    /// tombstones, of which the oldest are forgotten past twice that bound.
    fn prune_retired(&mut self) {
        let mut retired: Vec<(u64, Entity)> = self
            .streams
            .iter()
            .filter(|(_, s)| s.retired)
            .map(|(e, s)| (s.latest, *e))
            .collect();
        if retired.len() <= self.max_retired {
            return;
        }
        retired.sort_unstable();
        let excess = retired.len() - self.max_retired;
        for &(_, entity) in retired.iter().take(excess) {
            let Some(stream) = self.streams.remove(&entity) else {
                continue;
            };
            self.dropped_floor = self.dropped_floor.max(stream.latest);
            if self.by_name.get(&stream.name) == Some(&entity) {
                self.by_name.remove(&stream.name);
            }
            self.tombstones.insert(stream.name, stream.latest);
        }
        let max_tombstones = self.max_retired.saturating_mul(2);
        while self.tombstones.len() > max_tombstones {
            let Some(oldest) = self
                .tombstones
                .iter()
                .min_by_key(|(_, tail)| **tail)
                .map(|(name, _)| name.clone())
            else {
                break;
            };
            self.tombstones.remove(&oldest);
        }
    }
}

/// Approximate JSON size of an entry, for the byte bound.
fn weight(entry: &Entry) -> usize {
    fn json(value: &Value) -> usize {
        match value {
            Value::Null => 4,
            Value::Bool(_) => 5,
            Value::Number(_) => 20,
            Value::String(s) => s.len() + 2,
            Value::Array(items) => 2 + items.iter().map(|v| json(v) + 1).sum::<usize>(),
            Value::Object(map) => {
                2 + map
                    .iter()
                    .map(|(k, v)| k.len() + 4 + json(v))
                    .sum::<usize>()
            }
        }
    }
    48 + entry.workspace.len() + entry.name.len() + json(&entry.event)
}

// ---------------------------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------------------------

/// The counters `fux/server.info` reports; refreshed every update.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DiagnosticsSnapshot {
    pub wakeups: u64,
    pub panes_live: u32,
    pub viewers: u32,
    pub events_retained: u32,
    pub surface_inputs_dropped: u64,
}

pub struct EventsPlugin;

impl Plugin for EventsPlugin {
    fn build(&self, app: &mut App) {
        let limits = app
            .world()
            .get_resource::<Limits>()
            .cloned()
            .unwrap_or_default();
        app.insert_resource(EventLog::new(&limits))
            .init_resource::<DiagnosticsSnapshot>()
            .init_resource::<StepCounter>()
            .register_type::<PaneSpawned>()
            .register_type::<PaneOutput>()
            .register_type::<PaneTitleChanged>()
            .register_type::<PaneExited>()
            .register_type::<PaneClosed>()
            .register_type::<RootEmptied>()
            .register_type::<WorkspaceRetired>()
            .register_type::<ViewerAttached>()
            .register_type::<ViewerDetached>()
            .register_type::<Bell>()
            .register_type::<SurfaceInput>()
            .register_diagnostic(Diagnostic::new(WAKEUPS))
            .register_diagnostic(Diagnostic::new(PANES_LIVE))
            .register_diagnostic(Diagnostic::new(VIEWERS))
            .register_diagnostic(Diagnostic::new(EVENTS_RETAINED))
            .register_diagnostic(Diagnostic::new(SURFACE_INPUTS_DROPPED))
            .add_observer(log_event::<PaneSpawned>)
            .add_observer(log_event::<PaneOutput>)
            .add_observer(log_event::<PaneTitleChanged>)
            .add_observer(log_event::<PaneExited>)
            .add_observer(log_event::<PaneClosed>)
            .add_observer(log_event::<RootEmptied>)
            .add_observer(log_event::<WorkspaceRetired>)
            .add_observer(log_event::<ViewerAttached>)
            .add_observer(log_event::<ViewerDetached>)
            .add_observer(log_event::<Bell>)
            .add_observer(log_event::<SurfaceInput>)
            .add_observer(viewer_attached)
            .add_observer(viewer_detached)
            .add_observer(pane_closed)
            .add_observer(root_emptied)
            .add_observer(workspace_retired)
            .add_systems(Last, diagnostics.before(Phase::Effects));
        #[cfg(feature = "bell")]
        bell::build(app);
    }
}

/// Appends every public event to the log as it is triggered. A global observer runs once per
/// propagation hop, so only the original target's hop is retained should an observer ever
/// bubble an event up `ChildOf`.
fn log_event<E: Logged>(
    event: On<E>,
    mut log: ResMut<EventLog>,
    clock: Res<Clock>,
    names: Query<&WorkspaceName, Allow<Disabled>>,
) {
    if event.event_target() != event.original_event_target() {
        return;
    }
    let scope = event.scope();
    log.append(event.event(), clock.now_ms, || {
        names.get(scope).ok().map(|n| n.0.clone())
    });
}

// The component-lifecycle observers below turn every attach, detach, close and retire into
// its public event while the entity's ids are still readable, whichever code path caused it.

fn viewer_attached(
    add: On<Add, Viewer>,
    viewers: Query<(&ViewerId, &Viewing)>,
    mut commands: Commands,
) {
    let entity = add.entity;
    if let Ok((id, viewing)) = viewers.get(entity) {
        commands.trigger(ViewerAttached {
            entity,
            scope: viewing.0,
            viewer: *id,
        });
    }
}

fn viewer_detached(
    remove: On<Remove, Viewer>,
    viewers: Query<(&ViewerId, &Viewing)>,
    mut commands: Commands,
) {
    let entity = remove.entity;
    if let Ok((id, viewing)) = viewers.get(entity) {
        commands.trigger(ViewerDetached {
            entity,
            scope: viewing.0,
            viewer: *id,
        });
    }
}

fn pane_closed(
    remove: On<Remove, Pane>,
    panes: Query<(&PaneId, &PaneIn), Allow<Disabled>>,
    mut commands: Commands,
) {
    let entity = remove.entity;
    if let Ok((id, ws)) = panes.get(entity) {
        commands.trigger(PaneClosed {
            entity,
            scope: ws.0,
            pane: *id,
        });
    }
}

fn root_emptied(
    remove: On<Remove, TemplateRoot>,
    roots: Query<(&NodeId, &RootOf)>,
    mut commands: Commands,
) {
    let entity = remove.entity;
    if let Ok((id, of)) = roots.get(entity) {
        commands.trigger(RootEmptied {
            entity,
            scope: of.0,
            root: *id,
        });
    }
}

fn workspace_retired(
    add: On<Add, Retiring>,
    workspaces: Query<&WorkspaceName, (With<Workspace>, Allow<Disabled>)>,
    mut commands: Commands,
) {
    let entity = add.entity;
    if let Ok(name) = workspaces.get(entity) {
        commands.trigger(WorkspaceRetired {
            entity,
            scope: entity,
            workspace: name.0.clone(),
        });
    }
}

/// `Last`: the snapshot and the `bevy_diagnostic` measurements behind `fux/server.info`.
fn diagnostics(
    mut snapshot: ResMut<DiagnosticsSnapshot>,
    mut diagnostics: Diagnostics,
    steps: Res<StepCounter>,
    log: Res<EventLog>,
    drops: Res<SurfaceInputDrops>,
    panes: Query<&Process, (With<Pane>, Allow<Disabled>)>,
    viewers: Query<(), With<Viewer>>,
) {
    let next = DiagnosticsSnapshot {
        wakeups: steps.0,
        panes_live: u32::try_from(panes.iter().filter(|p| p.is_live()).count()).unwrap_or(u32::MAX),
        viewers: u32::try_from(viewers.iter().count()).unwrap_or(u32::MAX),
        events_retained: u32::try_from(log.retained()).unwrap_or(u32::MAX),
        surface_inputs_dropped: drops.0,
    };
    snapshot.set_if_neq(next);
    diagnostics.add_measurement(&WAKEUPS, || next.wakeups as f64);
    diagnostics.add_measurement(&PANES_LIVE, || f64::from(next.panes_live));
    diagnostics.add_measurement(&VIEWERS, || f64::from(next.viewers));
    diagnostics.add_measurement(&EVENTS_RETAINED, || f64::from(next.events_retained));
    diagnostics.add_measurement(&SURFACE_INPUTS_DROPPED, || next.surface_inputs_dropped as f64);
}

// ---------------------------------------------------------------------------------------------
// Bell (feature `bell`)
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "bell")]
mod bell {
    use std::sync::Arc;

    use bevy_app::prelude::*;
    use bevy_asset::{Assets, Handle};
    use bevy_audio::{AudioPlayer, AudioPlugin, AudioSource, PlaybackSettings};
    use bevy_ecs::prelude::*;

    use super::Bell;

    /// A short 8-bit PCM chime, checked in so no asset directory is needed.
    const CHIME: &[u8] = include_bytes!("../assets/bell.wav");

    #[derive(Resource)]
    struct Chime(Handle<AudioSource>);

    pub(super) fn build(app: &mut App) {
        app.add_plugins(AudioPlugin::default())
            .add_systems(Startup, load)
            .add_observer(ring);
    }

    fn load(mut commands: Commands, mut sources: ResMut<Assets<AudioSource>>) {
        let handle = sources.add(AudioSource {
            bytes: Arc::from(CHIME),
        });
        commands.insert_resource(Chime(handle));
    }

    /// Plays the chime once per `Bell`; without an audio device `bevy_audio` logs and drops it.
    fn ring(_bell: On<Bell>, chime: Option<Res<Chime>>, mut commands: Commands) {
        if let Some(chime) = chime {
            commands.spawn((AudioPlayer::new(chime.0.clone()), PlaybackSettings::DESPAWN));
        }
    }
}
