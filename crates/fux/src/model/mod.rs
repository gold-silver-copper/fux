//! The authoritative model: entity kinds, components, relationships, ids, messages and the
//! schedule vocabulary every subsystem orders against.
//!
//! Mutation ownership (prompt 3.2): one system set writes each component; relationships are the
//! source of truth and their reverse sides are indexes; `Changed<T>` is local invalidation only;
//! semantic revisions that cross the wire are explicit integers.

pub mod components;
pub mod ids;
pub mod invariants;
pub mod limits;
pub mod messages;
pub mod relations;

pub use components::*;
pub use ids::*;
pub use limits::*;
pub use messages::*;
pub use relations::*;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_state::prelude::*;

/// Server-wide mode (prompt 3.1): `OnEnter(ShuttingDown)` terminates every live pane; the runner
/// keeps stepping until no pane is live or the shutdown deadline passes.
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
/// | `First` | `Ingest` | runner batch → `Messages<Inbound>` → typed component writes (PTY output, exits, viewer input) |
/// | `First` (after `RemoteLast`, moved by the app) | — | BRP handlers have already run |
/// | `PreUpdate` | `Requests` then `Completions` | typed transitions requested by BRP/viewers; spawn completions; picking |
/// | `Update` | `Lifecycle` then `Layout` | pane/workspace state machine, template → instance cloning, camera sizes |
/// | `PostUpdate` | (`bevy_ui` layout) then `Projection` | `ComputedNode` → pane size fold, frames, projection components |
/// | `Last` | `Effects` | drain `Messages<Effect>`; `Messages<Inbound>` cleared |
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    Ingest,
    Requests,
    Completions,
    Lifecycle,
    Layout,
    Projection,
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
            .init_resource::<Messages<Inbound>>()
            .init_resource::<Messages<Effect>>()
            .init_state::<ServerMode>()
            .configure_sets(First, Phase::Ingest)
            .configure_sets(PreUpdate, (Phase::Requests, Phase::Completions).chain())
            .configure_sets(Update, (Phase::Lifecycle, Phase::Layout).chain())
            .configure_sets(
                PostUpdate,
                Phase::Projection.after(bevy_ui::UiSystems::PostLayout),
            )
            .configure_sets(Last, Phase::Effects)
            .add_systems(Last, clear_inbound.after(Phase::Effects));
        components::register_types(app);
    }
}

/// Bevy `Messages` are used only within one `update` (prompt 3.6): the runner writes the batch
/// and this system clears it, so nothing is retained across steps.
fn clear_inbound(mut inbound: ResMut<Messages<Inbound>>) {
    inbound.clear();
}
