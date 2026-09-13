//! `zor run`: one command in a throwaway fux workspace, its final screen and exit status.
//!
//! A workflow over fux primitives: the manager's create-only `create`, `split` with an
//! environment and a headless size, the retained `final` record, and workspace `kill`. Nothing
//! is durable and no zor service is involved. Cleanup owns the created workspace and exact
//! launched pane, including when that pane moves to another workspace.
use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A run reads the record within its own poll loop, so the record only has to outlive the last
/// `final` poll before the run's deadline: one request round trip after `--timeout`.
const FINAL_POLL_MARGIN_MS: u64 = 5_000;

/// The final-record retention a run asks fux for on `split`: the run's `--timeout` (it polls
/// `final` every 25 ms until then and gives up after) plus the last poll's round trip, clamped
/// to fux's ceiling, which fux would apply anyway.
fn final_retain_ms(timeout_ms: u64) -> u64 {
    timeout_ms
        .saturating_add(FINAL_POLL_MARGIN_MS)
        .min(crate::fux::MAX_FINAL_RETENTION_MS)
}

pub struct Run {
    /// Covers workspace creation, the launch and the wait for final evidence.
    pub timeout_ms: u64,
    pub rows: Option<u16>,
    pub columns: Option<u16>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    /// An explicit name; it must not exist yet. The default is unique to this process.
    pub workspace: Option<String>,
    pub argv: Vec<String>,
}

/// The workspace the manager created for this run: its identity pins every later request.
struct Owned {
    name: String,
    stream: u64,
    instance: String,
    control: PathBuf,
    pane: Option<u64>,
}

/// Runs the command and returns the child's exit status as a process exit code.
pub fn run(request: Run) -> Result<u8> {
    if request.argv.is_empty() {
        bail!("run requires a command after `--`");
    }
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or(0);
    let name = request
        .workspace
        .clone()
        .unwrap_or_else(|| format!("run-{}-{elapsed}", std::process::id()));
    anyhow::ensure!(
        crate::tasks::model::workspace(&name),
        "invalid workspace name {name:?}"
    );
    let cwd = request
        .cwd
        .as_deref()
        .map(std::path::absolute)
        .transpose()?;
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(request.timeout_ms))
        .context("run timeout is too large")?;
    let runtime = crate::fux::runtime()?;
    let mut owned = create_workspace(&runtime, &name, deadline)?;
    let result = run_in_workspace(&runtime, &mut owned, &request, cwd, deadline);
    // A reused name or a replacement server must never receive this run's cleanup request.
    // Attempt both cleanups even if one fails. Never kill the destination workspace.
    let pane_cleanup = release_pane(&runtime, &owned);
    let workspace_cleanup = release_workspace(&owned);
    let cleanup = pane_cleanup.and(workspace_cleanup);
    match (result, cleanup) {
        (Ok(code), Ok(())) => Ok(u8::try_from(code).unwrap_or(1)),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error.context("run workspace cleanup failed")),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("run workspace cleanup also failed: {cleanup:#}")))
        }
    }
}

/// Creates the run's workspace on the session server, starting one when none is listening.
/// Creation is create-only in the manager, so a name that already exists is refused rather
/// than borrowed.
fn create_workspace(runtime: &Path, name: &str, deadline: Instant) -> Result<Owned> {
    let endpoint = crate::fux::endpoint::Endpoint::new(runtime);
    let control = endpoint.workspace(name)?;
    let limit = deadline.min(Instant::now() + Duration::from_secs(15));
    let descriptor = match crate::fux::manager::create(runtime, name, limit) {
        Ok(descriptor) => descriptor,
        Err(error) if no_server(&error) => start_server(name, deadline)?,
        Err(error) => return Err(error),
    };
    Ok(Owned {
        name: name.to_owned(),
        stream: descriptor.stream,
        instance: descriptor.instance_nonce,
        control,
        pane: None,
    })
}

/// No session server is listening: `fux workspace new NAME` starts one whose initial workspace
/// is the run's. The descriptor it prints must carry stream 1, the mark of a fresh server's
/// first workspace; anything else means another server won startup and the name was resolved
/// against it, which a run never borrows.
fn start_server(name: &str, deadline: Instant) -> Result<crate::fux::manager::Descriptor> {
    let mut command = std::process::Command::new("fux");
    command.args(["workspace", "new", name]);
    let output = crate::platform::process::run_command(&mut command, deadline)
        .context("start a fux session server (`fux` must be on PATH)")?;
    anyhow::ensure!(
        output.status.success(),
        "fux session server startup failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let descriptor = crate::fux::manager::created_from_cli(&output.stdout, name)?;
    anyhow::ensure!(
        descriptor.stream == 1,
        "initial workspace was replaced during startup"
    );
    Ok(descriptor)
}

fn run_in_workspace(
    runtime: &Path,
    owned: &mut Owned,
    request: &Run,
    cwd: Option<PathBuf>,
    deadline: Instant,
) -> Result<u32> {
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .with_context(|| {
                format!(
                    "run command did not finish within {} ms",
                    request.timeout_ms
                )
            })
    };
    remaining()?;
    let pane = u64::from(crate::fux::pane::split(
        &owned.control,
        &owned.instance,
        crate::fux::pane::Spawn {
            stream: owned.stream,
            cwd: cwd.as_deref(),
            argv: &request.argv,
            env: &request.env,
            rows: request.rows,
            columns: request.columns,
            fixed_workspace: true,
            final_retain_ms: final_retain_ms(request.timeout_ms),
        },
        deadline,
    )?);
    owned.pane = Some(pane);
    let mut released = false;
    // The manager retains authoritative evidence even when the pane exits before its launch
    // reply or its workspace socket disappears; no event connection is needed.
    let record = loop {
        remaining()?;
        match crate::fux::manager::final_record(
            runtime,
            &owned.instance,
            u32::try_from(pane)?,
            deadline,
        )
        .context("run final evidence")?
        {
            crate::fux::manager::FinalOutcome::Record(record) => break record,
            crate::fux::manager::FinalOutcome::Pending => {
                if !released {
                    // Final polling and timeout cleanup now use exact manager identity.
                    // A fast exit between these reads is handled by the next final poll.
                    if let Some(location) = pane_location(runtime, owned, deadline)?
                        && location.accepts_input
                    {
                        released = match crate::fux::manager::release_pin(
                            runtime,
                            &owned.instance,
                            u32::try_from(pane)?,
                            location.pid,
                            deadline,
                        ) {
                            Ok(()) => true,
                            Err(error)
                                if crate::fux::manager::remote_code(&error)
                                    == Some("not-found") =>
                            {
                                false
                            }
                            Err(error) => return Err(error.context("run release-pane-pin failed")),
                        };
                    }
                }
                std::thread::sleep(remaining()?.min(Duration::from_millis(25)));
            }
        }
    };
    anyhow::ensure!(
        u64::from(record.pane) == pane
            && record.workspace == owned.name
            && record.stream == owned.stream,
        "run final evidence identity mismatch"
    );
    anyhow::ensure!(
        !record.capture.truncated,
        "run final screen exceeded the retained capture limit"
    );
    let text = &record.capture.text;
    if !text.is_empty() {
        writeln!(std::io::stdout().lock(), "{text}")?;
    }
    record
        .exit_status
        .context("run was released before its exit status was observed")
}

/// Mutable routing is read separately from the immutable launch attribution.
fn pane_location(
    runtime: &Path,
    owned: &Owned,
    deadline: Instant,
) -> Result<Option<crate::fux::manager::Location>> {
    let Some(pane) = owned.pane else {
        return Ok(None);
    };
    let location =
        match crate::fux::manager::locate(runtime, &owned.instance, u32::try_from(pane)?, deadline)
        {
            Ok(location) => location,
            Err(error) => {
                match crate::fux::manager::remote_code(&error) {
                    Some("not-found") => return Ok(None),
                    Some("conflict") => {
                        let info = crate::fux::info::manager(runtime, deadline)?;
                        if info.instance_nonce != owned.instance {
                            return Ok(None);
                        }
                    }
                    _ => {}
                }
                return Err(error.context("run pane location unavailable"));
            }
        };
    validate_location(owned, &location)?;
    Ok(Some(location))
}

fn validate_location(owned: &Owned, location: &crate::fux::manager::Location) -> Result<()> {
    anyhow::ensure!(
        location.instance == owned.instance
            && Some(u64::from(location.pane)) == owned.pane
            && location.pane != 0
            && location.pid != 0
            && location.origin_workspace == owned.name
            && location.origin_stream == owned.stream
            && crate::tasks::model::workspace(&location.workspace)
            && location.stream != 0,
        "run pane location identity mismatch"
    );
    Ok(())
}

/// Release only this run's pane, wherever it currently lives. A move racing the scoped kill
/// is harmless: retry discovery under the same instance/pane identity within a bounded deadline.
fn release_pane(runtime: &Path, owned: &Owned) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let location = match pane_location(runtime, owned, deadline) {
            Ok(Some(location)) => location,
            Ok(None) => return Ok(()),
            Err(error) if no_server(&error) => return Ok(()),
            Err(error) => return Err(error),
        };
        let reply = crate::fux::pane::act(
            &crate::fux::endpoint::Endpoint::new(runtime).workspace(&location.workspace)?,
            &owned.instance,
            location.pane,
            crate::fux::pane::Action::Kill,
            deadline,
        );
        match reply {
            Ok(()) => return Ok(()),
            Err(error) if crate::fux::pane::remote_code(&error) == Some("not-found") => (),
            Err(error) if no_server(&error) => (),
            Err(error) => return Err(error),
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "run pane kept moving during cleanup"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Kills the owned workspace, pinned to its instance and stream. A listener that is already gone,
/// a replaced workspace or a missing one all mean there is nothing of ours left to release.
fn release_workspace(owned: &Owned) -> Result<()> {
    match crate::fux::pane::kill_workspace(
        &owned.control,
        &owned.instance,
        owned.stream,
        &owned.name,
        Instant::now() + Duration::from_secs(5),
    ) {
        Ok(()) => Ok(()),
        Err(error) if no_server(&error) => Ok(()),
        Err(error)
            if matches!(
                crate::fux::pane::remote_code(&error),
                Some("conflict" | "not-found")
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error.context("owned workspace was not released")),
    }
}

/// No session server is listening (as opposed to one that answered badly).
fn no_server(error: &anyhow::Error) -> bool {
    crate::fux::error::is_unavailable(error)
}

/// `NAME=VALUE` pairs from the command line.
pub fn env_pairs(values: Vec<String>) -> Result<Vec<(String, String)>> {
    values
        .into_iter()
        .map(|pair| {
            let (name, value) = pair
                .split_once('=')
                .with_context(|| format!("--env requires NAME=VALUE, got {pair:?}"))?;
            anyhow::ensure!(!name.is_empty(), "--env requires a non-empty name");
            Ok((name.to_owned(), value.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn moved_run_location_requires_original_launch_identity() -> Result<()> {
        let owned = Owned {
            name: "launch".into(),
            stream: 2,
            instance: "server".into(),
            control: "/unused/launch.sock".into(),
            pane: Some(3),
        };
        let valid = json!({"instance":"server","pane":3,"pid":4,"accepts_input":true,"workspace":"destination",
            "stream":9,"origin_workspace":"launch","origin_stream":2,"tab":1,"layout_generation":1});
        validate_location(&owned, &serde_json::from_value(valid.clone())?)?;
        for (key, value) in [
            ("instance", json!("replacement")),
            ("pane", json!(30)),
            ("pid", json!(0)),
            ("workspace", json!("../escape")),
            ("stream", json!(0)),
            ("origin_workspace", json!("destination")),
            ("origin_stream", json!(9)),
        ] {
            let mut invalid = valid.clone();
            invalid
                .as_object_mut()
                .context("location object")?
                .insert(key.into(), value);
            assert!(
                validate_location(&owned, &serde_json::from_value(invalid)?).is_err(),
                "accepted {key}"
            );
        }
        Ok(())
    }

    #[test]
    fn final_retention_follows_the_timeout_and_stays_under_fux_ceiling() {
        assert_eq!(final_retain_ms(30_000), 30_000 + FINAL_POLL_MARGIN_MS);
        assert!(final_retain_ms(0) > 0, "fux refuses a zero retention");
        assert_eq!(
            final_retain_ms(u64::MAX),
            crate::fux::MAX_FINAL_RETENTION_MS
        );
        assert!(final_retain_ms(86_400_000) <= crate::fux::MAX_FINAL_RETENTION_MS);
    }

    #[test]
    fn env_pairs_split_on_the_first_equals_and_reject_malformed_entries() {
        let pairs = env_pairs(vec!["A=1".into(), "B=x=y".into(), "C=".into()]).unwrap_or_default();
        assert_eq!(
            pairs,
            vec![
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "x=y".to_owned()),
                ("C".to_owned(), String::new()),
            ]
        );
        assert!(env_pairs(vec!["NOVALUE".into()]).is_err());
        assert!(env_pairs(vec!["=v".into()]).is_err());
    }

    #[test]
    fn a_missing_socket_is_no_server_and_a_refusal_is_not() {
        let missing = std::env::temp_dir().join(format!("zor-run-missing-{}", std::process::id()));
        let error =
            crate::fux::manager::create(&missing, "test", Instant::now() + Duration::from_secs(1))
                .err()
                .map(|error| no_server(&error));
        assert_eq!(error, Some(true));
        assert!(!no_server(&anyhow::anyhow!(
            "run requires a fresh workspace"
        )));
    }

    #[test]
    fn an_empty_command_and_an_invalid_name_are_rejected_before_any_socket_is_touched() {
        let empty = run(Run {
            timeout_ms: 10,
            rows: None,
            columns: None,
            env: Vec::new(),
            cwd: None,
            workspace: None,
            argv: Vec::new(),
        });
        assert!(empty.is_err());
        let invalid = run(Run {
            timeout_ms: 10,
            rows: None,
            columns: None,
            env: Vec::new(),
            cwd: None,
            workspace: Some("../x".into()),
            argv: vec!["/bin/true".into()],
        });
        assert!(invalid.is_err());
    }
}
