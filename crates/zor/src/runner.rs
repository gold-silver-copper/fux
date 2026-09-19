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

use crate::model::{Clock, ClosedMs, Deadline, Delivery, Effect, Inbound, ServerMode, Signal, Task, WaitState};

/// Depth of the shared adapter channel.
pub const INBOUND_QUEUE: usize = 8192;

/// One effect sink: an OS-facing adapter (subprocesses, git, the fux client).
pub trait Adapter: Send {
    fn handles(&self, effect: &Effect) -> bool;
    fn apply(&mut self, effect: Effect) -> Result<(), bevy_ecs::error::BevyError>;
    /// Request bounded termination of adapter-owned work; never kill fux-owned tasks.
    fn shutdown(&mut self) {}
    /// True until owned work is reaped and its completion has been enqueued.
    fn pending(&self) -> bool { false }
}

pub struct ZorHost {
    adapters: Vec<Box<dyn Adapter>>,
    deadlines: QueryState<(&'static Deadline, &'static WaitState, &'static Delivery)>,
    closed: QueryState<&'static ClosedMs, With<Task>>,
    heartbeats: QueryState<&'static crate::lifecycle::Heartbeat>,
    observed: QueryState<Entity, With<crate::model::ObservedAgent>>,
    tick: Duration,
    shutdown_started: Option<std::time::Instant>,
}

impl ZorHost {
    pub fn new(world: &mut World, adapters: Vec<Box<dyn Adapter>>) -> Self {
        Self {
            adapters,
            deadlines: world.query::<(&Deadline, &WaitState, &Delivery)>(),
            closed: world.query_filtered::<&ClosedMs, With<Task>>(),
            heartbeats: world.query::<&crate::lifecycle::Heartbeat>(),
            observed: world.query_filtered::<Entity, With<crate::model::ObservedAgent>>(),
            tick: Params::default().tick,
            shutdown_started: None,
        }
    }
}

impl Host for ZorHost {
    type Inbound = Inbound;
    type Effect = Effect;

    fn before_step(&mut self, world: &mut World) {
        world.resource_mut::<Clock>().now_ms = wall_ms();
    }
    fn after_step(&mut self, world: &mut World, sources: &Sources<Inbound>) -> Option<u8> {
        let journal = world.resource::<crate::journal::Journal>();
        let unsafe_effects = !world.resource::<Messages<Effect>>().is_empty();
        if (!journal.is_frozen() && journal.is_dirty())
            || (journal.is_frozen() && unsafe_effects)
        {
            warn!("journal did not authorize this effect batch; exiting without dispatch");
            return Some(1);
        }
        if world.get_resource::<State<ServerMode>>()
            .is_some_and(|state| *state.get() == ServerMode::ShuttingDown)
        {
            let Some(started) = self.shutdown_started else {
                self.shutdown_started = Some(std::time::Instant::now());
                for adapter in &mut self.adapters { adapter.shutdown(); }
                return None;
            };
            if !unsafe_effects && !self.adapters.iter().any(|adapter| adapter.pending())
                && sources.inbound.is_empty() && sources.control.is_empty()
            {
                return Some(0);
            }
            if started.elapsed() >= Duration::from_secs(5) {
                warn!("adapter shutdown deadline elapsed; forcing owned process cleanup");
                return Some(1);
            }
        }
        None
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
        let prompt = self.deadlines
            .iter(world)
            .filter(|(_, wait, delivery)| !wait.is_terminal() && matches!(delivery, Delivery::Submitting | Delivery::Delivered | Delivery::Uncertain))
            .map(|(deadline, _, _)| deadline.0)
            .min();
        let provider = world.get_resource::<crate::providers::Captures>()
            .and_then(|captures| captures.next_deadline(self.observed.iter(world), now));
        let archive = self.closed.iter(world).map(|closed| closed.0).min()
            .and_then(|closed| world.resource::<crate::journal::Journal>().next_sweep_ms().map(|sweep| {
                sweep.max(closed.saturating_add(world.resource::<crate::model::Limits>().archive_after_ms))
            }));
        let absolute = [
            prompt,
            provider,
            archive,
            crate::plugins::next_deadline(world),
            crate::machines::next_deadline(world),
            crate::dashboard::next_deadline(world),
        ].into_iter().flatten().min()
            .map(|at| Duration::from_millis(at.saturating_sub(now).max(1)));
        absolute.into_iter()
            .chain(self.heartbeats.iter(world).map(|heartbeat| heartbeat.0.remaining().max(Duration::from_millis(1))))
            .min()
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
