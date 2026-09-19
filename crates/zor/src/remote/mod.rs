//! zor's BRP surface (prompt 4.3): `RemotePlugin` + `RemoteHttpPlugin` on a pre-probed loopback
//! port, the method table replaced with the token-checked allowlist, `<runtime>/zor/<name>.brp.json`,
//! reflected projections for the wrapped `world.*` reads, `zor/events+watch` over the retained
//! [`events::EventLog`]. Dispatch is zor's own, in fux's shape: at `Startup` the plugin takes
//! `bevy_remote`'s `BrpReceiver` out of the World and forwards its messages from `IoTaskPool`
//! into a mailbox while sending `Inbound::Wake`; the dispatcher in `RemoteLast` (moved after
//! `First`) answers unknown names itself instead of stalling the tick.
//!
//! Reused from fux: `Tokens`/`Grant`/`Capabilities`, the descriptor reader/writer and the
//! thin client. `Grant.workspace` is a scope name; zor tokens are unscoped for now.

pub mod check_methods;
pub mod dashboard_methods;
pub mod events;
pub mod group_methods;
pub mod machine_methods;
pub mod methods;
pub mod plugin_methods;
pub mod projection;
pub mod provider_methods;
pub mod resume_methods;
pub mod task_methods;
pub mod watch;

use std::path::PathBuf;

use async_channel::{Receiver, Sender};
use bevy_app::prelude::*;
use bevy_ecs::error::BevyError;
use bevy_ecs::prelude::*;
use bevy_ecs::schedule::ScheduleLabel;
use bevy_remote::http::{HostAddress, HostPort};
use bevy_remote::{
    BrpError, BrpMessage, BrpReceiver, RemoteLast, RemoteMethodSystemId, RemoteMethods,
    RemotePlugin, RemoteSystems, error_codes,
};
use bevy_tasks::IoTaskPool;
pub use fux::remote::descriptor::{self, Descriptor, DescriptorGuard, Endpoint};
pub use fux::remote::token::{self, Capabilities, Capability, Grant, Tokens};
pub use fux::remote::{client, schema};

use crate::model::{Inbound, Limits, Phase, ServerInstance};

/// Bound on requests parked between updates; HTTP tasks await on it, so a flood backs up into
/// the sockets instead of memory.
const MAILBOX_SIZE: usize = 64;
/// Requests answered per update, matching the runner's batch bound.
const DISPATCH_BATCH: usize = 1024;

pub struct RemoteHostPlugin {
    pub runtime_dir: PathBuf,
    pub server_name: String,
    /// The runner's inbound channel: `Inbound::Wake` is sent for every parked request.
    pub inbound: Sender<Inbound>,
}

/// Requests forwarded from `bevy_remote`'s mailbox, drained by [`dispatch`].
#[derive(Resource)]
struct Mailbox(Receiver<BrpMessage>);

/// Where `brp.json` lives; read by the descriptor writer at `PostStartup`.
#[derive(Resource, Debug, Clone)]
pub struct DescriptorPath(pub PathBuf);

/// Systems of this plugin, for ordering by others.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RemoteHostSystems {
    /// `RemoteLast`: answer parked requests, open streams.
    Dispatch,
    /// `PostUpdate`/`Phase::Projection`: refresh projection components.
    Projection,
    /// `Last`: deliver to open streams after this update's lifecycle events.
    Watch,
}

impl Plugin for RemoteHostPlugin {
    fn build(&self, app: &mut App) {
        let world = app.world_mut();
        let (nonce, token) = match (fux::attach::random_hex256(), fux::attach::random_hex256()) {
            (Ok(nonce), Ok(token)) => (nonce, token),
            (Err(e), _) | (_, Err(e)) => {
                bevy_log::error!("no randomness for the server token: {e}");
                world.write_message(AppExit::error());
                return;
            }
        };
        {
            let mut instance = world.get_resource_or_init::<ServerInstance>();
            if instance.nonce.is_empty() {
                instance.nonce = nonce;
                instance.name = self.server_name.clone();
                instance.pid = std::process::id();
                instance.started_ms = fux::runner::wall_ms();
            }
        }
        let limits = world.get_resource_or_init::<Limits>().clone();
        world.insert_resource(Tokens::new(token, limits.tokens));
        world.insert_resource(events::EventLog::new(&limits));
        world.insert_resource(DescriptorPath(descriptor::descriptor_path(
            &self.runtime_dir,
            &self.server_name,
        )));

        app.add_plugins((
            RemotePlugin::default(),
            fux::remote::http::BoundedHttpPlugin,
        ));

        let world = app.world_mut();
        let mut watches = watch::Watches::default();
        let placeholder = world.register_system(watch::placeholder);
        let mut table = RemoteMethods::new();
        for spec in methods::all_specs() {
            let id = match spec.handler {
                methods::Handler::Instant(handler) => {
                    RemoteMethodSystemId::Instant(world.register_system(handler))
                }
                methods::Handler::Watch(open) => {
                    watches.register(spec.name, open);
                    RemoteMethodSystemId::Watching(placeholder)
                }
            };
            table.insert(spec.name, id);
        }
        for (name, handler) in methods::WRAPPED {
            table.insert(
                *name,
                RemoteMethodSystemId::Instant(world.register_system(*handler)),
            );
        }
        for (name, open) in watch::WATCHED {
            watches.register(name, *open);
            table.insert(*name, RemoteMethodSystemId::Watching(placeholder));
        }
        world.insert_resource(table);
        world.insert_resource(watches);

        let remote_last = RemoteLast.intern();
        let mut order = world.resource_mut::<bevy_app::MainScheduleOrder>();
        order.labels.retain(|label| *label != remote_last);
        order.insert_after(First, RemoteLast);

        projection::register_types(app);
        events::register(app);
        let inbound = self.inbound.clone();
        app.add_systems(Startup, move |world: &mut World| {
            take_mailbox(world, &inbound);
        });
        app.add_systems(PostStartup, write_descriptor)
            .configure_sets(
                RemoteLast,
                RemoteHostSystems::Dispatch.before(RemoteSystems::ProcessRequests),
            )
            .add_systems(RemoteLast, dispatch.in_set(RemoteHostSystems::Dispatch))
            .configure_sets(
                PostUpdate,
                RemoteHostSystems::Projection.in_set(Phase::Projection),
            )
            .add_systems(
                PostUpdate,
                projection::sync.in_set(RemoteHostSystems::Projection),
            )
            .configure_sets(Last, RemoteHostSystems::Watch.before(Phase::Effects))
            .add_systems(Last, watch::poll.in_set(RemoteHostSystems::Watch));
    }
}

/// Moves `bevy_remote`'s receiver into a forwarder task that wakes the runner per request.
fn take_mailbox(world: &mut World, inbound: &Sender<Inbound>) {
    let Some(receiver) = world.remove_resource::<BrpReceiver>() else {
        return;
    };
    let source: Receiver<BrpMessage> = (*receiver).clone();
    let (tx, rx) = async_channel::bounded(MAILBOX_SIZE);
    world.insert_resource(Mailbox(rx));
    let inbound = inbound.clone();
    IoTaskPool::get()
        .spawn(async move {
            while let Ok(message) = source.recv().await {
                if tx.send(message).await.is_err() {
                    break;
                }
                let _ = inbound.send(Inbound::Wake).await;
            }
        })
        .detach();
}

/// Answers every parked request: instant handlers run now, a `+watch` opens its stream, and an
/// unknown name is answered here and never stalls the ones behind it.
fn dispatch(world: &mut World) {
    for _ in 0..DISPATCH_BATCH {
        let Some(message) = world
            .get_resource::<Mailbox>()
            .and_then(|m| m.0.try_recv().ok())
        else {
            return;
        };
        let handler = world
            .resource::<RemoteMethods>()
            .get(&message.method)
            .copied();
        let result = match handler {
            Some(RemoteMethodSystemId::Instant(id)) => world
                .run_system_with(id, message.params)
                .unwrap_or_else(|e| {
                    Err(BrpError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("failed to run method handler: {e}"),
                        data: None,
                    })
                }),
            Some(RemoteMethodSystemId::Watching(_)) => {
                watch::open(world, message);
                continue;
            }
            None => Err(BrpError {
                code: error_codes::METHOD_NOT_FOUND,
                message: format!("Method `{}` not found", message.method),
                data: None,
            }),
        };
        let _ = message.sender.force_send(result);
    }
}

/// Writes `brp.json` (0600, atomic) once the listener published its port.
fn write_descriptor(world: &mut World) -> Result<(), BevyError> {
    let path = world.resource::<DescriptorPath>().0.clone();
    let instance = world.resource::<ServerInstance>();
    let descriptor = Descriptor {
        instance: instance.nonce.clone(),
        pid: instance.pid,
        http: Endpoint {
            host: world.resource::<HostAddress>().0.to_string(),
            port: world.resource::<HostPort>().0,
        },
        attach: None,
        token: world.resource::<Tokens>().server().to_owned(),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    descriptor::write_descriptor(&path, &descriptor)?;
    world.insert_resource(DescriptorGuard(path));
    Ok(())
}
