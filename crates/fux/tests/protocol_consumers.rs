//! Every wire item of the control and manager protocols has a recorded consumer. The surface is
//! derived from the source with the same lightweight parsing the boundary review uses; the
//! fixture names who reads each item so dead surface cannot linger unnoticed.
#![allow(clippy::expect_used, clippy::panic)]
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const FIXTURE: &str = "tests/fixtures/control-consumers.json";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    consumers: Vec<String>,
    kind: String,
    why: String,
}

type Section = BTreeMap<String, Item>;

fn parse(root: &Path, relative: &str) -> syn::File {
    let path = root.join(relative);
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    syn::parse_file(&source).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn kebab(ident: &str) -> String {
    let mut out = String::new();
    for (index, ch) in ident.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// A `#[serde(rename = "...")]` on the item, when present.
fn serde_rename(attrs: &[syn::Attribute]) -> Option<String> {
    let mut rename = None;
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("serde")) {
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                let value: syn::LitStr = meta.value()?.parse()?;
                rename = Some(value.value());
            }
            Ok(())
        });
    }
    rename
}

fn find_enum<'a>(file: &'a syn::File, name: &str) -> &'a syn::ItemEnum {
    file.items
        .iter()
        .find_map(|item| match item {
            syn::Item::Enum(item) if item.ident == name => Some(item),
            _ => None,
        })
        .unwrap_or_else(|| panic!("enum {name} is not declared"))
}

fn find_struct<'a>(file: &'a syn::File, name: &str) -> &'a syn::ItemStruct {
    file.items
        .iter()
        .find_map(|item| match item {
            syn::Item::Struct(item) if item.ident == name => Some(item),
            _ => None,
        })
        .unwrap_or_else(|| panic!("struct {name} is not declared"))
}

/// Wire names of an enum's variants: the serde rename when given, else kebab-case.
fn variants(file: &syn::File, name: &str) -> BTreeSet<String> {
    find_enum(file, name)
        .variants
        .iter()
        .map(|variant| {
            serde_rename(&variant.attrs).unwrap_or_else(|| kebab(&variant.ident.to_string()))
        })
        .collect()
}

fn field_names(fields: &syn::Fields) -> BTreeSet<String> {
    fields
        .iter()
        .map(|field| field.ident.as_ref().expect("named field").to_string())
        .collect()
}

fn struct_fields(file: &syn::File, name: &str) -> BTreeSet<String> {
    field_names(&find_struct(file, name).fields)
}

fn variant_fields(file: &syn::File, name: &str, variant: &str) -> BTreeSet<String> {
    let variant = find_enum(file, name)
        .variants
        .iter()
        .find(|candidate| candidate.ident == variant)
        .unwrap_or_else(|| panic!("{name}::{variant} is not declared"));
    field_names(&variant.fields)
}

/// The current wire surface, section by section, derived from the source.
fn surface(root: &Path) -> BTreeMap<&'static str, BTreeSet<String>> {
    let control = parse(root, "src/proto/control.rs");
    let rpc = parse(root, "src/daemon/rpc.rs");
    let terminal = parse(root, "src/terminal.rs");
    let events = variants(&control, "Event");
    assert_eq!(
        events,
        variants(&control, "EventKind"),
        "Event and EventKind must name the same wire kinds"
    );
    BTreeMap::from([
        ("request", variants(&control, "Request")),
        ("command-result", variants(&control, "CommandResult")),
        ("event", events),
        ("pane-summary", struct_fields(&control, "PaneSummary")),
        ("tab-summary", struct_fields(&control, "TabSummary")),
        (
            "workspace-summary",
            struct_fields(&control, "WorkspaceSummary"),
        ),
        ("server-info", struct_fields(&control, "ServerInfo")),
        ("info-limits", struct_fields(&control, "InfoLimits")),
        (
            "capture-snapshot",
            struct_fields(&terminal, "CaptureSnapshot"),
        ),
        (
            "cells-reply",
            variant_fields(&control, "CommandResult", "Cells"),
        ),
        ("capture-line", struct_fields(&control, "CaptureLine")),
        ("input-receipt", struct_fields(&control, "InputReceipt")),
        ("final-record", struct_fields(&control, "FinalRecord")),
        ("manager-request", variants(&rpc, "ManagerRequest")),
        ("manager-reply", variants(&rpc, "ManagerReply")),
    ])
}

fn fixture(root: &Path) -> BTreeMap<String, Section> {
    let path = root.join(FIXTURE);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let mut value: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&text).expect("control-consumers.json is a JSON object");
    value.remove("_comment");
    serde_json::from_value(serde_json::Value::Object(value))
        .expect("control-consumers.json sections map item names to consumer records")
}

#[test]
fn every_protocol_item_is_in_the_fixture_and_the_fixture_names_only_live_items() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let surface = surface(root);
    let fixture = fixture(root);
    let mut problems = Vec::new();
    for (section, items) in &surface {
        let Some(recorded) = fixture.get(*section) else {
            problems.push(format!("{FIXTURE} lacks the section {section:?}"));
            continue;
        };
        for name in items {
            if !recorded.contains_key(name) {
                problems.push(format!(
                    "{section}.{name} is on the wire but missing from {FIXTURE}: record its consumers"
                ));
            }
        }
        for name in recorded.keys() {
            if !items.contains(name) {
                problems.push(format!(
                    "{section}.{name} is in {FIXTURE} but no longer on the wire: delete the entry"
                ));
            }
        }
    }
    for section in fixture.keys() {
        if !surface.contains_key(section.as_str()) {
            problems.push(format!("{FIXTURE} names an unknown section {section:?}"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_protocol_item_has_a_consumer_that_still_reads_it() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let zor = root.join("../zor/src");
    let mut zor_sources: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut problems = Vec::new();
    for (section, items) in fixture(root) {
        for (name, item) in items {
            let label = format!("{section}.{name}");
            assert!(
                matches!(item.kind.as_str(), "primitive" | "workflow"),
                "{label}: kind must be primitive or workflow"
            );
            assert!(!item.why.trim().is_empty(), "{label}: why is empty");
            assert!(!item.consumers.is_empty(), "{label}: consumers is empty");
            if item.consumers.iter().any(|consumer| consumer == "none") {
                problems.push(format!(
                    "{label} has no consumer: delete the item from the protocol or record who reads it"
                ));
                continue;
            }
            if item.kind == "workflow" && item.consumers.iter().all(|consumer| consumer == "cli") {
                problems.push(format!(
                    "{label} is a workflow whose only consumer is the fux CLI: move it to zor"
                ));
            }
            let mut seen = BTreeSet::new();
            for consumer in &item.consumers {
                assert!(
                    seen.insert(consumer),
                    "{label}: duplicate consumer {consumer}"
                );
                match consumer.as_str() {
                    "viewer" | "cli" => {}
                    other => {
                        let Some(relative) = other.strip_prefix("zor:") else {
                            panic!(
                                "{label}: consumer {other:?} is not viewer, cli, zor:<path> or none"
                            );
                        };
                        assert!(
                            !relative.is_empty()
                                && !relative.starts_with('/')
                                && !relative.contains(".."),
                            "{label}: zor path {relative:?} must be relative to crates/zor/src"
                        );
                        let path = zor.join(relative);
                        let source = zor_sources.entry(path.clone()).or_insert_with(|| {
                            fs::read_to_string(&path).unwrap_or_else(|error| {
                                panic!("{label}: zor consumer {relative} is unreadable: {error}")
                            })
                        });
                        if !source.contains(name.as_str()) {
                            problems.push(format!(
                                "{label}: crates/zor/src/{relative} no longer mentions {name:?}"
                            ));
                        }
                    }
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn kebab_case_matches_serde() {
    assert_eq!(kebab("InputReserve"), "input-reserve");
    assert_eq!(kebab("List"), "list");
    assert_eq!(kebab("SendKeys"), "send-keys");
}
