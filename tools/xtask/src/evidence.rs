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
        let source = root.join(path);
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
// Only retained regression checks opt into archived sources. Explicit report validation
// continues to require current source bytes, even when old hashes are known.
fn historical_workflow_provenance(value: &Value, root: &Path) -> Result<()> {
    let mut archived = value.clone();
    let sources = field(value, "/provenance/sources")?
        .as_object()
        .context("sources")?;
    let mut mapped = serde_json::Map::new();
    for (path, hash) in sources {
        ensure!(
            matches!(
                path.as_str(),
                "tools/comparisons/workflow.py"
                    | "tests/verify/zor_workflow.py"
                    | "tests/verify/zor_contention.py"
            ),
            "unexpected historical workflow source: {path}"
        );
        mapped.insert(
            format!("tools/archive/native-evidence/sources/{path}.txt"),
            hash.clone(),
        );
    }
    ensure!(mapped.len() == 3, "missing historical workflow sources");
    archived["provenance"]["sources"] = Value::Object(mapped);
    provenance(&archived, root)
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
    let value: Value = serde_json::from_slice(&fs::read(
        root.join("tools/archive/native-evidence/docs/comparisons/workflow.json"),
    )?)?;
    historical_workflow_provenance(&value, root)?;
    workflow(&value)?;
    let mut wrong_source = value.clone();
    wrong_source["provenance"]["sources"]["tests/verify/zor_contention.py"] = "0".repeat(64).into();
    ensure!(
        historical_workflow_provenance(&wrong_source, root).is_err(),
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
    fn historical_workflow_is_not_current_evidence() -> anyhow::Result<()> {
        use super::*;
        let root = root()?;
        let manifest: Value = serde_json::from_slice(&fs::read(
            root.join("tools/archive/native-evidence/manifest.json"),
        )?)?;
        for kind in ["reports", "sources"] {
            for entry in manifest[kind].as_array().context("archive entries")? {
                let path = entry["archive_path"].as_str().context("archive path")?;
                ensure!(
                    format!("{:x}", Sha256::digest(fs::read(root.join(path))?))
                        == entry["sha256"].as_str().context("archive hash")?,
                    "archive changed: {path}"
                );
            }
        }
        let mut value: Value = serde_json::from_slice(&fs::read(
            root.join("tools/archive/native-evidence/docs/comparisons/workflow.json"),
        )?)?;
        historical_workflow_provenance(&value, root)?;
        ensure!(
            provenance(&value, root).is_err(),
            "historical archive accepted as current source evidence"
        );
        value["provenance"]["sources"]
            .as_object_mut()
            .context("sources")?
            .remove("tests/verify/zor_contention.py");
        ensure!(
            historical_workflow_provenance(&value, root).is_err(),
            "missing archived source accepted"
        );
        Ok(())
    }
    #[test]
    fn fresh_captures_validate_current_sources_and_reject_wrong_hashes() -> anyhow::Result<()> {
        use super::*;
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .context("repository root")?;
        let mut value: Value = serde_json::from_slice(&fs::read(
            root.join("tools/archive/native-evidence/docs/comparisons/workflow.json"),
        )?)?;
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
/// Retained regression inputs are selected explicitly; current validators never use this.
fn retained_report(name: &str) -> Result<Value> {
    retained_document(&format!("docs/comparisons/{name}.json"))
}
pub(crate) fn retained_document(original: &str) -> Result<Value> {
    let manifest: Value = serde_json::from_slice(&fs::read(
        root()?.join("tools/archive/native-evidence/manifest.json"),
    )?)?;
    let entry = manifest["reports"]
        .as_array()
        .context("retained reports")?
        .iter()
        .find(|entry| entry["original_path"] == original)
        .context("unregistered retained report")?;
    let bytes = fs::read(root()?.join(entry["archive_path"].as_str().context("archive path")?))?;
    ensure!(
        format!("{:x}", Sha256::digest(&bytes))
            == entry["sha256"].as_str().context("report hash")?,
        "retained report changed: {original}"
    );
    Ok(serde_json::from_slice(&bytes)?)
}
fn retained_sources(name: &str, value: &Value, pointer: &str) -> Result<()> {
    let original = retained_report(name)?;
    ensure!(
        field(value, pointer)? == field(&original, pointer)?,
        "retained source inventory changed: {name}"
    );
    for (path, hash) in field(value, pointer)?
        .as_object()
        .context("retained sources")?
    {
        retained_hash(path, hash)?;
    }
    Ok(())
}
pub(crate) fn retained_hash(path: &str, hash: &Value) -> Result<()> {
    retained_bytes(path, hash).map(|_| ())
}
fn retained_bytes(path: &str, hash: &Value) -> Result<Vec<u8>> {
    let manifest: Value = serde_json::from_slice(&fs::read(
        root()?.join("tools/archive/native-evidence/manifest.json"),
    )?)?;
    let entry = manifest["sources"]
        .as_array()
        .context("source entries")?
        .iter()
        .find(|entry| entry["original_path"] == path && entry["sha256"] == *hash)
        .with_context(|| format!("unregistered retained source: {path}"))?;
    let bytes = fs::read(root()?.join(entry["archive_path"].as_str().context("archive path")?))?;
    ensure!(
        format!("{:x}", Sha256::digest(&bytes)) == hash.as_str().context("source hash")?,
        "retained source changed: {path}"
    );
    Ok(bytes)
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
    let source = root()?.join(path);
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
