//! Offline comparison checks preserve historical provenance and provider-policy attribution.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
fn field<'a>(value: &'a Value, path: &str) -> Result<&'a Value> {
    value
        .pointer(path)
        .with_context(|| format!("missing evidence {path}"))
}
fn workflow(value: &Value) -> Result<()> {
    let zor = field(value, "/zor")?;
    let herdr = field(value, "/herdr")?;
    ensure!(
        field(zor, "/exit_code")? == 0 && field(zor, "/stderr")? == "",
        "zor workflow failed"
    );
    ensure!(
        field(zor, "/stdout")?
            == "PASS two isolated workers, failed dependency repair, pinned handoff, receipt retry and owned cleanup\n",
        "zor workflow output"
    );
    ensure!(
        field(herdr, "/server_exit")? == 0
            && field(herdr, "/worker_exit_confirmed")? == true
            && field(herdr, "/worker_count")? == 3
            && field(herdr, "/main_preserved")? == true
            && field(herdr, "/owned_trees_removed")? == true,
        "herdr cleanup/ownership"
    );
    let checks = field(herdr, "/external_checks")?
        .as_array()
        .context("external checks")?;
    let actual = checks
        .iter()
        .map(|r| Ok((field(r, "/worker")?.clone(), field(r, "/passed")?.clone())))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        actual == vec![(json!("alpha"), json!(true)), (json!("beta"), json!(false))],
        "failed dependency check omitted"
    );
    ensure!(
        field(herdr, "/external_collected")?
            == &json!({"alpha":"result-alpha","beta":"result-beta"}),
        "repair output"
    );
    ensure!(
        field(herdr, "/external_handoff_capture")?
            .to_string()
            .contains("HANDOFF_RECEIVED result-alpha result-beta"),
        "external handoff missing"
    );
    let error = field(herdr, "/verification_api/error")?;
    ensure!(
        field(error, "/code")? == "invalid_request"
            && field(error, "/message")?
                .as_str()
                .context("error message")?
                .contains("unknown variant `task.verify`"),
        "unrelated error used as policy evidence"
    );
    for name in ["alpha", "beta"] {
        ensure!(
            field(herdr, &format!("/dirty_errors/{name}/error/code"))?
                == "dirty_worktree_requires_force",
            "dirty tree protection"
        );
    }
    let calls = field(herdr, "/transcript")?
        .as_array()
        .context("transcript")?;
    let mut creates = 0;
    let mut removes = 0;
    let mut repaired = false;
    for call in calls {
        let method = field(call, "/method")?;
        let reply = field(call, "/reply")?.as_object().context("RPC reply")?;
        if method == "worktree.create" && !reply.contains_key("error") {
            creates += 1;
        }
        if method == "worktree.remove"
            && field(call, "/params/force")? == true
            && !reply.contains_key("error")
        {
            removes += 1;
        }
        if method == "pane.send_input" && field(call, "/params/text")? == "result-beta\n" {
            repaired = true;
        }
    }
    ensure!(
        creates == 2 && removes == 2 && repaired,
        "missing create/remove/repair transcript"
    );
    Ok(())
}
fn provenance(value: &Value, root: &Path) -> Result<()> {
    for (path, expected) in field(value, "/provenance/sources")?
        .as_object()
        .context("provenance sources")?
    {
        // Exact original scripts are archived; retained captures keep their original paths/hashes.
        let source = match (path.as_str(), expected.as_str().context("source hash")?) {
            (
                "tools/comparisons/workflow.py",
                "217b868447d056f43d705f23d35fa1897842f47d88a02f690e7ca0b25b82b87b",
            )
            | (
                "tests/verify/zor_workflow.py",
                "67d99545444086e875e49539884ed787443d625ccceaa336103fddc525c8700e",
            )
            | (
                "tests/verify/zor_contention.py",
                "2436494f573378559056bd43cf4b2ba6141873aa6ed62e41ac70d7c1f186a189",
            ) => root.join("tools/archive").join(format!("{path}.txt")),
            _ => root.join(path),
        };
        ensure!(
            format!(
                "{:x}",
                Sha256::digest(
                    fs::read(&source).with_context(|| format!("read {}", source.display()))?
                )
            ) == expected.as_str().context("source hash")?,
            "historical source changed: {path}"
        );
    }
    ensure!(
        field(value, "/provenance/herdr_build/binary_sha256")?
            == field(value, "/provenance/binaries/herdr/sha256")?,
        "herdr binary provenance mismatch"
    );
    Ok(())
}
pub fn verify_workflow(args: Vec<String>) -> Result<()> {
    match args.as_slice() {
        [] => workflow_regressions(),
        [path] => {
            let value: Value = serde_json::from_slice(&fs::read(path)?)?;
            provenance(&value, root()?)?;
            workflow(&value)
        }
        _ => anyhow::bail!("usage: verify-workflow [REPORT_JSON]"),
    }
}
pub fn workflow_regressions() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("repository root")?;
    let value: Value =
        serde_json::from_slice(&fs::read(root.join("docs/comparisons/workflow.json"))?)?;
    provenance(&value, root)?;
    workflow(&value)?;
    let mut wrong_source = value.clone();
    wrong_source["provenance"]["sources"]["tests/verify/zor_contention.py"] = "0".repeat(64).into();
    ensure!(
        provenance(&wrong_source, root).is_err(),
        "wrong historical helper hash accepted"
    );
    for mutation in 0..4 {
        let mut invalid = value.clone();
        match mutation {
            0 => invalid["herdr"]["external_checks"][1]["passed"] = true.into(),
            1 => invalid["herdr"]["external_collected"]["beta"] = "incorrect-beta".into(),
            2 => {
                invalid["herdr"]["verification_api"]["error"]["message"] =
                    "server unavailable".into()
            }
            _ => invalid["herdr"]["worker_exit_confirmed"] = false.into(),
        }
        ensure!(
            workflow(&invalid).is_err(),
            "workflow mutation {mutation} accepted"
        );
    }
    println!(
        "PASS historical workflow provenance, failed-check repair, policy attribution and owned cleanup"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn workflow() {
        super::workflow_regressions().unwrap();
    }
    #[test]
    fn fresh_captures_validate_current_sources_and_reject_wrong_hashes() -> anyhow::Result<()> {
        use super::*;
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .context("repository root")?;
        let mut value: Value =
            serde_json::from_slice(&fs::read(root.join("docs/comparisons/workflow.json"))?)?;
        let path = "tools/xtask/src/workflow_capture.rs";
        value["provenance"]["sources"] =
            json!({path: format!("{:x}", Sha256::digest(fs::read(root.join(path))?))});
        provenance(&value, root)?;
        value["provenance"]["sources"][path] = "0".repeat(64).into();
        ensure!(
            provenance(&value, root).is_err(),
            "wrong current source hash accepted"
        );
        Ok(())
    }
}

mod resources;
mod setup;
mod traffic;
pub use resources::{regressions as resource_regressions, validate as validate_resource_case};
pub use setup::validate as validate_setup_case;
pub use setup::verify as verify_setup;
pub use traffic::verify as verify_traffic;
pub use traffic::{summarize as summarize_traffic, validate as validate_traffic_case};
fn root() -> Result<&'static Path> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("repository root")
}
fn report(name: &str) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(
        root()?
            .join("docs/comparisons")
            .join(format!("{name}.json")),
    )?)?)
}
fn number(value: &Value, path: &str) -> Result<f64> {
    field(value, path)?.as_f64().context("number")
}
fn integer(value: &Value, path: &str) -> Result<u64> {
    field(value, path)?.as_u64().context("integer")
}
fn array<'a>(value: &'a Value, path: &str) -> Result<&'a Vec<Value>> {
    field(value, path)?.as_array().context("array")
}
fn verify_hash(path: &str, expected: &Value) -> Result<()> {
    let source = if path == "tools/comparisons/prompt_boundary.py"
        && expected == "3a29778f9717501d45edf8968e28e7124f913eb3df0d5e6d2c794eef057a2d89"
    {
        root()?.join("tools/archive/tools/comparisons/prompt_boundary.py.txt")
    } else if path == "tools/comparisons/resources.py"
        && expected == "ea33ab75e5770f2234f02652e1bfea42f5c05a064adc2e943b8a72242d0c7209"
    {
        root()?.join("tools/archive/tools/comparisons/resources.py.txt")
    } else if path == "tools/comparisons/controller_setup.py"
        && expected == "81e1f8aa218cc6dc34a51ad943e768b268722f0fd916eb93c0805332e96e2cf8"
    {
        root()?.join("tools/archive/tools/comparisons/controller_setup.py.txt")
    } else if path == "tools/comparisons/capture_traffic.py"
        && expected == "020c831a57021cf88203dd301aae5815e41e141ade29d859d240e3683747dbb1"
    {
        root()?.join("tools/archive/tools/comparisons/capture_traffic.py.txt")
    } else if path == "tools/comparisons/detection_freshness.py"
        && expected == "f3b9e812a12a7e188587f2895ab5e48be435b3c9b6e797577600c1c7cca6971d"
    {
        root()?.join("tools/archive/tools/comparisons/detection_freshness.py.txt")
    } else if path == "tools/comparisons/detection_screens.py"
        && expected == "b6ba5464871932b881fff5832658eccd6bc1551d05b31afb31cb54c37be7500d"
    {
        root()?.join("tools/archive/tools/comparisons/detection_screens.py.txt")
    } else if path == "zor/src/main.rs"
        && expected == "23b0a94f3ca7249425afc8ba95d64536e042b4da791370e328efc39c3191a534"
    {
        root()?.join("tools/archive/zor/src/main.rs.txt")
    } else {
        root()?.join(path)
    };
    ensure!(
        format!("{:x}", Sha256::digest(fs::read(source)?)) == expected.as_str().context("hash")?,
        "source hash mismatch: {path}"
    );
    Ok(())
}
fn verify_sources(value: &Value, path: &str) -> Result<()> {
    for (source, hash) in field(value, path)?.as_object().context("sources")? {
        verify_hash(source, hash)?;
    }
    Ok(())
}
mod freshness;
pub use freshness::verify as verify_freshness;
mod screens;
pub use screens::capture as capture_screens;
pub use screens::verify as verify_screens;
