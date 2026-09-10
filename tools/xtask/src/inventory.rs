//! Declaration and executable inventory, without provider or authentication probes.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
fn digest(path: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}
fn read(path: &Path) -> Result<Value> {
    let parsed: toml::Value = toml::from_str(&fs::read_to_string(path)?)?;
    Ok(serde_json::to_value(parsed)?)
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    value.get(key).with_context(|| format!("missing {key}"))
}
fn which(name: &str) -> Result<Option<PathBuf>> {
    let executable = |path: &Path| {
        path.is_file() && nix::unistd::access(path, nix::unistd::AccessFlags::X_OK).is_ok()
    };
    if name.contains('/') {
        return if executable(Path::new(name)) {
            Ok(Some(std::path::absolute(name)?))
        } else {
            Ok(None)
        };
    }
    let path = std::env::var_os("PATH").unwrap_or_else(|| "/bin:/usr/bin".into());
    if path.is_empty() {
        return Ok(None);
    }
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        if executable(&candidate) {
            return Ok(Some(std::path::absolute(candidate)?));
        }
    }
    Ok(None)
}
fn collect(root: &Path, overrides: &BTreeMap<String, String>) -> Result<(Value, Vec<Vec<String>>)> {
    let mut paths = fs::read_dir(root.join("references/herdr/src/detect/manifests"))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.retain(|path| path.extension().is_some_and(|s| s == "toml"));
    paths.sort();
    let mut rows = Vec::new();
    let mut orders = Vec::new();
    for path in paths {
        let source = read(&path)?;
        let id = required(&source, "id")?.as_str().context("manifest id")?;
        let mut names = vec![id.to_owned()];
        if let Some(aliases) = source.get("aliases") {
            for alias in aliases.as_array().context("aliases")? {
                let alias = alias.as_str().context("alias")?.to_owned();
                if !names.contains(&alias) {
                    names.push(alias);
                }
            }
        }
        let mut found = BTreeMap::new();
        let mut order = Vec::new();
        for name in &names {
            if let Some(path) = which(name)? {
                order.push(name.clone());
                found.insert(name.clone(), path);
            }
        }
        if let Some(path) = overrides.get(id) {
            if !found.contains_key(id) {
                order.push(id.to_owned());
            }
            found.insert(id.to_owned(), Path::new(path).canonicalize()?);
        }
        orders.push(order);
        let zor_path = root.join("crates/zor/rules").join(format!("{id}.toml"));
        let zor = if zor_path.exists() {
            Some(read(&zor_path)?)
        } else {
            None
        };
        let mut herdr_rules = Vec::new();
        if let Some(rules) = source.get("rules") {
            for rule in rules.as_array().context("herdr rules")? {
                herdr_rules.push(json!({"id":required(rule,"id")?,"state":required(rule,"state")?,"region":rule.get("region"),"skip_state_update":rule.get("skip_state_update").unwrap_or(&Value::Bool(false))}));
            }
        }
        let mut zor_rules = Vec::new();
        if let Some(rules) = zor.as_ref().and_then(|v| v.get("rules")) {
            for rule in rules.as_array().context("zor rules")? {
                zor_rules.push(json!({"id":required(rule,"id")?,"state":required(rule,"state")?}));
            }
        }
        rows.push(json!({"agent":id,"herdr_source":path.strip_prefix(root)?,"herdr_sha256":digest(&path)?,"version":source.get("version"),"aliases":names,"herdr_rules":herdr_rules,"zor_source":if zor.is_some(){Some(zor_path.strip_prefix(root)?)}else{None},"zor_sha256":if zor.is_some(){Some(digest(&zor_path)?)}else{None},"zor_rules":zor_rules,"executables":found,"availability_scope":"PATH and explicit fixture-binary overrides only"}));
    }
    Ok((
        json!({"schema":1,"captured_at":time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?,"harness_sha256":digest(&std::env::current_exe()?)?,"harness_kind":"rust-binary","agents":rows,"limitation":"Manifest declarations are not measured detection accuracy or real-agent coverage. Executable availability is not authentication or provider availability."}),
        orders,
    ))
}
fn describe(rules: &Value) -> Result<String> {
    let rules = rules.as_array().context("rules")?;
    let states = rules
        .iter()
        .map(|rule| required(rule, "state")?.as_str().context("state"))
        .collect::<Result<BTreeSet<_>>>()?;
    Ok(format!(
        "{} / {}",
        rules.len(),
        if states.is_empty() {
            "none".into()
        } else {
            states.into_iter().collect::<Vec<_>>().join(", ")
        }
    ))
}
pub fn run(args: Vec<String>) -> Result<()> {
    let mut args = args.into_iter();
    let mut overrides = BTreeMap::new();
    let mut output = None;
    while let Some(flag) = args.next() {
        let value = args.next().context("missing option value")?;
        match flag.as_str() {
            "--binary" => {
                let (name, path) = value
                    .split_once('=')
                    .context("--binary requires AGENT=PATH")?;
                overrides.insert(name.into(), path.into());
            }
            "--output" => output = Some(PathBuf::from(value)),
            _ => anyhow::bail!("unknown option {flag}"),
        }
    }
    let output = output.context("--output required")?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("repository root")?;
    let (value, orders) = collect(root, &overrides)?;
    fs::write(&output, serde_json::to_string_pretty(&value)? + "\n")?;
    let mut lines = vec![
        "# Detection inventory".into(),
        "".into(),
        required(&value, "limitation")?
            .as_str()
            .context("limitation")?
            .to_owned(),
        "".into(),
        "| Agent | herdr declared rules/states | zor bundled rules/states | Available executable |"
            .into(),
        "|---|---|---|---|".into(),
    ];
    for (row, names) in value["agents"]
        .as_array()
        .context("agents")?
        .iter()
        .zip(orders)
    {
        lines.push(format!(
            "| {} | {} | {} | {} |",
            row["agent"].as_str().context("agent")?,
            describe(&row["herdr_rules"])?,
            describe(&row["zor_rules"])?,
            if names.is_empty() {
                "not found".into()
            } else {
                names.join(", ")
            }
        ));
    }
    lines.extend(["", "The JSON retains exact manifest paths, hashes, rule IDs, regions, aliases and executable paths.","A missing executable here means unavailable through the searched names in this environment; it does not establish an installation or authentication blocker.","The initial Claude/Codex/OpenCode real-state gaps remain in the completion checklist. Pi startup has been captured separately; no Pi state rule is inferred from its name.",""].map(str::to_owned));
    fs::write(output.with_extension("md"), lines.join("\n"))?;
    Ok(())
}
