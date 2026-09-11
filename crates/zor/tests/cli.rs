#![cfg(feature = "cli")]

use std::{fs, os::unix::fs::PermissionsExt as _, process::Command};

fn temp_dir(label: &str) -> Result<std::path::PathBuf, std::io::Error> {
    let path = std::env::temp_dir().join(format!("zor-{label}-{}", std::process::id()));
    fs::create_dir_all(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

#[test]
fn capabilities_are_read_only_and_unknown_agents_fail() -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_dir("capabilities")?;
    for agent in ["codex", "claude", "opencode", "unknown"] {
        let output = Command::new(env!("CARGO_BIN_EXE_zor"))
            .env_clear()
            .env("HOME", &root)
            .args(["task", "adapter-capabilities", "--agent", agent])
            .output()?;
        if agent == "unknown" {
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported-agent:"));
        } else {
            assert!(output.status.success());
            let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            assert_eq!(
                value.get("agent").and_then(serde_json::Value::as_str),
                Some(agent)
            );
            assert_eq!(
                value
                    .get("availability")
                    .and_then(serde_json::Value::as_str),
                Some("not-probed")
            );
        }
        assert_eq!(fs::read_dir(&root)?.count(), 0);
    }
    fs::remove_dir(root)?;
    Ok(())
}

#[test]
fn check_evaluates_fixture_expectation_and_rule() -> Result<(), Box<dyn std::error::Error>> {
    // Phase Z §8: check validates both expected state and matched rule id.
    let root = temp_dir("check")?;
    let rules = root.join("rules");
    fs::create_dir_all(&rules)?;
    fs::write(
        rules.join("agent.toml"),
        "id='agent'\nprompt_marker='>'\nblock_markers=[]\n[[rules]]\nid='working'\nstate='working'\nregion='whole'\ncontains=['busy']\n",
    )?;
    let fixture = root.join("fixture.txt");
    fs::write(
        &fixture,
        "# agent: agent\n# title: test\n# progress: 3:0\n# expect: working\n# matched: working\nbusy\n",
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_zor"))
        .args([
            "--rules",
            rules.to_string_lossy().as_ref(),
            "check",
            fixture.to_string_lossy().as_ref(),
        ])
        .output()?;
    assert!(
        output.status.success(),
        "status: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"working working\n");
    fs::remove_dir_all(root)?;
    Ok(())
}
