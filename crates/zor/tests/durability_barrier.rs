//! The real runner must not dispatch an effect whose preceding journal commit failed.
#![allow(
    clippy::unwrap_used,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]

use bevy_app::AppExit;
use bevy_ecs::prelude::*;
use fux::runner::{Params, Sources};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use zor::config::Config;
use zor::journal::Journal;
use zor::lifecycle::{self, LaunchSpec, Link, TaskSpec};
use zor::model::{Effect, Inbound, Location};
use zor::runner::{Adapter, ZorHost};

struct Recording(Arc<AtomicUsize>);
impl Adapter for Recording {
    fn handles(&self, effect: &Effect) -> bool {
        matches!(effect, Effect::FuxCall { .. })
    }
    fn apply(&mut self, effect: Effect) -> Result<Option<Inbound>, bevy_ecs::error::BevyError> {
        if matches!(effect, Effect::FuxCall { .. }) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        Ok(None)
    }
}

fn launch_and_run(fail_commit: bool) -> (AppExit, usize, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut app = zor::app::build_headless(&Config::default(), dir.path());
    app.update();
    app.world_mut().resource_mut::<Link>().instance = Some("expected-fux".into());
    let task = lifecycle::create_task(
        app.world_mut(),
        TaskSpec {
            id: "durability".into(),
            title: "durable launch".into(),
            location: Location::Cwd(dir.path().display().to_string()),
        },
    )
    .unwrap();
    lifecycle::launch(
        app.world_mut(),
        task,
        LaunchSpec {
            operation: "launch-once".into(),
            argv: vec!["/bin/true".into()],
            env: vec![],
            workspace: "default".into(),
            ephemeral: false,
            integration: None,
        },
    )
    .unwrap();
    if fail_commit {
        let blocker = dir.path().join("not-a-directory");
        std::fs::write(&blocker, b"block writes").unwrap();
        app.world_mut().resource_mut::<Journal>().path = blocker.join("journal.ron");
    }
    // Even an otherwise successful requested exit must not bypass a failed commit.
    app.world_mut().write_message(Effect::Exit { code: 0 });
    let calls = Arc::new(AtomicUsize::new(0));
    let host = ZorHost::new(
        app.world_mut(),
        vec![Box::new(Recording(Arc::clone(&calls)))],
    );
    let (_control_tx, control) = async_channel::bounded(16);
    let (_inbound_tx, inbound) = async_channel::bounded(16);
    let result = fux::runner::run(app, Sources { control, inbound }, host, Params::default());
    let recorded = calls.load(Ordering::SeqCst);
    (result, recorded, dir)
}

#[test]
fn failed_commit_rejects_launch_and_success_exit_before_any_adapter_dispatch() {
    let (result, calls, _) = launch_and_run(true);
    assert!(matches!(result, AppExit::Error(_)));
    assert_eq!(calls, 0);
}

#[test]
fn successful_commit_dispatches_once_and_restoration_never_replays_it() {
    let (result, calls, dir) = launch_and_run(false);
    assert_eq!(result, AppExit::Success);
    assert_eq!(calls, 1);
    let mut restored = zor::app::build_headless(&Config::default(), dir.path());
    restored.update();
    assert!(!restored.world_mut().resource_mut::<Messages<Effect>>().drain().any(|effect| {
        matches!(effect, Effect::FuxCall { method, .. } if method == "fux/root.new" || method == "fux/workspace.new")
    }));
}
