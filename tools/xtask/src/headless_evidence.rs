//! Offline integrity of retained headless viewer and journal measurements.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};
fn f<'a>(v: &'a Value, p: &str) -> Result<&'a Value> {
    v.pointer(p).with_context(|| format!("missing {p}"))
}
fn a<'a>(v: &'a Value, p: &str) -> Result<&'a Vec<Value>> {
    f(v, p)?.as_array().context("array")
}
fn n(v: &Value, p: &str) -> Result<f64> {
    f(v, p)?.as_f64().context("number")
}
fn u(v: &Value, p: &str) -> Result<u64> {
    f(v, p)?.as_u64().context("nonnegative integer counter")
}
fn eq(v: &Value, p: &str, w: Value) -> Result<()> {
    ensure!(f(v, p)? == &w, "mismatch {p}");
    Ok(())
}
fn keys(v: &Value) -> Result<BTreeSet<&str>> {
    Ok(v.as_object()
        .context("object")?
        .keys()
        .map(String::as_str)
        .collect())
}
fn hash(root: &Path, path: &str, expected: &Value) -> Result<()> {
    let wanted = expected.as_str().context("hash")?;
    ensure!(
        format!("{:x}", Sha256::digest(fs::read(root.join(path))?)) == wanted,
        "source hash mismatch: {path}"
    );
    Ok(())
}
pub fn validate(root: &Path, r: &Value) -> Result<()> {
    validate_with_sources(root, r, false)
}
fn validate_with_sources(root: &Path, r: &Value, historical: bool) -> Result<()> {
    let hash = |root: &Path, path: &str, expected: &Value| {
        if historical {
            crate::evidence::retained_hash(path, expected)
        } else {
            hash(root, path, expected)
        }
    };
    eq(r, "/schema", json!(1))?;
    eq(r, "/synthetic", json!(true))?;
    eq(r, "/geometry", json!([80, 24]))?;
    let rust = r.pointer("/provenance/harness_kind") == Some(&json!("rust-headless-performance"));
    let repetitions = if rust {
        let value = u(r, "/repetitions")?;
        ensure!((1..=5).contains(&value), "repetition bounds");
        value
    } else {
        3
    };
    hash(
        root,
        if rust {
            "tools/xtask/src/headless_performance.rs"
        } else {
            "tools/headless_performance.py"
        },
        f(r, "/provenance/harness_sha256")?,
    )?;
    if rust {
        let expected_paths = BTreeSet::from([
            "tools/xtask/src/headless_performance.rs",
            "tools/xtask/src/performance-worker.c",
            "tools/xtask/src/support/local.rs",
            "tools/xtask/src/support/attachment.rs",
            "tools/xtask/src/support/process.rs",
        ]);
        ensure!(
            keys(f(r, "/provenance/sources")?)? == expected_paths,
            "capture source inventory"
        );
        for (path, expected) in f(r, "/provenance/sources")?
            .as_object()
            .context("sources")?
        {
            hash(root, path, expected)?;
        }
    }
    let rows = a(r, "/results")?;
    ensure!(rows.len() as u64 == 4 * repetitions, "matrix size");
    let mut actual = BTreeSet::new();
    for row in rows {
        actual.insert((
            f(row, "/panes")?.as_u64().context("integer panes")?,
            f(row, "/viewers")?.as_u64().context("integer viewers")?,
            f(row, "/slow")?.as_bool().context("slow")?,
            f(row, "/repetition")?
                .as_u64()
                .context("integer repetition")?,
        ));
    }
    let expected = [(1, 1, false), (1, 4, false), (4, 4, false), (4, 4, true)]
        .into_iter()
        .flat_map(|(p, v, s)| (1..=repetitions).map(move |r| (p, v, s, r)))
        .collect::<BTreeSet<_>>();
    ensure!(actual == expected, "matrix");
    let calibration = r.get("calibration").filter(|v| !v.is_null());
    if let Some(c) = calibration {
        ensure!((0.95..=1.05).contains(&n(c, "/ratio")?), "calibration");
        hash(
            root,
            "tools/comparisons/resource_sampler.c",
            f(r, "/provenance/sampler_sha256")?,
        )?;
    }
    for row in rows {
        eq(row, "/passed", json!(true))?;
        eq(row, "/worker_exit_confirmed", json!(true))?;
        eq(row, "/owner_exits", json!({"fux":0,"zor":0}))?;
        eq(row, "/geometry", json!([80, 24]))?;
        ensure!(
            a(row, "/pane_rects")?.len() as u64 == u(row, "/panes")?,
            "pane geometry count"
        );
        let phases = a(row, "/phases")?;
        ensure!(
            phases
                .iter()
                .map(|p| f(p, "/name"))
                .collect::<Result<Vec<_>>>()?
                == vec![&json!("idle"), &json!("B01"), &json!("S01")],
            "phases"
        );
        let mut previous: Option<&Value> = None;
        let mut identities = BTreeMap::new();
        for phase in phases {
            ensure!(
                n(phase, "/workload_ms")? > 0. && n(phase, "/workload_ms")? < 12000.,
                "workload bounds"
            );
            if f(phase, "/name")? == "idle" {
                eq(phase, "/visible_ms", Value::Null)?;
            } else {
                ensure!(
                    f(phase, "/visible_ms")? == f(phase, "/workload_ms")?,
                    "visibility latency"
                );
            }
            for sample in [f(phase, "/before")?, f(phase, "/after")?] {
                let owners = f(sample, "/owners")?;
                ensure!(keys(owners)? == BTreeSet::from(["fux", "zor"]), "owner set");
                ensure!(
                    a(sample, "/viewers")?.len() as u64 == u(row, "/viewers")?,
                    "viewer count"
                );
                if let Some(p) = previous {
                    ensure!(
                        n(sample, "/monotonic")? > n(p, "/monotonic")?,
                        "sample ordering"
                    );
                }
                for owner in ["fux", "zor"] {
                    let v = f(owners, &format!("/{owner}"))?;
                    if v.is_null() {
                        ensure!(calibration.is_none(), "missing calibrated owner");
                        continue;
                    }
                    let c = calibration.context("unexpected measured owner")?;
                    ensure!(u(v, "/pid")? > 1, "owner PID");
                    let identity = (f(v, "/pid")?.clone(), f(v, "/start_abstime")?.clone());
                    ensure!(
                        identities.entry(owner).or_insert(identity.clone()) == &identity,
                        "owner replaced"
                    );
                    for key in ["timebase_numer", "timebase_denom"] {
                        ensure!(
                            f(v, &format!("/{key}"))? == f(c, &format!("/{key}"))?
                                && u(v, &format!("/{key}"))? > 0,
                            "timebase"
                        );
                    }
                    for key in ["rss_bytes", "footprint_bytes", "user_ticks", "system_ticks"] {
                        u(v, &format!("/{key}"))?;
                    }
                    if let Some(p) = previous {
                        for key in ["user_ticks", "system_ticks"] {
                            ensure!(
                                u(v, &format!("/{key}"))?
                                    >= u(p, &format!("/owners/{owner}/{key}"))?,
                                "decreasing CPU"
                            );
                        }
                    }
                }
                for (i, v) in a(sample, "/viewers")?.iter().enumerate() {
                    ensure!(
                        u(v, "/received_bytes")? >= u(v, "/decoded_frame_bytes")?
                            && u(v, "/decoded_frame_bytes")? > 0
                            && u(v, "/frames")? > 0
                            && u(v, "/pending_high_water_bytes")? > 0,
                        "viewer counters"
                    );
                    if let Some(p) = previous {
                        for key in keys(v)? {
                            ensure!(
                                u(v, &format!("/{key}"))? >= u(p, &format!("/viewers/{i}/{key}"))?,
                                "decreasing viewer counter"
                            );
                        }
                    }
                }
                previous = Some(sample);
            }
        }
    }
    Ok(())
}
pub fn verify(args: Vec<String>) -> Result<()> {
    match args.as_slice() {
        [] => regressions(),
        [path] => {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(Path::parent)
                .context("root")?;
            validate(root, &serde_json::from_slice(&fs::read(path)?)?)?;
            println!(
                "PASS headless viewer capture provenance, matrix, raw counters, identity and cleanup"
            );
            Ok(())
        }
        _ => anyhow::bail!("usage: verify-headless-performance [REPORT_JSON]"),
    }
}
pub fn regressions() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("root")?;
    let read = |name: &str| {
        crate::evidence::retained_document(&format!("docs/headless-performance/{name}"))
    };
    let hash =
        |_root: &Path, path: &str, expected: &Value| crate::evidence::retained_hash(path, expected);
    let validate = |root: &Path, value: &Value| validate_with_sources(root, value, true);
    let before = read("fux-before.json")?;
    let after = read("fux-after-sharing.json")?;
    validate(root, &before)?;
    validate(root, &after)?;
    for p in ["/platform", "/provenance/zor_sha256"] {
        ensure!(f(&before, p)? == f(&after, p)?, "paired {p}");
    }
    ensure!(
        f(&before, "/provenance/fux_sha256")? != f(&after, "/provenance/fux_sha256")?,
        "same fux binary"
    );
    for (b, a) in a(&before, "/results")?.iter().zip(a(&after, "/results")?) {
        for k in ["panes", "viewers", "slow", "repetition", "pane_rects"] {
            ensure!(
                f(b, &format!("/{k}"))? == f(a, &format!("/{k}"))?,
                "paired matrix/geometry"
            );
        }
    }
    for (name, r) in [
        ("baseline-build.json", &before),
        ("sharing-build.json", &after),
    ] {
        let build = read(name)?;
        for owner in ["fux", "zor"] {
            ensure!(
                f(&build, &format!("/binaries/{owner}/sha256"))?
                    == f(r, &format!("/provenance/{owner}_sha256"))?,
                "binary build provenance"
            );
        }
        let commands = a(&build, "/explicit_build/commands")?;
        ensure!(
            commands.first()
                == Some(&json!(
                    "RUSTFLAGS='' CARGO_PROFILE_DEV_OPT_LEVEL=0 CARGO_PROFILE_DEV_DEBUG=2 cargo +1.95.0 build --locked --bin fux"
                )),
            "fux build profile"
        );
        if name == "baseline-build.json" {
            ensure!(
                commands.len() == 2
                    && commands[1]
                        == "RUSTFLAGS='' CARGO_PROFILE_DEV_OPT_LEVEL=0 CARGO_PROFILE_DEV_DEBUG=2 cargo +1.91.0 build --manifest-path zor/Cargo.toml --locked --bin zor",
                "zor build profile"
            );
        } else {
            ensure!(commands.len() == 1, "sharing build profile");
            let sources = f(&build, "/sources")?;
            ensure!(
                keys(sources)?
                    == BTreeSet::from([
                        "Cargo.toml",
                        "Cargo.lock",
                        "src/view.rs",
                        "src/ecs/systems/snapshot.rs"
                    ]),
                "sharing sources"
            );
            for p in keys(sources)? {
                hash(root, p, &sources[p])?;
            }
        }
    }
    for mutation in 0..4 {
        let mut bad = before.clone();
        match mutation {
            0 => {
                bad["results"].as_array_mut().unwrap().pop();
            }
            1 => bad["results"][0]["worker_exit_confirmed"] = json!(false),
            2 => {
                let v =
                    &mut bad["results"][0]["phases"][0]["after"]["owners"]["fux"]["start_abstime"];
                *v = json!(v.as_u64().context("start")? + 1);
            }
            _ => bad["results"][0]["phases"][1]["after"]["viewers"][0]["received_bytes"] = json!(0),
        }
        ensure!(
            validate(root, &bad).is_err(),
            "accepted original mutation {mutation}"
        );
    }
    // Exact ordering must survive counters above f64's integer precision limit.
    let mut large = before.clone();
    for row in large["results"].as_array_mut().context("rows")? {
        for phase in row["phases"].as_array_mut().context("phases")? {
            for point in ["before", "after"] {
                for viewer in phase[point]["viewers"].as_array_mut().context("viewers")? {
                    for value in viewer.as_object_mut().context("viewer")?.values_mut() {
                        *value = json!(9007199254740993u64);
                    }
                }
                for owner in ["fux", "zor"] {
                    for key in ["user_ticks", "system_ticks"] {
                        phase[point]["owners"][owner][key] = json!(9007199254740993u64);
                    }
                }
            }
        }
    }
    validate(root, &large)?;
    for pointer in [
        "/results/0/phases/0/after/viewers/0/received_bytes",
        "/results/0/phases/0/after/viewers/0/frames",
        "/results/0/phases/0/after/owners/fux/user_ticks",
    ] {
        let mut bad = large.clone();
        *bad.pointer_mut(pointer).context("large counter mutation")? = json!(9007199254740992u64);
        ensure!(
            validate(root, &bad).is_err(),
            "rounded away counter decrease: {pointer}"
        );
    }
    let journal = read("journal-baseline.json")?;
    eq(&journal, "/schema", json!(1))?;
    hash(
        root,
        "tools/headless_journal.py",
        f(&journal, "/provenance/harness_sha256")?,
    )?;
    for owner in ["fux", "zor"] {
        let p = format!("/provenance/{owner}_sha256");
        ensure!(f(&journal, &p)? == f(&before, &p)?, "journal binaries");
    }
    let runs = a(&journal, "/runs")?;
    ensure!(runs.len() == 3, "journal repetitions");
    for (i, run) in runs.iter().enumerate() {
        eq(run, "/repetition", json!(i + 1))?;
        eq(run, "/server_exit", json!(0))?;
        eq(run, "/worker_exit_confirmed", json!(true))?;
        eq(run, "/final_generation", json!(32))?;
        let samples = a(run, "/samples")?;
        ensure!(samples.len() == 96, "journal sample count");
        for (i, s) in samples.iter().enumerate() {
            let kind = ["adopt", "inspect", "idempotent-adopt"][i / 32];
            eq(s, "/kind", json!(kind))?;
            ensure!(
                n(s, "/elapsed_ms")? > 0.
                    && n(s, "/elapsed_ms")? < 10000.
                    && n(s, "/child_cpu_ms")? >= 0.
                    && u(s, "/journal_bytes")? > 0
                    && u(s, "/journal_bytes")? <= 4 * 1024 * 1024,
                "journal sample bounds"
            );
            eq(s, "/committed_replacement", json!(kind == "adopt"))?;
            eq(
                s,
                "/logical_committed_bytes",
                if kind == "adopt" {
                    f(s, "/journal_bytes")?.clone()
                } else {
                    json!(0)
                },
            )?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rust_capture_requires_every_source_hash() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .context("root")?;
        // Reuse valid raw measurements only as an in-memory validator fixture.
        let mut fixture =
            crate::evidence::retained_document("docs/headless-performance/fux-before.json")?;
        fixture["provenance"]["harness_kind"] = json!("rust-headless-performance");
        fixture["repetitions"] = json!(3);
        fixture["provenance"]["sources"] = json!({});
        for path in [
            "tools/xtask/src/headless_performance.rs",
            "tools/xtask/src/performance-worker.c",
            "tools/xtask/src/support/local.rs",
            "tools/xtask/src/support/attachment.rs",
            "tools/xtask/src/support/process.rs",
        ] {
            fixture["provenance"]["sources"][path] =
                json!(format!("{:x}", Sha256::digest(fs::read(root.join(path))?)));
        }
        fixture["provenance"]["harness_sha256"] =
            fixture["provenance"]["sources"]["tools/xtask/src/headless_performance.rs"].clone();
        validate(root, &fixture)?;
        let mut empty = fixture.clone();
        empty["provenance"]["sources"] = json!({});
        ensure!(validate(root, &empty).is_err(), "empty source map accepted");
        for path in fixture["provenance"]["sources"].as_object().unwrap().keys() {
            let mut missing = fixture.clone();
            missing["provenance"]["sources"]
                .as_object_mut()
                .unwrap()
                .remove(path);
            ensure!(validate(root, &missing).is_err(), "missing {path} accepted");
        }
        Ok(())
    }
    #[test]
    fn archived_headless_sources_are_not_current() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .context("root")?;
        let mut report =
            crate::evidence::retained_document("docs/headless-performance/fux-before.json")?;
        validate_with_sources(root, &report, true)?;
        ensure!(
            validate(root, &report).is_err(),
            "archived harness accepted as current source"
        );
        report["provenance"]["harness_sha256"] = json!("0".repeat(64));
        ensure!(
            validate_with_sources(root, &report, true).is_err(),
            "wrong historical harness accepted"
        );
        Ok(())
    }
    #[test]
    fn retained_and_original_mutations() -> anyhow::Result<()> {
        super::regressions()
    }
}
