//! The authoritative model: entity kinds, components, relationships, ids, messages and the
//! schedule vocabulary every subsystem orders against. Contract: `crates/zor/docs/model.md`.

pub mod components;
pub mod graph;
pub mod ids;
pub mod invariants;
pub mod limits;
pub mod messages;
pub mod relations;

pub use components::*;
pub use graph::*;
pub use ids::*;
pub use limits::*;
pub use messages::*;
pub use relations::*;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_state::prelude::*;

/// Server-wide mode: `OnEnter(ShuttingDown)` stops accepting work; the runner keeps stepping
/// until every adapter task drained or the shutdown deadline passes.
#[derive(States, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ServerMode {
    #[default]
    Starting,
    Serving,
    ShuttingDown,
}

/// The ordered phases of one `update`, mapped onto `bevy_app`'s `Main` sub-schedules:
///
/// | schedule | set | owner |
/// |---|---|---|
/// | `First` | `Ingest` | runner batch → `Messages<Inbound>` → typed component writes |
/// | `First` (after `RemoteLast`, moved by the app) | — | BRP handlers have already run |
/// | `PreUpdate` | `Requests` then `Completions` | typed transitions requested over BRP; adapter results |
/// | `Update` | `Lifecycle` | task/attempt/prompt/check state machines |
/// | `PostUpdate` | `Projection` then `Journal` | projection components; the snapshot of what changed |
/// | `Last` | `Effects` | drain `Messages<Effect>`; `Messages<Inbound>` cleared |
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    Ingest,
    Requests,
    Completions,
    Lifecycle,
    Projection,
    Journal,
    Effects,
}

/// Registers everything in this module: types for reflection, id indexes, message buffers,
/// phase ordering and the server state.
pub struct ModelPlugin;

impl Plugin for ModelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Ids>()
            .init_resource::<Limits>()
            .init_resource::<ServerInstance>()
            .init_resource::<Clock>()
            .init_resource::<Generation>()
            .init_resource::<Messages<Inbound>>()
            .init_resource::<Messages<Effect>>()
            .init_state::<ServerMode>()
            .configure_sets(First, Phase::Ingest)
            .configure_sets(PreUpdate, (Phase::Requests, Phase::Completions).chain())
            .configure_sets(Update, Phase::Lifecycle)
            .configure_sets(PostUpdate, (Phase::Projection, Phase::Journal).chain())
            .configure_sets(Last, Phase::Effects)
            .add_systems(Startup, serving)
            .add_systems(First, ingest_signals.in_set(Phase::Ingest))
            .add_systems(Last, clear_inbound.after(Phase::Effects));
        components::register_types(app);
    }
}

fn serving(mut next: ResMut<NextState<ServerMode>>) {
    next.set(ServerMode::Serving);
}

/// Signals request the server-wide transition. The runner drains owned adapters and commits
/// their final evidence before exiting; a shutdown never kills fux-owned task panes.
fn ingest_signals(mut inbound: MessageReader<Inbound>, mut next: ResMut<NextState<ServerMode>>) {
    if inbound
        .read()
        .any(|message| matches!(message, Inbound::Signal(_)))
    {
        next.set(ServerMode::ShuttingDown);
    }
}

/// Bevy `Messages` are used only within one `update`: the runner writes the batch and this
/// system clears it, so nothing is retained across steps.
fn clear_inbound(mut inbound: ResMut<Messages<Inbound>>) {
    inbound.clear();
}
