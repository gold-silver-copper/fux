use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

// Model the parking runner without any remote request or painted frame waking it.
fn idle_until(app: &mut App, condition: impl Fn(&World) -> bool) -> Outcome {
    let timed_out = Arc::new(AtomicBool::new(false));
    let timeout = timed_out.clone();
    let runner = std::thread::current();
    let (cancel, cancelled) = std::sync::mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        if matches!(
            cancelled.recv_timeout(Duration::from_secs(8)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ) {
            timeout.store(true, Ordering::Release);
            runner.unpark();
        }
    });
    while !condition(app.world()) {
        if pending(app.world()) {
            std::thread::park_timeout(Duration::from_millis(25));
        } else {
            std::thread::park();
        }
        // The safety wake must fail, never rescue a missed asset wake with an update.
        if timed_out.load(Ordering::Acquire) {
            return Err("idle runner missed asset wake".into());
        }
        app.update();
    }
    drop(cancel);
    watchdog.join().map_err(|_| "asset watchdog failed")?;
    Ok(())
}

#[test]
fn load_state_cannot_replace_idle_runner_pending_bridge() -> Outcome {
    const CHILD: &str = "FUX_ASSET_SPIKE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "assets::tests::reload_spike::load_state_cannot_replace_idle_runner_pending_bridge",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "asset spike child failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        print!("{}", String::from_utf8_lossy(&output.stdout));
        return Ok(());
    }
    // Isolate the global pool from concurrent unit tests; one blocked worker
    // exposes the exact interval before reload_internal's spawned task starts.
    IoTaskPool::get_or_init(|| bevy_tasks::TaskPoolBuilder::new().num_threads(1).build());
    assert_eq!(IoTaskPool::get().thread_num(), 1);
    let directory = std::env::temp_dir().join(format!("fux-asset-spike-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("fux.json");
    std::fs::write(&path, r#"{"prefix":"ctrl-x"}"#)?;
    let mut app = App::new();
    app.add_plugins(bevy_app::TaskPoolPlugin::default());
    app.insert_resource(Wake(std::thread::current()));
    install(&mut app, &path)?;
    app.finish();
    app.cleanup();
    app.update();
    idle_until(&mut app, |world| {
        !pending(world) && world.resource::<Settings>().prefix.as_str() == "ctrl-x"
    })?;
    let server = app.world().resource::<AssetServer>().clone();
    let handle = app.world().resource::<ConfigAssets>().settings.clone();
    assert!(server.load_state(handle.id()).is_loaded());

    let (started, ready) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel::<()>();
    IoTaskPool::get()
        .spawn(async move {
            let _ = started.send(());
            let _ = blocked.recv_timeout(Duration::from_secs(8));
        })
        .detach();
    ready.recv_timeout(Duration::from_secs(5))?;
    std::fs::write(&path, r#"{"prefix":"ctrl-y"}"#)?;
    // These are the watch bridge's mark and Bevy's event handler's reload call.
    app.world()
        .resource::<PendingAssets>()
        .start(PathBuf::from("fux.json"));
    server.reload("fux.json");
    app.update();
    assert!(server.load_state(handle.id()).is_loaded());
    assert!(!server.load_state(handle.id()).is_loading());
    assert!(pending(app.world()));
    assert_eq!(app.world().resource::<Settings>().prefix.as_str(), "ctrl-x");
    println!(
        "barrier: after reload + full update, native state=Loaded, native pending=false, bridge pending=true"
    );
    release.send(())?;
    idle_until(&mut app, |world| {
        !pending(world) && world.resource::<Settings>().prefix.as_str() == "ctrl-y"
    })?;

    // From here on only native file watcher events and the existing pending
    // deadline wake this otherwise idle runner. No explicit reload call.
    std::fs::write(&path, "invalid configuration")?;
    idle_until(&mut app, |world| {
        !pending(world) && server.load_state(handle.id()).is_failed()
    })?;
    assert_eq!(app.world().resource::<Settings>().prefix.as_str(), "ctrl-y");
    std::fs::write(&path, r#"{"prefix":"ctrl-z","layout":"absent.scn.ron"}"#)?;
    idle_until(&mut app, |world| {
        !pending(world) && world.resource::<Settings>().prefix.as_str() == "ctrl-z"
    })?;
    let layout = app
        .world()
        .resource::<ConfigAssets>()
        .layout
        .as_ref()
        .need()?
        .1
        .clone();
    assert!(server.load_state(layout.id()).is_failed());
    assert!(!app.world().resource::<ConfigAssets>().layout_needs_apply);
    std::fs::write(&path, r#"{"prefix":"ctrl-w"}"#)?;
    idle_until(&mut app, |world| {
        !pending(world) && world.resource::<Settings>().prefix.as_str() == "ctrl-w"
    })?;
    assert!(app.world().resource::<ConfigAssets>().layout.is_none());
    assert!(
        !app.world()
            .resource::<PendingAssets>()
            .0
            .lock()
            .contains_key(Path::new("absent.scn.ron"))
    );
    println!(
        "idle runner: initial load, queued reload, invalid config, recovery, failed layout and removed layout all settled without requests"
    );
    drop(app);
    std::fs::remove_dir_all(directory)?;
    Ok(())
}
