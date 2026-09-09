#!/usr/bin/env python3
"""Reconstruct optional integration sources without commits or unpublished releases.

Standalone fux builds, tests, and packages never require this tool.

export: refresh reviewed patches from the owning repositories, including new source files.
apply: clone missing checkouts at pinned bases and apply patches; refuse divergent worktrees.
verify: apply patches to temporary clean local clones and compare all versioned source bytes.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import shutil
import tempfile

ROOT = Path(__file__).resolve().parents[1]
PATCHES = ROOT / "dependency-patches"
MANIFEST = json.loads((PATCHES / "manifest.json").read_text())


def git(repo, *args, expected=(0,)):
    result = subprocess.run(["git", "-C", str(repo), *args], capture_output=True)
    if result.returncode not in expected:
        raise RuntimeError(f"git {' '.join(args)} in {repo}: {result.stderr.decode()}")
    return result.stdout


def snapshot_patch(repo, base):
    patch = git(repo, "diff", "--binary", base, "--")
    for filename in git(repo, "ls-files", "--others", "--exclude-standard", "-z").split(b"\0"):
        if filename:
            patch += git(repo, "diff", "--no-index", "--binary", "--", "/dev/null",
                         filename.decode(), expected=(0, 1))
    return patch


def check_base(repo, spec):
    actual = git(repo, "rev-parse", "HEAD").decode().strip()
    if actual != spec["base"]:
        raise RuntimeError(f"{repo}: expected base {spec['base']}, found {actual}; update the manifest deliberately")


def apply(repo, patch):
    if not patch.read_bytes():
        if git(repo, "status", "--porcelain"):
            raise RuntimeError(f"{repo}: unexpected changes with an empty dependency patch")
        return
    # Idempotent for an already reconstructed checkout; never overwrite unrelated work.
    done = subprocess.run(["git", "-C", str(repo), "apply", "--reverse", "--check", str(patch)],
                          capture_output=True)
    if done.returncode == 0:
        base = git(repo, "rev-parse", "HEAD").decode().strip()
        if snapshot_patch(repo, base) != patch.read_bytes():
            raise RuntimeError(f"{repo}: patch is present but additional local changes diverge from it")
        return
    if git(repo, "status", "--porcelain"):
        raise RuntimeError(f"{repo}: divergent local changes; export or reconcile them before applying")
    git(repo, "apply", "--check", str(patch))
    git(repo, "apply", str(patch))


def source_files(repo):
    names = git(repo, "ls-files", "--cached", "--others", "--exclude-standard", "-z")
    return {name.decode(): (repo / name.decode()).read_bytes()
            for name in names.split(b"\0") if name and (repo / name.decode()).is_file()}


def verify_combined_build():
    """Build/test source reconstructed only from fux inputs and recorded dependency patches."""
    with tempfile.TemporaryDirectory(prefix="fux-combined-reconstruct-") as directory:
        reconstructed = Path(directory) / "fux"
        for name in source_files(ROOT):
            destination = reconstructed / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / name, destination, follow_symlinks=False)
        for spec in MANIFEST.values():
            dependency = reconstructed / spec["path"]
            dependency.parent.mkdir(parents=True, exist_ok=True)
            git(ROOT, "clone", "--shared", "--no-checkout", "--",
                str(ROOT / spec["path"]), str(dependency))
            git(dependency, "checkout", "--detach", spec["base"])
            apply(dependency, reconstructed / "dependency-patches" / spec["patch"])
        environment = os.environ.copy()
        target = ROOT / "target" / "dependency-verification"
        environment["CARGO_TARGET_DIR"] = str(target)
        environment["ZOR_BIN"] = str(target / "debug" / "zor")
        environment["FUX_REQUIRE_ZOR_BIN"] = "1"
        environment["FUX_BIN"] = str(target / "debug" / "fux")
        environment["KOH_REQUIRE_FUX_BIN"] = "1"
        # Each reconstruction has a new absolute source path. Reused owner artifacts can retain
        # env!("CARGO_MANIFEST_DIR") pointing at an already deleted reconstruction. Keep third-party
        # dependency caches, but rebuild all three owner packages for this exact source snapshot.
        commands = [
            ["cargo", "clean", "-p", "fux"],
            ["cargo", "clean", "--manifest-path", "zor/Cargo.toml", "-p", "zor"],
            ["cargo", "clean", "--manifest-path", "references/koh/Cargo.toml", "-p", "koh"],
            ["cargo", "check", "--all-targets", "--locked"],
            ["cargo", "build", "--locked", "--bin", "fux"],
            ["cargo", "build", "--manifest-path", "zor/Cargo.toml", "--locked", "--bin", "zor"],
            ["cargo", "test", "--locked", "--test", "ecs", "--test", "local_cli",
             "--test", "zor_integration", "--", "--test-threads=1"],
            ["cargo", "test", "--manifest-path", "references/koh/Cargo.toml", "--locked", "--lib", "gateway::"],
            ["cargo", "test", "--manifest-path", "references/koh/Cargo.toml", "--locked", "--test", "gateway"],
        ]
        for command in commands:
            print("combined: " + " ".join(command), flush=True)
            subprocess.run(command, cwd=reconstructed, env=environment, check=True)
        print("combined: reconstructed build and integration tests passed", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("export", "apply", "verify"))
    parser.add_argument("--build", action="store_true",
                        help="with verify, build and integration-test the reconstructed combined tree")
    args = parser.parse_args()
    if args.build and args.action != "verify":
        parser.error("--build requires verify")
    for name, spec in MANIFEST.items():
        repo = ROOT / spec["path"]
        patch = PATCHES / spec["patch"]
        if args.action == "apply" and not (repo / ".git").exists():
            repo.parent.mkdir(parents=True, exist_ok=True)
            git(ROOT, "clone", "--no-checkout", "--", spec["repository"], str(repo))
            git(repo, "checkout", "--detach", spec["base"])
        check_base(repo, spec)
        if args.action == "export":
            patch.write_bytes(snapshot_patch(repo, spec["base"]))
        elif args.action == "apply":
            apply(repo, patch)
        else:
            if snapshot_patch(repo, spec["base"]) != patch.read_bytes():
                raise RuntimeError(f"{name}: patch is stale; run python3 tools/dependencies.py export")
            with tempfile.TemporaryDirectory(prefix=f"fux-{name}-reconstruct-") as directory:
                reconstructed = Path(directory) / name
                git(ROOT, "clone", "--shared", "--no-checkout", "--", str(repo), str(reconstructed))
                git(reconstructed, "checkout", "--detach", spec["base"])
                apply(reconstructed, patch)
                if source_files(reconstructed) != source_files(repo):
                    raise RuntimeError(f"{name}: reconstructed source differs from its owning repository")
        print(f"{name}: {args.action} complete", flush=True)
    if args.build:
        verify_combined_build()


if __name__ == "__main__":
    main()
