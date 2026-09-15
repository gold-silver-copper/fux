//! The custom runner (prompt 3.1): `finish`/`cleanup` once, then block on the inbound channel
//! (with a deadline only when the World has timed work), feed a bounded batch, `update` once,
//! and route `Effect`s to the adapters. No reactor of its own and no busy loop: idle means the
//! runner thread sleeps in `recv`.

pub mod signals;

use core::num::NonZero;
use core::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use async_channel::{Receiver, TryRecvError};
use async_io::Timer;
use bevy_app::{App, AppExit, PluginsState};
use bevy_ecs::prelude::*;
use bevy_log::{error, warn};
use bevy_state::prelude::*;
use bevy_tasks::futures_lite::future;

use crate::attach::AttachAdapter;
use crate::lifecycle::Clock;
use crate::model::{Effect, FinalRecord, Inbound, ServerMode};
use crate::pty::PtyAdapter;

/// Runner parameters; the old stream coalescing values are the defaults.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// Most `Inbound` messages fed into one `update`.
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

/// Makes `app.run()` use [`run`].
pub fn install(app: &mut App, receiver: Receiver<Inbound>, pty: PtyAdapter, attach: AttachAdapter) {
    app.set_runner(move |app| run(app, receiver, pty, attach, Params::default()));
}

pub fn run(
    mut app: App,
    receiver: Receiver<Inbound>,
    mut pty: PtyAdapter,
    mut attach: AttachAdapter,
    params: Params,
) -> AppExit {
    while app.plugins_state() == PluginsState::Adding {
        bevy_tasks::tick_global_task_pools_on_main_thread();
    }
    app.finish();
    app.cleanup();

    let mut records = app.world_mut().query::<&FinalRecord>();
    let mut batch: Vec<Inbound> = Vec::with_capacity(params.batch);
    // The first step runs immediately so `Startup` happens before anything arrives.
    let mut immediate = true;
    loop {
        if !immediate {
            let deadline = deadline(&mut app, &mut records, params.tick);
            match wait(&receiver, deadline) {
                Wait::Message(message) => {
                    batch.push(message);
                    if let Wait::Message(next) = wait(&receiver, Some(params.coalesce)) {
                        batch.push(next);
                    }
                }
                Wait::Timeout => {}
                Wait::Closed => {
                    error!("inbound channel closed; exiting");
                    return AppExit::error();
                }
            }
        }
        while batch.len() < params.batch {
            match receiver.try_recv() {
                Ok(message) => batch.push(message),
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
        let world = app.world_mut();
        world.resource_mut::<Clock>().now_ms = wall_ms();
        world
            .resource_mut::<Messages<Inbound>>()
            .write_batch(batch.drain(..));
        app.update();

        let mut exit: Option<u8> = None;
        for effect in app.world_mut().resource_mut::<Messages<Effect>>().drain() {
            match effect {
                Effect::Exit { code } => exit = Some(code),
                effect if PtyAdapter::handles(&effect) => {
                    if let Err(error) = pty.apply(effect) {
                        warn!("pty adapter: {error}");
                    }
                }
                effect if AttachAdapter::handles(&effect) => {
                    if !attach.apply(effect) {
                        warn!("attach adapter refused an effect");
                    }
                }
                effect => warn!("unrouted effect {effect:?}"),
            }
        }
        if let Some(code) = exit {
            return NonZero::new(code).map_or(AppExit::Success, AppExit::Error);
        }
        if let Some(exit) = app.should_exit() {
            return exit;
        }
        immediate = matches!(
            app.world().resource::<NextState<ServerMode>>(),
            NextState::Pending(_)
        ) || !receiver.is_empty();
    }
}

enum Wait {
    Message(Inbound),
    Timeout,
    Closed,
}

fn wait(receiver: &Receiver<Inbound>, deadline: Option<Duration>) -> Wait {
    match deadline {
        None => receiver.recv_blocking().map_or(Wait::Closed, Wait::Message),
        Some(duration) => bevy_tasks::block_on(future::or(
            async { receiver.recv().await.map_or(Wait::Closed, Wait::Message) },
            async {
                Timer::after(duration).await;
                Wait::Timeout
            },
        )),
    }
}

/// How long the runner may sleep: bounded by the shutdown poll and the earliest `FinalRecord`
/// expiry; `None` sleeps until a message arrives.
fn deadline(
    app: &mut App,
    records: &mut QueryState<&FinalRecord>,
    tick: Duration,
) -> Option<Duration> {
    let world = app.world_mut();
    let shutting_down = world
        .get_resource::<State<ServerMode>>()
        .is_some_and(|s| *s.get() == ServerMode::ShuttingDown);
    if shutting_down {
        return Some(tick);
    }
    let now = wall_ms();
    records
        .iter(world)
        .map(|r| r.expires_ms)
        .min()
        .map(|expires| Duration::from_millis(expires.saturating_sub(now).max(1)))
}

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
