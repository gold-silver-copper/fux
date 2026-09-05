#![allow(clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn production_spawns_exist_only_in_reviewed_owner_modules() {
    let approved = [
        "src/client/mod.rs",
        "src/client/terminal.rs",
        "src/daemon/spawn.rs",
        "src/host/mod.rs",
        "src/main.rs",
        "src/pty.rs", // Owns pane processes and bounded pumps; teardown is covered by host lifecycle tests.
        "src/runtime/mod.rs",
    ];
    for file in rust_files("src") {
        let source = read(&file);
        if source.contains(".spawn()")
            || source.contains("thread::spawn")
            || source.contains("tokio::spawn")
        {
            let relative = relative(&file);
            assert!(
                approved.contains(&relative.as_str()),
                "spawn ownership invariant: {relative} must use an existing reviewed owner or be added here with signal/reap/join evidence"
            );
        }
    }
}

#[test]
fn channels_and_registries_remain_bounded_or_reaped() {
    for file in rust_files("src") {
        let source = read(&file);
        assert!(
            !source.contains("unbounded_channel"),
            "bounded-channel invariant: {} uses an unbounded channel",
            relative(&file)
        );
        assert!(
            !source.contains("crossbeam_channel::unbounded"),
            "bounded-channel invariant: {} uses an unbounded crossbeam channel",
            relative(&file)
        );
    }
    let host = read(Path::new("src/host/mod.rs"));
    for evidence in [
        "workers.drain(..)",
        "external_workers.drain(..)",
        "pty.shutdown()",
    ] {
        assert!(
            host.contains(evidence),
            "worker-reaping invariant: host is missing `{evidence}`"
        );
    }
}

#[test]
fn portable_production_paths_do_not_bypass_platform_policy() {
    for file in rust_files("src") {
        let relative = relative(&file);
        let contents = read(&file);
        let source = strip_test_modules(&contents);
        if relative == "src/config.rs" {
            continue;
        }
        assert!(
            !source.contains("\"/bin/sh\"") && !source.contains("\"/usr/bin/env\""),
            "portable-path invariant: {relative} must resolve tools through config/platform policy"
        );
    }
}

#[test]
fn pure_model_verification_has_no_wall_clock_or_placeholder_escape_hatches() {
    let pure = [
        Path::new("tests/verification_corpus.rs"),
        Path::new("tests/verify/interpreters/model.rs"),
    ];
    for file in pure {
        let source = read(file);
        for forbidden in ["thread::sleep", "tokio::time::sleep"] {
            assert!(
                !source.contains(forbidden),
                "pure-test invariant: {} contains forbidden `{forbidden}`",
                file.display()
            );
        }
    }
    let mut guarded = Vec::new();
    for root in [
        "src",
        "tests",
        "zor/src",
        "zor/tests",
        "tests/verify/fixture-child/src",
        "tests/verify/fixture-child/tests",
    ] {
        if Path::new(root).is_dir() {
            guarded.extend(rust_files(root));
        }
    }
    for file in guarded {
        if file == Path::new("tests/verification_structure.rs") {
            continue;
        }
        let source = read(&file);
        for forbidden in ["#[ignore", "todo!", "unimplemented!"] {
            assert!(
                !source.contains(forbidden),
                "verification invariant: {} contains forbidden `{forbidden}`",
                file.display()
            );
        }
    }
}

#[test]
fn wire_events_have_the_documented_dotted_spellings_exactly_twice() {
    let source = read(Path::new("src/control/protocol.rs"));
    for name in [
        "pane.opened",
        "pane.closed",
        "pane.focused",
        "pane.title",
        "agent.state",
        "pane.output",
        "workspace.resized",
        "client.attached",
        "client.detached",
    ] {
        assert_eq!(
            source.matches(&format!("rename = \"{name}\"")).count(),
            2,
            "control-event invariant: `{name}` must name both Event and EventKind exactly once"
        );
    }
}

#[test]
fn dependency_and_ci_surfaces_keep_the_verification_layers_enabled() {
    let manifest = read(Path::new("Cargo.toml"));
    for dependency in [
        "koh = { version = \"=0.12.1\"",
        "zor = { version = \"=0.1.2\"",
        "loom = \"0.7\"",
    ] {
        assert!(
            manifest.contains(dependency),
            "dependency-direction invariant: missing `{dependency}`"
        );
    }
    let workflow = read(Path::new(".github/workflows/ci.yml"));
    for command in [
        "fmt --all --check",
        "clippy --all-targets --all-features --locked",
        "test --all-features --locked",
        "doc --no-deps --all-features --locked",
        "tests/verify/fixture-child/Cargo.toml",
        "--no-default-features --all-targets --locked",
        "aarch64-linux-android",
        "cargo package --locked",
    ] {
        assert!(
            workflow.contains(command),
            "CI-surface invariant: workflow does not execute `{command}`"
        );
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

#[test]
fn project_dependencies_and_application_imports_respect_ownership() {
    for (manifest, forbidden) in [
        (
            "Cargo.toml",
            &["iroh", "iroh-base", "iroh-net", "ed25519-dalek"][..],
        ),
        ("references/koh/Cargo.toml", &["fux", "zor"][..]),
        ("zor/Cargo.toml", &["fux", "koh"][..]),
    ] {
        let source = read(Path::new(manifest));
        let document: toml::Value = toml::from_str(&source).expect("dependency manifest");
        check_dependency_tables(&document, forbidden, manifest);
    }
    for file in rust_files("src") {
        let source = read(&file);
        for forbidden in [
            "iroh::",
            "transport_iroh",
            "koh::pty::",
            "koh::key_passphrase::",
            "SecretKey",
            "load_or_create_secret_key",
            "zor::detect",
            "zor::machine",
            "zor::pty",
        ] {
            assert!(
                !source.contains(forbidden),
                "project ownership invariant: {} imports/implements {forbidden}",
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
