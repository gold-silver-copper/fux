//! Compile the standalone Rust fixture harness outside Cargo's active target lock.
/// The workspace root: the fux crate lives at `crates/fux`.
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

use std::{path::PathBuf, process::Command, sync::OnceLock};

pub fn rust_harness() -> &'static PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let root = workspace_root();
        let target = root.join("target/rust-harness");
        let output = Command::new("cargo")
            .args(["build", "--locked", "--manifest-path"])
            .arg(root.join("tools/xtask/Cargo.toml"))
            .arg("--target-dir")
            .arg(&target)
            .output()
            .unwrap_or_else(|error| panic!("building Rust harness: {error}"));
        assert!(
            output.status.success(),
            "Rust harness build failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        target.join("debug/fux-xtask")
    })
}

#[allow(dead_code)] // Only integration tests need the separate provider fixture.
pub fn zor_argv_fixture() -> &'static PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let root = workspace_root();
        let target = root.join("target/rust-zor-fixtures");
        let output = Command::new("cargo")
            .args(["build", "--locked", "--manifest-path"])
            .arg(root.join("crates/zor/tools/xtask/Cargo.toml"))
            .arg("--target-dir")
            .arg(&target)
            .args(["--bin", "zor-argv-fixture"])
            .output()
            .unwrap_or_else(|error| panic!("building Rust provider fixture: {error}"));
        assert!(
            output.status.success(),
            "Rust provider fixture build failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        target.join("debug/zor-argv-fixture")
    })
}

#[allow(dead_code)]
pub fn zor_notifier_fixture() -> &'static PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let root = workspace_root();
        let target = root.join("target/rust-zor-fixtures");
        let output = Command::new("cargo")
            .args(["build", "--locked", "--manifest-path"])
            .arg(root.join("crates/zor/tools/xtask/Cargo.toml"))
            .arg("--target-dir")
            .arg(&target)
            .args(["--bin", "zor-notifier-fixture"])
            .output()
            .unwrap_or_else(|error| panic!("building Rust notifier fixture: {error}"));
        assert!(
            output.status.success(),
            "Rust notifier fixture build failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        target.join("debug/zor-notifier-fixture")
    })
}

#[allow(dead_code)]
pub fn zor_codex_fixture() -> &'static PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let root = workspace_root();
        let target = root.join("target/rust-zor-fixtures");
        let output = Command::new("cargo")
            .args(["build", "--locked", "--manifest-path"])
            .arg(root.join("crates/zor/tools/xtask/Cargo.toml"))
            .arg("--target-dir")
            .arg(&target)
            .args(["--bin", "zor-codex-fixture"])
            .output()
            .unwrap_or_else(|error| panic!("building Rust Codex fixture: {error}"));
        assert!(
            output.status.success(),
            "Rust Codex fixture build failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        target.join("debug/zor-codex-fixture")
    })
}
