use super::*;

/// Each sequence is one outer-terminal key press. The frontend decodes it,
/// the server re-encodes it for the focused PTY, and a raw-mode child records
/// what arrived. For canonical xterm encodings the two must be identical:
/// an application cannot tell the difference between fux and a bare terminal.
pub(super) fn run(s: &mut Server, sequences: &[String]) -> Result<()> {
    let f = s.attach(24, 80)?;
    let viewer = s.frontend(f)?.viewer;
    child_command(
        s,
        f,
        "stty raw -echo; printf '\\033[2J\\033[HKEYS-READY'; exec cat > keys.bin",
    )?;
    s.wait("raw key receiver ready", |s| {
        Ok(s.frame(viewer, 24, 80)?.starts_with("KEYS-READY")
            && s.directory.join("keys.bin").is_file())
    })?;
    running(s)?;
    let mut expected = Vec::new();
    let mut mismatches = Vec::new();
    for (index, sequence) in sequences.iter().enumerate() {
        let before = expected.len();
        expected.extend_from_slice(sequence.as_bytes());
        s.send(f, sequence.as_bytes())?;
        // One key per write, each acknowledged before the next: a lone Escape
        // must resolve through its disambiguation deadline, never by merging
        // with the following key.
        let mut received = Vec::new();
        let delivered = s
            .wait("key delivered byte-exact", |s| {
                received = fs::read(s.directory.join("keys.bin"))?;
                Ok(received.len() >= expected.len())
            })
            .is_ok()
            && received == expected;
        if !delivered {
            let actual = received.get(before..).unwrap_or_default().to_vec();
            mismatches.push(json!({
                "index": index,
                "sent": sequence.as_bytes(),
                "received": actual,
                "sent_text": format!("{}", sequence.escape_default()),
                "received_text": format!("{}", String::from_utf8_lossy(&actual).escape_default()),
            }));
            // Resynchronize the oracle on what actually arrived so later keys
            // are judged independently of an earlier mismatch.
            expected = received;
        }
    }
    s.journal.record(
        "keys_verified",
        json!({"sequences":sequences.len(),"mismatches":mismatches}),
    )?;
    ensure(
        mismatches.is_empty(),
        &format!(
            "application: {} of {} key encodings changed between the outer terminal and the pane: {}",
            mismatches.len(),
            sequences.len(),
            serde_json::to_string(&mismatches)?
        ),
    )
}
