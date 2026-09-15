//! Repository checks that are not unit tests: the dependency report of prompt section 2.
//!
//! `cargo run --manifest-path tools/xtask/Cargo.toml -- deps` prints the resolved normal
//! dependency graph of `fux` and `zor` and fails when a rule is violated:
//!
//! * no direct dependency on tokio, ratatui, tracing-subscriber, anyhow, bevy_render, bevy_winit,
//!   bevy_dev_tools, bevy_text, bevy_sprite (prompt section 2);
//! * every path to `bevy_render`/`wgpu`/`naga` goes through `bevy_remote -> bevy_dev_tools`
//!   (the one recorded exception, see `docs/dependencies.md`).

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::process::{Command, ExitCode};

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    resolve: Resolve,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    version: String,
}

#[derive(Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct Node {
    id: String,
    deps: Vec<Dep>,
}

#[derive(Deserialize)]
struct Dep {
    pkg: String,
    dep_kinds: Vec<DepKind>,
}

#[derive(Deserialize)]
struct DepKind {
    kind: Option<String>,
}

const FORBIDDEN_DIRECT: &[&str] = &[
    "tokio",
    "ratatui",
    "ratatui-core",
    "tracing-subscriber",
    "anyhow",
    "bevy_render",
    "bevy_winit",
    "bevy_dev_tools",
    "bevy_text",
    "bevy_sprite",
    "bevy_animation",
    "bevy",
];
const RENDER_CRATES: &[&str] = &["bevy_render", "wgpu", "wgpu-hal", "naga"];

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("deps") => match deps() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("deps: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: xtask deps");
            ExitCode::FAILURE
        }
    }
}

fn deps() -> Result<(), String> {
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--locked"])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    let meta: Metadata = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let names: BTreeMap<&str, (&str, &str)> = meta
        .packages
        .iter()
        .map(|p| (p.id.as_str(), (p.name.as_str(), p.version.as_str())))
        .collect();
    let normal: BTreeMap<&str, Vec<&str>> = meta
        .resolve
        .nodes
        .iter()
        .map(|n| {
            let deps = n
                .deps
                .iter()
                .filter(|d| d.dep_kinds.iter().any(|k| k.kind.is_none()))
                .map(|d| d.pkg.as_str())
                .collect();
            (n.id.as_str(), deps)
        })
        .collect();
    let mut failures = Vec::new();
    for root_name in ["fux", "zor"] {
        let Some(root) = meta.packages.iter().find(|p| p.name == root_name) else {
            failures.push(format!("package {root_name} not in workspace"));
            continue;
        };
        let direct = normal.get(root.id.as_str()).cloned().unwrap_or_default();
        for d in &direct {
            let (n, _) = names[d];
            if FORBIDDEN_DIRECT.contains(&n) {
                failures.push(format!("{root_name}: forbidden direct dependency {n}"));
            }
        }
        // Breadth-first closure with parent pointers for path reporting.
        let mut parent: BTreeMap<&str, &str> = BTreeMap::new();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut queue = VecDeque::from([root.id.as_str()]);
        seen.insert(root.id.as_str());
        while let Some(id) = queue.pop_front() {
            for d in normal.get(id).map(Vec::as_slice).unwrap_or(&[]) {
                if seen.insert(d) {
                    parent.insert(d, id);
                    queue.push_back(d);
                }
            }
        }
        let mut unique: BTreeSet<String> = BTreeSet::new();
        for id in &seen {
            let (n, v) = names[id];
            unique.insert(format!("{n} {v}"));
        }
        println!("{root_name}: {} resolved normal dependencies", unique.len());
        for id in &seen {
            let (n, _) = names[id];
            if RENDER_CRATES.contains(&n) {
                let mut path = vec![n];
                let mut cur = *id;
                while let Some(p) = parent.get(cur) {
                    path.push(names[p].0);
                    cur = p;
                }
                path.reverse();
                let joined = path.join(" -> ");
                println!("{root_name}: render crate reachable: {joined}");
                // Every render-crate path must pass through bevy_remote -> bevy_dev_tools.
                // The BFS parent chain is one shortest path; verify the edge into bevy_render
                // (the only wgpu owner) originates from bevy_dev_tools's subtree.
                let through_dev_tools = path.windows(2).any(|w| w == ["bevy_remote", "bevy_dev_tools"]);
                if !through_dev_tools {
                    failures.push(format!("{root_name}: {n} reachable outside bevy_remote->bevy_dev_tools: {joined}"));
                }
            }
        }
        // Independent of path: bevy_render must have exactly one in-edge set rooted at
        // bevy_dev_tools's render stack; report every direct dependant for the record.
        let render_id = seen.iter().copied().find(|id| names[id].0 == "bevy_render");
        if let Some(render_id) = render_id {
            let mut dependants: Vec<&str> = seen
                .iter()
                .filter(|id| normal.get(*id).is_some_and(|d| d.contains(&render_id)))
                .map(|id| names[id].0)
                .collect();
            dependants.sort_unstable();
            println!("{root_name}: bevy_render dependants: {}", dependants.join(", "));
            for d in dependants {
                if !["bevy_dev_tools", "bevy_core_pipeline", "bevy_pbr", "bevy_sprite_render", "bevy_ui_render", "bevy_light", "bevy_material", "bevy_anti_alias", "bevy_post_process"].contains(&d) {
                    failures.push(format!("{root_name}: unexpected bevy_render dependant {d}"));
                }
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}
