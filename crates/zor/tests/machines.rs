//! Real CLI profile lifecycle: configuration must not accidentally become local task routing.
#![cfg(feature = "cli")]
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{path::Path, process::Command};

fn command(root: &Path, args: &[&str], success: bool) -> Result<Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_zor"))
        .arg("--machines-file")
        .arg(root.join("config/machines.json"))
        .arg("--state-directory")
        .arg(root.join("must-not-open"))
        .args(args)
        .output()?;
    ensure!(
        output.status.success() == success,
        "unexpected CLI result: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        !root.join("must-not-open").exists(),
        "profile operation touched task state"
    );
    if success {
        Ok(serde_json::from_slice(&output.stdout)?)
    } else {
        Ok(Value::Null)
    }
}

fn field<'a>(value: &'a Value, pointer: &str) -> Result<&'a Value> {
    value
        .pointer(pointer)
        .with_context(|| format!("missing {pointer}"))
}

#[test]
fn saved_profiles_keep_stable_identity_and_distinct_grants_across_cli_processes() -> Result<()> {
    let root = tempfile::tempdir()?;
    let list = command(root.path(), &["machine", "list"], true)?;
    ensure!(field(&list, "/local/id")? == "local", "Local missing");
    let endpoint = "a".repeat(64);
    let viewer = "b".repeat(64);
    let first = command(
        root.path(),
        &[
            "machine",
            "add",
            "alpha",
            "--endpoint",
            &endpoint,
            "--key-file",
            "/private/control",
        ],
        true,
    )?;
    let id = field(&first, "/id")?.as_str().context("stable ID")?;
    command(root.path(), &["machine", "add", "beta"], true)?;
    let renamed = command(
        root.path(),
        &["machine", "rename", "alpha", "builder"],
        true,
    )?;
    ensure!(
        field(&renamed, "/id")? == id,
        "rename changed identity or returned another machine"
    );
    command(
        root.path(),
        &[
            "machine",
            "bind",
            id,
            "--workspace",
            "other",
            "--endpoint",
            &viewer,
            "--key-file",
            "/private/viewer",
        ],
        true,
    )?;
    let profile = command(root.path(), &["machine", "inspect", "builder"], true)?;
    ensure!(
        field(&profile, "/control/endpoint")?.as_str() == Some(endpoint.as_str())
            && field(&profile, "/attachments/other/endpoint")?.as_str() == Some(viewer.as_str()),
        "grants changed"
    );
    command(root.path(), &["machine", "control", id, "--clear"], true)?;
    let profile = command(root.path(), &["machine", "inspect", id], true)?;
    ensure!(
        field(&profile, "/control")?.is_null()
            && field(&profile, "/attachments/other/endpoint")?.as_str() == Some(viewer.as_str()),
        "clearing control changed attachment grant"
    );
    command(root.path(), &["machine", "remove", "builder"], true)?;
    command(root.path(), &["machine", "inspect", id], false)?;
    ensure!(
        field(
            &command(root.path(), &["machine", "list"], true)?,
            "/machines"
        )?
        .as_array()
        .context("machines")?
        .len()
            == 1,
        "remove affected another machine"
    );
    Ok(())
}

#[test]
fn invalid_or_unknown_profile_commands_fail_without_local_fallback() -> Result<()> {
    let root = tempfile::tempdir()?;
    command(root.path(), &["machine", "add", "Local"], false)?;
    command(root.path(), &["machine", "inspect", "missing"], false)?;
    command(root.path(), &["machine", "add", "alpha"], true)?;
    command(root.path(), &["machine", "add", "ALPHA"], false)?;
    command(root.path(), &["machine", "control", "alpha"], false)?;
    command(
        root.path(),
        &[
            "machine",
            "control",
            "alpha",
            "--endpoint",
            "bad",
            "--key-file",
            "/private/key",
        ],
        false,
    )?;
    command(
        root.path(),
        &[
            "machine",
            "bind",
            "alpha",
            "--workspace",
            "../escape",
            "--endpoint",
            &"b".repeat(64),
            "--key-file",
            "/private/key",
        ],
        false,
    )?;
    ensure!(
        *field(
            &command(root.path(), &["machine", "inspect", "alpha"], true)?,
            "/attachments"
        )? == serde_json::json!({}),
        "failed edit changed profile"
    );
    Ok(())
}

#[test]
fn scoped_dashboard_rejects_invalid_routing_before_starting_helpers_or_local_services() -> Result<()>
{
    let root = tempfile::tempdir()?;
    for (args, diagnostic) in [
        (vec!["--machine", "missing", "dashboard"], "unknown machine"),
        (
            vec![
                "--machine",
                "missing",
                "dashboard",
                "--directory",
                "/local/override",
            ],
            "cannot select a local --directory",
        ),
        (
            vec!["--machine", "local", "dashboard"],
            "requires a terminal",
        ),
        (
            vec!["--machine", "local", "dashboard", "--bell"],
            "requires a terminal",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_zor"))
            .env_clear()
            .env("HOME", root.path())
            .env("XDG_RUNTIME_DIR", root.path().join("runtime"))
            .arg("--machines-file")
            .arg(root.path().join("config/machines.json"))
            .arg("--koh-binary")
            .arg(root.path().join("must-not-start-koh"))
            .args(args)
            .output()?;
        ensure!(!output.status.success(), "invalid dashboard route accepted");
        ensure!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        ensure!(
            !root.path().join("runtime").exists(),
            "dashboard started a local service"
        );
    }
    Ok(())
}
