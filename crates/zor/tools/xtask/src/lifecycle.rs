//! Historical lifecycle validation retains capture provenance and negative cases.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

pub fn validate(value: &Value) -> Result<()> {
    ensure!(
        value["server_exit"] == 0 && value["input_sent"] == false,
        "cleanup or input claim"
    );
    let stages = value["stages"].as_array().context("stages")?;
    ensure!(
        stages
            .iter()
            .map(|s| s["name"].as_str())
            .collect::<Vec<_>>()
            == vec![Some("initial"), Some("narrow"), Some("restored")],
        "lifecycle stage order"
    );
    let captures = stages
        .iter()
        .map(|s| s["captures"].as_array().context("stage captures"))
        .collect::<Result<Vec<_>>>()?;
    let first = captures[0].last().context("initial capture")?;
    let narrow = captures[1].last().context("narrow capture")?;
    let restored = captures[2].last().context("restored capture")?;
    let first_rows = first.get("rows").context("initial rows")?;
    let narrow_rows = narrow.get("rows").context("narrow rows")?;
    let restored_rows = restored.get("rows").context("restored rows")?;
    ensure!(
        first["columns"].as_u64().context("initial columns")?
            > narrow["columns"].as_u64().context("narrow columns")?
            && first_rows == narrow_rows,
        "resize not observed"
    );
    ensure!(
        restored_rows == first_rows && restored["columns"] == first["columns"],
        "viewport not restored"
    );
    ensure!(
        captures[0]
            .iter()
            .any(|c| c["text"].as_str().is_some_and(|t| !t.is_empty())),
        "no visible initial output"
    );
    let instance = value.get("instance").context("instance")?;
    let pane = value
        .get("pane")
        .and_then(|p| p.get("id"))
        .context("pane id")?;
    for (stage, captures) in stages.iter().zip(captures) {
        ensure!(
            captures.len() <= 32
                && stage.get("instance") == Some(instance)
                && stage.get("pane") == Some(pane),
            "capture bound or target changed"
        );
        ensure!(
            captures.iter().all(|c| c["input_sequence"] == 0),
            "input replayed"
        );
    }
    let final_record = value.get("final").context("final record")?;
    ensure!(
        final_record.get("pane") == Some(pane) && final_record["input_sequence"] == 0,
        "final target or input changed"
    );
    ensure!(
        final_record["command"] == json!([value.get("agent").context("agent")?]),
        "final argv changed"
    );
    ensure!(
        value["exit_trigger"] == "explicit fux kill; not natural completion",
        "unsupported natural completion claim"
    );
    Ok(())
}

fn provenance(value: &Value, startup: &Value, hash: &str) -> Result<()> {
    ensure!(
        value["harness_sha256"] == hash,
        "historical lifecycle source differs"
    );
    ensure!(
        value
            .get("binary_sha256")
            .context("lifecycle binary hash")?
            == startup
                .get("binary_sha256")
                .context("startup binary hash")?,
        "binary provenance differs"
    );
    Ok(())
}
pub fn regressions() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("zor root")?;
    // These bytes produced the retained traces; a new Rust implementation is not their provenance.
    let historical = fs::read(root.join("tools/archive/capture_lifecycle.py.txt"))?;
    let hash = format!("{:x}", Sha256::digest(&historical));
    for agent in ["claude-2.1.263", "codex-0.153.4", "opencode-1.18.29"] {
        let directory = root.join("tests/fixtures/agents").join(agent);
        let value: Value = serde_json::from_slice(&fs::read(directory.join("lifecycle.json"))?)?;
        let startup: Value = serde_json::from_slice(&fs::read(directory.join("startup.json"))?)?;
        provenance(&value, &startup, &hash)?;
        let mut missing_hash = value.clone();
        missing_hash
            .as_object_mut()
            .context("evidence object")?
            .remove("binary_sha256");
        let mut missing_startup_hash = startup.clone();
        missing_startup_hash
            .as_object_mut()
            .context("startup object")?
            .remove("binary_sha256");
        ensure!(
            provenance(&missing_hash, &missing_startup_hash, &hash).is_err(),
            "accepted absent binary provenance"
        );
        validate(&value)?;
        let mut missing_rows = value.clone();
        for stage in missing_rows["stages"].as_array_mut().context("stages")? {
            stage["captures"]
                .as_array_mut()
                .context("captures")?
                .last_mut()
                .context("final stage capture")?
                .as_object_mut()
                .context("capture object")?
                .remove("rows");
        }
        ensure!(
            validate(&missing_rows).is_err(),
            "accepted missing viewport rows"
        );
        for (field, replacement) in [("columns", json!(80)), ("input_sequence", json!(1))] {
            let mut bad = value.clone();
            let captures = bad["stages"][1]["captures"]
                .as_array_mut()
                .context("captures")?;
            captures.last_mut().context("capture")?[field] = replacement;
            ensure!(validate(&bad).is_err(), "accepted false {field}");
        }
        let mut bad = value.clone();
        bad.as_object_mut()
            .context("evidence object")?
            .remove("final");
        ensure!(validate(&bad).is_err(), "accepted missing final");
        let mut bad = value.clone();
        bad["exit_trigger"] = "natural successful completion".into();
        ensure!(validate(&bad).is_err(), "accepted natural exit");
        let mut bad = value.clone();
        bad["stages"][1]["instance"] = "foreign".into();
        ensure!(validate(&bad).is_err(), "accepted foreign target");
        let mut bad = value.clone();
        bad["server_exit"] = 1.into();
        ensure!(validate(&bad).is_err(), "accepted failed cleanup");
    }
    println!(
        "PASS historical lifecycle provenance, real resize/release and all misleading-evidence mutations"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn retained_traces_and_all_original_negative_cases() -> anyhow::Result<()> {
        super::regressions()
    }
}
