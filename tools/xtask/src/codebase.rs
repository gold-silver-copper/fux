//! Local fux/zor verification using the existing bounded gate process owner.
//! Companion checkout and publication operations are deliberately separate.
use crate::gate_process::{self, Cancellation, State};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, process::Command, time::Duration};

fn plan(full: bool) -> Vec<Vec<String>> {
    let mut commands: Vec<Vec<String>> = Vec::new();
    let mut add = |args: &[&str]| commands.push(args.iter().map(|arg| (*arg).into()).collect());
    add(&["cargo", "+stable", "fmt", "--all", "--check"]);
    add(&[
        "cargo",
        "+stable",
        "fmt",
        "--manifest-path",
        "tools/xtask/Cargo.toml",
        "--check",
    ]);
    add(&[
        "cargo",
        "+stable",
        "run",
        "--locked",
        "--manifest-path",
        "tools/xtask/Cargo.toml",
        "--",
        "verify-boundaries",
    ]);
    if full {
        add(&[
            "cargo",
            "+stable",
            "fmt",
            "--manifest-path",
            "crates/zor/tools/xtask/Cargo.toml",
            "--check",
        ]);
        add(&[
            "cargo",
            "+stable",
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ]);
        for features in [
            &["--all-features"][..],
            &["--no-default-features"][..],
            &["--no-default-features", "--features", "cli"][..],
        ] {
            let mut args = vec![
                "cargo",
                "+stable",
                "clippy",
                "-p",
                "zor",
                "--all-targets",
                "--locked",
            ];
            args.extend(features);
            args.extend(["--", "-D", "warnings"]);
            add(&args);
        }
        add(&[
            "cargo",
            "+stable",
            "clippy",
            "--manifest-path",
            "crates/zor/tools/xtask/Cargo.toml",
            "--target-dir",
            "@FIXTURES@",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ]);
        add(&[
            "cargo",
            "+stable",
            "clippy",
            "--manifest-path",
            "tools/xtask/Cargo.toml",
            "--target-dir",
            "@HARNESS@",
            "--features",
            "betamax",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ]);
    }
    add(&[
        "cargo", "+stable", "build", "--locked", "-p", "fux", "--bin", "fux",
    ]);
    add(&[
        "cargo",
        "+stable",
        "build",
        "--locked",
        "-p",
        "zor",
        "--no-default-features",
        "--features",
        "cli",
        "--bin",
        "zor",
    ]);
    add(&[
        "cargo",
        "+stable",
        "build",
        "--manifest-path",
        "tools/xtask/Cargo.toml",
        "--target-dir",
        "@HARNESS@",
        "--features",
        "betamax",
        "--locked",
    ]);
    add(&[
        "cargo",
        "+stable",
        "build",
        "--manifest-path",
        "crates/zor/tools/xtask/Cargo.toml",
        "--target-dir",
        "@FIXTURES@",
        "--bins",
        "--locked",
    ]);
    add(&[
        "cargo",
        "+stable",
        "test",
        "--manifest-path",
        "tools/xtask/Cargo.toml",
        "--target-dir",
        "@HARNESS@",
        "--features",
        "betamax",
        "--locked",
        "--",
        "--test-threads=1",
    ]);
    if full {
        add(&[
            "cargo",
            "+stable",
            "test",
            "--workspace",
            "--locked",
            "--no-fail-fast",
            "--",
            "--test-threads=1",
        ]);
        for features in [
            &["--all-features"][..],
            &["--no-default-features"][..],
            &["--no-default-features", "--features", "cli"][..],
        ] {
            let mut args = vec!["cargo", "+stable", "test", "-p", "zor", "--locked"];
            args.extend(features);
            args.extend(["--", "--test-threads=1"]);
            add(&args);
        }
        add(&[
            "cargo",
            "+stable",
            "test",
            "--manifest-path",
            "crates/zor/tools/xtask/Cargo.toml",
            "--target-dir",
            "@FIXTURES@",
            "--locked",
            "--",
            "--test-threads=1",
        ]);
        add(&[
            "cargo",
            "+stable",
            "clippy",
            "--manifest-path",
            "crates/fux/tests/verify/fixture-child/Cargo.toml",
            "--locked",
            "--",
            "-D",
            "warnings",
        ]);
        add(&[
            "cargo",
            "+stable",
            "test",
            "--manifest-path",
            "crates/fux/tests/verify/fixture-child/Cargo.toml",
            "--locked",
            "--",
            "--test-threads=1",
        ]);
        add(&[
            "cargo",
            "+stable",
            "doc",
            "--workspace",
            "--no-deps",
            "--locked",
        ]);
        add(&[
            "cargo",
            "+stable",
            "doc",
            "-p",
            "zor",
            "--no-deps",
            "--all-features",
            "--locked",
        ]);
        for package in ["fux", "zor"] {
            add(&[
                "cargo",
                "+stable",
                "package",
                "--locked",
                "--allow-dirty",
                "-p",
                package,
                "-p",
                "local-ipc",
            ]);
        }
        add(&["node", "crates/zor/tools/test_opencode_adapter.mjs"]);
        for check in [
            "verify-opencode-integration",
            "verify-opencode-resume",
            "verify-zor-resume",
            "verify-lifecycle",
            "verify-codex-authenticated",
            "verify-claude-authenticated",
            "verify-opencode-events",
        ] {
            add(&["@ZOR_XTASK@", check]);
        }
        for check in [
            "verify-detection-screens",
            "verify-controller-setup",
            "verify-resources",
            "verify-capture-traffic",
            "verify-workflow",
            "verify-detection-freshness",
            "verify-headless-performance",
        ] {
            add(&["@XTASK@", check]);
        }
    } else {
        add(&[
            "cargo", "+stable", "test", "-p", "fux", "--locked", "--lib", "client::",
        ]);
        add(&[
            "cargo",
            "+stable",
            "test",
            "-p",
            "zor",
            "--locked",
            "--lib",
            "tasks::lifecycle",
        ]);
        add(&[
            "cargo",
            "+stable",
            "test",
            "-p",
            "fux",
            "--locked",
            "--test",
            "agent_boundary",
            "--test",
            "protocol_consumers",
            "--test",
            "fixtures",
        ]);
        for scenario in [
            "viewer-history-controls",
            "viewer-modals",
            "viewer-gestures",
            "viewer-transfer-input",
            "viewer-tiny-layout",
            "zor-launch",
            "zor-resume",
            "zor-tasks",
            "zor-recovery",
            "zor-worktree",
        ] {
            add(&["@XTASK@", "scenario", scenario, "@FUX@", "@ZOR@"]);
        }
    }
    commands
}

pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(
        args.len() == 1 && matches!(args[0].as_str(), "targeted" | "full"),
        "verify-codebase <targeted|full>"
    );
    let full = args[0] == "full";
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let output = tempfile::Builder::new()
        .prefix("fux-codebase-gate-")
        .tempdir()?
        .keep();
    println!("codebase gate evidence: {}", output.display());
    let target = std::path::absolute(
        std::env::var_os("CARGO_TARGET_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| root.join("target")),
    )?;
    let harness = root.join("target/rust-harness");
    let fixtures = root.join("target/rust-zor-fixtures");
    let replacements = BTreeMap::from([
        ("@HARNESS@", harness.clone()),
        ("@FIXTURES@", fixtures.clone()),
        ("@FUX@", target.join("debug/fux")),
        ("@ZOR@", target.join("debug/zor")),
        ("@XTASK@", harness.join("debug/fux-xtask")),
        ("@ZOR_XTASK@", fixtures.join("debug/zor-xtask")),
    ]);
    let commands = plan(full)
        .into_iter()
        .map(|args| {
            args.into_iter()
                .map(|arg| {
                    replacements
                        .get(arg.as_str())
                        .map_or(arg.clone(), |path| path.display().to_string())
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut environment = BTreeMap::new();
    for name in [
        "PATH",
        "HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "TMPDIR",
        "SDKROOT",
        "DEVELOPER_DIR",
        "FUX_BETAMAX_FONT",
    ] {
        if let Ok(value) = std::env::var(name) {
            environment.insert(name.to_owned(), value);
        }
    }
    for (name, value) in [
        ("CARGO_TARGET_DIR", target.display().to_string()),
        ("RUSTUP_TOOLCHAIN", "stable".into()),
        ("LC_ALL", "C".into()),
        ("TERM", "dumb".into()),
        ("FUX_BIN", target.join("debug/fux").display().to_string()),
        ("ZOR_BIN", target.join("debug/zor").display().to_string()),
        ("FUX_REQUIRE_ZOR_BIN", "1".into()),
        (
            "FUX_BETAMAX_DIR",
            output.join("betamax").display().to_string(),
        ),
        (
            "FUX_HARNESS_ARTIFACTS",
            output.join("failures").display().to_string(),
        ),
    ] {
        environment.insert(name.into(), value);
    }
    let before = fux_xtask::support::failure::source_identity()?;
    let mut record = json!({"profile":args[0], "source_before":before, "commands":commands, "checks":[], "complete":false});
    let save = |record: &Value| -> Result<()> {
        use std::io::Write;
        let mut temporary = tempfile::NamedTempFile::new_in(&output)?;
        serde_json::to_writer_pretty(&mut temporary, record)?;
        temporary.write_all(b"\n")?;
        temporary.persist(output.join("gate.json"))?;
        Ok(())
    };
    save(&record)?;
    let cancellation = Cancellation::install()?;
    let execution = (|| -> Result<()> {
        for (index, args) in commands.iter().enumerate() {
            println!("codebase check {index}: {}", args.join(" "));
            let (program, args) = args.split_first().context("empty gate command")?;
            let mut child = Command::new(program);
            child
                .args(args)
                .current_dir(&root)
                .env_clear()
                .envs(&environment);
            record["running"] = json!(index);
            save(&record)?;
            let outcome = gate_process::run(
                child,
                &output.join(format!("{index:03}")),
                Duration::from_secs(1800),
                16 * 1024 * 1024,
                &cancellation.flag,
            )?;
            let passed = outcome.state == State::Passed;
            record["checks"]
                .as_array_mut()
                .context("checks")?
                .push(serde_json::to_value(outcome)?);
            record["running"] = Value::Null;
            save(&record)?;
            ensure!(
                passed,
                "codebase check {index} failed; see {}",
                output.display()
            );
        }
        Ok(())
    })();
    // Produce reviewable images even when a scenario failed. A report failure is
    // also a gate failure; capture alone is never treated as visual approval.
    let report = if output.join("betamax").exists() {
        let mut command = Command::new(harness.join("debug/fux-xtask"));
        command
            .arg("betamax-report")
            .arg(output.join("betamax"))
            .current_dir(&root)
            .env_clear()
            .envs(&environment);
        gate_process::run(
            command,
            &output.join("render"),
            Duration::from_secs(1800),
            16 * 1024 * 1024,
            &cancellation.flag,
        )
        .and_then(|outcome| {
            let passed = outcome.state == State::Passed;
            record["render"] = serde_json::to_value(outcome)?;
            ensure!(passed, "Betamax report failed");
            Ok(())
        })
    } else {
        Err(anyhow::anyhow!(
            "gate produced no Betamax captures; evidence: {}",
            output.display()
        ))
    };
    record["source_after"] = fux_xtask::support::failure::source_identity()?;
    let unchanged = record["source_before"] == record["source_after"];
    record["complete"] = json!(execution.is_ok() && report.is_ok() && unchanged);
    save(&record)?;
    execution?;
    report?;
    ensure!(
        unchanged,
        "source changed during gate; results do not verify the new source"
    );
    println!(
        "PASS codebase checks; manual image review and release performance comparison remain separate: {}",
        output.display()
    );
    Ok(())
}
