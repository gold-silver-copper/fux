//! A seeded adversarial byte stream through a real child while the viewer
//! resizes and scrolls: valid and truncated escapes, C1 bytes, invalid UTF-8,
//! long lines, scroll regions, wrap off, origin mode.
use super::*;

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut x = *state;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

pub(super) fn stream(seed: u64, bytes: usize) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(bytes + 64);
    let words: [&[u8]; 6] = [
        b"alpha ",
        b"BETA ",
        "界".as_bytes(),
        "é".as_bytes(),
        b"\t",
        b"x",
    ];
    while out.len() < bytes {
        let r = splitmix(&mut state);
        let a = (r >> 8) as u8;
        let b = (r >> 16) as u8;
        match r % 24 {
            0 => out.extend_from_slice(b"\x1b[2J"),
            1 => out.extend_from_slice(format!("\x1b[{};{}H", 1 + a % 40, 1 + b % 200).as_bytes()),
            2 => out.extend_from_slice(
                format!(
                    "\x1b[{}m",
                    [0, 1, 4, 7, 31, 42, 91]
                        .get(usize::from(a % 7))
                        .copied()
                        .unwrap_or(0)
                )
                .as_bytes(),
            ),
            3 => out.extend_from_slice(b"\x1b[K"),
            4 => out.extend_from_slice(b"\x1b["), // truncated CSI
            5 => out.extend_from_slice(b"\x1b"),  // lone ESC
            6 => out.push(0x80 + a % 32),         // C1
            7 => out.extend_from_slice(&[0xc0, 0x80]), // overlong
            8 => out.push(0x80 | (a & 0x3f)),     // lone continuation
            9 => out.extend_from_slice(&[0xf0, 0x9f]), // truncated 4-byte
            10 => out.extend(std::iter::repeat_n(b'L', 500)),
            11 => out.extend_from_slice(format!("\x1b[{};{}r", 1 + a % 10, 12 + b % 12).as_bytes()),
            12 => out.extend_from_slice(b"\x1b[r"),
            13 => out.extend_from_slice(if a.is_multiple_of(2) {
                b"\x1b[?7l"
            } else {
                b"\x1b[?7h"
            }),
            14 => out.extend_from_slice(if a.is_multiple_of(2) {
                b"\x1b[?6h"
            } else {
                b"\x1b[?6l"
            }),
            15 => out.extend_from_slice(b"\r\n"),
            16 => out.extend_from_slice(b"\x1b[1000000000000C"), // absurd parameter
            17 => out.extend_from_slice(b"\x1b]0;title\x07"),
            18 => out.extend_from_slice(b"\x1b]52;c;bm9wZQ==\x07"), // OSC 52 from the child
            19 => out.extend_from_slice(b"\x07\x08\x0b\x0c"),
            20 => out.extend_from_slice(b"\x1b[?1049h"),
            21 => out.extend_from_slice(b"\x1b[?1049l"),
            22 => out.extend_from_slice(b"\x1b[38;2;1;2;3;48;5;200m"),
            _ => out.extend_from_slice(words.get(usize::from(a % 6)).copied().unwrap_or(b"x")),
        }
    }
    out.truncate(bytes);
    out
}
fn screen(s: &mut Server, f: usize) -> Result<String> {
    Ok(s.frontend(f)?.parser.screen().contents())
}
fn no_panic(s: &mut Server, label: &str) -> Result<()> {
    let err = s.stderr_text()?;
    let bad = err
        .lines()
        .find(|l| l.contains("panicked") || l.contains("thread '"));
    ensure(
        bad.is_none(),
        &format!("application: server panic {label}: {bad:?}"),
    )
}
fn inspect_now(s: &mut Server, v: u64, rows: u16, cols: u16, label: &str) -> Result<()> {
    let paint = s
        .rpc("fux.frame", json!({"viewer":v}))?
        .get("paint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let faults = super::chrome::inspect(&paint, rows, cols)?;
    ensure(
        faults.is_empty(),
        &format!(
            "application: paint broke the viewport {label} at {rows}x{cols}: {}",
            faults
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        ),
    )
}

pub(super) fn run(s: &mut Server, seed: u64) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(screen(s, f)?.contains("DEFAULT-SHELL"))
    })?;
    let shell = *running(s)?.first().ok_or("no shell")?;
    let bytes = stream(seed, 160 * 1024);
    fs::write(s.directory.join("stream.bin"), &bytes)?;
    s.journal
        .record("stream", json!({"seed":seed,"bytes":bytes.len()}))?;
    // Trickle the stream in 1 KiB pieces so resizes and scrolls interleave
    // with it, then end with a clean marker on a fresh screen.
    child_command(
        s,
        f,
        "stty raw -echo; i=0; while [ $i -lt 160 ]; do dd if=stream.bin bs=1024 skip=$i count=1 2>/dev/null; i=$((i+1)); sleep 0.02; done; E=EN; printf \"\\033[r\\033[?6l\\033[?7h\\033[?1049l\\033[0m\\033[2J\\033[H${E}DED\"; exec cat > /dev/null",
    )?;
    let sizes = [
        (24u16, 80u16),
        (10, 30),
        (3, 3),
        (40, 120),
        (24, 80),
        (2, 2),
        (24, 80),
    ];
    let mut i = 0;
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_millis(3800) {
        let (r, c) = sizes.get(i % sizes.len()).copied().unwrap_or((24, 80));
        i += 1;
        s.resize(f, r, c)?;
        s.wait("viewer resized", |s| {
            let st = s
                .query(VIEWER)?
                .into_iter()
                .find(|row| id(row).ok() == Some(v));
            Ok(
                st.and_then(|row| row.pointer("/components/fux::model::Viewer/rows")?.as_u64())
                    == Some(u64::from(r)),
            )
        })?;
        inspect_now(s, v, r, c, "during the stream")?;
        s.control(
            v,
            json!({"kind":"scroll","order":if i.is_multiple_of(2) {"previous"} else {"next"}}),
        )?;
        inspect_now(s, v, r, c, "after a scroll during the stream")?;
        no_panic(s, "during the stream")?;
        ensure(alive(shell), "application: the shell died under the stream")?;
    }
    s.resize(f, 24, 80)?;
    s.send(f, b"x")?; // return to live output
    s.wait("stream ended", |s| {
        Ok(s.frame(v, 24, 80)?.contains("ENDED"))
    })?;
    inspect_now(s, v, 24, 80, "after the stream")?;
    let mut server_view = String::new();
    let mut front_view = String::new();
    s.wait("frontend converges with the server frame", |s| {
        server_view = s.frame(v, 24, 80)?;
        front_view = screen(s, f)?;
        Ok(server_view == front_view && server_view.contains("ENDED"))
    })
    .map_err(|e| {
        let diff = server_view
            .lines()
            .zip(front_view.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| format!("line {i}: server {a:?} frontend {b:?}"))
            .unwrap_or_else(|| "line counts differ".into());
        format!(
            "application: after the adversarial stream the frontend did not converge: {diff}: {e}"
        )
    })?;
    no_panic(s, "at the end")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::stream;
    #[test]
    fn stream_is_seeded_and_bounded() {
        assert_eq!(stream(7, 4096), stream(7, 4096));
        assert_ne!(stream(7, 4096), stream(8, 4096));
        assert_eq!(stream(1, 1000).len(), 1000);
    }
}
