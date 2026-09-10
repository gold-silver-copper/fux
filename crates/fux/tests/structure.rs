//! Structural invariants of the repository: process ownership stays in reviewed modules, queues
//! stay bounded, owner programs stay independent, and CI keeps the verification layers enabled.
#![allow(clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn production_spawns_exist_only_in_reviewed_owner_modules() {
    let approved = [
        "src/os/pty.rs",         // pane processes and their pump threads; joined on release
        "src/server/adapter.rs", // blocking spawn/terminate/join tasks tracked in JoinSets
        "src/server/connections.rs", // socket tasks per connection, aborted on stop
        "src/server/mod.rs",     // manager/listener tasks joined on shutdown
        "src/client/mod.rs",     // frame reader task and workspace lookups, aborted on exit
        "src/client/io.rs",      // stdin/SIGWINCH producers, cancelled and joined
        "src/daemon/startup.rs", // background server child, killed when readiness fails
        "src/main.rs",           // `fux run` capture-reader thread, joined before the CLI exits
    ];
    for file in rust_files("src") {
        let source = strip_test_modules(&read(&file)).to_owned();
        // Entity spawns (`world.spawn(...)`) are ECS bookkeeping, not processes or tasks.
        if source.contains("thread::spawn")
            || source.contains("tokio::spawn")
            || source.contains("spawn_blocking")
            || source.contains("tasks.spawn")
            || source.contains(".spawn()")
            || source.contains("spawn_command")
        {
            let relative = relative(&file);
            assert!(
                approved.contains(&relative.as_str()),
                "spawn ownership invariant: {relative} must be a reviewed owner listed here"
            );
        }
    }
}

#[test]
fn channels_stay_bounded_and_ecs_systems_never_block_on_the_operating_system() {
    for file in rust_files("src") {
        let source = read(&file);
        assert!(
            !source.contains("unbounded_channel") && !source.contains("mpsc::channel::<")
                || file.starts_with("src/os")
                || relative(&file).starts_with("src/server")
                || relative(&file).starts_with("src/client"),
            "bounded-channel invariant: {}",
            relative(&file)
        );
        assert!(
            !source.contains("unbounded_channel"),
            "bounded-channel invariant: {} uses an unbounded channel",
            relative(&file)
        );
    }
    for file in rust_files("src/ecs") {
        let source = strip_test_modules(&read(&file)).to_owned();
        for forbidden in [
            "std::thread::",
            "tokio::",
            "UnixStream",
            "UnixListener",
            "portable_pty",
            "std::process::Command",
            "std::fs::",
            "Mutex<",
        ] {
            assert!(
                !source.contains(forbidden),
                "ECS purity invariant: {} contains `{forbidden}`",
                relative(&file)
            );
        }
    }
}

#[test]
fn ecs_is_the_only_authoritative_model() {
    // No legacy host: nothing outside `src/ecs` defines workspace/tab/pane state machines.
    for file in rust_files("src") {
        let relative = relative(&file);
        if relative.starts_with("src/ecs/") {
            continue;
        }
        let source = strip_test_modules(&read(&file)).to_owned();
        for forbidden in [
            "struct WorkspaceHost",
            "struct WorkspaceState",
            "SessionHost",
        ] {
            assert!(
                !source.contains(forbidden),
                "single-authority invariant: {relative} defines `{forbidden}`"
            );
        }
    }
    let manifest = read(Path::new("Cargo.toml"));
    assert!(manifest.contains("bevy_ecs = { version = \"=0.19.1\""));
    assert!(manifest.contains("default-features = false, features = [\"std\"]"));
    let lib = read(Path::new("src/lib.rs"));
    assert!(lib.contains("pub mod ecs;"));
}

#[test]
fn wire_events_have_the_documented_dotted_spellings_exactly_twice() {
    let source = read(Path::new("src/proto/control.rs"));
    for name in [
        "pane.opened",
        "pane.closed",
        "pane.title",
        "pane.output",
        "tab.opened",
        "tab.closed",
        "client.attached",
        "client.detached",
    ] {
        assert_eq!(
            source.matches(&format!("rename = \"{name}\"")).count(),
            2,
            "control-event invariant: `{name}` must name both Event and EventKind exactly once"
        );
    }
    for removed in [
        "agent.state",
        "workspace.resized",
        "popup",
        "set-status",
        "zoom",
    ] {
        assert!(
            !source.contains(&format!("\"{removed}\"")),
            "removed protocol surface `{removed}` is still declared"
        );
    }
}

#[test]
fn no_placeholder_escape_hatches_in_source_or_tests() {
    let mut files = rust_files("src");
    files.extend(rust_files("tests"));
    for file in files {
        if file == Path::new("tests/structure.rs") {
            continue;
        }
        let source = read(&file);
        for forbidden in ["#[ignore", "todo!", "unimplemented!"] {
            assert!(
                !source.contains(forbidden),
                "verification invariant: {} contains `{forbidden}`",
                file.display()
            );
        }
    }
}

#[test]
fn dependency_and_ci_surfaces_keep_the_verification_layers_enabled() {
    let workflow = read(Path::new(".github/workflows/ci.yml"));
    for command in [
        "fmt --all --check",
        "clippy --all-targets --locked -- -D warnings",
        "test --locked",
        "doc --no-deps --locked",
        "tests/verify/fixture-child/Cargo.toml",
        "aarch64-linux-android",
        "cargo package --locked",
        "rust: 1.95.0",
    ] {
        assert!(
            workflow.contains(command),
            "CI-surface invariant: workflow does not contain `{command}`"
        );
    }
    let manifest = read(Path::new("Cargo.toml"));
    assert!(manifest.contains("rust-version = \"1.95\""));
}

#[test]
fn default_ci_and_release_verification_require_only_fux() {
    for path in [
        ".github/workflows/ci.yml",
        ".github/workflows/nightly.yml",
        ".github/workflows/release-verify.yml",
    ] {
        let source = read(Path::new(path));
        let mut optional_job = false;
        for line in source.lines() {
            if line.starts_with("  ") && !line.starts_with("   ") && line.ends_with(':') {
                optional_job = line == "  cross-repository:";
            }
            if !optional_job {
                for forbidden in [
                    "references/koh",
                    "zor/Cargo.toml",
                    "gold-silver-copper/koh",
                    "gold-silver-copper/zor",
                    "tools/dependencies.py",
                ] {
                    assert!(
                        !line.contains(forbidden),
                        "standalone CI invariant: {path}: {line}"
                    );
                }
            }
        }
    }
    let release = read(Path::new("tests/verify/release-package.sh"));
    assert!(!release.contains("zor") && !release.contains("koh"));
    let fixture = "tests/verify/fixture-child/Cargo.toml";
    let document = toml::from_str(&read(Path::new(fixture))).expect("fixture manifest");
    check_dependency_tables(&document, &["koh", "zor"], fixture);
}

#[test]
fn project_dependencies_and_application_imports_respect_ownership() {
    for (manifest, forbidden) in [
        (
            "Cargo.toml",
            &[
                "koh",
                "zor",
                "iroh",
                "iroh-base",
                "iroh-net",
                "ed25519-dalek",
                "bevy",
                "bevy_app",
                "bevy_render",
            ][..],
        ),
        ("references/koh/Cargo.toml", &["fux", "zor", "bevy_ecs"][..]),
        ("zor/Cargo.toml", &["fux", "koh", "bevy_ecs"][..]),
    ] {
        // Optional owner checkouts are checked when present; standalone tests require only fux.
        if manifest != "Cargo.toml" && !Path::new(manifest).exists() {
            continue;
        }
        let source = read(Path::new(manifest));
        let document: toml::Value = toml::from_str(&source).expect("dependency manifest");
        check_dependency_tables(&document, forbidden, manifest);
    }
    for file in rust_files("src") {
        let source = read(&file);
        for forbidden in [
            "iroh::",
            "koh::",
            "zor::",
            "SecretKey",
            "bevy_app",
            "bevy_reflect",
        ] {
            assert!(
                !source.contains(forbidden),
                "project ownership invariant: {} imports {forbidden}",
                relative(&file)
            );
        }
    }
}

fn check_dependency_tables(document: &toml::Value, forbidden: &[&str], manifest: &str) {
    let Some(table) = document.as_table() else {
        return;
    };
    for (name, value) in table {
        if matches!(
            name.as_str(),
            "dependencies" | "dev-dependencies" | "build-dependencies"
        ) {
            if let Some(dependencies) = value.as_table() {
                for (alias, spec) in dependencies {
                    let package = spec
                        .get("package")
                        .and_then(toml::Value::as_str)
                        .unwrap_or(alias);
                    assert!(
                        !forbidden.contains(&package),
                        "project ownership invariant: {manifest} depends on {package} ({alias})"
                    );
                }
            }
        } else {
            check_dependency_tables(value, forbidden, manifest);
        }
    }
}

fn rust_files(root: &str) -> Vec<PathBuf> {
    fn visit(directory: &Path, output: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(directory).expect("read source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                if !matches!(
                    path.file_name().and_then(|name| name.to_str()),
                    Some("target" | ".git")
                ) {
                    visit(&path, output);
                }
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                output.push(path);
            }
        }
    }
    let mut files = Vec::new();
    visit(Path::new(root), &mut files);
    files.sort();
    files
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn relative(path: &Path) -> String {
    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn strip_test_modules(source: &str) -> &str {
    source.split("#[cfg(test)]").next().unwrap_or(source)
}
