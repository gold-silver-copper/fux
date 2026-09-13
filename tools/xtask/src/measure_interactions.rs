//! Release scroll/render and manager round-trip samples, with observable completion.
use anyhow::{Context, Result, ensure};
use fux_xtask::support::{
    local::{Root, completed, rpc},
    terminal::Terminal,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};

struct Screen {
    terminal: Terminal,
    parser: vt100::Parser,
    bytes: u64,
}
impl Screen {
    fn pump(&mut self) -> Result<()> {
        self.terminal.pump()?;
        let bytes = self.terminal.take_output();
        self.bytes += bytes.len() as u64;
        self.parser.process(&bytes);
        Ok(())
    }
    fn wait(&mut self, predicate: impl Fn(&vt100::Screen) -> bool) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.pump()?;
            if predicate(self.parser.screen()) {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "render observation timed out: {}",
                self.parser.screen().contents()
            );
            ensure!(
                self.terminal.child.0.try_wait()?.is_none(),
                "viewer exited during measurement"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn pane(&self) -> String {
        self.parser
            .screen()
            .rows(0, 80)
            .take(22)
            .collect::<Vec<_>>()
            .join("\n")
    }
}
fn distribution(samples: &[f64]) -> Value {
    let mut ordered = samples.to_vec();
    ordered.sort_by(f64::total_cmp);
    json!({"samples_ms":samples,"median_ms":ordered[ordered.len()/2],
        "p95_ms":ordered[(ordered.len()*95/100).min(ordered.len()-1)]})
}
pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(args.len() == 1, "measure-interactions FUX_BINARY");
    ensure!(
        std::env::var_os("FUX_BETAMAX_DIR").is_none(),
        "disable Betamax capture for timing"
    );
    let binary = Path::new(&args[0]).canonicalize()?;
    let root = Root::new("interaction-perf-", &[
        "/bin/sh".into(), "-c".into(),
        "i=1; while [ $i -le 2000 ]; do printf 'H%04d\\n' $i; i=$((i+1)); done; printf 'HISTORY_READY\\n'; exec /bin/cat".into(),
    ])?;
    let mut server = root.server(&binary)?;
    let result = (|| -> Result<Value> {
        let mut viewer = Screen {
            terminal: Terminal::start(&root, &binary)?,
            parser: vt100::Parser::new(24, 80, 0),
            bytes: 0,
        };
        viewer.wait(|screen| screen.contents().contains("HISTORY_READY"))?;
        let listing = completed(&root.control(), json!({"command":"list","id":1}))?;
        let pane = listing
            .pointer("/workspaces/0/tabs/0/panes/0")
            .context("pane")?;
        let instance = listing["instance"].as_str().context("instance")?;
        let pane_id = pane["id"].as_u64().context("pane id")?;
        let pane_pid = pane["pid"].as_u64().context("pane pid")?;
        let manager = root.control().with_file_name("manager.sock");
        let request = json!({"request":"pane-location","instance":instance,"pane":pane_id});
        let initial = rpc(&manager, request.clone())?;
        let location = initial
            .pointer("/result/result/value/location")
            .context("manager location")?;
        ensure!(
            location["instance"] == instance
                && location["pane"] == pane_id
                && location["pid"] == pane_pid,
            "initial manager identity"
        );
        let server_pid = server.child.id();
        let viewer_pid = viewer.terminal.child.0.id();
        let start_server_cpu = crate::measure::cpu(server_pid)?;
        let start_viewer_cpu = crate::measure::cpu(viewer_pid)?;
        let start_bytes = viewer.bytes;
        let mut scroll = Vec::new();
        let mut scroll_bytes = Vec::new();
        let mut rss = Vec::new();
        for index in 1..=100 {
            let previous = viewer.pane();
            let before_bytes = viewer.bytes;
            let start = Instant::now();
            viewer.terminal.send(b"\x1b[<64;3;3M")?;
            let marker = format!("offset {} ", index * 3);
            viewer.wait(|screen| {
                screen.contents().contains(&marker)
                    && screen.rows(0, 80).take(22).collect::<Vec<_>>().join("\n") != previous
            })?;
            scroll.push(start.elapsed().as_secs_f64() * 1000.0);
            scroll_bytes.push(viewer.bytes - before_bytes);
            if index % 10 == 0 {
                rss.push(json!({"after_scroll":index,"server_kib":crate::measure::rss(server_pid)?,"viewer_kib":crate::measure::rss(viewer_pid)?}));
            }
        }
        let scroll_server_cpu = crate::measure::cpu(server_pid)? - start_server_cpu;
        let scroll_viewer_cpu = crate::measure::cpu(viewer_pid)? - start_viewer_cpu;
        let total_bytes = viewer.bytes - start_bytes;
        viewer.terminal.send(b"\x1b")?;
        viewer.wait(|screen| {
            screen.contents().contains("HISTORY_READY")
                && !screen.contents().contains("History pane")
        })?;
        let mut manager_samples = Vec::new();
        let start_cpu = crate::measure::cpu(server_pid)?;
        for _ in 0..200 {
            let start = Instant::now();
            let observed = rpc(&manager, request.clone())?;
            manager_samples.push(start.elapsed().as_secs_f64() * 1000.0);
            ensure!(
                observed == initial,
                "manager lookup changed immutable identity/location"
            );
        }
        let manager_cpu = crate::measure::cpu(server_pid)? - start_cpu;
        viewer.terminal.close()?;
        Ok(
            json!({"binary":binary,"geometry":[24,80],"scroll":distribution(&scroll),
            "scroll_server_cpu_s":scroll_server_cpu,"scroll_viewer_cpu_s":scroll_viewer_cpu,
            "scroll_pty_bytes":total_bytes,"scroll_pty_byte_samples":scroll_bytes,"rss_samples":rss,
            "manager":distribution(&manager_samples),"manager_server_cpu_s":manager_cpu,
            "limits":"Scroll completes after actual offset and changed pane rows render; 1 ms observation polling is included. Manager latency includes authenticated socket and JSON round-trip. RSS samples are discrete observations, not kernel peak or whole-process-tree memory. No Betamax or diagnostic capture during timing."}),
        )
    })();
    let cleanup = server.finish();
    let value = result?;
    cleanup?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
