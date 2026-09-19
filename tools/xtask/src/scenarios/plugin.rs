//! Real plugin action lifecycle: an owned shell group, public logs and disable cleanup.

use serde_json::{Value, json};

use super::brp::Descriptor;
use super::pty::Terminal;
use super::{
    Fixture, Outcome, Result, TARGET, WAIT, capture, err, first_pane, live_attempt, nonce,
    require_target, until,
};

fn active(zor: &Descriptor) -> Result<()> {
    until(WAIT, "plugin activation and hook cursor seed", || {
        let state = zor.call("zor/plugin.inspect", json!({"name": "scenario"}))?;
        Ok((state["plugin"]["active"].as_bool() == Some(true)).then_some(()))
    })
}

fn action_runs(zor: &Descriptor) -> Result<Vec<Value>> {
    let state = zor.call("zor/plugin.inspect", json!({"name": "scenario"}))?;
    Ok(state["plugin"]["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|run| {
            run["kind"].as_str() == Some("action") && run["entry"].as_str() == Some("run")
        })
        .cloned()
        .collect())
}

fn binding(terminal: &Terminal) -> Result<()> {
    terminal.send(b"\x02")?;
    std::thread::sleep(super::TICK * 4);
    terminal.send(b"p")
}

struct DisableOnDrop(std::path::PathBuf);

impl Drop for DisableOnDrop {
    fn drop(&mut self) {
        // Read again because the hook scenario deliberately replaces the controller.
        if let Ok(zor) = Descriptor::read(&self.0) {
            let _ = zor.call("zor/plugin.disable", json!({"name": "scenario"}));
        }
    }
}

pub(super) fn lifecycle(fixture: &mut Fixture) -> Result<Outcome> {
    let target = require_target(fixture)?;
    let root = fixture.local.path().join("home/scenario-plugin");
    std::fs::create_dir_all(&root)?;
    let script = root.join("action.sh");
    std::fs::write(
        &script,
        concat!(
            "#!/bin/sh\n",
            "set -eu\n",
            "cp \"$ZOR_BRP\" scoped-zor.json\n",
            "cp \"$FUX_BRP\" scoped-fux.json\n",
            "printf '%s\\n' \"$ZOR_PLUGIN_WORKSPACE\" >> action-starts\n",
            "trap '' TERM\n",
            "sleep 60 &\n",
            "printf '%s %s\\n' \"$$\" \"$!\" > action-pids\n",
            "printf 'SCENARIO_PLUGIN_STARTED\\n'\n",
            "wait\n",
        ),
    )?;
    let argv = serde_json::to_string(&[
        "/bin/sh",
        script.to_str().ok_or_else(|| err("script path"))?,
    ])?;
    let hook = serde_json::to_string(&[
        "/bin/sh",
        "-c",
        "printf '%s\\n' \"$ZOR_PLUGIN_EVENT_NAME\" >> \"$1\"",
        "hook",
        root.join("hooks")
            .to_str()
            .ok_or_else(|| err("hook path"))?,
    ])?;
    std::fs::write(
        root.join("zor-plugin.toml"),
        format!(
            "name = \"scenario\"\nversion = \"1\"\n[[actions]]\nid = \"run\"\ntitle = \"Run\"\ncommand = {argv}\n\
         [[events]]\npattern = \"zor/TaskOpened\"\ncommand = {hook}\n"
        ),
    )?;
    let zor = fixture.local.zor()?;
    let fux = fixture.local.fux()?;
    let local_pane = first_pane(&fux)?;
    let workspace = "scenario-plugin";
    fux.call("fux/workspace.new", json!({"name": workspace}))?;
    let config = root.join("viewer-config");
    std::fs::create_dir_all(config.join("fux"))?;
    std::fs::write(
        config.join("fux/fux.toml"),
        "prefix = \"C-b\"\n[bindings]\np = \"plugin:scenario/run\"\n",
    )?;
    let mut command = fixture.local.command(fixture.local.fux_binary());
    command
        .env("XDG_CONFIG_HOME", &config)
        .env("ZOR_BRP", fixture.local.zor_descriptor_path());
    command.args([
        "attach",
        "--brp",
        fixture
            .local
            .fux_descriptor_path()
            .to_str()
            .ok_or_else(|| err("fux descriptor"))?,
        "--workspace",
        workspace,
    ]);
    let mut terminal = Terminal::spawn(command, 24, 120)?;
    let linked = zor.call("zor/plugin.link", json!({"path": root, "enabled": true}))?;
    let _cleanup = DisableOnDrop(fixture.local.zor_descriptor_path());
    until(WAIT, "plugin link operation committed", || {
        let operation = zor.call(
            "zor/plugin.operation",
            json!({"operation": linked["operation"]}),
        )?;
        match operation["state"].as_str() {
            Some("complete") => Ok(Some(())),
            Some("pending") => Ok(None),
            _ => Err(format!("plugin link failed: {operation}").into()),
        }
    })?;
    active(&zor)?;
    let exercised = (|| -> Result<(u64, Vec<i32>)> {
        let viewer = until(WAIT, "plugin binding viewer attached", || {
            if !terminal.running()? {
                return Err(err("plugin viewer exited"));
            }
            let viewers = fux.call("fux/viewer.list", json!({}))?;
            Ok(viewers["viewers"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|viewer| viewer["workspace"].as_str() == Some(workspace))
                .cloned())
        })?;
        let pane = viewer["target"]
            .as_u64()
            .ok_or_else(|| err("plugin viewer target pane"))?;
        // Attachment is server-side registration, not a painted input focus. Observe the
        // actual shell cells before sending the command once; do not retry dropped input.
        until(WAIT, "plugin viewer paints its target pane", || {
            if !terminal.running()? {
                return Err(err("plugin viewer exited before pane paint"));
            }
            let content = capture(&fux, pane)?;
            let screen = terminal.screen(24, 120);
            let pane_painted = content
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .any(|line| screen.iter().take(23).any(|cell| cell.trim() == line));
            let identity_painted = screen.last().is_some_and(|line| {
                line.contains(workspace) && line.contains(&format!("pane {pane}"))
            });
            Ok((pane_painted && identity_painted).then_some(()))
        })?;
        let marker = format!("PLUGIN-VIEWER-{}", nonce());
        terminal.send(format!("printf '\\n%s\\n' '{marker}'\r").as_bytes())?;
        let screen = until(WAIT, "plugin viewer renders real shell output", || {
            let screen = terminal.screen(24, 120);
            Ok(screen
                .iter()
                .any(|line| line.trim() == marker)
                .then_some(screen))
        })?;
        if !action_runs(&zor)?.is_empty() {
            return Err(err("plugin action ran before terminal binding"));
        }
        binding(&terminal)?;
        let accepted_screen = until(WAIT, "plugin binding renders accepted action", || {
            let screen = terminal.screen(24, 120);
            Ok(screen
                .iter()
                .any(|line| line.contains("plugin:scenario/run: accepted"))
                .then_some(screen))
        })?;
        let run_record = until(
            WAIT,
            "prefix plugin binding launches one owned action",
            || {
                let runs = action_runs(&zor)?;
                if runs.len() > 1 {
                    return Err(err("one plugin keybinding launched multiple actions"));
                }
                Ok(runs.into_iter().find(|run| {
                    run["state"].as_str() == Some("running")
                        && run["workspace"].as_str() == Some(workspace)
                }))
            },
        )?;
        let run = run_record["id"]
            .as_u64()
            .ok_or_else(|| err("plugin run id"))?;
        std::fs::write(
            fixture.artifacts.join("plugin-keybinding.json"),
            serde_json::to_vec_pretty(&json!({
                "binding": "C-b p", "action": "plugin:scenario/run", "viewer": viewer,
                "run": run_record, "local_fux_instance": fux.instance, "local_zor_instance": zor.instance,
                "remote_target": target, "screen_before_binding": screen,
                "screen_after_binding": accepted_screen,
            }))?,
        )?;
        let pids = until(WAIT, "plugin shell and child proof", || {
            let Ok(text) = std::fs::read_to_string(root.join("action-pids")) else {
                return Ok(None);
            };
            let pids = text
                .split_whitespace()
                .map(str::parse::<i32>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok((pids.len() == 2 && pids.iter().all(|pid| *pid > 1)).then_some(pids))
        })?;
        let logs = until(WAIT, "public plugin action logs", || {
            let logs = zor.call("zor/plugin.logs", json!({"name": "scenario", "limit": 100}))?;
            Ok(logs["lines"]
                .as_array()
                .is_some_and(|lines| {
                    lines.iter().any(|line| {
                        line.as_str()
                            .is_some_and(|text| text.contains("SCENARIO_PLUGIN_STARTED"))
                    })
                })
                .then_some(logs))
        })?;
        let scoped_zor = Descriptor::read(&root.join("scoped-zor.json"))?;
        // A valid grant must be usable, but must not administer the plugin that owns it.
        scoped_zor.call("zor/server.info", json!({}))?;
        if scoped_zor
            .call("zor/plugin.disable", json!({"name": "scenario"}))
            .is_ok()
        {
            return Err(err("plugin grant improperly authorizes admin mutation"));
        }
        let scoped_fux = Descriptor::read(&root.join("scoped-fux.json"))?;
        scoped_fux.call("fux/server.info", json!({}))?;
        if scoped_fux
            .call("fux/pane.capture", json!({"pane": local_pane}))
            .is_ok()
            || scoped_fux
                .call(
                    "fux/workspace.new",
                    json!({"name": "forbidden-plugin-workspace"}),
                )
                .is_ok()
        {
            return Err(err(
                "workspace-scoped plugin grant accessed another workspace or administered fux",
            ));
        }
        if !scoped_fux.raw["attach"].is_null() {
            return Err(err(
                "scoped plugin descriptor leaked the server-wide attachment credential",
            ));
        }
        let scoped_path = root.join("scoped-fux.json");
        let mut forbidden = fixture.local.command(fixture.local.fux_binary());
        forbidden.args([
            "attach",
            "--brp",
            scoped_path
                .to_str()
                .ok_or_else(|| err("scoped descriptor path"))?,
            "--workspace",
            workspace,
        ]);
        let mut refused_viewer = Terminal::spawn(forbidden, 24, 120)?;
        until(
            WAIT,
            "plugin grant refuses independent attachment authority",
            || Ok((!refused_viewer.running()?).then_some(())),
        )?;
        std::fs::write(
            fixture.artifacts.join("plugin-attachment-refusal.ansi"),
            refused_viewer.output(),
        )?;
        std::fs::write(
            fixture.artifacts.join("plugin-logs.json"),
            serde_json::to_vec_pretty(&logs)?,
        )?;
        if std::fs::read_to_string(root.join("action-starts"))? != format!("{workspace}\n") {
            return Err(err(
                "plugin binding ran outside its viewer workspace or ran more than once",
            ));
        }
        Ok((run, pids))
    })();
    // Cleanup is attempted even when startup/log evidence fails.
    let disabled = zor.call("zor/plugin.disable", json!({"name": "scenario"}));
    let saved_ansi = std::fs::write(fixture.artifacts.join("plugin.ansi"), terminal.output());
    let saved_cells = std::fs::write(
        fixture.artifacts.join("plugin-screen.txt"),
        terminal.screen(24, 120).join("\n"),
    );
    let (run, pids) = exercised?;
    disabled?;
    saved_ansi?;
    saved_cells?;
    until(WAIT, "plugin process group gone after disable", || {
        for pid in &pids {
            match nix::sys::signal::kill(nix::unistd::Pid::from_raw(*pid), None) {
                Err(nix::errno::Errno::ESRCH) => {}
                Ok(()) => return Ok(None),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Some(()))
    })?;
    let before_disabled: Vec<Value> = action_runs(&zor)?
        .into_iter()
        .map(|run| run["id"].clone())
        .collect();
    binding(&terminal)?;
    let disabled_screen = until(WAIT, "disabled plugin binding renders refusal", || {
        let screen = terminal.screen(24, 120);
        Ok(screen
            .iter()
            .any(|line| line.contains("plugin:scenario/run") && line.contains("disabled"))
            .then_some(screen))
    })?;
    if action_runs(&zor)?
        .iter()
        .map(|run| run["id"].clone())
        .collect::<Vec<_>>()
        != before_disabled
        || std::fs::read_to_string(root.join("action-starts"))? != format!("{workspace}\n")
    {
        return Err(err("disabled plugin keybinding executed an action"));
    }
    std::fs::write(
        fixture.artifacts.join("plugin-disabled-screen.txt"),
        disabled_screen.join("\n"),
    )?;
    let revoked_zor = Descriptor::read(&root.join("scoped-zor.json"))?;
    let revoked_fux = Descriptor::read(&root.join("scoped-fux.json"))?;
    until(
        WAIT,
        "disabled plugin grants revoked on both servers",
        || {
            // Owner probes distinguish revocation from a server that simply stopped responding.
            zor.call("zor/server.info", json!({}))?;
            fixture.local.fux()?.call("fux/server.info", json!({}))?;
            Ok((revoked_zor.call("zor/server.info", json!({})).is_err()
                && revoked_fux.call("fux/server.info", json!({})).is_err())
            .then_some(()))
        },
    )?;
    if revoked_zor
        .call(
            "zor/plugin.run",
            json!({"name": "scenario", "action": "run",
        "workspace": workspace, "expected_fux_instance": fux.instance}),
        )
        .is_ok()
    {
        return Err(err("revoked plugin descriptor accepted an action"));
    }
    if action_runs(&zor)?
        .iter()
        .map(|run| run["id"].clone())
        .collect::<Vec<_>>()
        != before_disabled
        || std::fs::read_to_string(root.join("action-starts"))? != format!("{workspace}\n")
    {
        return Err(err("revoked action executed a process"));
    }
    drop(terminal);
    if live_attempt(fixture, TARGET)? != Some(target) {
        return Err(err("disabling Local plugin changed Remote task ownership"));
    }
    std::fs::write(
        fixture.artifacts.join("plugin-cleanup.json"),
        serde_json::to_vec_pretty(&json!({
            "run": run, "pids": pids, "gone": true, "remote_target": target,
            "disabled_binding_refused": true, "revoked_action_refused": true,
            "local_pane_capture": capture(&fux, local_pane)?,
        }))?,
    )?;
    // Hooks run real processes after a persisted event claim. Reopening the owner and then
    // creating another event must not replay the previously completed external append.
    zor.call("zor/plugin.enable", json!({"name": "scenario"}))?;
    active(&zor)?;
    let cwd = fixture.local.path().join("home");
    let cwd = cwd.to_str().ok_or_else(|| err("hook cwd"))?;
    fixture.local.zor_run(&[
        "task",
        "create",
        "scn-hook-before",
        "--title",
        "before",
        "--cwd",
        cwd,
    ])?;
    until(WAIT, "first hook artifact", || {
        Ok(std::fs::read_to_string(root.join("hooks"))
            .ok()
            .filter(|text| text == "zor/TaskOpened\n")
            .map(|_| ()))
    })?;
    fixture.local.kill_zor()?;
    fixture.local.start_zor()?;
    active(&fixture.local.zor()?)?;
    fixture.local.zor_run(&[
        "task",
        "create",
        "scn-hook-after",
        "--title",
        "after",
        "--cwd",
        cwd,
    ])?;
    let hooks = until(WAIT, "second hook without replay", || {
        let text = std::fs::read_to_string(root.join("hooks")).unwrap_or_default();
        if text.lines().count() > 2 {
            return Err(err("hook replay repeated an external append"));
        }
        Ok((text == "zor/TaskOpened\nzor/TaskOpened\n").then_some(text))
    })?;
    fixture
        .local
        .zor()?
        .call("zor/plugin.disable", json!({"name": "scenario"}))?;
    if std::fs::read_to_string(root.join("action-starts"))? != format!("{workspace}\n") {
        return Err(err(
            "disabled action replayed after plugin enable or controller restart",
        ));
    }
    std::fs::write(fixture.artifacts.join("plugin-hooks.txt"), hooks)?;
    Ok(Outcome::Pass(format!(
        "terminal C-b p invoked plugin:scenario/run as owned action {run} with public logs and viewer-workspace grants; disabled binding rendered refusal and revoked action did not run; disable reaped shell and child {pids:?}; two event hooks across restart appended once each; Remote pane {} pid {} preserved",
        target.0, target.1,
    )))
}
