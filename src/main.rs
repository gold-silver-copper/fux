mod actions;
mod assets;
mod chrome;
mod control;
mod encode;
mod frame;
mod interaction;
mod model;
mod navigation;
mod paste;
mod presentation;
mod protocol;
mod selection;
mod server;
mod terminal;
#[cfg(test)]
mod testing;
mod transport;
mod unix_http;
mod viewer;

use bevy_app::{App, AppExit, TaskPoolPlugin};
use bevy_ecs::prelude::*;
use bevy_remote::{BrpReceiver, BrpSender};
use bevy_tasks::IoTaskPool;
use serde_json::json;
use std::{
    os::unix::net::UnixListener,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

fn main() {
    if let Err(error) = execute() {
        eprintln!("fux: {error}");
        std::process::exit(1);
    }
}
const USAGE: &str = "fux server [--socket PATH] [--config FILE]\nfux attach [WORKSPACE]\nfux rpc METHOD [JSON]\nfux stop\n\nThe server listens only on a Unix domain socket. Its path is --socket, else\nFUX_SOCKET, else $XDG_RUNTIME_DIR/fux/server.sock, else $TMPDIR/fux/server.sock;\nclients use FUX_SOCKET, else the same default. The socket's directory must be\nyours with mode 0700; the socket is created with mode 0600. FUX_ENDPOINT,\n--address and --port were removed.\nAttached controls: ctrl-b opens the command column; ctrl-b d detaches.";

fn execute() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "help".into());
    if matches!(mode.as_str(), "help" | "--help" | "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    if !matches!(mode.as_str(), "server" | "attach" | "rpc" | "stop") {
        return Err("expected server, attach, rpc, stop or help".into());
    }
    transport::reject_retired_environment()?;
    if mode == "server" {
        let mut socket = None;
        let mut config = "fux.json".to_owned();
        while let Some(flag) = args.next() {
            if matches!(flag.as_str(), "--address" | "--port") {
                return Err(format!(
                    "{flag} was removed: fux serves only on a Unix domain socket; use --socket PATH"
                ));
            }
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))?;
            match flag.as_str() {
                "--socket" => socket = Some(value),
                "--config" => config = value,
                _ => return Err(format!("unknown server option {flag}")),
            }
        }
        return serve(&transport::socket_path(socket.as_deref())?, &config);
    }
    let socket = transport::socket_path(None)?;
    transport::check_client_socket(&socket)?;
    match mode.as_str() {
        "attach" => viewer::run(&socket, args.next().as_deref()),
        "rpc" => {
            let method = args.next().ok_or("rpc requires METHOD [JSON]")?;
            let params = args
                .next()
                .map(|s| serde_json::from_str(&s))
                .transpose()
                .map_err(|e| e.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&viewer::rpc(&socket, &method, params)?)
                    .map_err(|e| e.to_string())?
            );
            Ok(())
        }
        _ => viewer::rpc(
            &socket,
            "world.trigger_event",
            Some(json!({"event":"fux::control::Shutdown","value":null})),
        )
        .map(|_| ()),
    }
}

fn serve(socket: &Path, config: &str) -> Result<(), String> {
    // Bind before anything else, so a location or ownership problem fails
    // startup with its reason instead of leaving a server nobody can reach.
    let (endpoint, listener) = transport::bind_socket(socket)?;
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        bevy_time::TimePlugin,
        bevy_log::LogPlugin::default(),
    ))
    .insert_resource(model::Wake(thread::current()));
    assets::install(&mut app, Path::new(config))?;
    app.add_plugins((server::ServerPlugin, server::remote()));
    app.set_runner(move |app| run(app, listener));
    eprintln!(
        "fux trusted BRP unix:{} — unrestricted same-user command execution",
        endpoint.path().display()
    );
    let exit = app.run();
    // Dropping the endpoint removes the socket this server bound.
    drop(endpoint);
    match exit {
        AppExit::Success => Ok(()),
        exit => Err(format!("server exited: {exit:?}")),
    }
}
fn run(mut app: App, listener: UnixListener) -> AppExit {
    let (closed_sender, closed_receiver) = async_channel::unbounded();
    app.insert_resource(server::Disconnected(closed_receiver));
    let stopping = Arc::new(AtomicBool::new(false));
    let mut signals = match signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ]) {
        Ok(signals) => signals,
        Err(error) => {
            eprintln!("signal handler: {error}");
            return AppExit::error();
        }
    };
    let signal_handle = signals.handle();
    let signal_stopping = Arc::clone(&stopping);
    let signal_runner = thread::current();
    let signal_thread = thread::spawn(move || {
        if signals.forever().next().is_some() {
            signal_stopping.store(true, Ordering::Release);
            signal_runner.unpark();
        }
    });
    app.finish();
    app.cleanup();
    app.update();
    let Some(requests) = app
        .world()
        .get_resource::<BrpSender>()
        .map(|sender| async_channel::Sender::clone(sender))
    else {
        eprintln!("BRP mailbox missing");
        return AppExit::error();
    };
    let failure = Arc::new(parking_lot::Mutex::new(None::<String>));
    let serving = {
        let failure = Arc::clone(&failure);
        let runner = thread::current();
        transport::serve(listener, requests, move |error| {
            *failure.lock() = Some(error);
            runner.unpark();
        })
    };
    let serving = match serving {
        Ok(serving) => serving,
        Err(error) => {
            eprintln!("{error}");
            return AppExit::error();
        }
    };
    let (sender, receiver) = async_channel::bounded(64);
    let incoming = std::mem::replace(
        &mut **app.world_mut().resource_mut::<BrpReceiver>(),
        receiver,
    );
    let runner = thread::current();
    let bridge = IoTaskPool::get().spawn(async move {
        while let Ok(message) = incoming.recv().await {
            // Native attachment lifetime follows Bevy's response channel. All
            // requests still go unchanged to the full stock method registry.
            if message.method == "fux.frame+watch"
                && let Some(id) = message
                    .params
                    .as_ref()
                    .and_then(|p| p.get("viewer"))
                    .and_then(|v| v.as_u64())
                    .and_then(Entity::try_from_bits)
            {
                let response = message.sender.clone();
                let closed = closed_sender.clone();
                let wake = runner.clone();
                IoTaskPool::get()
                    .spawn(async move {
                        response.closed().await;
                        let _ = closed.send(id).await;
                        wake.unpark();
                    })
                    .detach();
            }
            if sender.send(message).await.is_err() {
                break;
            }
            runner.unpark();
        }
    });
    let exit = loop {
        if stopping.load(Ordering::Acquire) {
            break AppExit::Success;
        }
        if let Some(error) = failure.lock().take() {
            eprintln!("{error}");
            break AppExit::error();
        }
        if let Some(exit) = app.should_exit() {
            break exit;
        }
        if assets::pending(app.world()) || server::pending_scenes(app.world_mut()) {
            thread::park_timeout(Duration::from_millis(25));
        } else {
            thread::park();
        }
        app.update();
    };
    let world = app.world_mut();
    let mut terminals = world.query_filtered::<Entity, With<terminal::Terminal>>();
    let entities: Vec<_> = terminals.iter(world).collect();
    for entity in entities {
        world.entity_mut(entity).remove::<terminal::Terminal>();
    }
    drop(serving);
    drop(bridge);
    signal_handle.close();
    let _ = signal_thread.join();
    exit
}
