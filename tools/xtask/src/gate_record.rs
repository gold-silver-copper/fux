//! Durable evidence for the fixed sequential verification gate.
use crate::gate_process::{self, Outcome, State};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inputs {
    pub source: String,
    pub toolchain: BTreeMap<String, String>,
    pub environment: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub target: PathBuf,
    pub commands: Vec<Vec<String>>,
    pub exclusions: Vec<String>,
    pub timeout_seconds: u64,
    pub stream_limit: u64,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Status {
    NotRun,
    Running,
    Passed,
    Failed,
    Interrupted,
}

#[derive(Debug, Serialize, Deserialize)]
struct Check {
    status: Status,
    attempts: Vec<Attempt>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Attempt {
    logs: PathBuf,
    outcome: Option<Outcome>,
    error: Option<String>,
    log_hash: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    inputs: Inputs,
    checks: Vec<Check>,
    // Only written after all children are reaped. A killed runner has no valid
    // checkpoint and cannot lend earlier passing records to another invocation.
    checkpoint: Option<String>,
    complete: bool,
}

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

/// Streaming, sorted, content-based state hash. Never follow symlinks or include
/// access times; reading evidence must not invalidate it. Unsupported objects fail.
pub fn tree_hash(path: &Path) -> Result<String> {
    fn visit(path: &Path, digest: &mut Sha256) -> Result<()> {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                field(digest, b"absent");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        field(digest, &meta.permissions().mode().to_le_bytes());
        if meta.file_type().is_symlink() {
            field(digest, b"symlink");
            field(digest, fs::read_link(path)?.as_os_str().as_encoded_bytes());
        } else if meta.is_dir() {
            field(digest, b"directory");
            let mut entries = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                field(digest, entry.file_name().as_encoded_bytes());
                visit(&entry.path(), digest)?;
            }
        } else {
            ensure!(
                meta.is_file(),
                "unsupported gate state object: {}",
                path.display()
            );
            field(digest, b"file");
            field(digest, &meta.len().to_le_bytes());
            let mut file = File::open(path)?;
            let mut buffer = [0; 64 * 1024];
            loop {
                let length = file.read(&mut buffer)?;
                if length == 0 {
                    break;
                }
                digest.update(&buffer[..length]);
            }
        }
        Ok(())
    }
    let mut digest = Sha256::new();
    visit(path, &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}

fn checkpoint(inputs: &Inputs) -> Result<String> {
    // Hash retained build state once per stopped invocation, not once per check.
    // This deliberately avoids a speculative per-command dependency graph.
    Ok(hash(&serde_json::to_vec(&[
        tree_hash(&inputs.cwd)?,
        tree_hash(&inputs.target)?,
    ])?))
}

fn save(directory: &Path, manifest: &Manifest) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(&serde_json::to_vec_pretty(manifest)?)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(directory.join("manifest.json"))?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

/// Caller retains the exclusive gate lock throughout reconstruction and execution.
/// `current_inputs` rechecks live sources/configuration after execution as well.
pub fn execute(
    directory: &Path,
    inputs: Inputs,
    resume: bool,
    cancel: &AtomicBool,
    current_inputs: impl Fn() -> Result<Inputs>,
) -> Result<()> {
    let mut manifest = if resume {
        let mut record: Manifest =
            serde_json::from_slice(&fs::read(directory.join("manifest.json"))?)?;
        ensure!(
            record.version == 1 && record.inputs == inputs,
            "gate inputs changed; start a fresh run"
        );
        ensure!(
            record.checks.len() == inputs.commands.len(),
            "gate check plan is incomplete"
        );
        for check in &mut record.checks {
            if check.status == Status::Running {
                check.status = Status::Interrupted;
            }
        }
        save(directory, &record)?;
        ensure!(
            record.checkpoint.as_ref() == Some(&checkpoint(&inputs)?),
            "gate state missing or changed; start a fresh run"
        );
        for check in &record.checks {
            if check.status == Status::Passed {
                let attempt = check
                    .attempts
                    .last()
                    .context("passing check has no attempt")?;
                ensure!(
                    attempt.logs.starts_with(directory) && attempt.logs.parent() == Some(directory),
                    "invalid gate log location"
                );
                ensure!(
                    attempt
                        .outcome
                        .as_ref()
                        .is_some_and(|o| o.state == State::Passed && o.exit_code == Some(0)),
                    "passing check lacks successful outcome"
                );
                ensure!(
                    attempt.log_hash.as_ref() == Some(&tree_hash(&attempt.logs)?),
                    "gate diagnostics missing or changed; start a fresh run"
                );
            }
        }
        record
    } else {
        ensure!(
            !directory.join("manifest.json").exists(),
            "fresh gate manifest already exists"
        );
        Manifest {
            version: 1,
            checks: inputs
                .commands
                .iter()
                .map(|_| Check {
                    status: Status::NotRun,
                    attempts: Vec::new(),
                })
                .collect(),
            inputs: inputs.clone(),
            checkpoint: None,
            complete: false,
        }
    };
    manifest.checkpoint = None;
    manifest.complete = false;
    save(directory, &manifest)?;
    let mut cleanup_proven = true;
    let execution = (|| -> Result<()> {
        for (index, command) in inputs.commands.iter().enumerate() {
            if manifest.checks[index].status == Status::Passed {
                println!("combined: reuse check {index}: {}", command.join(" "));
                continue;
            }
            ensure!(
                !cancel.load(Ordering::SeqCst),
                "gate interrupted before check {index}"
            );
            let (program, args) = command.split_first().context("empty gate command")?;
            let logs = directory.join(format!(
                "{index:03}-{:03}",
                manifest.checks[index].attempts.len()
            ));
            manifest.checks[index].status = Status::Running;
            manifest.checks[index].attempts.push(Attempt {
                logs: logs.clone(),
                outcome: None,
                error: None,
                log_hash: None,
            });
            save(directory, &manifest)?;
            println!(
                "combined: check {index}: {}; logs {}",
                command.join(" "),
                logs.display()
            );
            let mut child = Command::new(program);
            child
                .args(args)
                .current_dir(&inputs.cwd)
                .env_clear()
                .envs(&inputs.environment);
            let result = gate_process::run(
                child,
                &logs,
                Duration::from_secs(inputs.timeout_seconds),
                inputs.stream_limit,
                cancel,
            );
            let check = &mut manifest.checks[index];
            let attempt = check
                .attempts
                .last_mut()
                .context("missing active attempt")?;
            match result {
                Ok(outcome) => {
                    check.status = match outcome.state {
                        State::Passed => Status::Passed,
                        State::Interrupted => Status::Interrupted,
                        _ => Status::Failed,
                    };
                    attempt.log_hash = Some(tree_hash(&logs)?);
                    attempt.outcome = Some(outcome);
                }
                Err(error) => {
                    // Infrastructure errors include cleanup failures. Do not
                    // checkpoint state that an un-reaped writer may still alter.
                    cleanup_proven = false;
                    check.status = if cancel.load(Ordering::SeqCst) {
                        Status::Interrupted
                    } else {
                        Status::Failed
                    };
                    attempt.error = Some(format!("{error:#}"));
                }
            }
            let passed = check.status == Status::Passed;
            save(directory, &manifest)?;
            ensure!(
                passed,
                "gate check {index} did not pass; inspect {}",
                logs.display()
            );
        }
        Ok(())
    })();
    // Changed source/configuration never becomes reusable passing evidence.
    ensure!(
        current_inputs()? == inputs,
        "gate inputs changed during execution; evidence cannot be reused"
    );
    ensure!(
        cleanup_proven,
        "gate process cleanup is unproven; continuation is unavailable"
    );
    manifest.checkpoint = Some(checkpoint(&inputs)?);
    manifest.complete = execution.is_ok();
    save(directory, &manifest)?;
    execution
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(commands: &[&str]) -> Result<(tempfile::TempDir, PathBuf, Inputs)> {
        let root = tempfile::tempdir()?;
        let directory = root.path().join("record");
        let cwd = root.path().join("source");
        fs::create_dir(&directory)?;
        fs::create_dir(&cwd)?;
        let inputs = Inputs {
            source: "fixture-source-v1".into(),
            toolchain: BTreeMap::new(),
            environment: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
            cwd,
            target: root.path().join("target"),
            commands: commands
                .iter()
                .map(|c| vec!["/bin/sh".into(), "-c".into(), (*c).into()])
                .collect(),
            exclusions: vec!["R6".into()],
            timeout_seconds: 5,
            stream_limit: 1024,
        };
        Ok((root, directory, inputs))
    }

    fn read(directory: &Path) -> Result<Manifest> {
        Ok(serde_json::from_slice(&fs::read(
            directory.join("manifest.json"),
        )?)?)
    }

    #[test]
    fn actual_gate_reuses_only_passing_checks_and_retries_failure() -> Result<()> {
        let (_root, directory, inputs) = setup(&[
            "if read line; then exit 42; fi; echo first >> calls",
            "if [ ! -e retried ]; then touch retried; echo diagnostic >&2; exit 7; fi; echo second >> calls",
            "echo third >> calls",
        ])?;
        let cancel = AtomicBool::new(false);
        assert!(
            execute(&directory, inputs.clone(), false, &cancel, || Ok(
                inputs.clone()
            ))
            .is_err()
        );
        let record = read(&directory)?;
        assert_eq!(
            record.checks.iter().map(|c| &c.status).collect::<Vec<_>>(),
            [&Status::Passed, &Status::Failed, &Status::NotRun]
        );
        assert_eq!(
            record.checks[1].attempts[0]
                .outcome
                .as_ref()
                .context("outcome")?
                .exit_code,
            Some(7)
        );
        execute(&directory, inputs.clone(), true, &cancel, || {
            Ok(inputs.clone())
        })?;
        assert_eq!(
            fs::read_to_string(inputs.cwd.join("calls"))?,
            "first\nsecond\nthird\n"
        );
        let record = read(&directory)?;
        assert!(record.complete);
        assert_eq!(
            record
                .checks
                .iter()
                .map(|c| c.attempts.len())
                .collect::<Vec<_>>(),
            [1, 2, 1]
        );
        Ok(())
    }

    #[test]
    fn changed_inputs_missing_logs_and_changed_artifacts_cannot_count_as_passing() -> Result<()> {
        for variant in 0..7 {
            let (_root, directory, mut inputs) = setup(&["echo ran >> calls"])?;
            let cancel = AtomicBool::new(false);
            execute(&directory, inputs.clone(), false, &cancel, || {
                Ok(inputs.clone())
            })?;
            match variant {
                0 => inputs.source.push('2'),
                1 => {
                    inputs.environment.insert("TERM".into(), "different".into());
                }
                2 => {
                    inputs.toolchain.insert("rustc".into(), "different".into());
                }
                3 => inputs.commands[0].push("different".into()),
                4 => fs::remove_file(directory.join("000-000/stdout.log"))?,
                5 => {
                    fs::create_dir_all(&inputs.target)?;
                    fs::write(inputs.target.join("changed"), "artifact")?;
                }
                _ => fs::write(inputs.cwd.join("changed"), "source")?,
            }
            assert!(
                execute(&directory, inputs.clone(), true, &cancel, || Ok(
                    inputs.clone()
                ))
                .is_err(),
                "variant {variant}"
            );
            assert_eq!(fs::read_to_string(inputs.cwd.join("calls"))?, "ran\n");
        }
        Ok(())
    }

    #[test]
    fn actual_interruption_is_persisted_and_unstarted_checks_remain_not_run() -> Result<()> {
        let (_root, directory, inputs) = setup(&["sleep 30 & wait", "echo must-not-run"])?;
        let flag = std::sync::Arc::new(AtomicBool::new(false));
        let copy = flag.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            copy.store(true, Ordering::SeqCst);
        });
        assert!(
            execute(&directory, inputs.clone(), false, &flag, || Ok(
                inputs.clone()
            ))
            .is_err()
        );
        thread
            .join()
            .map_err(|_| anyhow::anyhow!("interrupt thread"))?;
        let record = read(&directory)?;
        assert_eq!(record.checks[0].status, Status::Interrupted);
        assert_eq!(record.checks[1].status, Status::NotRun);
        assert!(!record.complete);
        Ok(())
    }

    #[test]
    fn input_changes_during_run_and_uncheckpointed_crashes_refuse_reuse() -> Result<()> {
        let (_root, directory, inputs) = setup(&["echo ran >> calls"])?;
        let cancel = AtomicBool::new(false);
        let mut changed = inputs.clone();
        changed.source.push('2');
        assert!(
            execute(&directory, inputs.clone(), false, &cancel, || Ok(
                changed.clone()
            ))
            .is_err()
        );
        assert!(read(&directory)?.checkpoint.is_none());
        assert!(
            execute(&directory, inputs.clone(), true, &cancel, || Ok(
                inputs.clone()
            ))
            .is_err()
        );
        let mut record = read(&directory)?;
        record.checks[0].status = Status::Running;
        save(&directory, &record)?;
        assert!(
            execute(&directory, inputs.clone(), true, &cancel, || Ok(
                inputs.clone()
            ))
            .is_err()
        );
        assert_eq!(read(&directory)?.checks[0].status, Status::Interrupted);
        assert_eq!(fs::read_to_string(inputs.cwd.join("calls"))?, "ran\n");
        Ok(())
    }

    #[test]
    fn runner_infrastructure_errors_never_produce_reusable_checkpoints() -> Result<()> {
        let (_root, directory, mut inputs) = setup(&["echo unused"])?;
        inputs.commands[0][0] = inputs.cwd.join("missing-program").display().to_string();
        let cancel = AtomicBool::new(false);
        assert!(
            execute(&directory, inputs.clone(), false, &cancel, || Ok(
                inputs.clone()
            ))
            .is_err()
        );
        let record = read(&directory)?;
        assert_eq!(record.checks[0].status, Status::Failed);
        assert!(record.checks[0].attempts[0].error.is_some());
        assert!(record.checkpoint.is_none());
        assert!(
            execute(&directory, inputs.clone(), true, &cancel, || Ok(
                inputs.clone()
            ))
            .is_err()
        );
        Ok(())
    }
}
