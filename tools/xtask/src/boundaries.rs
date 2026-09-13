//! Resolved production dependency closures across supported feature/target compositions.
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeSet, path::Path, process::Command};

const TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "aarch64-linux-android",
];

fn closure(
    manifest: &Path,
    package: &str,
    flags: &[&str],
    target: &str,
) -> Result<BTreeSet<String>> {
    let output = Command::new("cargo")
        .args(["+stable", "tree", "--locked", "--manifest-path"])
        .arg(manifest)
        .args([
            "-p",
            package,
            "--target",
            target,
            "--edges",
            "normal,build",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .args(flags)
        .output()?;
    ensure!(
        output.status.success(),
        "dependency resolution failed for {package}/{target}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Cargo emits actual package identities, including for renamed dependency declarations.
    let source = String::from_utf8(output.stdout)?;
    let packages = source
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    ensure!(
        packages.contains(package),
        "empty or unexpected dependency closure for {package}"
    );
    Ok(packages)
}

fn verify(
    manifest: &Path,
    package: &str,
    label: &str,
    flags: &[&str],
    forbidden: &[&str],
    required: &[&str],
) -> Result<()> {
    for target in TARGETS {
        let packages = closure(manifest, package, flags, target)?;
        for name in forbidden {
            ensure!(
                !packages.contains(*name),
                "{package}/{label}/{target} includes forbidden production capability {name}"
            );
        }
        for name in required {
            ensure!(
                packages.contains(*name),
                "{package}/{label}/{target} lost required capability {name}"
            );
        }
        println!(
            "PASS {package}/{label}/{target}: {} production packages",
            packages.len()
        );
    }
    Ok(())
}

pub fn run(args: Vec<String>) -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let koh = match args.as_slice() {
        [] => None,
        [flag, path] if flag == "--koh" => Some(
            Path::new(path)
                .canonicalize()
                .context("koh development checkout")?,
        ),
        _ => anyhow::bail!("usage: verify-boundaries [--koh CHECKOUT]"),
    };
    let manifest = root.join("Cargo.toml");
    verify(
        &manifest,
        "fux",
        "default",
        &[],
        &["koh", "zor", "iroh", "quinn", "reqwest"],
        &[],
    )?;
    let zor_manifest = root.join("crates/zor/Cargo.toml");
    for (label, flags) in [
        ("default", &[][..]),
        ("cli", &["--no-default-features", "--features", "cli"][..]),
        ("protocol", &["--no-default-features"][..]),
    ] {
        verify(
            &zor_manifest,
            "zor",
            label,
            flags,
            &["fux", "koh", "iroh", "bevy_ecs", "portable-pty", "vt100"],
            &[],
        )?;
    }
    verify(
        &zor_manifest,
        "zor",
        "wrap",
        &["--no-default-features", "--features", "wrap"],
        &["fux", "koh", "iroh", "bevy_ecs"],
        &["portable-pty", "vt100"],
    )?;
    if let Some(koh) = koh {
        let manifest = koh.join("Cargo.toml");
        for (label, flags) in [
            (
                "gateway-library",
                &["--no-default-features", "--features", "gateway"][..],
            ),
            (
                "gateway-cli",
                &["--no-default-features", "--features", "cli,gateway"][..],
            ),
        ] {
            verify(
                &manifest,
                "koh",
                label,
                flags,
                &[
                    "fux",
                    "zor",
                    "portable-pty",
                    "vt100",
                    "termina",
                    "crossterm",
                    "qwertty",
                ],
                &["iroh"],
            )?;
        }
        verify(
            &manifest,
            "koh",
            "default-shell",
            &[],
            &["fux", "zor"],
            &["iroh", "portable-pty", "vt100", "termina"],
        )?;
    }
    Ok(())
}
