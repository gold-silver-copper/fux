//! Bounded evidence retained across fixture teardown. Successful scenarios leave
//! no artifacts. Runtime content logging remains separate and opt-in.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::VecDeque,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const MAX_TERMINALS: usize = 16;
const MAX_EVENTS: usize = 256;
const MAX_ROWS: u16 = 128;
const MAX_COLUMNS: u16 = 512;
const MAX_CAPTURE_BYTES: u64 = 8 * 1024 * 1024;
thread_local! { static ACTIVE: RefCell<Option<Recording>> = const { RefCell::new(None) }; }

struct Terminal {
    parser: Option<vt100::Parser>,
    rows: u16,
    columns: u16,
    output_bytes: u64,
    events: VecDeque<Value>,
    dropped_events: u64,
    checkpoint: Option<String>,
}
struct Recording {
    identity: Value,
    terminals: Vec<Terminal>,
    dropped_terminals: u64,
    rpc: VecDeque<Value>,
    dropped_rpc: u64,
    diagnostics: Vec<Value>,
    dropped_diagnostics: u64,
}

struct ActiveGuard;
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        if let Some(recording) = ACTIVE.with(|active| active.borrow_mut().take()) {
            let parent = std::env::var_os("FUX_HARNESS_ARTIFACTS");
            match recording.save(
                "scenario unwound before returning a result",
                parent.as_deref().map(Path::new),
            ) {
                Ok(path) => eprintln!("failure artifacts: {}", path.display()),
                Err(error) => eprintln!("failure artifact write failed: {error}"),
            }
        }
    }
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "identity target is not a regular file"
    );
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn source_identity() -> Result<Value> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let git = |args: &[&str]| -> Result<Vec<u8>> {
        let mut command = std::process::Command::new("git");
        command.current_dir(&root).args(args);
        let output =
            super::process::output(command, std::time::Duration::from_secs(5), 4 * 1024 * 1024)?;
        ensure!(
            output.status.success(),
            "source identity git command failed"
        );
        Ok(output.stdout)
    };
    let revision = String::from_utf8(git(&["rev-parse", "HEAD"])?)?
        .trim()
        .to_owned();
    let listing = git(&[
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
    ])?;
    let mut paths: Vec<_> = listing
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .collect();
    paths.sort();
    paths.dedup();
    let mut hash = Sha256::new();
    let mut files = 0;
    let mut source_bytes = 0_u64;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    for path in paths {
        ensure!(
            std::time::Instant::now() < deadline,
            "source identity time bound exceeded"
        );
        let relative = std::str::from_utf8(path)?;
        if relative.starts_with("docs/verification/") {
            continue;
        }
        let path = root.join(relative);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                hash.update(relative.as_bytes());
                hash.update(b"\0deleted\0");
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file() {
            continue;
        }
        ensure!(
            metadata.len() <= 64 * 1024 * 1024,
            "source file exceeds identity bound"
        );
        source_bytes = source_bytes.saturating_add(metadata.len());
        ensure!(
            source_bytes <= 128 * 1024 * 1024 && files < 16384,
            "source identity work bound exceeded"
        );
        hash.update(relative.as_bytes());
        hash.update(b"\0");
        hash.update(hash_file(&path)?.as_bytes());
        hash.update(b"\0");
        files += 1;
    }
    Ok(
        json!({"revision":revision,"worktree_sha256":format!("{:x}",hash.finalize()),"files":files,
        "scope":"tracked and nonignored untracked regular files, excluding docs/verification"}),
    )
}

fn observed(result: Result<Value>) -> Value {
    result.unwrap_or_else(
        |error| json!({"unavailable":error.to_string().chars().take(256).collect::<String>()}),
    )
}

/// Scenario entry point: capture identities before execution, retain observations
/// after its fixtures drop, and write a private artifact only on failure.
pub fn run(args: &[String], body: impl FnOnce() -> Result<()>) -> Result<()> {
    let binaries: Vec<_> = args.iter().skip(1).take(8).map(|path| {
        json!({"path":path,"identity":observed(hash_file(Path::new(path)).map(|hash| json!({"sha256":hash})))})
    }).collect();
    let identity = json!({"scenario":args.first(),"arguments":args,"binaries":binaries,
        "source_observed_before_run":observed(source_identity()),
        "seed":std::env::var("FUX_CONTROL_SEED").ok().and_then(|value| value.parse::<u64>().ok())});
    ACTIVE.with(|active| {
        ensure!(active.borrow().is_none(), "nested scenario recording");
        *active.borrow_mut() = Some(Recording {
            identity,
            terminals: Vec::new(),
            dropped_terminals: 0,
            rpc: VecDeque::new(),
            dropped_rpc: 0,
            diagnostics: Vec::new(),
            dropped_diagnostics: 0,
        });
        Ok::<_, anyhow::Error>(())
    })?;
    let _guard = ActiveGuard;
    let result = body();
    let recording = ACTIVE
        .with(|active| active.borrow_mut().take())
        .context("scenario recording missing")?;
    match result {
        Ok(()) => Ok(()),
        Err(error) => match recording.save(
            &format!("{error:#}"),
            std::env::var_os("FUX_HARNESS_ARTIFACTS")
                .as_deref()
                .map(Path::new),
        ) {
            Ok(path) => Err(error.context(format!("failure artifacts: {}", path.display()))),
            Err(artifact_error) => {
                Err(error.context(format!("failure artifact write failed: {artifact_error}")))
            }
        },
    }
}

pub fn terminal(rows: u16, columns: u16) -> Option<usize> {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let recording = active.as_mut()?;
        if recording.terminals.len() == MAX_TERMINALS {
            recording.dropped_terminals += 1;
            return None;
        }
        let id = recording.terminals.len();
        recording.terminals.push(Terminal {
            parser: (rows > 0 && columns > 0 && rows <= MAX_ROWS && columns <= MAX_COLUMNS)
                .then(|| vt100::Parser::new(rows, columns, 0)),
            rows,
            columns,
            output_bytes: 0,
            events: VecDeque::new(),
            dropped_events: 0,
            checkpoint: None,
        });
        Some(id)
    })
}

pub fn enabled() -> bool {
    ACTIVE.with(|active| active.borrow().is_some())
}

/// Called before the isolated root is removed. Only the explicitly selected
/// diagnostic log is retained; journals, prompts and environment files are not.
pub fn collect_root(root: &Path) {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let Some(recording) = active.as_mut() else { return; };
        for (kind, relative) in [("fux", "state/fux/diagnostics.log"), ("zor", "state/zor/diagnostics.jsonl")] {
        let path = root.join(relative);
        if !path.exists() { continue; }
        if recording.diagnostics.len() == 16 { recording.dropped_diagnostics += 1; continue; }
        let result = (|| -> Result<Value> {
            use std::io::{Seek, SeekFrom};
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)?;
            let metadata = file.metadata()?;
            ensure!(metadata.is_file(), "diagnostics is not a regular file");
            let length = metadata.len();
            file.seek(SeekFrom::Start(length.saturating_sub(65536)))?;
            let mut bytes = Vec::new();
            file.take(65536).read_to_end(&mut bytes)?;
            Ok(json!({"kind":kind,"tail":String::from_utf8_lossy(&bytes),"truncated":length > 65536}))
        })();
        recording.diagnostics.push(observed(result));
        }
    });
}
fn with_terminal(id: Option<usize>, update: impl FnOnce(&mut Terminal)) {
    ACTIVE.with(|active| {
        if let Some(terminal) = active
            .borrow_mut()
            .as_mut()
            .and_then(|recording| recording.terminals.get_mut(id?))
        {
            update(terminal);
        }
    });
}
fn event(terminal: &mut Terminal, value: Value) {
    if terminal.events.len() == MAX_EVENTS {
        terminal.events.pop_front();
        terminal.dropped_events += 1;
    }
    terminal.events.push_back(value);
}
pub fn output(id: Option<usize>, bytes: &[u8]) {
    with_terminal(id, |terminal| {
        terminal.output_bytes = terminal.output_bytes.saturating_add(bytes.len() as u64);
        if terminal.output_bytes > MAX_CAPTURE_BYTES {
            terminal.parser = None;
        }
        if let Some(parser) = &mut terminal.parser {
            parser.process(bytes);
        }
    });
}
pub fn input(id: Option<usize>, bytes: &[u8]) {
    with_terminal(id, |terminal| {
        event(
            terminal,
            json!({"stage":"attempt","input_bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))}),
        )
    });
}
pub fn resize(id: Option<usize>, rows: u16, columns: u16) {
    with_terminal(id, |terminal| {
        terminal.rows = rows;
        terminal.columns = columns;
        if rows == 0 || columns == 0 || rows > MAX_ROWS || columns > MAX_COLUMNS {
            terminal.parser = None;
        } else if let Some(parser) = &mut terminal.parser {
            parser.screen_mut().set_size(rows, columns);
        }
        event(terminal, json!({"resize":{"rows":rows,"columns":columns}}));
    });
}
pub fn checkpoint(id: Option<usize>, label: &str) {
    with_terminal(id, |terminal| {
        let label: String = label.chars().take(128).collect();
        terminal.checkpoint = Some(label.clone());
        event(terminal, json!({"checkpoint":label}));
    });
}

pub fn rpc(request: &Value, completed: bool, elapsed: std::time::Duration) {
    ACTIVE.with(|active| {
        if let Some(recording) = active.borrow_mut().as_mut() {
            if recording.rpc.len() == MAX_EVENTS { recording.rpc.pop_front(); recording.dropped_rpc += 1; }
            recording.rpc.push_back(json!({
                "command":request.get("command").or_else(|| request.get("request")).and_then(Value::as_str).map(|command| command.chars().take(64).collect::<String>()),
                "id":request.get("id").and_then(Value::as_u64),
                "pane":request.get("pane").and_then(Value::as_u64),
                "transport_completed":completed,"elapsed_us":elapsed.as_micros().min(u128::from(u64::MAX)) as u64
            }));
        }
    });
}

impl Recording {
    fn save(self, error: &str, parent: Option<&Path>) -> Result<PathBuf> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("fux-harness-failure-");
        let directory = match parent {
            Some(parent) => {
                fs::create_dir_all(parent)?;
                builder.tempdir_in(parent)?
            }
            None => builder.tempdir()?,
        };
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
        let terminals: Vec<_> = self.terminals.into_iter().enumerate().map(|(id, terminal)| {
            let screen = terminal.parser.as_ref().map(|parser| parser.screen().contents());
            let screen_truncated = screen.as_ref().is_some_and(|screen| screen.chars().count() > 65536);
            let screen = screen.map(|screen| screen.chars().take(65536).collect::<String>());
            json!({"id":id,"rows":terminal.rows,"columns":terminal.columns,"screen":screen,
                "screen_omitted":terminal.parser.is_none(),"screen_truncated":screen_truncated,"output_bytes":terminal.output_bytes,
                "last_checkpoint":terminal.checkpoint,"events":terminal.events,"dropped_events":terminal.dropped_events})
        }).collect();
        let value = json!({"version":1,"identity":self.identity,"error":error.chars().take(4096).collect::<String>(),
            "terminals":terminals,"dropped_terminals":self.dropped_terminals,"rpc":self.rpc,"dropped_rpc":self.dropped_rpc,
            "diagnostics":self.diagnostics,"dropped_diagnostics":self.dropped_diagnostics,
            "note":"Final fixture screen, not a visual approval. Input records contain lengths/hashes only."});
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.path().join("failure.json"))?;
        serde_json::to_writer_pretty(&mut file, &value)?;
        file.write_all(b"\n")?;
        Ok(directory.keep())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn begin() -> ActiveGuard {
        ACTIVE.with(|active| {
            *active.borrow_mut() = Some(Recording {
                identity: json!({"test":true}),
                terminals: Vec::new(),
                dropped_terminals: 0,
                rpc: VecDeque::new(),
                dropped_rpc: 0,
                diagnostics: Vec::new(),
                dropped_diagnostics: 0,
            })
        });
        ActiveGuard
    }

    #[test]
    fn collects_only_selected_diagnostics_and_rejects_fifo() -> Result<()> {
        let _guard = begin();
        let root = tempfile::tempdir()?;
        let zor = root.path().join("state/zor");
        let fux = root.path().join("state/fux");
        fs::create_dir_all(&zor)?;
        fs::create_dir_all(&fux)?;
        fs::write(zor.join("diagnostics.jsonl"), "safe-metadata")?;
        fs::write(zor.join("journal.json"), "private-prompt")?;
        nix::unistd::mkfifo(&fux.join("diagnostics.log"), nix::sys::stat::Mode::S_IRUSR)?;
        collect_root(root.path());
        let recording = ACTIVE
            .with(|active| active.borrow_mut().take())
            .context("recording")?;
        ensure!(
            recording.diagnostics.len() == 2,
            "both diagnostics must be observed"
        );
        let serialized = serde_json::to_string(&recording.diagnostics)?;
        ensure!(serialized.contains("safe-metadata"), "zor metadata lost");
        ensure!(
            serialized.contains("not a regular file"),
            "FIFO must be rejected"
        );
        ensure!(!serialized.contains("private-prompt"), "journal collected");
        Ok(())
    }

    #[test]
    fn private_artifact_preserves_screen_but_not_input_contents() -> Result<()> {
        let _guard = begin();
        let id = terminal(24, 80).context("terminal")?;
        output(Some(id), b"VISIBLE");
        input(Some(id), b"secret-prompt-not-echoed");
        rpc(
            &json!({"command":"send-keys","id":7,"pane":2,"keys":"secret-prompt-not-echoed"}),
            true,
            std::time::Duration::from_millis(1),
        );
        checkpoint(Some(id), "ready");
        let recording = ACTIVE
            .with(|active| active.borrow_mut().take())
            .context("recording")?;
        let parent = tempfile::tempdir()?;
        let directory = recording.save("controlled failure", Some(parent.path()))?;
        let path = directory.join("failure.json");
        let bytes = fs::read(&path)?;
        let value: Value = serde_json::from_slice(&bytes)?;
        ensure!(value["terminals"][0]["screen"] == "VISIBLE", "screen lost");
        ensure!(
            !String::from_utf8(bytes)?.contains("secret-prompt-not-echoed"),
            "input contents leaked"
        );
        ensure!(
            fs::metadata(path)?.permissions().mode() & 0o777 == 0o600,
            "artifact permissions"
        );
        ensure!(
            fs::metadata(directory)?.permissions().mode() & 0o777 == 0o700,
            "directory permissions"
        );
        Ok(())
    }

    #[test]
    fn oversized_recording_reports_omissions_and_bounds_event_retention() -> Result<()> {
        let _guard = begin();
        let id = terminal(24, 80).context("terminal")?;
        for _ in 0..300 {
            input(Some(id), b"x");
        }
        for _ in 0..MAX_TERMINALS + 2 {
            terminal(24, 80);
        }
        output(Some(id), &vec![0; MAX_CAPTURE_BYTES as usize + 1]);
        let recording = ACTIVE
            .with(|active| active.borrow_mut().take())
            .context("recording")?;
        ensure!(
            recording.terminals.len() == MAX_TERMINALS && recording.dropped_terminals == 3,
            "terminal retention bound"
        );
        let terminal = recording.terminals.first().context("terminal")?;
        ensure!(
            terminal.parser.is_none()
                && terminal.events.len() == MAX_EVENTS
                && terminal.dropped_events == 44,
            "terminal event/content bound"
        );
        Ok(())
    }

    #[test]
    fn failed_scenario_keeps_terminal_evidence_after_fixture_teardown() -> Result<()> {
        let result = run(&["artifact-probe".into(), "/bin/sh".into()], || {
            let root = super::super::local::Root::new("fartifact-", &["/bin/cat".into()])?;
            let mut terminal = super::super::terminal::Terminal::start_with_args(
                &root,
                Path::new("/bin/sh"),
                &["-c", "printf ARTIFACT_READY"],
            )?;
            terminal.wait_for("ARTIFACT_READY", std::time::Duration::from_secs(3))?;
            anyhow::bail!("intentional artifact probe")
        });
        let error = result.err().context("probe must fail")?;
        let message = error.to_string();
        let directory = PathBuf::from(
            message
                .strip_prefix("failure artifacts: ")
                .context("artifact path")?,
        );
        let value: Value = serde_json::from_slice(&fs::read(directory.join("failure.json"))?)?;
        ensure!(
            value["error"] == "intentional artifact probe",
            "original error lost: {value}"
        );
        ensure!(
            value["terminals"][0]["screen"]
                .as_str()
                .is_some_and(|screen| screen.contains("ARTIFACT_READY")),
            "post-teardown screen missing"
        );
        ensure!(
            value["identity"]["binaries"][0]["identity"]["sha256"]
                .as_str()
                .is_some(),
            "binary identity missing"
        );
        ensure!(
            value["identity"]["source_observed_before_run"]["worktree_sha256"]
                .as_str()
                .is_some(),
            "source identity missing"
        );
        fs::remove_dir_all(directory)?;
        Ok(())
    }
}
