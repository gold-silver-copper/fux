use super::*;
use crate::runtime::Frontend;
use std::time::{Duration, Instant};

const PREFIX: &[u8] = b"\x02";
const RIGHT: &[u8] = b"\x1b[C";
const DOWN: &[u8] = b"\x1b[B";
const HOME: &[u8] = b"\x1b[H";
const DISABLED: &str = "clipboard disabled";
pub(super) const COPIED: &str = "selection copied via OSC52";

/// Standard base64 with padding, as OSC 52 requires; written here so the
/// oracle does not share an encoder with the application.
pub(super) fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        let index = |shift: u32| usize::try_from((n >> shift) & 63).unwrap_or(0);
        for (i, shift) in [18u32, 12, 6, 0].iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(*TABLE.get(index(*shift)).unwrap_or(&b'A')));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn osc52(text: &str) -> Vec<u8> {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes())).into_bytes()
}

fn viewer_state(s: &mut Server, viewer: u64) -> Result<Value> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("copy viewer disappeared")?;
    component(row, VIEWER)
}

fn notice_text(s: &mut Server, viewer: u64) -> Result<Option<String>> {
    Ok(viewer_state(s, viewer)?
        .pointer("/notice/text")
        .and_then(Value::as_str)
        .map(str::to_owned))
}

fn enter_copy_mode(s: &mut Server, f: usize, viewer: u64) -> Result<()> {
    s.send(f, PREFIX)?;
    s.send(f, b"c")?;
    s.wait("copy mode entered", |s| {
        Ok(notice_text(s, viewer)?.is_some_and(|t| t.starts_with("Copy:")))
    })
}

fn repeat(s: &mut Server, f: usize, bytes: &[u8], times: usize) -> Result<()> {
    for _ in 0..times {
        s.send(f, bytes)?;
    }
    Ok(())
}

/// Presses `y` and waits for the exact OSC 52 payload on the outer terminal.
/// A successful copy also ends copy mode, clearing the notice and scrollback.
fn copy_and_expect(s: &mut Server, f: usize, viewer: u64, label: &str, text: &str) -> Result<()> {
    let wanted = osc52(text);
    let before = s.frontend(f)?.capture.total;
    s.send(f, b"y")?;
    s.journal.record(
        "copy_expectation",
        json!({"label":label,"text":text,"osc52":String::from_utf8_lossy(&wanted)}),
    )?;
    let delivered = s.wait("OSC 52 delivered to the outer terminal", |s| {
        Ok(since(s.frontend(f)?, before)
            .windows(wanted.len())
            .any(|w| w == wanted))
    });
    if let Err(error) = delivered {
        let capture = since(s.frontend(f)?, before);
        let received: Vec<String> = capture
            .split(|b| *b == 0x07)
            .filter_map(|part| {
                let start = part.windows(5).rposition(|w| w == b"\x1b]52;")?;
                Some(String::from_utf8_lossy(part.get(start..)?).into_owned())
            })
            .collect();
        let notice = notice_text(s, viewer)?;
        return Err(format!(
            "application: {label}: expected clipboard text {text:?} ({}), notice {notice:?}, OSC 52 payloads seen: {received:?}: {error}",
            String::from_utf8_lossy(&wanted)
        )
        .into());
    }
    // Success is reported in the bar and stays until later input.
    s.wait("copy reported in the bar", |s| {
        let state = viewer_state(s, viewer)?;
        Ok(state.pointer("/notice/text") == Some(&json!(COPIED))
            && state.pointer("/notice/error") == Some(&json!(false))
            && state.get("scrollback") == Some(&json!(0)))
    })?;
    Ok(())
}

/// Only bytes the outer terminal received after `total`, so an identical
/// earlier payload cannot satisfy a later expectation.
fn since(frontend: &Frontend, total: u64) -> Vec<u8> {
    let bytes = frontend.capture.bytes();
    let new = usize::try_from(frontend.capture.total.saturating_sub(total)).unwrap_or(usize::MAX);
    bytes
        .get(bytes.len().saturating_sub(new)..)
        .unwrap_or_default()
        .to_vec()
}

pub(super) fn run(s: &mut Server, reload: bool) -> Result<()> {
    let f = s.attach(24, 80)?;
    let viewer = s.frontend(f)?.viewer;
    // Row 0..2: plain, wide glyph (octal UTF-8: readline in the cleared C
    // locale would strip typed non-ASCII bytes), short. Rows 3..4: one 85-column line that
    // wraps at 80. Row 5: a marker proving the paint is complete.
    let wide = "W".repeat(80);
    child_command(
        s,
        f,
        &format!(
            "stty raw -echo; printf '\\033[2J\\033[HALPHA BRAVO\\r\\nCHARLIE \\347\\225\\214 DELTA\\r\\nECHO\\r\\n{wide}XXXXX\\r\\nREADY\\r\\n'; exec cat > /dev/null"
        ),
    )?;
    s.wait("copy source painted", |s| {
        let frame = s.frame(viewer, 24, 80)?;
        Ok(frame.starts_with("ALPHA BRAVO") && frame.contains("\nREADY"))
    })?;
    running(s)?;

    // A single word on the first row.
    enter_copy_mode(s, f, viewer)?;
    s.send(f, b" ")?;
    repeat(s, f, RIGHT, 4)?;
    if reload {
        s.send(f, b"y")?;
        s.wait("copy refused while the clipboard is disabled", |s| {
            Ok(notice_text(s, viewer)?.is_some_and(|t| t.contains(DISABLED)))
        })?;
        s.journal.record("hot_reload", json!({"file":"fux.json"}))?;
        fs::write(
            s.directory.join("fux.json"),
            serde_json::to_vec(&json!({"clipboard":"write-only"}))?,
        )?;
        // Retry the copy until the watched configuration has been applied.
        let wanted = osc52("ALPHA");
        let before = s.frontend(f)?.capture.total;
        let end = Instant::now() + Duration::from_secs(5);
        let mut attempts = 0;
        loop {
            attempts += 1;
            s.send(f, b"y")?;
            let mut delivered = false;
            let end_attempt = (Instant::now() + Duration::from_millis(250)).min(end);
            while Instant::now() < end_attempt {
                s.healthy()?;
                if since(s.frontend(f)?, before)
                    .windows(wanted.len())
                    .any(|w| w == wanted)
                {
                    delivered = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            if delivered {
                break;
            }
            ensure(
                Instant::now() < end,
                &format!(
                    "application: clipboard configuration created at runtime was not applied after {attempts} copy attempts; notice {:?}",
                    notice_text(s, viewer)?
                ),
            )?;
        }
        s.journal
            .record("hot_reload_applied", json!({"attempts":attempts}))?;
        s.wait("reload copy reported in the bar", |s| {
            Ok(notice_text(s, viewer)? == Some(COPIED.into()))
        })?;
    } else {
        copy_and_expect(s, f, viewer, "single word", "ALPHA")?;
    }

    // Two rows: the tail of one and the head of the next, joined by a newline.
    enter_copy_mode(s, f, viewer)?;
    repeat(s, f, RIGHT, 6)?;
    s.send(f, b" ")?;
    s.send(f, DOWN)?;
    copy_and_expect(s, f, viewer, "two rows", "BRAVO\nCHARLIE")?;

    // A wide glyph: Right steps over both of its cells, and the continuation
    // cell never appears in the copied text.
    enter_copy_mode(s, f, viewer)?;
    s.send(f, DOWN)?;
    repeat(s, f, RIGHT, 8)?;
    s.send(f, b" ")?;
    repeat(s, f, RIGHT, 2)?;
    copy_and_expect(s, f, viewer, "wide glyph", "\u{754c} D")?;

    // A soft-wrapped row joins its continuation without an invented newline.
    enter_copy_mode(s, f, viewer)?;
    repeat(s, f, DOWN, 3)?;
    repeat(s, f, RIGHT, 78)?;
    s.send(f, b" ")?;
    s.send(f, DOWN)?;
    s.send(f, HOME)?;
    copy_and_expect(s, f, viewer, "wrapped row", "WWX")?;

    // Mouse drag selects without copy mode; `y` copies after release.
    s.send(f, b"\x1b[<0;1;1M")?;
    s.send(f, b"\x1b[<32;3;1M")?;
    s.send(f, b"\x1b[<0;5;1m")?;
    copy_and_expect(s, f, viewer, "mouse drag", "ALPHA")?;

    // Escape clears and exits in one press; the pane gets ordinary input again.
    enter_copy_mode(s, f, viewer)?;
    s.send(f, b"\x1b")?;
    s.wait("escape exits copy mode", |s| {
        Ok(viewer_state(s, viewer)?.get("notice") == Some(&Value::Null))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base64_matches_rfc_4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64("\u{754c} D".as_bytes()), "55WMIEQ=");
        assert_eq!(osc52("ALPHA"), b"\x1b]52;c;QUxQSEE=\x07");
    }
}
