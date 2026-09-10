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
    fn hold(&mut self, mut check: impl FnMut(&Self) -> bool, seconds: f64) -> Result<()> {
        let end = Instant::now() + Duration::from_secs_f64(seconds);
        while Instant::now() < end {
            self.pump(0.03)?;
            ensure!(check(self), "viewer isolation changed");
        }
        Ok(())
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
                return Ok(state);
            }
            ensure!(Instant::now() < end, "{label}: {state:?}");
            self.pump(0.03)?;
        }
    }
    fn resize(&mut self, index: usize, rows: u16, columns: u16) -> Result<()> {
        self.viewers[index]
            .screen
            .screen_mut()
            .set_size(rows, columns);
        self.viewers[index].terminal.resize(rows, columns)
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
                failures.push(format!("{error:#}"));
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

pub(super) fn run(binary: &Path) -> Result<()> {
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
    h.wait(
        |h| Ok(h.text(0).contains("Panes") && h.text(0).contains("Session")),
        "popup headings",
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
        |h| Ok(h.text(0).contains("split side by side")),
        "rename escape",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.pump(0.15)?;
    ensure!(
        h.tabs("default")?[1]["name"] == "tab-2",
        "cancel changed rename"
    );
    h.send(0, "\x01,\x15renamed界\r".as_bytes())?;
    h.state(|s| s[1]["name"] == "renamed界", "Unicode rename", 8)?;
    h.send(0, b"\x01|")?;
    let state = h.state(|s| panes(&s[1]) == 2, "split", 8)?;
    let before = widths(&state[1]);
    h.send(0, b"\x01r")?;
    h.wait(|h| Ok(h.text(0).contains("Resize")), "resize hint", 8)?;
    h.send(0, b"jj\r")?;
    h.state(|s| widths(&s[1]) != before, "repeated resize", 8)?;
    h.send(
        0,
        format!("\x01r{}\r\x01,\x15burst-done\r", "jk".repeat(128)).as_bytes(),
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
        |h| Ok(h.text(0).contains("Close tab") && h.text(0).contains("1 pane")),
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
    h.send(0, b"\x01Xy")?;
    h.state(|s| panes(&s[0]) == 1, "second split close", 8)?;
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
    h.send(0, b"\x01s")?;
    h.wait(
        |h| Ok(h.text(0).contains("Choose workspace")),
        "workspace chooser",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.pump(0.1)?;
    h.wait(
        |h| Ok(h.text(0).contains("split side by side")),
        "chooser cancel",
        8,
    )?;
    h.send(0, b"\x1b")?;
    h.pump(0.1)?;
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
    h.finish()?;
    println!(
        "PASS viewer scenarios: launch, popup, tabs, splits, resize, close, copy, viewers, tiny screens, workspaces, detach"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unfinished_osc_is_bounded_across_terminal_reads() -> Result<()> {
        let mut bound = EscapeBound::default();
        bound.feed(b"\x1b")?;
        bound.feed(b"]52;c;")?;
        for _ in 0..31 {
            bound.feed(&vec![b'x'; 65536])?;
        }
        assert!(bound.feed(&vec![b'x'; 65536]).is_err());
        let mut bounded = EscapeBound::default();
        for _ in 0..40 {
            bounded.feed(b"\x1b]52;c;")?;
            bounded.feed(&vec![b'x'; 65536])?;
            bounded.feed(b"\x1b")?;
            bounded.feed(b"\\")?;
        }
        Ok(())
    }
}
