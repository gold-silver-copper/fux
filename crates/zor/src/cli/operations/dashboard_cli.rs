//! The parent switches real fux viewers; the dashboard itself remains a hosted Bevy scene.
use super::*;
use nix::sys::termios::{SetArg, Termios, tcgetattr, tcsetattr};
use std::io::Write;
use std::sync::LazyLock;

fn signal_receiver() -> Result<async_channel::Receiver<fux::model::Signal>, BevyError> {
    static SIGNALS: LazyLock<Result<async_channel::Receiver<fux::model::Signal>, String>> =
        LazyLock::new(|| {
            bevy_tasks::IoTaskPool::get_or_init(|| {
                bevy_tasks::TaskPoolBuilder::new()
                    .num_threads(1)
                    .thread_name("zor-cli-signals".into())
                    .build()
            });
            let (sender, receiver) = async_channel::bounded(16);
            fux::runner::signals::install_with(sender, |signal| signal)
                .map_err(|e| e.to_string())?;
            Ok(receiver)
        });
    SIGNALS.clone().map_err(BevyError::from)
}

struct Viewer {
    child: Child,
    terminal: Option<Termios>,
    signals: async_channel::Receiver<fux::model::Signal>,
}
impl Viewer {
    fn start(
        brp: &Path,
        zor_brp: &Path,
        workspace: &str,
        pane: Option<&PaneIdentity>,
    ) -> Result<Self, BevyError> {
        // Save the parent's termios so forced child cleanup cannot strand raw mode.
        let terminal = tcgetattr(std::io::stdin()).ok();
        let signals = signal_receiver()?;
        let child = viewer(brp, zor_brp, workspace, pane)?;
        Ok(Self {
            child,
            terminal,
            signals,
        })
    }
    fn done(&mut self) -> Result<Option<i32>, BevyError> {
        if self.signals.try_recv().is_ok() {
            return Ok(Some(130));
        }
        Ok(self
            .child
            .try_wait()?
            .map(|status| status.code().unwrap_or(1)))
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            // This is the child we spawned, not a discovered or recorded pid.
            if let Ok(pid) = i32::try_from(self.child.id()) {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid),
                    nix::sys::signal::Signal::SIGTERM,
                );
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if !matches!(self.child.try_wait(), Ok(Some(_))) {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
        }
        if let Some(terminal) = &self.terminal {
            let _ = tcsetattr(std::io::stdin(), SetArg::TCSANOW, terminal);
        }
    }
}

pub(super) fn exact_viewer(
    brp: &Path,
    zor_brp: &Path,
    pane: &PaneIdentity,
) -> Result<i32, BevyError> {
    let mut viewer = Viewer::start(brp, zor_brp, &pane.workspace, Some(pane))?;
    loop {
        if let Some(code) = viewer.done()? {
            return Ok(code);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct Session<'a> {
    control: &'a Descriptor,
    id: u64,
    owned_workspace: Option<(&'a Descriptor, &'a str)>,
    fux: &'a Descriptor,
    surface: u64,
    provider: &'a str,
}
impl Drop for Session<'_> {
    fn drop(&mut self) {
        let _ = client::call_with(self.control, "zor/dashboard.close", json!({"id":self.id}));
        let _ = client::call_with(
            self.fux,
            "fux/surface.close",
            json!({
                "surface":self.surface,"expected_provider":self.provider,
            }),
        );
        if let Some((fux, workspace)) = self.owned_workspace {
            // One guarded attempt: an ambiguous response never authorizes mutation replay.
            let _ = client::call_with(
                fux,
                "fux/workspace.kill",
                json!({"name":workspace,"empty_only":true}),
            );
        }
    }
}

pub fn run(
    paths: &Paths,
    control: &Descriptor,
    machine: Option<&str>,
    args: DashboardArgs,
) -> Result<i32, BevyError> {
    if args.once {
        return super::super::print_reply(client::call_with(
            control,
            "zor/dashboard.rows",
            json!({"machine":machine}),
        )?);
    }
    let config = crate::config::Config::load(&paths.config_file())?;
    let fux = client::read_descriptor(&paths.fux_descriptor(&config.fux_server))?;
    let owns_workspace = args.workspace.is_none();
    let (workspace, node, generation) = if let (Some(workspace), Some(node)) =
        (args.workspace, args.node)
    {
        (
            workspace,
            node,
            args.generation
                .ok_or("an existing dashboard leaf requires --generation")?,
        )
    } else {
        let mut suffix = fux::attach::random_hex256()?;
        suffix.truncate(12);
        let workspace = format!("dashboard-{suffix}");
        client::call_with(
            &fux,
            "fux/workspace.new",
            json!({"name":workspace,"empty":true}),
        )?;
        let listed = client::call_with(&fux, "fux/workspace.list", json!({}))?;
        let root = listed
            .get("workspaces")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|entry| entry.get("name").and_then(Value::as_str) == Some(&workspace))
            .and_then(|entry| entry.get("roots"))
            .and_then(Value::as_array)
            .and_then(|roots| roots.first())
            .ok_or("created dashboard workspace has no root")?;
        let leaf = client::call_with(
            &fux,
            "fux/node.spawn",
            json!({
                "parent":root["id"],"generation":root["generation"],
                "patch":{"flex_grow":1.0,"width":"100%","height":"100%","min_width":"0px","min_height":"0px"},
            }),
        )?;
        (
            workspace,
            leaf.get("node")
                .and_then(Value::as_u64)
                .ok_or("surface leaf identity absent")?,
            leaf.get("generation")
                .and_then(Value::as_u64)
                .ok_or("surface leaf generation absent")?,
        )
    };
    let opened = client::call_with(
        control,
        "zor/dashboard.open",
        json!({"workspace":workspace,"node":node,"generation":generation,"machine":machine}),
    )?;
    let id = opened
        .get("id")
        .and_then(Value::as_u64)
        .ok_or("dashboard identity absent")?;
    let provider = opened
        .get("provider")
        .and_then(Value::as_str)
        .ok_or("dashboard provider identity absent")?;
    let _session = Session {
        control,
        id,
        fux: &fux,
        surface: node,
        provider,
        owned_workspace: owns_workspace.then_some((&fux, workspace.as_str())),
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let state = client::call_with(control, "zor/dashboard.state", json!({"id":id}))?;
        match state.get("status").and_then(Value::as_str) {
            Some("open") => break,
            Some("failed" | "closed") => {
                return Err(state
                    .get("problem")
                    .and_then(Value::as_str)
                    .unwrap_or("dashboard closed while opening")
                    .to_owned()
                    .into());
            }
            _ if Instant::now() >= deadline => {
                return Err("dashboard did not open before deadline".into());
            }
            _ => std::thread::sleep(Duration::from_millis(30)),
        }
    }
    let brp = temporary_descriptor(paths, &fux)?;
    let zor_brp = temporary_descriptor(paths, control)?;
    let mut viewer = Viewer::start(&brp.0, &zor_brp.0, &workspace, None)?;
    loop {
        if let Some(code) = viewer.done()? {
            return Ok(code);
        }
        let state: crate::dashboard::State = serde_json::from_value(client::call_with(
            control,
            "zor/dashboard.state",
            json!({"id":id}),
        )?)?;
        if matches!(state.status.as_str(), "closed" | "failed") {
            if let Some(problem) = state.problem {
                return Err(problem.into());
            }
            return Ok(0);
        }
        if let Some(handoff) = state.handoff {
            drop(viewer);
            let selected = Some(handoff.target.machine.as_str());
            let target_control = selected_control(control, selected);
            let outcome = target_control.and_then(|target_control| {
                attach(
                    paths,
                    control,
                    &target_control,
                    selected,
                    &handoff.target.task,
                    Some(&handoff.target),
                )
            });
            client::call_with(
                control,
                "zor/dashboard.return",
                json!({"id":id,"handoff":handoff.id}),
            )?;
            match outcome {
                Ok(130) => return Ok(130),
                Ok(_) => {}
                Err(error) => {
                    let _ = writeln!(std::io::stderr(), "dashboard attachment refused: {error}");
                }
            }
            viewer = Viewer::start(&brp.0, &zor_brp.0, &workspace, None)?;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
