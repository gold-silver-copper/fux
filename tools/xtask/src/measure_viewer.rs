//! Main's real-viewer PTY benchmark. Text is an escape-stripped transcript, not a screen.
use crate::measure::{cpu, round};
use anyhow::{Context, Result, ensure};
use fux_xtask::support::{local::Root, terminal::Terminal};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

struct Reader {
    terminal: Terminal,
    raw: Vec<u8>,
    bytes: u64,
    escapes: regex::bytes::Regex,
}
impl Reader {
    fn pump(&mut self, duration: Duration) -> Result<()> {
        self.terminal.pump_for(duration, |bytes| {
            self.bytes += bytes.len() as u64;
            self.raw.extend_from_slice(bytes);
            let excess = self.raw.len().saturating_sub(1 << 20);
            self.raw.drain(..excess);
        })
    }
    fn wait_for(&mut self, predicate: impl Fn(&[u8]) -> bool, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            self.pump(
                Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())),
            )?;
            if predicate(&self.escapes.replace_all(&self.raw, &b""[..])) {
                return Ok(());
            }
            ensure!(
                self.terminal.child.0.try_wait()?.is_none(),
                "viewer exited before benchmark marker"
            );
        }
        anyhow::bail!("viewer output did not arrive")
    }
}
fn contains(text: &[u8], marker: &[u8]) -> bool {
    text.windows(marker.len()).any(|window| window == marker)
}

pub fn run(args: Vec<String>) -> Result<()> {
    let mut args = args.into_iter();
    let binary = std::fs::canonicalize(
        args.next()
            .context("measure-viewer BINARY [--keystrokes N] [--rows N] [--columns N]")?,
    )?;
    let mut keystrokes: usize = 200;
    let mut rows: u16 = 24;
    let mut columns: u16 = 80;
    while let Some(arg) = args.next() {
        let value = args.next().context("missing option value")?;
        match arg.as_str() {
            "--keystrokes" => keystrokes = value.parse()?,
            "--rows" => rows = value.parse()?,
            "--columns" => columns = value.parse()?,
            _ => anyhow::bail!("unknown measure-viewer option: {arg}"),
        }
    }
    ensure!(
        (1..=10000).contains(&keystrokes) && rows > 0 && columns > 0,
        "invalid viewer benchmark dimensions or keystrokes"
    );
    let root = Root::new(
        "fux-viewer-",
        &[
            "/bin/sh".into(),
            "-c".into(),
            "printf READY; exec /bin/sh".into(),
        ],
    )?;
    let mut server = root.server(&binary)?;
    let result: Result<Value> = (|| {
        let terminal = Terminal::start_with_size(&root, &binary, &[], rows, columns)?;
        let mut reader = Reader {
            terminal,
            raw: Vec::new(),
            bytes: 0,
            escapes: regex::bytes::Regex::new(
                r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-9;?<>=]*[ -/]*[@-~]|\x1b[()][A-Za-z0-9]|\x1b[=>]",
            )?,
        };
        reader.wait_for(|text| contains(text, b"READY"), Duration::from_secs(10))?;
        reader.pump(Duration::from_millis(500))?;
        reader.raw.clear();
        let viewer = reader.terminal.child.0.id();
        let server_pid = server.child.id();
        let viewer_cpu = cpu(viewer)?;
        let server_cpu = cpu(server_pid)?;
        let before = reader.bytes;
        let begin = Instant::now();
        for index in 0..keystrokes {
            if index > 0 && index % 40 == 0 {
                reader.raw.clear();
                reader.terminal.send(&[0x15])?;
                reader.wait_for(
                    |text| !text[text.len().saturating_sub(200)..].contains(&b'a'),
                    Duration::from_secs(5),
                )?;
                reader.raw.clear();
            }
            let wanted = index % 40 + 1;
            reader.terminal.send(b"a")?;
            reader.wait_for(
                |text| text.iter().filter(|&&byte| byte == b'a').count() >= wanted,
                Duration::from_secs(5),
            )?;
        }
        let keys = begin.elapsed().as_secs_f64();
        let viewer_keys = cpu(viewer)? - viewer_cpu;
        let server_keys = cpu(server_pid)? - server_cpu;
        let bytes_keys = reader.bytes - before;
        reader.terminal.send(&[0x15])?;
        reader.pump(Duration::from_millis(300))?;
        reader.raw.clear();
        let viewer_cpu = cpu(viewer)?;
        let server_cpu = cpu(server_pid)?;
        let before = reader.bytes;
        reader.terminal.send(b"i=0; while [ $i -lt 20000 ]; do echo line$i; i=$((i+1)); done; printf BURST''DONE\\\\n\n")?;
        let begin = Instant::now();
        reader.wait_for(|text| contains(text, b"BURSTDONE"), Duration::from_secs(60))?;
        let burst = begin.elapsed().as_secs_f64();
        reader.pump(Duration::from_millis(300))?;
        let result = json!({"binary":binary,"size":format!("{rows}x{columns}"),"keystrokes":keystrokes,
            "keystroke_loop_s":round(keys,3),"viewer_cpu_s_per_1000_keystrokes":round(viewer_keys*1000./keystrokes as f64,3),
            "server_cpu_s_per_1000_keystrokes":round(server_keys*1000./keystrokes as f64,3),"pty_bytes_per_keystroke":bytes_keys/keystrokes as u64,
            "burst_s":round(burst,3),"burst_viewer_cpu_s":round(cpu(viewer)?-viewer_cpu,3),
            "burst_server_cpu_s":round(cpu(server_pid)?-server_cpu,3),"burst_pty_bytes":reader.bytes-before});
        reader.terminal.send(b"\x01d")?;
        reader.pump(Duration::from_secs(1))?;
        reader.terminal.close()?;
        Ok(result)
    })();
    let cleanup = server.finish();
    let result = result?;
    cleanup?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
