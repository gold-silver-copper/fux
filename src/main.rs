mod assets;
mod chrome;
mod control;
mod model;
mod presentation;
mod protocol;
mod server;
mod terminal;
mod viewer;

use bevy_app::{App, AppExit, TaskPoolPlugin};
use bevy_ecs::prelude::*;
use bevy_remote::{BrpReceiver, http::RemoteHttpPlugin};
use bevy_tasks::IoTaskPool;
use serde_json::json;
use std::{
    net::IpAddr,
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
fn execute() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "help".into());
    let endpoint =
        std::env::var("FUX_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:15702".into());
    match mode.as_str() {
        "attach" => viewer::run(&endpoint, args.next().as_deref()),
        "rpc" => {
            let method = args.next().ok_or("rpc requires METHOD [JSON]")?;
            let params = args
                .next()
                .map(|s| serde_json::from_str(&s))
                .transpose()
                .map_err(|e| e.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&viewer::rpc(&endpoint, &method, params)?)
                    .map_err(|e| e.to_string())?
            );
            Ok(())
        }
        "stop" => viewer::rpc(
            &endpoint,
            "world.trigger_event",
            Some(json!({"event":"fux::control::Shutdown","value":null})),
        )
        .map(|_| ()),
        "server" => {
            let mut address: IpAddr = "127.0.0.1"
                .parse()
                .map_err(|e: std::net::AddrParseError| e.to_string())?;
            let mut port = 15702;
            let mut config = "fux.json".to_owned();
            while let Some(flag) = args.next() {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for {flag}"))?;
                match flag.as_str() {
                    "--address" => {
                        address = value
                            .parse()
                            .map_err(|e: std::net::AddrParseError| e.to_string())?
                    }
                    "--port" => port = value.parse::<u16>().map_err(|e| e.to_string())?,
                    "--config" => config = value,
                    _ => return Err(format!("unknown server option {flag}")),
                }
            }
            let mut app = App::new();
            app.add_plugins((
                TaskPoolPlugin::default(),
                bevy_time::TimePlugin,
                bevy_log::LogPlugin::default(),
            ))
            .insert_resource(model::Wake(thread::current()));
            assets::install(&mut app, Path::new(&config))?;
            app.add_plugins((
                server::ServerPlugin,
                server::remote(),
                RemoteHttpPlugin::default()
                    .with_address(address)
                    .with_port(port),
            ));
            app.set_runner(run);
            eprintln!(
                "fux trusted BRP http://{address}:{port} — unrestricted same-user command execution"
            );
            match app.run() {
                AppExit::Success => Ok(()),
                exit => Err(format!("server exited: {exit:?}")),
            }
        }
        "help" | "--help" | "-h" => {
            println!(
                "fux server [--address IP] [--port PORT] [--config FILE]\nfux attach [WORKSPACE]\nfux rpc METHOD [JSON]\nfux stop\n\nFUX_ENDPOINT selects the client endpoint (default http://127.0.0.1:15702).\nAttached controls: ctrl-b then ? for help; ctrl-b d detaches."
            );
            Ok(())
        }
        _ => Err("expected server, attach, rpc, stop or help".into()),
    }
}
fn run(mut app: App) -> AppExit {
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
            {
                let response = message.sender.clone();
                let closed = closed_sender.clone();
                let wake = runner.clone();
                IoTaskPool::get()
                    .spawn(async move {
                        response.closed().await;
                        let _ = closed.send(Entity::from_bits(id)).await;
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
        if let Some(exit) = app.should_exit() {
            break exit;
        }
        if assets::pending(app.world()) {
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
    drop(bridge);
    signal_handle.close();
    let _ = signal_thread.join();
    exit
}
