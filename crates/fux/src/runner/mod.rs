//! The custom runner (prompt 3.1): `finish`/`cleanup` once, then block on the control and
//! inbound channels (with a deadline only when the World has timed work), feed a bounded batch
//! with every control message ahead of the adapter batch, `update` once, and route effects to
//! the adapters. No reactor of its own and no busy loop: idle means the runner thread sleeps in
//! `block_on`.
//!
//! Control messages (signals) have their own small channel so a hot pane streaming output
//! never delays them beyond one step.
//!
//! The loop is generic over the message types through [`Host`]: fux and zor share [`run`] and
//! [`collect`]; everything OS- or model-specific (adapters, clocks, the sleep deadline) is the
//! host's. fux's host is [`FuxHost`].

pub mod signals;

use core::num::NonZero;
use core::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use async_channel::{Receiver, TryRecvError};
use async_io::Timer;
use bevy_app::{App, AppExit, PluginsState};
use bevy_ecs::message::Message;
use bevy_ecs::prelude::*;
use bevy_log::{error, warn};
use bevy_state::prelude::*;
use bevy_tasks::futures_lite::future;

use crate::attach::AttachAdapter;
use crate::lifecycle::Clock;
use crate::model::{Effect, FinalRecord, Inbound, ServerMode};
use crate::pty::PacingWake;
use crate::pty::PtyAdapter;
use crate::session::SessionState;

/// Runner parameters; the old stream coalescing values are the defaults.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// Most inbound messages fed into one `update`.
    pub batch: usize,
    /// After the first message of a batch, how long to wait for more before stepping.
    pub coalesce: Duration,
    /// Poll interval while shutting down (the deadline is a `Clock` comparison).
    pub tick: Duration,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            batch: 1024,
            coalesce: Duration::from_millis(1),
            tick: Duration::from_millis(250),
        }
    }
}

/// Depth of the control channel: signals coalesce, so a handful is plenty.
pub const CONTROL_QUEUE: usize = 4;

/// Runner steps since start (`fux/runner/wakeups`); incremented before every `update`.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StepCounter(pub u64);

/// The two sources the runner blocks on: `control` (signals) is polled first and drained in
/// full ahead of every step; `inbound` is the shared adapter channel.
pub struct Sources<I> {
    pub control: Receiver<I>,
    pub inbound: Receiver<I>,
}

/// What an application supplies to the generic loop: its message types, its adapters and how
/// long it may sleep.
pub trait Host {
    type Inbound: Message;
    type Effect: Message;

    /// Runs before every `update`, after the batch was written: stamp clocks here.
    fn before_step(&mut self, world: &mut World);

    /// Final authority barrier after schedules and before any external effect is dispatched.
    /// Returning an exit code rejects this entire effect batch.
    fn after_step(&mut self, _world: &mut World, _sources: &Sources<Self::Inbound>) -> Option<u8> {
        None
    }

    /// How long the runner may sleep before the next step; `None` sleeps until a message
    /// arrives, `Some(ZERO)` steps at once.
    fn deadline(&mut self, world: &mut World) -> Option<Duration>;

    /// Routes one drained effect to its adapter. Immediate completions may be written only
    /// to `Messages<Inbound>` for the next step; `Messages<Effect>` is scoped out while routing.
    /// `Some(code)` exits after routing the rest of this step's effects.
    fn apply(&mut self, world: &mut World, effect: Self::Effect) -> Option<u8>;

    /// Whether the next step must run without waiting (a pending state transition).
    fn pending(&self, world: &World) -> bool;
}

/// fux's host: PTY and attachment adapters, the wall clock, final-record and pacing deadlines,
/// the `ServerMode` transition poll.
pub struct FuxHost {
    pty: PtyAdapter,
    attach: AttachAdapter,
    records: QueryState<&'static FinalRecord>,
    /// The shutdown poll interval (`Params::tick`).
    tick: Duration,
}

impl FuxHost {
    pub fn new(world: &mut World, pty: PtyAdapter, attach: AttachAdapter, tick: Duration) -> Self {
        Self {
            pty,
            attach,
            records: world.query::<&FinalRecord>(),
            tick,
        }
    }
}

impl Host for FuxHost {
    type Inbound = Inbound;
    type Effect = Effect;

    fn before_step(&mut self, world: &mut World) {
        world.resource_mut::<Clock>().now_ms = wall_ms();
    }

    /// Bounded by the shutdown poll, the earliest `FinalRecord` expiry, a pending paced
    /// `PaneOutput` and a throttled session save.
    fn deadline(&mut self, world: &mut World) -> Option<Duration> {
        let shutting_down = world
            .get_resource::<State<ServerMode>>()
            .is_some_and(|s| *s.get() == ServerMode::ShuttingDown);
        if shutting_down {
            return Some(self.tick);
        }
        let now = wall_ms();
        let pacing = world.get_resource::<PacingWake>().and_then(|w| w.at_ms);
        let save = world
            .get_resource::<SessionState>()
            .and_then(|s| s.next_save_ms);
        self.records
            .iter(world)
            .map(|r| r.expires_ms)
            .chain(pacing)
            .chain(save)
            .min()
            .map(|at| Duration::from_millis(at.saturating_sub(now).max(1)))
    }

    fn apply(&mut self, _world: &mut World, effect: Effect) -> Option<u8> {
        match effect {
            Effect::Exit { code } => return Some(code),
            effect if PtyAdapter::handles(&effect) => {
                if let Err(error) = self.pty.apply(effect) {
                    warn!("pty adapter: {error}");
                }
            }
            effect if AttachAdapter::handles(&effect) => {
                if !self.attach.apply(effect) {
                    warn!("attach adapter refused an effect");
                }
            }
            effect => warn!("unrouted effect {effect:?}"),
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

/// Makes `app.run()` use [`run`] with fux's host.
pub fn install(app: &mut App, sources: Sources<Inbound>, pty: PtyAdapter, attach: AttachAdapter) {
    let params = Params::default();
    let host = FuxHost::new(app.world_mut(), pty, attach, params.tick);
    app.set_runner(move |app| run(app, sources, host, params));
}

pub fn run<H: Host>(
    mut app: App,
    sources: Sources<H::Inbound>,
    mut host: H,
    params: Params,
) -> AppExit {
    while app.plugins_state() == PluginsState::Adding {
        bevy_tasks::tick_global_task_pools_on_main_thread();
    }
    app.finish();
    app.cleanup();

    let mut batch: Vec<H::Inbound> = Vec::with_capacity(params.batch);
    // The first step runs immediately so `Startup` happens before anything arrives.
    let mut immediate = true;
    loop {
        let deadline = if immediate {
            Some(Duration::ZERO)
        } else {
            host.deadline(app.world_mut())
        };
        if collect(&sources, &mut batch, &params, deadline).is_err() {
            error!("runner channel closed; exiting");
            return AppExit::error();
        }
        let world = app.world_mut();
        world.get_resource_or_init::<StepCounter>().0 += 1;
        world
            .resource_mut::<Messages<H::Inbound>>()
            .write_batch(batch.drain(..));
        host.before_step(world);
        app.update();
        if let Some(code) = host.after_step(app.world_mut(), &sources) {
            return NonZero::new(code).map_or(AppExit::Success, AppExit::Error);
        }

        let mut exit: Option<u8> = None;
        app.world_mut()
            .resource_scope(|world, mut effects: Mut<Messages<H::Effect>>| {
                for effect in effects.drain() {
                    if let Some(code) = host.apply(world, effect) {
                        exit = Some(code);
                    }
                }
            });
        if let Some(code) = exit {
            return NonZero::new(code).map_or(AppExit::Success, AppExit::Error);
        }
        if let Some(exit) = app.should_exit() {
            return exit;
        }
        immediate = host.pending(app.world())
            || !app.world().resource::<Messages<H::Inbound>>().is_empty()
            || !sources.inbound.is_empty()
            || !sources.control.is_empty();
    }
}

/// A runner channel has no senders left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;

/// Gathers one step's messages into `batch`: blocks until a message arrives on either source
/// (a zero `deadline` does not block; `None` blocks indefinitely), briefly waits for a second
/// one to coalesce, then drains the control channel in full and at most `params.batch` inbound
/// messages. Control messages are placed ahead of everything else, so a signal is applied by
/// the next `update` no matter how much output is queued.
pub fn collect<I>(
    sources: &Sources<I>,
    batch: &mut Vec<I>,
    params: &Params,
    deadline: Option<Duration>,
) -> Result<(), Closed> {
    if deadline != Some(Duration::ZERO) {
        match wait(sources, deadline) {
            Wait::Message(message) => {
                batch.push(message);
                if let Wait::Message(next) = wait(sources, Some(params.coalesce)) {
                    batch.push(next);
                }
            }
            Wait::Timeout => {}
            Wait::Closed => return Err(Closed),
        }
    }
    let waited = batch.len();
    while let Ok(message) = sources.control.try_recv() {
        batch.push(message);
    }
    // Control first: the messages the wait returned move behind the drained control messages.
    batch.rotate_left(waited);
    while batch.len() < params.batch {
        match sources.inbound.try_recv() {
            Ok(message) => batch.push(message),
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
        }
    }
    Ok(())
}

enum Wait<I> {
    Message(I),
    Timeout,
    Closed,
}

/// Blocks on both sources, control polled first; nothing else runs on this thread meanwhile.
fn wait<I>(sources: &Sources<I>, deadline: Option<Duration>) -> Wait<I> {
    let message = future::or(sources.control.recv(), sources.inbound.recv());
    match deadline {
        None => bevy_tasks::block_on(message).map_or(Wait::Closed, Wait::Message),
        Some(duration) => bevy_tasks::block_on(future::or(
            async { message.await.map_or(Wait::Closed, Wait::Message) },
            async {
                Timer::after(duration).await;
                Wait::Timeout
            },
        )),
    }
}

/// Milliseconds since the Unix epoch; the `Clock` stamp of every step.
pub fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
