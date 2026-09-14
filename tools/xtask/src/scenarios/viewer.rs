//! Production viewer interaction contract, observed through private terminal models.
use crate::support::{
    local::{Root, stop_servers},
    process,
    terminal::Terminal,
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Default)]
struct Clipboard(Vec<Vec<u8>>);
impl vt100::Callbacks for Clipboard {
    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, kind: &[u8], data: &[u8]) {
        if kind == b"c" {
            self.0.push(data.to_vec());
        }
    }
}
struct Viewer {
    terminal: Terminal,
    screen: vt100::Parser<Clipboard>,
    escape: EscapeBound,
}
#[derive(Default)]
struct EscapeBound {
    osc_bytes: Option<usize>,
    escaped: bool,
}
impl EscapeBound {
    fn feed(&mut self, bytes: &[u8]) -> Result<()> {
        for byte in bytes {
            if let Some(count) = &mut self.osc_bytes {
                *count += 1;
                ensure!(*count < 2 * 1024 * 1024, "unbounded terminal escape");
                if *byte == 7 || (self.escaped && *byte == b'\\') {
                    self.osc_bytes = None;
                }
            } else if self.escaped && *byte == b']' {
                self.osc_bytes = Some(2);
            }
            self.escaped = *byte == 27;
        }
        Ok(())
    }
}
struct Harness {
    root: Root,
    binary: PathBuf,
    viewers: Vec<Viewer>,
    finished: bool,
}
impl Harness {
    fn add(&mut self, args: &[&str]) -> Result<()> {
        self.viewers.push(Viewer {
            terminal: Terminal::start_with_args(&self.root, &self.binary, args)?,
            screen: vt100::Parser::new_with_callbacks(24, 80, 0, Clipboard::default()),
            escape: EscapeBound::default(),
        });
        Ok(())
    }
    fn send(&self, index: usize, bytes: &[u8]) -> Result<()> {
        self.viewers[index].terminal.send(bytes)
    }
    fn text(&self, index: usize) -> String {
        self.rows(index).join("\n")
    }
    fn rows(&self, index: usize) -> Vec<String> {
        let screen = self.viewers[index].screen.screen();
        screen.rows(0, screen.size().1).collect()
    }
    fn bar(&self, index: usize) -> String {
        self.rows(index).pop().unwrap_or_default()
    }
    fn copies(&self, index: usize) -> usize {
        self.viewers[index]
            .screen
            .callbacks()
            .0
            .iter()
            .filter(|data| data.as_slice() == b"Q09QWV9UQVJHRVQ=")
            .count()
    }
    fn pump(&mut self, seconds: f64) -> Result<()> {
        let end = Instant::now() + Duration::from_secs_f64(seconds);
        loop {
            for viewer in &mut self.viewers {
                viewer.terminal.pump()?;
                let bytes = viewer.terminal.take_output();
                viewer.escape.feed(&bytes)?;
                viewer.screen.process(&bytes);
                ensure!(
                    viewer
                        .screen
                        .callbacks()
                        .0
                        .iter()
                        .map(Vec::len)
                        .sum::<usize>()
                        <= 2 * 1024 * 1024,
                    "clipboard fixture bound"
                );
            }
            if Instant::now() >= end {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn wait(
        &mut self,
        mut check: impl FnMut(&mut Self) -> Result<bool>,
        label: &str,
        seconds: u64,
    ) -> Result<()> {
        let end =
            Instant::now() + Duration::from_secs(seconds) * crate::support::local::deadline_scale();
        loop {
            self.pump(0.03)?;
            if check(self)? {
                self.checkpoint(label)?;
                return Ok(());
            }
            ensure!(
                Instant::now() < end,
                "{label}: {}",
                (0..self.viewers.len())
                    .map(|i| self.text(i))
                    .collect::<Vec<_>>()
                    .join("\n--- viewer ---\n")
            );
        }
    }
    #[track_caller]
    fn hold(&mut self, mut check: impl FnMut(&Self) -> bool, seconds: f64) -> Result<()> {
        let end = Instant::now() + Duration::from_secs_f64(seconds);
        while Instant::now() < end {
            self.pump(0.03)?;
            ensure!(
                check(self),
                "viewer isolation changed at {}:\n{}",
                std::panic::Location::caller(),
                (0..self.viewers.len())
                    .map(|i| self.text(i))
                    .collect::<Vec<_>>()
                    .join("\n--- viewer ---\n")
            );
        }
        Ok(())
    }
    /// The viewer's screen once no paint has changed it for a short quiet window. A viewer that
    /// just attached or reacted keeps painting for a while; a snapshot taken mid-way is not a
    /// baseline to compare against later.
    fn settled(&mut self, index: usize) -> Result<String> {
        let quiet = Duration::from_millis(300);
        let end = Instant::now() + Duration::from_secs(8) * crate::support::local::deadline_scale();
        let mut last = self.text(index);
        let mut since = Instant::now();
        loop {
            self.pump(0.03)?;
            let now = self.text(index);
            if now != last {
                last = now;
                since = Instant::now();
            } else if since.elapsed() >= quiet {
                return Ok(last);
            }
            ensure!(
                Instant::now() < end,
                "viewer {index} never settled:\n{last}"
            );
        }
    }
    fn cli(&self, args: &[&str]) -> Result<Value> {
        let mut command = self.root.command(&self.binary);
        command.args(args);
        let result = process::output(command, Duration::from_secs(5), 1024 * 1024)?;
        ensure!(
            result.status.success(),
            "CLI {args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        Ok(serde_json::from_slice(&result.stdout)?)
    }
    fn listing(&self, workspace: &str) -> Result<Value> {
        Ok(self.cli(&[workspace, "list"])?["result"]["value"]["workspaces"][0].clone())
    }
    fn tabs(&self, workspace: &str) -> Result<Vec<Value>> {
        Ok(self.listing(workspace)?["tabs"]
            .as_array()
            .context("tabs")?
            .clone())
    }
    fn state(
        &mut self,
        mut check: impl FnMut(&[Value]) -> bool,
        label: &str,
        seconds: u64,
    ) -> Result<Vec<Value>> {
        let end = Instant::now() + Duration::from_secs(seconds);
        loop {
            let state = self.tabs("default")?;
            if check(&state) {
                if std::env::var_os("FUX_BETAMAX_DIR").is_some() {
                    self.pump(0.03)?;
                }
                self.checkpoint(label)?;
                return Ok(state);
            }
            ensure!(Instant::now() < end, "{label}: {state:?}");
            self.pump(0.03)?;
        }
    }
    fn resize(&mut self, index: usize, rows: u16, columns: u16) -> Result<()> {
        self.checkpoint("before-resize")?;
        self.viewers[index]
            .screen
            .screen_mut()
            .set_size(rows, columns);
        self.viewers[index].terminal.resize(rows, columns)
    }
    fn checkpoint(&mut self, label: &str) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while self
            .viewers
            .iter()
            .map(|viewer| viewer.terminal.frame_pending())
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .any(|pending| pending)
        {
            self.pump(0.005)?;
            ensure!(
                Instant::now() < deadline,
                "unfinished synchronized viewer frame at {label}"
            );
        }
        for viewer in &mut self.viewers {
            if let Some(actual) = viewer.terminal.checkpoint(label)? {
                let expected = viewer
                    .screen
                    .screen()
                    .rows(0, viewer.screen.screen().size().1)
                    .collect::<Vec<_>>()
                    .join("\n");
                let normalize = |text: &str| {
                    text.lines()
                        .map(str::trim_end)
                        .collect::<Vec<_>>()
                        .join("\n")
                        .trim_end()
                        .to_owned()
                };
                ensure!(
                    normalize(&actual) == normalize(&expected),
                    "Betamax/vt100 screen mismatch at {label}:\nBetamax: {actual:?}\nvt100: {expected:?}"
                );
            }
        }
        Ok(())
    }
    fn exited(&mut self, index: usize) -> Result<bool> {
        Ok(self.viewers[index].terminal.child.0.try_wait()?.is_some())
    }
    fn success(&mut self, index: usize) -> Result<bool> {
        Ok(self.viewers[index]
            .terminal
            .child
            .0
            .try_wait()?
            .is_some_and(|s| s.success()))
    }
    fn finish(&mut self) -> Result<()> {
        let mut failures = Vec::new();
        for viewer in &mut self.viewers {
            if let Err(error) = viewer.terminal.close() {
                failures.push(format!(
                    "viewer {} cleanup: {error:#}",
                    viewer.terminal.child.0.id()
                ));
            }
        }
        self.viewers.clear();
        if let Err(error) = stop_servers(self.root.path()) {
            failures.push(format!("{error:#}"));
        }
        self.finished = true;
        ensure!(failures.is_empty(), "{}", failures.join("; "));
        Ok(())
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        if !self.finished
            && let Err(error) = self.finish()
        {
            eprintln!("viewer cleanup: {error:#}");
        }
    }
}
fn panes(tab: &Value) -> usize {
    tab["panes"].as_array().map_or(0, Vec::len)
}
fn widths(tab: &Value) -> Vec<Value> {
    tab["panes"]
        .as_array()
        .map(|p| p.iter().map(|p| p["geometry"]["width"].clone()).collect())
        .unwrap_or_default()
}

fn click_popup_entry(h: &mut Harness, text: &str) -> Result<()> {
    click_popup_entry_at(h, 0, text)
}

fn click_popup_entry_at(h: &mut Harness, viewer: usize, text: &str) -> Result<()> {
    h.wait(
        |h| Ok(h.rows(viewer).iter().take(23).any(|row| row.contains(text))),
        "mouse chooser entry",
        8,
    )?;
    let row = h
        .rows(viewer)
        .iter()
        .take(23)
        .position(|row| row.contains(text))
        .context("popup row")?
        + 1;
    h.send(
        viewer,
        format!("\x1b[<0;75;{row}M\x1b[<0;75;{row}m").as_bytes(),
    )?;
    h.pump(0.1)
}

fn mouse_workspace_action(h: &mut Harness, action: &str, target: &str) -> Result<()> {
    h.send(0, b"\x1b[<2;2;24M\x1b[<2;2;24m")?;
    click_popup_entry(h, action)?;
    let heading = if action == "reorder workspace" {
        "Place workspace before"
    } else {
        "Choose workspace"
    };
    h.wait(
        |h| Ok(h.text(0).contains(heading)),
        "workspace destination chooser",
        8,
    )?;
    click_popup_entry(h, target)
}

fn mouse_reorder_tab(h: &mut Harness, label: &str, before: &str) -> Result<()> {
    let column = h.bar(0).find(label).context("source tab label")? + 2;
    h.send(
        0,
        format!("\x1b[<2;{column};24M\x1b[<2;{column};24m").as_bytes(),
    )?;
    click_popup_entry(h, "reorder tab")?;
    h.wait(
        |h| Ok(h.text(0).contains("Place current tab before")),
        "tab reorder chooser",
        8,
    )?;
    click_popup_entry(h, before)
}

fn verify_traversal(h: &mut Harness, tab: usize) -> Result<()> {
    let shows_focus = |h: &Harness, pane: &Value| {
        h.bar(0)
            .rsplit('│')
            .next()
            .is_some_and(|field| field.trim().split(':').next() == Some(pane.to_string().as_str()))
    };
    let instance = h.cli(&["default", "list"])?["result"]["value"]["instance"]
        .as_str()
        .context("instance")?
        .to_owned();
    let state = h.tabs("default")?;
    let panes = state[tab]["panes"].as_array().context("traversal panes")?;
    let ids: Vec<_> = panes.iter().map(|pane| pane["id"].clone()).collect();
    let pids: Vec<_> = panes.iter().map(|pane| pane["pid"].clone()).collect();
    let start = panes
        .iter()
        .position(|pane| pane["focused"] == true)
        .context("focused pane")?;
    let original_label = panes[start]["label"].as_str().unwrap_or("").to_owned();
    h.wait(
        |h| Ok(shows_focus(h, &ids[start])),
        "traversal initial frame",
        8,
    )?;
    for (forward, key, command) in [(true, b'o', "next"), (false, b'u', "previous")] {
        let mut index = start;
        if ids.len() > 1 {
            h.send(0, b"\x01z")?;
            h.wait(
                |h| {
                    Ok(h.rows(0)
                        .iter()
                        .take(23)
                        .all(|row| !row.contains(['│', '─'])))
                },
                "zoomed frame before traversal",
                8,
            )?;
        }
        for _ in 0..ids.len() {
            index = if forward {
                (index + 1) % ids.len()
            } else {
                (index + ids.len() - 1) % ids.len()
            };
            h.send(0, &[1, key])?;
            h.wait(
                |h| Ok(shows_focus(h, &ids[index])),
                "keyboard traversal and wrap",
                8,
            )?;
        }
        ensure!(index == start, "cycle did not return to original pane");
        for _ in 0..ids.len() {
            index = if forward {
                (index + 1) % ids.len()
            } else {
                (index + ids.len() - 1) % ids.len()
            };
            let reply = h.cli(&["default", "focus", command])?;
            ensure!(
                reply["result"]["value"]["pane"] == ids[index],
                "CLI traversal target: {reply}"
            );
            // A later metadata change forces an observable attachment snapshot after the CLI focus.
            let marker = format!("cycle-{command}-{index}");
            h.cli(&[
                "default",
                "rename-pane",
                &ids[start].to_string(),
                &marker,
                "--instance",
                &instance,
            ])?;
            h.wait(
                |h| Ok(h.bar(0).contains(&marker) && shows_focus(h, &ids[start])),
                "CLI preserves private focus in a later snapshot",
                8,
            )?;
        }
    }
    h.cli(&[
        "default",
        "rename-pane",
        &ids[start].to_string(),
        &original_label,
        "--instance",
        &instance,
    ])?;
    let after = h.tabs("default")?;
    ensure!(
        after[tab]["panes"]
            .as_array()
            .context("panes after cycle")?
            .iter()
            .map(|pane| pane["pid"].clone())
            .collect::<Vec<_>>()
            == pids,
        "traversal changed a process"
    );
    Ok(())
}

pub(super) fn run(binary: &Path) -> Result<()> {
    modal::run(binary)?;
    verify_tiny_layout(binary)?;
    let root = Root::new(
        "fview-rs-",
        &[
            "/bin/sh".into(),
            "-c".into(),
            "printf COPY_TARGET; exec cat".into(),
        ],
    )?;
    let config = root.path().join("config/fux/config.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)? + "clipboard = \"write-only\"\n",
    )?;
    let mut h = Harness {
        root,
        binary: binary.into(),
        viewers: Vec::new(),
        finished: false,
    };
    h.add(&[])?;
    h.wait(|h| Ok(h.text(0).contains("COPY_TARGET")), "first pane", 8)?;
    h.wait(
        |h| Ok(h.bar(0).trim_end().ends_with("│ 1")),
        "focused pane bar",
        8,
    )?;
    ensure!(
        h.bar(0).starts_with(" default │ main "),
        "bar missing: {}",
        h.bar(0)
    );
    ensure!(
        !h.text(0).contains("split side by side") && !h.text(0).contains(['┌', '┐', '└', '┘']),
        "unexpected popup/frame"
    );
    ensure!(
        h.rows(0).iter().take(23).all(|row| !row.contains('│')),
        "single pane separators"
    );
    let listing = h.listing("default")?;
    let tabs = h.tabs("default")?;
    ensure!(
        listing["name"] == "default"
            && tabs.len() == 1
            && tabs[0]["name"] == "main"
            && panes(&tabs[0]) == 1,
        "fresh topology"
    );
    let original_pid = tabs[0]["panes"][0]["pid"].clone();
    let instance = h.cli(&["default", "list"])?["result"]["value"]["instance"]
        .as_str()
        .context("server instance")?
        .to_owned();
    h.send(0, "\x01;build界\r".as_bytes())?;
    h.state(
        |s| s[0]["panes"][0]["label"] == "build界",
        "manual pane label",
        8,
    )?;
    h.wait(
        |h| Ok(h.bar(0).contains("1: build界")),
        "manual label in bar",
        8,
    )?;
    ensure!(
        h.tabs("default")?[0]["panes"][0]["pid"] == original_pid,
        "rename replaced process"
    );
    ensure!(
        h.text(0).contains("COPY_TARGET"),
        "rename lost terminal content"
    );
    h.send(0, b"\x01;\x15discarded\x1b")?;
    h.pump(0.2)?;
    ensure!(
        h.tabs("default")?[0]["panes"][0]["label"] == "build界",
        "cancel changed pane label"
    );
    h.send(0, b"\x1b")?;
    h.pump(0.15)?;
    h.cli(&["default", "rename-pane", "1", "", "--instance", &instance])?;
    h.wait(
        |h| Ok(h.bar(0).trim_end().ends_with("│ 1")),
        "clear manual pane label",
        8,
    )?;
    ensure!(
        h.tabs("default")?[0]["panes"][0]["label"].is_null(),
        "CLI did not clear manual label"
    );
    h.send(0, b"\x01?")?;
    h.wait(
        |h| Ok(h.text(0).contains("Pane 1 actions")),
        "keyboard pane menu",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.pump(0.15)?;
    ensure!(
        !h.text(0).contains("Pane 1 actions"),
        "menu escape did not dismiss"
    );
    h.send(0, b"\x01")?;
    h.wait(
        |h| Ok(h.text(0).contains("split side by side")),
        "prefix popup",
        8,
    )?;
    let rows: Vec<_> = h
        .rows(0)
        .into_iter()
        .filter(|row| row.contains("split side by side"))
        .collect();
    ensure!(
        !rows.is_empty()
            && rows.iter().all(|row| row.find('|').is_some_and(|i| i > 40)
                && row.chars().take(40).all(char::is_whitespace)),
        "popup column: {rows:?}"
    );
    ensure!(h.text(0).contains("Panes"), "first popup heading");
    h.send(0, b"\x1b[6~")?;
    h.wait(
        |h| Ok(h.text(0).contains("Session")),
        "scrolled popup headings",
        8,
    )?;
    h.send(0, b"\x1b[5~")?;
    h.wait(
        |h| Ok(h.text(0).contains("Panes")),
        "popup returns to first heading",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.pump(0.15)?;
    ensure!(
        !h.text(0).contains("split side by side"),
        "escape did not dismiss popup"
    );
    h.send(0, b"\x01t")?;
    h.state(|s| s.len() == 2, "new tab", 8)?;
    h.pump(0.2)?;
    ensure!(
        !h.text(0).contains("split side by side"),
        "fast command flashed popup"
    );
    h.wait(
        |h| Ok(h.bar(0).contains("tab-2") && h.bar(0).contains("main")),
        "tabs in bar",
        8,
    )?;
    h.send(0, b"\x01!")?;
    h.wait(
        |h| Ok(h.bar(0).trim_end().ends_with("│ 1")),
        "last pane returns across tabs",
        8,
    )?;
    h.send(0, b"\x01!")?;
    h.wait(
        |h| Ok(h.bar(0).trim_end().ends_with("│ 2")),
        "last pane toggles back",
        8,
    )?;
    let last = h.cli(&["default", "focus", "last"])?;
    ensure!(
        last["result"]["value"]["pane"] == 1,
        "CLI last pane target: {last}"
    );
    ensure!(
        h.bar(0).trim_end().ends_with("│ 2"),
        "default focus changed viewer focus"
    );
    let last = h.cli(&["default", "focus", "last"])?;
    ensure!(
        last["result"]["value"]["pane"] == 2,
        "CLI last pane toggle: {last}"
    );
    ensure!(
        h.tabs("default")?[0]["panes"][0]["pid"] == original_pid,
        "last focus replaced process"
    );
    h.send(0, b"\x01*")?;
    h.wait(
        |h| Ok(h.tabs("default")?[1]["panes"][0]["right_click"] == "fux"),
        "right-click keyboard policy",
        8,
    )?;
    h.send(0, b"\x01?")?;
    h.wait(
        |h| Ok(h.text(0).contains("right-click: fux")),
        "pane menu policy",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.pump(0.15)?;
    h.cli(&[
        "default",
        "pane-input",
        "2",
        "--right-click",
        "pane",
        "--instance",
        &instance,
    ])?;
    h.wait(
        |h| Ok(h.tabs("default")?[1]["panes"][0]["right_click"] == "pane"),
        "CLI pane policy",
        8,
    )?;
    // Observe the attachment update before issuing a cycle based on the viewer's policy.
    h.pump(0.2)?;
    h.send(0, b"\x01?")?;
    h.wait(
        |h| Ok(h.text(0).contains("right-click: pane")),
        "viewer observes CLI policy",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.pump(0.15)?;
    h.send(0, b"\x01*")?;
    h.wait(
        |h| {
            Ok(h.tabs("default")?[1]["panes"][0]
                .get("right_click")
                .is_none())
        },
        "policy resets to auto",
        8,
    )?;
    h.send(0, b"\x010")?;
    h.wait(
        |h| Ok(h.text(0).contains("split side by side")),
        "unknown key popup",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.pump(0.15)?;
    h.send(0, b"\x01\x01Q")?;
    h.wait(|h| Ok(h.text(0).contains('Q')), "literal prefix echo", 8)?;
    h.send(0, b"\x01t\x01,\x15second\r")?;
    h.state(
        |s| s.len() == 3 && s[2]["name"] == "second",
        "new-tab rename ordering",
        8,
    )?;
    ensure!(h.tabs("default")?[0]["name"] == "main", "stale rename");
    h.send(0, b"\x01w")?;
    h.wait(|h| Ok(h.text(0).contains("Choose tab")), "tab chooser", 8)?;
    h.send(0, b"k\r")?;
    h.state(|s| s[1]["focused"] == true, "tab selection", 8)?;
    h.send(0, b"\x01,\x15discarded")?;
    h.wait(|h| Ok(h.text(0).contains("Rename tab")), "rename prompt", 8)?;
    h.send(0, b"\x1b")?;
    h.pump(0.15)?;
    h.wait(
        |h| Ok(!h.text(0).contains("Rename tab") && !h.text(0).contains("split side by side")),
        "rename Escape returns normal",
        8,
    )?;
    h.send(0, b"AFTER_RENAME_CANCEL")?;
    h.wait(
        |h| Ok(h.text(0).contains("AFTER_RENAME_CANCEL")),
        "input after rename cancellation",
        8,
    )?;
    ensure!(
        h.tabs("default")?[1]["name"] == "tab-2",
        "cancel changed rename"
    );
    h.send(0, "\x01,\x15renamed界\r".as_bytes())?;
    h.state(|s| s[1]["name"] == "renamed界", "Unicode rename", 8)?;
    h.wait(
        |h| Ok(h.bar(0).contains("renamed")),
        "tab label rendered",
        8,
    )?;
    mouse_reorder_tab(&mut h, "renamed", "main")?;
    h.state(
        |s| s[0]["name"] == "renamed界" && s[1]["name"] == "main" && s[0]["focused"] == true,
        "mouse-only tab reorder",
        8,
    )?;
    mouse_reorder_tab(&mut h, "renamed", "second")?;
    h.state(
        |s| s[0]["name"] == "main" && s[1]["name"] == "renamed界" && s[1]["focused"] == true,
        "mouse-only tab order restore",
        8,
    )?;

    h.wait(|h| Ok(h.bar(0).contains(" main ")), "inactive tab label", 8)?;
    let main_column = h.bar(0).find(" main ").context("main label")? + 2;
    h.send(
        0,
        format!("\x1b[<2;{main_column};24M\x1b[<2;{main_column};24m").as_bytes(),
    )?;
    h.wait(
        |h| Ok(h.text(0).contains("Tab main (1) actions")),
        "inactive tab menu",
        8,
    )?;
    h.send(0, b"j\r\x15menu-main\r")?;
    h.state(
        |s| s[0]["name"] == "menu-main" && s[1]["focused"] == true,
        "contextual tab rename retains selection",
        8,
    )?;
    h.cli(&["default", "tab", "rename", "1", "main"])?;
    let retained_pid = h.tabs("default")?[1]["panes"][0]["pid"].clone();
    h.cli(&[
        "default",
        "split",
        "horizontal",
        "--ratio",
        "7000",
        "--no-focus",
    ])?;
    let state = h.state(|s| panes(&s[1]) == 2, "split with initial ratio", 8)?;
    ensure!(
        state[1]["panes"][0]["focused"] == true && state[1]["panes"][0]["pid"] == retained_pid,
        "no-focus split changed focus or existing process"
    );
    ensure!(
        state[1]["panes"][0]["geometry"]["width"] == 55
            && state[1]["panes"][1]["geometry"]["width"] == 24,
        "initial 70/30 ratio not applied: {}",
        state[1]
    );
    h.wait(
        |h| Ok(h.bar(0).trim_end().ends_with("│ 2")),
        "viewer focus retained after CLI split",
        8,
    )?;
    // Restore equal geometry for the existing mouse-coordinate scenarios below.
    let exported = h.cli(&["default", "layout", "2", "export"])?;
    let value = &exported["result"]["value"];
    let mut document = value["document"].clone();
    for node in document["nodes"].as_array_mut().context("layout nodes")? {
        if node.get("ratio").is_some() {
            node["ratio"] = 5000.into();
        }
    }
    h.cli(&[
        "default",
        "ctl",
        &serde_json::json!({"command":"layout", "id":1,
        "instance":instance, "tab":2, "generation":value["generation"],
        "action":{"operation":"apply","document":document}})
        .to_string(),
    ])?;
    h.send(0, b"\x01o")?;
    let state = h.state(
        |s| s[1]["panes"][1]["focused"] == true,
        "focus new pane after no-focus split",
        8,
    )?;
    let clicked = state[1]["panes"][0].clone();
    ensure!(
        clicked["focused"] == false,
        "expected a nonfocused menu target"
    );
    h.send(0, b"\x1b[<2;2;2M\x1b[<2;2;2m")?;
    h.wait(
        |h| {
            Ok(h.text(0)
                .contains(&format!("Pane {} actions", clicked["id"])))
        },
        "nonfocused pane menu",
        8,
    )?;
    h.send(0, b"\r\x15menu-pane\r")?;
    h.state(
        |s| s[1]["panes"][0]["label"] == "menu-pane" && s[1]["panes"][0]["focused"] == false,
        "contextual pane rename retains target",
        8,
    )?;
    ensure!(
        h.tabs("default")?[1]["panes"][0]["pid"] == clicked["pid"],
        "context action replaced pane"
    );
    let swap_source = state[1]["panes"][1].clone();
    h.send(0, b"\x01.\r")?;
    h.state(
        |s| s[1]["panes"][0]["id"] == swap_source["id"] && s[1]["panes"][1]["id"] == clicked["id"],
        "explicit keyboard swap",
        8,
    )?;
    h.send(0, b"\x01.")?;
    h.wait(
        |h| Ok(h.text(0).contains("Swap pane") && h.text(0).contains("menu-pane")),
        "swap destination chooser",
        8,
    )?;
    let rows = h.rows(0);
    let (row, line) = rows
        .iter()
        .enumerate()
        .find(|(_, line)| line.contains("menu-pane"))
        .context("swap destination row")?;
    let column = line
        .find(&format!("{}:", clicked["id"]))
        .context("swap destination column")?
        + 1;
    let row = row + 1;
    h.send(
        0,
        format!("\x1b[<0;{column};{row}M\x1b[<0;{column};{row}m").as_bytes(),
    )?;
    let restored = h.state(
        |s| s[1]["panes"][0]["id"] == clicked["id"] && s[1]["panes"][1]["id"] == swap_source["id"],
        "mouse swap restores layout",
        8,
    )?;
    ensure!(
        restored[1]["panes"][0]["pid"] == clicked["pid"]
            && restored[1]["panes"][1]["pid"] == swap_source["pid"],
        "swap replaced a process"
    );
    // The server listing above does not prove the viewer has consumed the swap
    // frame. A private focus round trip supplies an observable attachment ack
    // before starting a generation-pinned drag, then restores the original focus.
    let focused = |h: &Harness| {
        h.bar(0)
            .rsplit('│')
            .next()?
            .trim()
            .split(':')
            .next()?
            .parse::<u32>()
            .ok()
    };
    let original_focus = focused(&h).context("focus before drag synchronization")?;
    h.send(0, b"\x01o")?;
    h.wait(
        |h| Ok(focused(h).is_some_and(|pane| pane != original_focus)),
        "focus after swap frame",
        8,
    )?;
    h.send(0, b"\x01u")?;
    h.wait(
        |h| Ok(focused(h) == Some(original_focus)),
        "restore focus before drag",
        8,
    )?;
    let before = widths(&state[1]);
    let tab_id = state[1]["id"].to_string();
    let layout_before = h.cli(&["default", "layout", &tab_id, "export"])?;
    h.send(0, b"\x1b[<8;2;2M\x1b[<40;78;3M")?;
    // The hint and the destination highlight are one drag state; wait for both together.
    h.wait(
        |h| {
            Ok(h.text(0).contains("release to apply")
                && h.viewers[0]
                    .screen
                    .screen()
                    .cell(2, 77)
                    .is_some_and(|cell| cell.bgcolor() == vt100::Color::Idx(6)))
        },
        "pane drag target and destination highlight",
        8,
    )?;
    // Neither a right-button release nor a wheel report may commit a left-button drag.
    h.send(0, b"\x1b[<2;78;3m\x1b[<64;78;3M")?;
    h.hold(|h| h.text(0).contains("release to apply"), 0.2)?;
    ensure!(
        h.cli(&["default", "layout", &tab_id, "export"])? == layout_before,
        "other mouse buttons committed layout drag"
    );
    h.send(0, b"\x1b")?;
    h.pump(0.15)?;
    h.send(0, b"\x1b[<2;78;3m\x1b[<40;78;3M\x1b[<0;78;3m")?;
    h.pump(0.15)?;
    ensure!(
        h.cli(&["default", "layout", &tab_id, "export"])? == layout_before,
        "cancelled mouse tail changed layout"
    );
    ensure!(
        h.viewers[0]
            .screen
            .screen()
            .cell(2, 77)
            .is_some_and(|cell| cell.bgcolor() != vt100::Color::Idx(6)),
        "cancelled drag left its highlight visible"
    );
    h.send(0, b"\x01r")?;
    h.wait(|h| Ok(h.text(0).contains("Resize")), "resize hint", 8)?;
    h.send(0, b"hh\r")?;
    h.state(|s| widths(&s[1]) != before, "repeated resize", 8)?;
    h.send(
        0,
        format!("\x01r{}\r\x01,\x15burst-done\r", "hH".repeat(128)).as_bytes(),
    )?;
    h.state(|s| s[1]["name"] == "burst-done", "resize burst", 30)?;
    h.send(0, b"\x01h")?;
    h.state(|s| s[1]["panes"][0]["focused"] == true, "focus", 8)?;
    let focused = h.tabs("default")?[1]["panes"]
        .as_array()
        .context("panes")?
        .iter()
        .find(|p| p["focused"] == true)
        .context("focused pane")?["id"]
        .clone();
    h.send(0, b"\x01x")?;
    h.wait(
        |h| Ok(h.text(0).contains(&format!("Close pane {focused}?"))),
        "close prompt",
        8,
    )?;
    ensure!(
        panes(&h.tabs("default")?[1]) == 2,
        "close before confirmation"
    );
    h.send(0, b"n")?;
    h.pump(0.1)?;
    h.send(0, b"\x1b")?;
    h.pump(0.1)?;
    ensure!(
        panes(&h.tabs("default")?[1]) == 2,
        "cancelled close killed pane"
    );
    h.send(0, b"\x01xy")?;
    h.state(|s| panes(&s[1]) == 1, "confirmed close", 8)?;
    h.send(0, b"\x01c")?;
    h.wait(
        |h| Ok(h.text(0).contains("Close tab") && h.text(0).contains("All its panes")),
        "tab close prompt",
        8,
    )?;
    h.send(0, b"y")?;
    h.state(|s| s.len() == 2, "tab close", 8)?;
    h.send(0, b"\x01w")?;
    h.wait(|h| Ok(h.text(0).contains("Choose tab")), "tab chooser2", 8)?;
    h.send(0, b"k\r")?;
    h.state(|s| s[0]["focused"] == true, "first tab", 8)?;
    h.send(0, b"\x01[")?;
    h.wait(|h| Ok(h.text(0).contains("Copy ·")), "copy hint", 8)?;
    h.send(
        0,
        format!("{}{} {}", "h".repeat(20), "k".repeat(5), "l".repeat(10)).as_bytes(),
    )?;
    h.wait(
        |h| Ok(h.text(0).contains("Copy selection")),
        "copy selection",
        8,
    )?;
    h.send(0, b"y")?;
    h.wait(|h| Ok(h.copies(0) >= 1), "clipboard copy", 8)?;
    h.send(0, b"\x1b[<4;1;1M\x1b[<36;11;1M\x1b[<4;11;1m")?;
    h.wait(
        |h| Ok(h.text(0).contains("Copy selection")),
        "shift drag",
        8,
    )?;
    h.send(0, b"y")?;
    h.wait(|h| Ok(h.copies(0) >= 2), "shift drag copy", 8)?;
    h.send(0, b"\x01\\")?;
    h.state(|s| panes(&s[0]) == 2, "side split", 8)?;
    h.wait(
        |h| Ok(h.rows(0)[2].matches('│').count() == 1),
        "one separator",
        8,
    )?;
    ensure!(
        !h.text(0).contains(['┌', '┐', '└', '┘']),
        "pane frames after split"
    );
    h.send(0, b"\x01_")?;
    h.state(|s| panes(&s[0]) == 3, "stacked split", 8)?;
    h.wait(|h| Ok(h.text(0).contains("├─")), "separator junction", 8)?;
    verify_traversal(&mut h, 0)?;
    // Preserve main's combined-read regression: entering copy mode, selecting,
    // moving and copying must work without an intervening rendered frame.
    h.send(0, b"\x01[ hhhy")?;
    h.wait(|h| Ok(h.bar(0).contains("Copied")), "burst copy notice", 8)?;
    h.wait(
        |h| Ok(!h.bar(0).contains("Copied")),
        "burst notice expiry",
        5,
    )?;
    let copies_before = h.copies(0);
    h.wait(
        |h| Ok(h.text(0).matches("COPY_TARGET").count() == 3),
        "pane output before copy",
        8,
    )?;
    h.send(0, b"\x01[")?;
    h.wait(|h| Ok(h.text(0).contains("Copy ·")), "copy mode notice", 8)?;
    h.send(
        0,
        format!("{} {}y", "h".repeat(20), "l".repeat(10)).as_bytes(),
    )?;
    h.wait(|h| Ok(h.copies(0) > copies_before), "notice copy", 8)?;
    h.wait(|h| Ok(h.bar(0).contains("Copied")), "copied notice", 8)?;
    h.wait(|h| Ok(!h.bar(0).contains("Copied")), "notice expiry", 5)?;
    h.cli(&["default", "tab", "new"])?;
    h.state(|s| s.len() == 3, "third tab", 8)?;
    h.send(0, format!("\x01,\x15{}\r", "x".repeat(70)).as_bytes())?;
    h.wait(|h| Ok(h.bar(0).contains('…')), "long label truncation", 8)?;
    ensure!(
        h.bar(0).starts_with(" default │ xxxx"),
        "current tab priority: {}",
        h.bar(0)
    );
    h.send(0, b"\x01,\x15main\r")?;
    h.state(|s| s[0]["name"] == "main", "rename back", 8)?;
    let extra = h
        .tabs("default")?
        .iter()
        .find(|t| t["name"] != "main" && t["name"] != "second")
        .context("extra tab")?["id"]
        .to_string();
    h.cli(&["default", "tab", "close", &extra])?;
    h.state(|s| s.len() == 2, "third tab close", 8)?;
    h.cli(&["default", "tab", "select", "0"])?;
    h.send(0, b"\x01xy")?;
    h.state(|s| panes(&s[0]) == 2, "split close", 8)?;
    verify_traversal(&mut h, 0)?;
    h.send(0, b"\x01Xy")?;
    h.state(|s| panes(&s[0]) == 1, "second split close", 8)?;
    verify_traversal(&mut h, 0)?;
    h.add(&[])?;
    h.wait(
        |h| Ok(h.text(1).contains("COPY_TARGET")),
        "second viewer",
        8,
    )?;
    h.send(0, b"\x01")?;
    h.wait(
        |h| Ok(h.text(0).contains("split side by side")),
        "first popup",
        8,
    )?;
    h.hold(|h| !h.text(1).contains("split side by side"), 0.3)?;
    h.send(1, b"SECOND_INPUT\r")?;
    h.wait(
        |h| Ok(h.text(1).contains("SECOND_INPUT")),
        "second input",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.pump(0.15)?;
    h.send(
        0,
        format!("\x01[{} {}", "h".repeat(20), "l".repeat(3)).as_bytes(),
    )?;
    h.wait(
        |h| Ok(h.text(0).contains("Copy selection")),
        "first selection",
        8,
    )?;
    ensure!(!h.text(1).contains("Copy"), "copy mode crossed viewers");
    h.send(1, b"\x01")?;
    h.wait(
        |h| Ok(h.text(1).contains("split side by side")),
        "second popup",
        8,
    )?;
    h.send(1, b"\x1b")?;
    h.pump(0.1)?;
    h.send(1, b"MORE_OUTPUT\r")?;
    h.wait(
        |h| Ok(h.text(1).contains("MORE_OUTPUT")),
        "second output during copy",
        8,
    )?;
    for _ in 0..3 {
        h.send(0, b"\x1b")?;
        h.pump(0.1)?;
    }
    ensure!(
        h.viewers[1].screen.callbacks().0.is_empty(),
        "clipboard crossed viewers"
    );
    h.resize(0, 4, 18)?;
    h.send(0, b"\x01")?;
    h.wait(
        |h| Ok(h.text(0).contains("|  split") && h.text(0).contains("more")),
        "tiny popup",
        8,
    )?;
    let mut seen = BTreeSet::new();
    for _ in 0..40 {
        for row in h.rows(0).iter().take(3) {
            if !row.trim().is_empty() && !row.contains("more") {
                seen.insert(row.trim().to_owned());
            }
        }
        h.send(0, b"\x1b[B")?;
        h.pump(0.03)?;
    }
    ensure!(
        seen.len() >= 23,
        "tiny scrolling exposed {} rows: {seen:?}",
        seen.len()
    );
    h.resize(0, 1, 1)?;
    h.pump(0.3)?;
    ensure!(!h.exited(0)?, "one-cell crash");
    h.resize(0, 24, 80)?;
    h.wait(
        |h| Ok(h.text(0).contains("Session")),
        "resized scrolled command context",
        8,
    )?;
    h.send(0, b"\x1b[5~")?;
    h.wait(
        |h| Ok(h.text(0).contains("split side by side")),
        "resize command context",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.pump(0.15)?;
    h.resize(0, 2, 20)?;
    h.pump(0.3)?;
    ensure!(!h.exited(0)?, "two-row crash");
    h.wait(|h| Ok(h.bar(0).starts_with(" default")), "two-row bar", 8)?;
    ensure!(
        h.rows(0).len() == 2 && !h.rows(0)[0].contains('│'),
        "two-row pane ownership"
    );
    h.resize(0, 24, 80)?;
    h.pump(0.3)?;
    h.send(0, b"\x01a")?;
    h.wait(
        |h| Ok(h.text(0).contains("New workspace")),
        "new workspace prompt",
        8,
    )?;
    h.send(0, b"other\r")?;
    h.wait(
        |h| Ok(h.text(0).contains("other")),
        "new workspace attachment",
        8,
    )?;
    h.wait(
        |h| {
            Ok(h.cli(&["workspace", "list"])?["names"]
                .as_array()
                .context("workspace names")?
                .contains(&Value::from("other")))
        },
        "other listed",
        5,
    )?;
    h.wait(
        |h| Ok(h.bar(0).starts_with(" other")),
        "workspace bar ready",
        8,
    )?;
    mouse_workspace_action(&mut h, "reorder workspace", "default")?;
    h.wait(
        |h| Ok(h.cli(&["workspace", "list"])?["names"] == serde_json::json!(["other", "default"])),
        "mouse-only workspace reorder",
        8,
    )?;
    mouse_workspace_action(&mut h, "choose workspace", "default")?;
    h.wait(
        |h| Ok(h.bar(0).starts_with(" default")),
        "mouse workspace select",
        8,
    )?;
    mouse_workspace_action(&mut h, "reorder workspace", "other")?;
    h.wait(
        |h| Ok(h.cli(&["workspace", "list"])?["names"] == serde_json::json!(["default", "other"])),
        "mouse-only workspace restore",
        8,
    )?;
    mouse_workspace_action(&mut h, "choose workspace", "other")?;
    h.wait(
        |h| Ok(h.bar(0).starts_with(" other")),
        "mouse workspace return",
        8,
    )?;

    h.send(0, b"\x01s")?;
    h.wait(
        |h| Ok(h.text(0).contains("Choose workspace")),
        "workspace chooser",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.pump(0.1)?;
    h.wait(
        |h| Ok(!h.text(0).contains("Choose workspace") && !h.text(0).contains("split side by side")),
        "chooser Escape returns normal",
        8,
    )?;
    h.send(0, b"\x01sk\r\x01,\x15switched\r")?;
    h.state(|s| s[0]["name"] == "switched", "switch suffix", 8)?;
    ensure!(
        h.tabs("other")?[0]["name"] == "main",
        "suffix wrong destination"
    );
    h.send(0, b"\x01s")?;
    h.wait(
        |h| Ok(h.text(0).contains("Choose workspace")),
        "workspace chooser2",
        8,
    )?;
    h.send(0, b"j\r")?;
    h.wait(|h| Ok(h.text(0).contains("other")), "switch back", 8)?;
    h.send(0, b"BEFORE_DETACH\r\x01d\x01t")?;
    h.wait(|h| h.exited(0), "first detach", 8)?;
    ensure!(
        h.success(0)? && h.tabs("other")?.len() == 1,
        "detach suffix executed or failed"
    );
    h.add(&["other"])?;
    h.wait(
        |h| Ok(h.text(2).contains("BEFORE_DETACH")),
        "detach preceding input",
        8,
    )?;
    h.cli(&[
        "other",
        "split",
        "horizontal",
        "--",
        "/bin/sh",
        "-c",
        "exec cat -v",
    ])?;
    h.state(|_| true, "listing", 8)?;
    h.wait(|h| Ok(panes(&h.tabs("other")?[0]) == 2), "cat-v pane", 8)?;
    h.send(2, b"\x01l")?;
    h.pump(0.2)?;
    h.send(2, b"\x1b")?;
    h.wait(|h| Ok(h.text(2).contains("^[")), "lone Escape", 8)?;
    h.send(2, b"\x1bq")?;
    h.wait(|h| Ok(h.text(2).contains("^[q")), "Escape prefixed key", 8)?;
    h.send(2, b"\x01d")?;
    h.wait(|h| h.exited(2), "third detach", 8)?;
    h.send(1, b"\x01d")?;
    h.wait(|h| h.exited(1), "second detach", 8)?;
    ensure!(h.success(1)?, "second detach failed");
    h.add(&["fresh-source"])?;
    h.wait(
        |h| Ok(h.bar(3).contains("fresh-source") && h.text(3).contains("COPY_TARGET")),
        "fresh workspace viewer",
        8,
    )?;
    let original = h.tabs("fresh-source")?[0]["panes"][0].clone();
    h.send(3, b"\x01y")?;
    h.wait(
        |h| Ok(h.text(3).contains("Move pane to new workspace")),
        "workspace transfer prompt",
        8,
    )?;
    h.send(3, b"followed\r")?;
    h.wait(
        |h| Ok(h.bar(3).starts_with(" followed") && !h.exited(3)?),
        "viewer follows moved pane",
        8,
    )?;
    let moved = h.tabs("followed")?[0]["panes"][0].clone();
    ensure!(
        moved["id"] == original["id"] && moved["pid"] == original["pid"],
        "viewer transfer replaced its pane"
    );
    h.wait(
        |h| {
            Ok(!h.cli(&["workspace", "list"])?["names"]
                .as_array()
                .context("workspace names")?
                .contains(&Value::from("fresh-source")))
        },
        "emptied source retires",
        8,
    )?;
    h.send(3, b"\x01f")?;
    h.wait(
        |h| Ok(h.text(3).contains("Place workspace before")),
        "workspace order prompt",
        8,
    )?;
    h.send(3, b"$")?;
    h.wait(
        |h| {
            Ok(h.cli(&["workspace", "list"])?["names"]
                .as_array()
                .context("workspace names")?
                .last()
                == Some(&Value::from("followed")))
        },
        "viewer workspace ordering",
        8,
    )?;
    h.send(3, b"\x01i")?;
    h.wait(
        |h| Ok(h.text(3).contains("to workspace")),
        "existing workspace destination chooser",
        8,
    )?;
    h.send(3, b"j\r")?;
    h.wait(
        |h| Ok(h.bar(3).starts_with(" other") && !h.exited(3)?),
        "viewer follows into existing workspace",
        8,
    )?;
    let destination_tabs = h.tabs("other")?;
    let continued = destination_tabs
        .iter()
        .flat_map(|tab| tab["panes"].as_array().into_iter().flatten())
        .find(|pane| pane["id"] == original["id"])
        .context("moved pane at existing destination")?;
    ensure!(
        continued["pid"] == original["pid"],
        "existing workspace move replaced process"
    );
    h.send(3, b"FOLLOW_STILL_ALIVE\r")?;
    let input_after_transfer = h.wait(
        |h| Ok(h.text(3).contains("FOLLOW_STILL_ALIVE")),
        "input after viewer transfer",
        8,
    );
    if let Err(error) = input_after_transfer {
        let capture = h.cli(&["other", "capture", &original["id"].to_string()]);
        return Err(error.context(format!(
            "moved pane capture: {capture:?}; current tabs: {:?}",
            h.tabs("other")
        )));
    }
    let main_tab = destination_tabs
        .iter()
        .find(|tab| tab["name"] == "main")
        .context("tab drag destination")?["id"]
        .clone();
    let main_column = h.bar(3).find(" main ").context("visible destination tab")? + 2;
    h.send(
        3,
        format!("\x1b[<8;2;2M\x1b[<40;{main_column};24M").as_bytes(),
    )?;
    // The hint and the highlighted destination tab are one drag state; wait for both rather
    // than sampling the highlight once after the hint's paint.
    h.wait(
        |h| {
            Ok(h.text(3).contains("to tab main")
                && h.viewers[3]
                    .screen
                    .screen()
                    .cell(23, (main_column - 1) as u16)
                    .is_some_and(|cell| cell.bgcolor() == vt100::Color::Idx(6)))
        },
        "tab drag hint and drop highlight",
        8,
    )?;
    h.send(3, format!("\x1b[<0;{main_column};24m").as_bytes())?;
    h.wait(
        |h| {
            let tabs = h.tabs("other")?;
            Ok(tabs.len() == destination_tabs.len() - 1
                && tabs.iter().any(|tab| {
                    tab["id"] == main_tab
                        && tab["panes"].as_array().is_some_and(|panes| {
                            panes.iter().any(|pane| {
                                pane["id"] == original["id"] && pane["pid"] == original["pid"]
                            })
                        })
                }))
        },
        "pane tab drop preserves process and removes empty source",
        8,
    )?;
    h.send(3, b"\x01d")?;
    h.wait(|h| h.exited(3), "moved viewer detach", 8)?;
    ensure!(h.success(3)?, "moved viewer detach failed");
    h.add(&["overflow"])?;
    h.wait(
        |h| Ok(h.bar(4).starts_with(" overflow")),
        "overflow viewer",
        8,
    )?;
    let overflow_pane = h.tabs("overflow")?[0]["panes"][0].clone();
    h.cli(&["overflow", "tab", "new", &"long-destination-".repeat(6)])?;
    h.cli(&["overflow", "tab", "new", "hidden-drop"])?;
    h.send(4, b"\x01,\x15origin\r")?;
    h.wait(
        |h| {
            Ok(h.bar(4).contains(" origin ")
                && h.bar(4).contains("long-destination")
                && !h.bar(4).contains("hidden-drop"))
        },
        "hidden tab destination",
        8,
    )?;
    let origin_column = h.bar(4).find(" origin ").context("visible source tab")? + 2;
    h.send(
        4,
        format!("\x1b[<8;2;2M\x1b[<65;{origin_column};24M\x1b[<65;{origin_column};24M").as_bytes(),
    )?;
    h.wait(
        |h| Ok(h.text(4).contains("to tab hidden-drop")),
        "wheel chooses hidden tab",
        8,
    )?;
    h.send(4, format!("\x1b[<0;{origin_column};24m").as_bytes())?;
    h.wait(
        |h| {
            Ok(h.tabs("overflow")?.iter().any(|tab| {
                tab["name"] == "hidden-drop"
                    && tab["panes"].as_array().is_some_and(|panes| {
                        panes.iter().any(|pane| {
                            pane["id"] == overflow_pane["id"] && pane["pid"] == overflow_pane["pid"]
                        })
                    })
            }))
        },
        "drop into hidden tab preserves process",
        8,
    )?;
    h.send(4, b"\x01d")?;
    h.wait(|h| h.exited(4), "overflow viewer detach", 8)?;
    ensure!(h.success(4)?, "overflow viewer detach failed");
    h.add(&["mouse-source"])?;
    h.wait(
        |h| Ok(h.bar(5).starts_with(" mouse-source")),
        "mouse workspace source",
        8,
    )?;
    let mouse_pane = h.tabs("mouse-source")?[0]["panes"][0].clone();
    h.send(5, b"\x1b[<8;2;2M\x1b[<40;2;24M")?;
    h.wait(
        |h| Ok(h.text(5).contains("release to choose")),
        "workspace drop hint",
        8,
    )?;
    h.send(5, b"\x1b[<0;2;24m")?;
    h.wait(
        |h| Ok(h.rows(5).iter().any(|row| row.trim() == "other")),
        "mouse workspace chooser",
        8,
    )?;
    let other_row = h
        .rows(5)
        .iter()
        .position(|row| row.trim() == "other")
        .context("other workspace row")?
        + 1;
    h.send(
        5,
        format!("\x1b[<0;79;{other_row}M\x1b[<0;79;{other_row}m").as_bytes(),
    )?;
    h.wait(
        |h| {
            Ok(h.bar(5).starts_with(" other")
                && h.tabs("other")?.iter().any(|tab| {
                    tab["panes"].as_array().is_some_and(|panes| {
                        panes.iter().any(|pane| {
                            pane["id"] == mouse_pane["id"] && pane["pid"] == mouse_pane["pid"]
                        })
                    })
                }))
        },
        "mouse workspace transfer follows original process",
        8,
    )?;
    h.send(5, b"MOUSE_WORKSPACE_ALIVE\r")?;
    h.wait(
        |h| Ok(h.text(5).contains("MOUSE_WORKSPACE_ALIVE")),
        "input after mouse transfer",
        8,
    )?;
    h.send(5, b"\x01d")?;
    h.wait(|h| h.exited(5), "mouse workspace viewer detach", 8)?;
    ensure!(h.success(5)?, "mouse workspace viewer detach failed");
    let other_pid = h.tabs("other")?[0]["panes"][0]["pid"].clone();
    h.add(&["close-menu"])?;
    h.add(&["close-menu"])?;
    h.wait(
        |h| Ok(h.bar(6).starts_with(" close-menu") && h.bar(7).starts_with(" close-menu")),
        "workspace close viewers",
        8,
    )?;
    let close_pid = h.tabs("close-menu")?[0]["panes"][0]["pid"].clone();
    let close_stream = h.listing("close-menu")?["event_cursor"]["stream"].clone();
    h.send(6, b"\x01=")?;
    h.wait(
        |h| Ok(h.text(6).contains("Rename workspace")),
        "workspace label editor",
        8,
    )?;
    h.send(6, b"Build team\r")?;
    h.wait(
        |h| Ok(h.bar(6).starts_with(" Build team") && h.bar(7).starts_with(" Build team")),
        "shared workspace display label",
        8,
    )?;
    let renamed = h.listing("close-menu")?;
    ensure!(
        renamed["name"] == "close-menu" && renamed["label"] == "Build team",
        "rename changed routing name"
    );
    ensure!(
        renamed["event_cursor"]["stream"] == close_stream,
        "rename changed workspace lifetime"
    );
    ensure!(
        h.tabs("close-menu")?[0]["panes"][0]["pid"] == close_pid,
        "rename replaced live pane"
    );
    h.cli(&[
        "workspace",
        "rename",
        "close-menu",
        "",
        "--instance",
        &instance,
        "--stream",
        &close_stream.to_string(),
    ])?;
    h.wait(
        |h| Ok(h.bar(6).starts_with(" close-menu") && h.bar(7).starts_with(" close-menu")),
        "cleared workspace display label",
        8,
    )?;
    h.send(6, b"\x1b[<2;2;24M\x1b[<2;2;24m")?;
    click_popup_entry_at(&mut h, 6, "close workspace")?;
    h.wait(
        |h| Ok(h.text(6).contains("Close workspace close-menu?")),
        "workspace close confirmation",
        8,
    )?;
    click_popup_entry_at(&mut h, 6, "Cancel")?;
    h.wait(
        |h| Ok(!h.text(6).contains("Close workspace close-menu?")),
        "mouse workspace cancel",
        8,
    )?;
    ensure!(
        h.tabs("close-menu")?[0]["panes"][0]["pid"] == close_pid,
        "cancel closed workspace"
    );
    h.send(6, b"\x1b[<2;2;24M\x1b[<2;2;24m")?;
    h.wait(
        |h| Ok(h.text(6).contains("Workspace close-menu actions")),
        "workspace close menu",
        8,
    )?;
    click_popup_entry_at(&mut h, 6, "close workspace")?;
    h.wait(
        |h| Ok(h.text(6).contains("Close workspace close-menu?")),
        "menu close confirmation",
        8,
    )?;
    click_popup_entry_at(&mut h, 6, "Confirm close")?;
    h.wait(
        |h| Ok(h.exited(6)? && h.exited(7)?),
        "workspace close detaches every viewer",
        8,
    )?;
    ensure!(
        h.success(6)? && h.success(7)?,
        "workspace close viewer exit failed"
    );
    ensure!(
        h.tabs("other")?[0]["panes"][0]["pid"] == other_pid,
        "workspace close affected another workspace"
    );
    h.cli(&["workspace", "new", "close-menu"])?;
    let recreated = h.listing("close-menu")?;
    let new_stream = recreated["event_cursor"]["stream"].clone();
    ensure!(new_stream != close_stream, "workspace lifetime reused");
    let mut stale = h.root.command(&h.binary);
    stale.args([
        "workspace",
        "close",
        "close-menu",
        "--instance",
        &instance,
        "--stream",
        &close_stream.to_string(),
    ]);
    let stale = process::output(stale, Duration::from_secs(8), 1048576)?;
    ensure!(
        !stale.status.success(),
        "stale CLI close killed recreated workspace"
    );
    let reply: Value = serde_json::from_slice(&stale.stdout)?;
    ensure!(
        reply["error"]["code"] == "conflict",
        "unexpected stale close reply: {reply}"
    );
    ensure!(
        h.listing("close-menu")?["event_cursor"]["stream"] == new_stream,
        "stale close changed replacement"
    );
    h.cli(&[
        "workspace",
        "close",
        "close-menu",
        "--instance",
        &instance,
        "--stream",
        &new_stream.to_string(),
    ])?;
    h.finish()?;
    println!(
        "PASS viewer scenarios: launch, popup, tabs, splits, resize, close, copy, viewers, tiny screens, workspaces, detach"
    );
    Ok(())
}

mod gesture;
mod modal;
mod mouse;
pub(super) fn gestures(binary: &Path) -> Result<()> {
    gesture::run(binary)
}
pub(super) fn modals(binary: &Path) -> Result<()> {
    modal::run(binary)
}
pub(super) fn mouse_app(binary: &Path) -> Result<()> {
    mouse::run(binary)
}

mod history;
pub(super) fn history_controls(binary: &Path) -> Result<()> {
    history::run(binary)
}

mod transfer;
pub(super) fn transfer_input(binary: &Path) -> Result<()> {
    transfer::run(binary)
}

mod tiny;
pub(super) fn verify_tiny_layout(binary: &Path) -> Result<()> {
    tiny::run(binary)
}

mod manager;
pub(super) fn manager_delay(binary: &Path) -> Result<()> {
    manager::run(binary)
}
