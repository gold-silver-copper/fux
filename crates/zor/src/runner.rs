//! zor's runner: `fux::runner::run` (the generic control + inbound loop, batch, coalescing) with
//! a [`ZorHost`] that stamps the `Clock`, bounds the sleep by the earliest prompt deadline and
//! routes `Effect`s to the registered [`Adapter`]s. The fux client adapter is registered here;
//! provider, check and git adapters are later milestone-6 slices and register the same way.

use core::time::Duration;

use async_channel::Sender;
use bevy_app::App;
use bevy_ecs::prelude::*;
use bevy_log::warn;
use bevy_state::prelude::*;
use fux::runner::{Host, Params, Sources, wall_ms};

pub use fux::runner::{CONTROL_QUEUE, StepCounter};

use crate::model::{Clock, Deadline, Effect, Inbound, ServerMode, Signal};

/// Depth of the shared adapter channel.
pub const INBOUND_QUEUE: usize = 8192;

/// One effect sink: an OS-facing adapter (subprocesses, git, the fux client).
pub trait Adapter: Send {
    fn handles(&self, effect: &Effect) -> bool;
    fn apply(&mut self, effect: Effect) -> Result<(), bevy_ecs::error::BevyError>;
}

pub struct ZorHost {
    adapters: Vec<Box<dyn Adapter>>,
    deadlines: QueryState<&'static Deadline>,
    tick: Duration,
}

impl ZorHost {
    pub fn new(world: &mut World, adapters: Vec<Box<dyn Adapter>>) -> Self {
        Self {
            adapters,
            deadlines: world.query::<&Deadline>(),
            tick: Params::default().tick,
        }
    }
}

impl Host for ZorHost {
    type Inbound = Inbound;
    type Effect = Effect;

    fn before_step(&mut self, world: &mut World) {
        world.resource_mut::<Clock>().now_ms = wall_ms();
    }

    /// Bounded by the shutdown poll and the earliest stored prompt deadline (a deadline is
    /// intent, TASKS.md:177: the step only lets the lifecycle observe that it passed).
    fn deadline(&mut self, world: &mut World) -> Option<Duration> {
        let shutting_down = world
            .get_resource::<State<ServerMode>>()
            .is_some_and(|s| *s.get() == ServerMode::ShuttingDown);
        if shutting_down {
            return Some(self.tick);
        }
        let now = wall_ms();
        self.deadlines
            .iter(world)
            .map(|d| d.0)
            .filter(|at| *at > now)
            .min()
            .map(|at| Duration::from_millis(at.saturating_sub(now).max(1)))
    }

    fn apply(&mut self, effect: Effect) -> Option<u8> {
        if let Effect::Exit { code } = effect {
            return Some(code);
        }
        match self.adapters.iter_mut().find(|a| a.handles(&effect)) {
            Some(adapter) => {
                if let Err(error) = adapter.apply(effect) {
                    warn!("adapter: {error}");
                }
            }
            None => warn!("unrouted effect {effect:?}"),
        }
        None
    }

    fn pending(&self, world: &World) -> bool {
        matches!(
            world.resource::<NextState<ServerMode>>(),
            NextState::Pending(_)
        )
    }
}

/// Makes `app.run()` use the shared loop with [`ZorHost`].
pub fn install(app: &mut App, sources: Sources<Inbound>, adapters: Vec<Box<dyn Adapter>>) {
    let host = ZorHost::new(app.world_mut(), adapters);
    app.set_runner(move |app| fux::runner::run(app, sources, host, Params::default()));
}

/// `SIGINT`/`SIGTERM` → `Inbound::Signal` on the control channel (fux's self-pipe forwarder).
pub fn install_signals(control: Sender<Inbound>) -> Result<(), bevy_ecs::error::BevyError> {
    fux::runner::signals::install_with(control, |signal: Signal| Inbound::Signal(signal))
}
