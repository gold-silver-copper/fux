use super::*;

const LIMIT: usize = 65_536;
const BEGIN: &[u8] = b"\x1b[200~";
const END: &[u8] = b"\x1b[201~";

fn notice(s: &mut Server, viewer: u64) -> Result<Value> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("paste viewer disappeared")?;
    Ok(component(row, VIEWER)?
        .get("notice")
        .cloned()
        .unwrap_or(Value::Null))
}

/// The documented limit is 64 KiB of payload. Terminal framing is envelope, not
/// payload, and a rejected paste must deliver nothing at all.
fn expected(payload: &str, bracketed: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if payload.len() <= LIMIT {
        if bracketed {
            bytes.extend_from_slice(BEGIN);
        }
        bytes.extend_from_slice(payload.as_bytes());
        if bracketed {
            bytes.extend_from_slice(END);
        }
    }
    // A following ordinary key proves the decoder resumed after the end marker,
    // including after rejection, and gives delivery a FIFO barrier to wait on.
    bytes.push(b'!');
    bytes
}

pub(super) fn run(
    s: &mut Server,
    text: &str,
    repeats: usize,
    bracketed: bool,
    chunk_bytes: usize,
) -> Result<()> {
    let payload = text.repeat(repeats); // validated compact recipe; <= 65,537 bytes
    let f = s.attach(24, 80)?;
    let viewer = s.frontend(f)?.viewer;
    let mode = if bracketed { 'h' } else { 'l' };
    child_command(
        s,
        f,
        &format!(
            "stty raw -echo; printf '\\033[?2004{mode}\\033[2J\\033[HPASTE-READY'; exec cat > paste.bin"
        ),
    )?;
    s.wait("raw paste receiver ready", |s| {
        Ok(s.frame(viewer, 24, 80)?.starts_with("PASTE-READY")
            && s.directory.join("paste.bin").is_file())
    })?;
    // Record the real child so failure-path cleanup checks still apply here.
    running(s)?;
    s.journal.record(
        "paste_expectation",
        json!({"payload_bytes":payload.len(),"bracketed_target":bracketed,
            "chunk_bytes":chunk_bytes,"accepted":payload.len() <= LIMIT,
            "expected_bytes_with_barrier":expected(&payload, bracketed).len()}),
    )?;
    for chunk in BEGIN.chunks(chunk_bytes) {
        s.send(f, chunk)?;
    }
    s.wait("paste ownership acknowledged", |s| {
        Ok(notice(s, viewer)?.get("text") == Some(&json!("pasting...")))
    })?;
    for chunk in payload.as_bytes().chunks(chunk_bytes) {
        s.send(f, chunk)?;
    }
    for chunk in END.chunks(chunk_bytes) {
        s.send(f, chunk)?;
    }
    let mut completion = Value::Null;
    s.wait("paste completion", |s| {
        completion = notice(s, viewer)?;
        Ok(completion.get("text") != Some(&json!("pasting...")))
    })?;
    let delivered = fs::read(s.directory.join("paste.bin"))?.len();
    s.journal.record(
        "paste_completion",
        json!({"notice":completion,"payload_bytes":payload.len(),
            "bracketed_target":bracketed,"delivered_before_barrier":delivered}),
    )?;
    if payload.len() <= LIMIT {
        ensure(
            completion.is_null(),
            &format!(
                "valid {}-byte paste rejected (bracketed target={bracketed}): {completion}; child received {delivered} bytes",
                payload.len()
            ),
        )?;
    } else {
        ensure(
            completion.get("error") == Some(&json!(true))
                && completion
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains("paste exceeds 64 KiB")),
            &format!("oversized paste was not explicitly discarded: {completion}"),
        )?;
    }
    s.send(f, b"!")?;
    let wanted = expected(&payload, bracketed);
    s.wait("paste byte-exact delivery and decoder recovery", |s| {
        Ok(fs::read(s.directory.join("paste.bin"))? == wanted)
    })?;
    s.journal.record(
        "paste_delivery_verified",
        json!({"bytes":wanted.len(),"byte_exact":true}),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oracle_counts_payload_bytes_and_expects_nothing_from_a_rejected_paste() {
        let maximum = "x".repeat(LIMIT);
        assert_eq!(expected(&maximum, false).len(), LIMIT + 1);
        // Envelope bytes are framing, so the accepted payload stays 64 KiB.
        assert_eq!(
            expected(&maximum, true).len(),
            LIMIT + BEGIN.len() + END.len() + 1
        );
        let over = "x".repeat(LIMIT + 1);
        assert_eq!(expected(&over, true), b"!");
        assert_eq!(expected(&over, false), b"!");
        // Multibyte payloads are bounded by UTF-8 length, not character count.
        let wide = "界".repeat(21_845);
        assert_eq!(wide.len(), 65_535);
        assert_eq!(expected(&wide, false).len(), 65_536);
    }
}
