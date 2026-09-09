use super::*;
use std::collections::BTreeMap;
fn normalize(text: &str) -> Result<String> {
    let split = regex::Regex::new(r"\r\n|[\n\r\x0b\x0c\x1c-\x1e\x{85}\x{2028}\x{2029}]")?;
    let mut lines = split
        .split(text)
        .map(|line| {
            line.trim_end_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
        })
        .collect::<Vec<_>>();
    while lines.last() == Some(&"") {
        lines.pop();
    }
    Ok(if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n") + "\n"
    })
}
fn corpus(historical: bool) -> Result<Vec<Value>> {
    let retained = historical
        .then(|| retained_report("detection-screens"))
        .transpose()?;
    let default = "real agent viewport";
    let mut cases = Vec::new();
    let working = regex::Regex::new(r"(?m)^[•◦] Working \(")?;
    for (id, agent, file, expected, select, note) in [
        (
            "codex-sign-in",
            "codex",
            "codex-0.153.4/startup.json",
            "blocked",
            "last",
            default,
        ),
        (
            "codex-trust",
            "codex",
            "codex-0.153.4/authenticated.json",
            "blocked",
            "trust",
            default,
        ),
        (
            "codex-working",
            "codex",
            "codex-0.153.4/authenticated.json",
            "working",
            "codex-working",
            default,
        ),
        (
            "codex-response-input",
            "codex",
            "codex-0.153.4/authenticated.json",
            "idle",
            "last",
            "response followed by empty composer; input readiness, not task completion",
        ),
        (
            "codex-command-approval",
            "codex",
            "codex-0.153.4/approval.json",
            "blocked",
            "last",
            default,
        ),
        (
            "claude-theme",
            "claude",
            "claude-2.1.263/startup.json",
            "blocked",
            "last",
            default,
        ),
        (
            "claude-working",
            "claude",
            "claude-2.1.263/authenticated.json",
            "working",
            "claude-working",
            default,
        ),
        (
            "claude-response-input",
            "claude",
            "claude-2.1.263/authenticated.json",
            "idle",
            "last",
            "response followed by empty composer and shortcut footer; input readiness, not task completion",
        ),
        (
            "opencode-startup",
            "opencode",
            "opencode-1.18.29/startup.json",
            "idle",
            "last",
            default,
        ),
        (
            "opencode-permission",
            "opencode",
            "opencode-1.18.29/integration.json",
            "blocked",
            "single",
            "real agent with synthetic provider; unanswered native permission",
        ),
        (
            "opencode-question",
            "opencode",
            "opencode-1.18.29/integration-storage.json",
            "blocked",
            "single",
            "real agent with synthetic provider; unanswered native question",
        ),
    ] {
        let source = format!("zor/tests/fixtures/agents/{file}");
        let bytes = if let Some(report) = &retained {
            let row = array(report, "/results")?
                .iter()
                .find(|row| row["source"] == source)
                .context("missing historical viewport source")?;
            retained_bytes(&source, field(row, "/source_sha256")?)?
        } else {
            fs::read(root()?.join(&source))?
        };
        let value: Value = serde_json::from_slice(&bytes)?;
        let pointer = if select == "single" {
            "/capture".into()
        } else {
            let entries = array(&value, "/captures")?;
            let index = if select == "last" {
                entries.len().checked_sub(1).context("empty captures")?
            } else {
                let mut selected = None;
                for (i, entry) in entries.iter().enumerate() {
                    let text = field(entry, "/capture/text")?
                        .as_str()
                        .context("capture text")?;
                    let matched = match select {
                        "trust" => text.contains("Do you trust the contents"),
                        "codex-working" => working.is_match(text),
                        _ => {
                            text.contains("Sonnet 5 · API Usage Billing")
                                && text.contains("esc to interrupt")
                        }
                    };
                    if matched {
                        selected = Some(i);
                        break;
                    }
                }
                selected.context("missing selected screen")?
            };
            format!("/captures/{index}/capture")
        };
        let capture = field(&value, &pointer)?;
        cases.push(json!({"id":id,"agent":agent,"expected":expected,"kind":"real","source":source,"source_sha256":format!("{:x}",Sha256::digest(bytes)),"pointer":pointer,"rows":field(capture,"/rows")?,"columns":field(capture,"/columns")?,"text":normalize(field(capture,"/text")?.as_str().context("text")?)?,"note":note}));
    }
    for base in cases.clone() {
        let text = field(&base, "/text")?.as_str().context("text")?;
        let agent = field(&base, "/agent")?.as_str().context("agent")?;
        for (kind, mutated) in [
            (
                "stale-transcript",
                format!("{text}Current unrelated command finished.\n$ "),
            ),
            (
                "quoted-screen",
                text.lines()
                    .map(|line| format!("> {line}"))
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n",
            ),
            ("shell-output", format!("$ cat saved-screen\n{text}")),
            ("echoed-name", format!("$ printf {agent}\n{agent}\n$ \n")),
            (
                "nested-tool-output",
                format!("Tool output follows:\n{text}Tool returned. Waiting for a new request.\n"),
            ),
            (
                "malformed-control",
                format!("{text}\x1b]7877;malformed\x07"),
            ),
        ] {
            let mut case = base.clone();
            case["id"] = format!("{}/{kind}", field(&base, "/id")?.as_str().context("id")?).into();
            case["expected"] = "unknown".into();
            case["kind"] = kind.into();
            case["text"] = mutated.into();
            case["note"] =
                "synthetic derived negative; detached text, not additional real-agent coverage"
                    .into();
            cases.push(case);
        }
    }
    Ok(cases)
}
fn summary(results: &[Value]) -> Result<Value> {
    let mut summary = serde_json::Map::new();
    for backend in ["zor", "herdr"] {
        let real = results
            .iter()
            .filter(|r| r["kind"] == "real")
            .collect::<Vec<_>>();
        let negatives = results
            .iter()
            .filter(|r| r["kind"] != "real")
            .collect::<Vec<_>>();
        let blockers = real
            .iter()
            .filter(|r| r["expected"] == "blocked")
            .collect::<Vec<_>>();
        let mut confusion = BTreeMap::new();
        for r in &real {
            let key = format!(
                "{} -> {}",
                field(r, "/expected")?.as_str().context("expected")?,
                field(r, &format!("/{backend}/state"))?
                    .as_str()
                    .context("state")?
            );
            *confusion.entry(key).or_insert(0) += 1;
        }
        summary.insert(backend.into(),json!({"real_screens":real.len(),"correct":real.iter().filter(|r|r[backend]["state"]==r["expected"]).count(),"blockers":blockers.len(),"missed_blockers":blockers.iter().filter(|r|r[backend]["state"]!="blocked").count(),"negative_screens":negatives.len(),"negative_nonunknown":negatives.iter().filter(|r|r[backend]["state"]!="unknown").count(),"negative_matched_rule":negatives.iter().filter(|r|!r[backend]["rule"].is_null()).count(),"real_correct_with_rule":real.iter().filter(|r|r[backend]["state"]==r["expected"]&&!r[backend]["rule"].is_null()).count(),"real_confusion":confusion}));
    }
    Ok(Value::Object(summary))
}
fn validate(value: &Value, historical: bool) -> Result<()> {
    let hash = if historical {
        retained_hash
    } else {
        verify_hash
    };
    hash(
        if value.pointer("/provenance/harness_kind")
            == Some(&json!("rust-production-cli-screen-matching"))
        {
            "tools/xtask/src/evidence/screens.rs"
        } else {
            "tools/comparisons/detection_screens.py"
        },
        field(value, "/provenance/harness_sha256")?,
    )?;
    if historical {
        retained_sources("detection-screens", value, "/provenance/source_hashes")?;
        let original = retained_report("detection-screens")?;
        ensure!(
            value["provenance"]["harness_sha256"] == original["provenance"]["harness_sha256"],
            "historical screen harness changed"
        );
    } else {
        verify_sources(value, "/provenance/source_hashes")?;
    }
    let cases = corpus(historical)?;
    let rows = array(value, "/results")?;
    ensure!(cases.len() == rows.len(), "missing screen case");
    for (case, row) in cases.iter().zip(rows) {
        for (key, expected) in case.as_object().context("case")? {
            if key != "text" {
                ensure!(
                    row.get(key) == Some(expected),
                    "case metadata mismatch {key}"
                );
            }
        }
        ensure!(
            field(row, "/text_sha256")?.as_str().context("text hash")?
                == format!(
                    "{:x}",
                    Sha256::digest(field(case, "/text")?.as_str().context("text")?.as_bytes())
                ),
            "screen hash mismatch"
        );
        for backend in ["zor", "herdr"] {
            field(row, &format!("/{backend}/rule"))?;
            ensure!(
                ["unknown", "idle", "blocked", "working"].contains(
                    &field(row, &format!("/{backend}/state"))?
                        .as_str()
                        .context("state")?
                ),
                "invalid verdict"
            );
        }
        if field(row, "/herdr/rule")?.is_null() {
            for key in ["visible_blocker", "visible_working", "visible_idle"] {
                ensure!(
                    field(row, &format!("/herdr/{key}"))? == false,
                    "fallback treated as visible evidence"
                );
            }
        }
    }
    ensure!(
        field(value, "/summary")? == &summary(rows)?,
        "score accounting mismatch"
    );
    let build = if historical {
        retained_report("controller-setup-build")?
    } else {
        report("controller-setup-build")?
    };
    ensure!(
        field(value, "/provenance/binaries/herdr/sha256")? == field(&build, "/binary_sha256")?,
        "reference build mismatch"
    );
    Ok(())
}
pub fn regressions() -> Result<()> {
    let value = retained_report("detection-screens")?;
    validate(&value, true)?;
    let mut missing = value.clone();
    missing["results"]
        .as_array_mut()
        .context("results")?
        .iter_mut()
        .find(|row| row["zor"]["rule"].is_null())
        .context("null zor rule")?["zor"]
        .as_object_mut()
        .context("zor verdict")?
        .remove("rule");
    ensure!(
        validate(&missing, true).is_err(),
        "missing null rule accepted"
    );
    let mut invalid = value.clone();
    invalid["results"].as_array_mut().context("results")?.pop();
    ensure!(
        validate(&invalid, true).is_err(),
        "omitted negative accepted"
    );
    let mut invalid = value.clone();
    invalid["summary"]["zor"]["correct"] = (integer(&value, "/summary/zor/correct")? + 1).into();
    ensure!(validate(&invalid, true).is_err(), "invented score accepted");
    let mut invalid = value.clone();
    invalid["results"]
        .as_array_mut()
        .context("results")?
        .iter_mut()
        .find(|r| r["herdr"]["rule"].is_null())
        .context("fallback row")?["herdr"]["visible_idle"] = true.into();
    ensure!(
        validate(&invalid, true).is_err(),
        "fallback visible evidence accepted"
    );
    println!("PASS screen corpus, provenance, negative coverage and score attribution");
    Ok(())
}

/// Paired production CLI matching on identical retained viewport strings.
pub fn capture(args: Vec<String>) -> Result<()> {
    use fux_xtask::support::{local::Root, process};
    use std::{io::Write, time::Duration};
    let mut flags = BTreeMap::new();
    ensure!(
        args.len().is_multiple_of(2),
        "usage: capture-detection-screens --zor PATH --herdr PATH --output NEW_JSON"
    );
    for pair in args.chunks_exact(2) {
        ensure!(
            ["--zor", "--herdr", "--output"].contains(&pair[0].as_str()),
            "unknown argument {}",
            pair[0]
        );
        flags.insert(pair[0].as_str(), pair[1].as_str());
    }
    let output = Path::new(flags.get("--output").context("missing --output")?);
    ensure!(!output.exists(), "use a new output file");
    let digest = |p: &Path| -> Result<String> { Ok(format!("{:x}", Sha256::digest(fs::read(p)?))) };
    let mut binaries = BTreeMap::new();
    let mut provenance = json!({"harness_kind":"rust-production-cli-screen-matching","harness_sha256":format!("{:x}", Sha256::digest(include_bytes!("screens.rs"))),"binaries":{},"source_hashes":{}});
    for name in ["zor", "herdr"] {
        let flag = format!("--{name}");
        let binary =
            Path::new(flags.get(flag.as_str()).context("missing binary")?).canonicalize()?;
        provenance["binaries"][name] = json!({"path":binary,"sha256":digest(&binary)?});
        binaries.insert(name, binary);
    }
    let repository = root()?;
    let mut sources = Vec::new();
    for directory in ["zor/rules", "references/herdr/src/detect/manifests"] {
        for entry in fs::read_dir(repository.join(directory))? {
            let p = entry?.path();
            if p.extension().is_some_and(|e| e == "toml") {
                sources.push(p);
            }
        }
    }
    sources.extend(
        [
            "zor/src/rules/eval.rs",
            "zor/src/main.rs",
            "references/herdr/src/detect/manifest.rs",
            "references/herdr/src/cli/agent.rs",
        ]
        .map(|p| repository.join(p)),
    );
    for p in sources {
        provenance["source_hashes"][p
            .strip_prefix(repository)?
            .to_str()
            .context("source path")?] = digest(&p)?.into();
    }
    let mut isolated = Root::new("detect-pair-rs-", &["/bin/cat".into()])?;
    isolated.env.remove("SHELL");
    isolated.env.remove("TERM");
    isolated.env.insert(
        "XDG_CACHE_HOME".into(),
        isolated
            .path()
            .join("cache")
            .to_str()
            .context("cache")?
            .into(),
    );
    for (name, binary) in &binaries {
        let mut command = isolated.command(binary);
        command.arg("--version");
        let reply = process::output(command, Duration::from_secs(5), 1024 * 1024)?;
        ensure!(
            reply.status.success(),
            "{name} version: {}",
            String::from_utf8_lossy(&reply.stderr)
        );
        provenance["binaries"][name]["version"] = String::from_utf8(reply.stdout)?.trim().into();
    }
    let mut results = Vec::new();
    let verdict = regex::Regex::new(r"\A(unknown|blocked|working|idle) ([^\s]+)\n\z")?;
    for case in corpus(false)? {
        let text = field(&case, "/text")?.as_str().context("text")?;
        let screen = isolated.path().join("screen.txt");
        fs::write(&screen, text)?;
        let agent = field(&case, "/agent")?.as_str().context("agent")?;
        let mut row = case.clone();
        row.as_object_mut().context("case")?.remove("text");
        row["text_sha256"] = format!("{:x}", Sha256::digest(text.as_bytes())).into();
        for (name, binary) in &binaries {
            let mut command = isolated.command(binary);
            if *name == "herdr" {
                command
                    .args(["agent", "explain", "--file"])
                    .arg(&screen)
                    .args(["--agent", agent, "--json"]);
            } else {
                command.arg("check").arg(&screen).args(["--agent", agent]);
            }
            let result = process::output(command, Duration::from_secs(10), 1024 * 1024)?;
            if *name == "herdr" {
                ensure!(
                    result.status.success(),
                    "herdr {}: {}",
                    row["id"],
                    String::from_utf8_lossy(&result.stderr)
                );
                let value: Value = serde_json::from_slice(&result.stdout)?;
                ensure!(
                    field(&value, "/manifest_source")? == "bundled",
                    "nonbundled manifest"
                );
                let mut selected = serde_json::Map::new();
                for key in [
                    "state",
                    "visible_blocker",
                    "visible_working",
                    "visible_idle",
                    "manifest_version",
                    "fallback_reason",
                    "skip_state_update",
                ] {
                    selected.insert(key.into(), field(&value, &format!("/{key}"))?.clone());
                }
                selected.insert(
                    "rule".into(),
                    value
                        .pointer("/matched_rule/id")
                        .cloned()
                        .unwrap_or(Value::Null),
                );
                row[name] = Value::Object(selected);
            } else {
                ensure!(
                    result.status.code() == Some(1) && result.stderr.is_empty(),
                    "zor {}: {}",
                    row["id"],
                    String::from_utf8_lossy(&result.stderr)
                );
                let stdout = String::from_utf8(result.stdout)?;
                let matched = verdict
                    .captures(&stdout)
                    .context("zor operational error or malformed verdict")?;
                row[name] = json!({"state":&matched[1],"rule":if &matched[2]=="none"{Value::Null}else{json!(&matched[2])}});
            }
        }
        results.push(row);
    }
    let summary = summary(&results)?;
    let value = json!({"schema":1,"captured_at":time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?,"provenance":provenance,"scope":"Production CLI screen matching, identified agent supplied, default bundled rules, identical normalized viewport text.","limitations":["No OSC/title input, runtime activity arbitration, identity discovery, hysteresis, freshness or live detection latency.","Eleven selected real screens and derived negatives; not a representative accuracy rate or universal agent coverage.","Zor rules were developed from these retained fixtures; this measures conformance, not held-out generalization.","Negative strings are detached text. Exact copied live viewports remain indistinguishable to passive screen matchers.","OpenCode permission/question screens use a synthetic provider. Codex response and approval use a real authenticated model."],"results":results,"summary":summary});
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec_pretty(&value)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)?
        .write_all(&bytes)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

pub fn verify(args: Vec<String>) -> Result<()> {
    match args.as_slice() {
        [] => regressions(),
        [path] => validate(&serde_json::from_slice(&fs::read(path)?)?, false),
        _ => anyhow::bail!("usage: verify-detection-screens [REPORT_JSON]"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn archived_screen_provenance_is_not_current() -> anyhow::Result<()> {
        use super::*;
        let mut value = retained_report("detection-screens")?;
        validate(&value, true)?;
        ensure!(
            validate(&value, false).is_err(),
            "historical screens accepted as current"
        );
        value["results"][0]["source_sha256"] = json!("0".repeat(64));
        ensure!(
            validate(&value, true).is_err(),
            "changed viewport hash accepted"
        );
        Ok(())
    }

    #[test]
    fn retained_and_negative_cases() {
        super::regressions().unwrap();
    }
}
