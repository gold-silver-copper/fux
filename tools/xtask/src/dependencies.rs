//! Byte-exact companion patch reconstruction and mandatory verification planning.
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Deserialize)]
struct Spec {
    path: PathBuf,
    repository: String,
    base: String,
    patch: PathBuf,
}
#[derive(Deserialize)]
struct Checks {
    full: Vec<Vec<String>>,
    headless: Vec<Vec<String>>,
    deferred_r6_tests: Vec<String>,
}

fn git(repo: &Path, args: &[&OsStr], expected: &[i32]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    ensure!(
        expected
            .iter()
            .any(|code| Some(*code) == output.status.code()),
        "git {args:?} in {}: {}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}
fn command(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    git(repo, &args.iter().map(OsStr::new).collect::<Vec<_>>(), &[0])
}
fn text(bytes: Vec<u8>) -> Result<String> {
    Ok(String::from_utf8(bytes)?)
}
fn head(repo: &Path) -> Result<String> {
    Ok(text(command(repo, &["rev-parse", "HEAD"])?)?
        .trim()
        .to_owned())
}

fn snapshot_patch(repo: &Path, base: &str) -> Result<Vec<u8>> {
    let mut patch = command(repo, &["diff", "--binary", base, "--"])?;
    for name in command(repo, &["ls-files", "--others", "--exclude-standard", "-z"])?
        .split(|b| *b == 0)
        .filter(|name| !name.is_empty())
    {
        let name = std::str::from_utf8(name)?;
        patch.extend(git(
            repo,
            &[
                OsStr::new("diff"),
                OsStr::new("--no-index"),
                OsStr::new("--binary"),
                OsStr::new("--"),
                OsStr::new("/dev/null"),
                OsStr::new(name),
            ],
            &[0, 1],
        )?);
    }
    Ok(patch)
}
fn check_base(repo: &Path, spec: &Spec) -> Result<()> {
    let actual = head(repo)?;
    ensure!(
        actual == spec.base,
        "{}: expected base {}, found {actual}; update the manifest deliberately",
        repo.display(),
        spec.base
    );
    Ok(())
}
fn apply(repo: &Path, patch: &Path) -> Result<()> {
    let bytes = fs::read(patch)?;
    let dirty = || -> Result<bool> { Ok(!command(repo, &["status", "--porcelain"])?.is_empty()) };
    if bytes.is_empty() {
        ensure!(
            !dirty()?,
            "{}: unexpected changes with an empty dependency patch",
            repo.display()
        );
        return Ok(());
    }
    let already = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["apply", "--reverse", "--check"])
        .arg(patch)
        .output()?
        .status
        .success();
    if already {
        ensure!(
            snapshot_patch(repo, &head(repo)?)? == bytes,
            "{}: patch is present but additional local changes diverge from it",
            repo.display()
        );
        return Ok(());
    }
    ensure!(
        !dirty()?,
        "{}: divergent local changes; export or reconcile them before applying",
        repo.display()
    );
    git(
        repo,
        &[
            OsStr::new("apply"),
            OsStr::new("--check"),
            patch.as_os_str(),
        ],
        &[0],
    )?;
    git(repo, &[OsStr::new("apply"), patch.as_os_str()], &[0])?;
    Ok(())
}
fn source_files(repo: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut files = BTreeMap::new();
    for name in command(
        repo,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?
    .split(|b| *b == 0)
    .filter(|name| !name.is_empty())
    {
        let path = PathBuf::from(std::str::from_utf8(name)?);
        if repo.join(&path).is_file() {
            files.insert(path.clone(), fs::read(repo.join(path))?);
        }
    }
    Ok(files)
}
fn clone_at(root: &Path, origin: &Path, destination: &Path, base: &str) -> Result<()> {
    fs::create_dir_all(destination.parent().context("clone parent")?)?;
    git(
        root,
        &[
            OsStr::new("clone"),
            OsStr::new("--shared"),
            OsStr::new("--no-checkout"),
            OsStr::new("--"),
            origin.as_os_str(),
            destination.as_os_str(),
        ],
        &[0],
    )?;
    command(destination, &["checkout", "--detach", base])?;
    Ok(())
}
fn checks() -> Result<Checks> {
    Ok(serde_json::from_str(include_str!("../checks.json"))?)
}

fn gate_source(
    root: &Path,
    reconstructed: &Path,
    repositories: &[PathBuf],
    copied_paths: &BTreeSet<PathBuf>,
) -> Result<String> {
    use crate::gate_record::hash;
    use std::os::unix::fs::PermissionsExt;
    let mut sources = BTreeMap::new();
    for repository in repositories {
        for (name, bytes) in source_files(&root.join(repository))? {
            let relative = repository.join(name);
            let original = root.join(&relative);
            let copy = reconstructed.join(&relative);
            let meta = fs::symlink_metadata(&original)?;
            let copy_meta = fs::symlink_metadata(&copy)?;
            let link = if meta.file_type().is_symlink() {
                Some(fs::read_link(&original)?)
            } else {
                None
            };
            ensure!(
                fs::read(&copy)? == bytes
                    && meta.permissions().mode() == copy_meta.permissions().mode()
                    && meta.file_type().is_symlink() == copy_meta.file_type().is_symlink()
                    && (!meta.file_type().is_symlink()
                        || fs::read_link(&copy)? == fs::read_link(&original)?),
                "reconstructed gate source differs: {}",
                relative.display()
            );
            sources.insert(relative, (hash(&bytes), meta.permissions().mode(), link));
        }
    }
    ensure!(
        sources.keys().cloned().collect::<BTreeSet<_>>() == *copied_paths,
        "gate source inventory changed after reconstruction; start a fresh run"
    );
    Ok(hash(&serde_json::to_vec(&sources)?))
}

fn gate_inputs(
    root: &Path,
    reconstructed: &Path,
    target: &Path,
    headless: bool,
) -> Result<crate::gate_record::Inputs> {
    use crate::gate_record::{Inputs, hash, tree_hash};
    use std::os::unix::fs::PermissionsExt;
    let specs: BTreeMap<String, Spec> =
        serde_json::from_slice(&fs::read(root.join("dependency-patches/manifest.json"))?)?;
    let mut repositories = vec![PathBuf::new(), PathBuf::from("references/herdr")];
    repositories.extend(specs.values().map(|spec| spec.path.clone()));
    let copied_paths = serde_json::from_slice(&fs::read(
        reconstructed
            .parent()
            .context("gate directory")?
            .join("source-paths.json"),
    )?)?;
    let source = gate_source(root, reconstructed, &repositories, &copied_paths)?;
    let mut environment = BTreeMap::new();
    // Required checks have no account dependency. Do not forward credentials or
    // ambient Rust/test options that could silently alter the mandatory plan.
    for name in [
        "PATH",
        "HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "TMPDIR",
        "SDKROOT",
        "DEVELOPER_DIR",
    ] {
        if let Ok(value) = std::env::var(name) {
            environment.insert(name.to_owned(), value);
        }
    }
    for (name, value) in [
        ("LC_ALL", "C".to_owned()),
        ("TERM", "dumb".to_owned()),
        ("CARGO_TARGET_DIR", target.display().to_string()),
        ("ZOR_BIN", target.join("debug/zor").display().to_string()),
        ("FUX_BIN", target.join("debug/fux").display().to_string()),
        ("FUX_REQUIRE_ZOR_BIN", "1".to_owned()),
        ("KOH_REQUIRE_FUX_BIN", "1".to_owned()),
        ("KOH_REQUIRE_ZOR_BIN", "1".to_owned()),
    ] {
        environment.insert(name.to_owned(), value);
    }
    let mut toolchain = BTreeMap::new();
    for (program, args) in [
        ("cargo", vec!["--version", "--verbose"]),
        ("rustc", vec!["--version", "--verbose"]),
        ("node", vec!["--version"]),
        ("git", vec!["--version"]),
        ("cc", vec!["--version"]),
    ] {
        let mut c = Command::new(program);
        c.args(args)
            .current_dir(reconstructed)
            .env_clear()
            .envs(&environment);
        let output = fux_xtask::support::process::output(
            c,
            std::time::Duration::from_secs(30),
            1024 * 1024,
        )?;
        ensure!(
            output.status.success(),
            "cannot inspect gate toolchain: {program}"
        );
        toolchain.insert(program.to_owned(), String::from_utf8(output.stdout)?);
        let path = std::env::split_paths(environment.get("PATH").context("PATH required")?)
            .map(|base| base.join(program))
            .find(|path| {
                fs::metadata(path)
                    .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            })
            .with_context(|| format!("cannot resolve gate program {program}"))?
            .canonicalize()?;
        toolchain.insert(
            format!("{program}-executable"),
            format!("{}:{}", path.display(), tree_hash(&path)?),
        );
    }
    toolchain.insert(
        "runner-sha256".into(),
        tree_hash(&std::env::current_exe()?)?,
    );
    let home = PathBuf::from(
        environment
            .get("HOME")
            .context("HOME required for installed toolchain")?,
    );
    let cargo_home = environment
        .get("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cargo"));
    let rustup_home = environment
        .get("RUSTUP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".rustup"));
    // Store hashes, never configuration contents or credential files.
    let mut configuration = vec![
        cargo_home.join("config"),
        cargo_home.join("config.toml"),
        rustup_home.join("settings.toml"),
    ];
    for ancestor in reconstructed.ancestors() {
        configuration.extend([
            ancestor.join(".cargo/config"),
            ancestor.join(".cargo/config.toml"),
        ]);
    }
    for path in configuration {
        let content = match fs::read(&path) {
            Ok(bytes) => hash(&bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => "absent".into(),
            Err(error) => return Err(error.into()),
        };
        toolchain.insert(path.display().to_string(), content);
    }
    let plan = checks()?;
    Ok(Inputs {
        source,
        toolchain,
        environment,
        cwd: reconstructed.to_owned(),
        target: target.to_owned(),
        commands: if headless { plan.headless } else { plan.full },
        exclusions: if headless {
            let mut exclusions = plan.deferred_r6_tests;
            exclusions.push("gateway integration suite (real network endpoints)".into());
            exclusions
        } else {
            Vec::new()
        },
        timeout_seconds: 1800,
        stream_limit: 32 * 1024 * 1024,
    })
}

fn verify_build(
    root: &Path,
    manifest: &BTreeMap<String, Spec>,
    headless: bool,
    resume: Option<&str>,
) -> Result<()> {
    let logs = root.join(".verification");
    fs::create_dir_all(&logs)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(logs.join("gate.lock"))?;
    lock.try_lock()
        .context("another verification gate owns the build state")?;
    let run_logs = if let Some(name) = resume {
        ensure!(
            name.starts_with("gate-")
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "resume requires a gate directory name, not a path"
        );
        let path = logs.join(name);
        ensure!(
            fs::symlink_metadata(&path)?.is_dir(),
            "resume directory must be a real directory"
        );
        path
    } else {
        tempfile::Builder::new()
            .prefix("gate-")
            .tempdir_in(&logs)?
            .keep()
    };
    let reconstructed = run_logs.join("fux");
    if resume.is_none() {
        let mut copied_paths = BTreeSet::new();
        for repository in std::iter::once(PathBuf::new())
            .chain(manifest.values().map(|spec| spec.path.clone()))
            .chain(std::iter::once(PathBuf::from("references/herdr")))
        {
            copied_paths.extend(
                source_files(&root.join(&repository))?
                    .into_keys()
                    .map(|name| repository.join(name)),
            );
        }
        fs::write(
            run_logs.join("source-paths.json"),
            serde_json::to_vec(&copied_paths)?,
        )?;
        for name in source_files(root)?.keys() {
            let source = root.join(name);
            let destination = reconstructed.join(name);
            fs::create_dir_all(destination.parent().context("source parent")?)?;
            if fs::symlink_metadata(&source)?.file_type().is_symlink() {
                std::os::unix::fs::symlink(fs::read_link(&source)?, &destination)?;
            } else {
                fs::copy(&source, &destination)?;
            }
        }
        for spec in manifest.values() {
            let dependency = reconstructed.join(&spec.path);
            clone_at(root, &root.join(&spec.path), &dependency, &spec.base)?;
            apply(
                &dependency,
                &reconstructed.join("dependency-patches").join(&spec.patch),
            )?;
        }
        let reference = root.join("references/herdr");
        let reference_pin: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("tools/xtask/reference.json"))?)?;
        let commit = reference_pin["commit"]
            .as_str()
            .context("missing herdr reference commit")?;
        ensure!(
            commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid herdr reference commit"
        );
        ensure!(
            head(&reference)? == commit,
            "herdr: comparison reference is not at its recorded commit"
        );
        clone_at(
            root,
            &reference,
            &reconstructed.join("references/herdr"),
            commit,
        )?;
    }
    let target = root.join("target/dependency-verification");
    let cancellation = crate::gate_process::Cancellation::install()?;
    let inputs = gate_inputs(root, &reconstructed, &target, headless)?;
    println!(
        "combined: durable verification record {}",
        run_logs.display()
    );
    if headless {
        println!("combined: R6 remote runtime explicitly deferred; no remote acceptance claimed");
        for name in &inputs.exclusions {
            println!("combined: deferred {name}");
        }
        println!("combined: deferred gateway integration suite (real network endpoints)");
    }
    crate::gate_record::execute(
        &run_logs,
        inputs,
        resume.is_some(),
        &cancellation.flag,
        || gate_inputs(root, &reconstructed, &target, headless),
    )?;
    println!(
        "combined: reconstructed {} build and integration tests passed",
        if headless { "non-R6 headless" } else { "full" }
    );
    Ok(())
}

pub fn run(args: Vec<String>) -> Result<()> {
    let Some(action) = args.first().map(String::as_str) else {
        bail!("missing dependency action")
    };
    ensure!(
        ["export", "apply", "verify"].contains(&action),
        "unknown dependency action: {action}"
    );
    ensure!(
        args.iter()
            .skip(1)
            .all(|arg| arg == "--build" || arg == "--headless" || arg.starts_with("--resume=")),
        "unknown dependency argument"
    );
    let build = args.iter().any(|arg| arg == "--build");
    let headless = args.iter().any(|arg| arg == "--headless");
    let resumes: Vec<_> = args
        .iter()
        .filter_map(|arg| arg.strip_prefix("--resume="))
        .collect();
    ensure!(
        resumes.len() <= 1 && (resumes.is_empty() || action == "verify" && build),
        "--resume=gate-NAME requires verify --build"
    );
    ensure!(!build || action == "verify", "--build requires verify");
    ensure!(
        !headless || action == "verify" && build,
        "--headless requires verify --build"
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let patches = root.join("dependency-patches");
    let manifest: BTreeMap<String, Spec> =
        serde_json::from_slice(&fs::read(patches.join("manifest.json"))?)?;
    for (name, spec) in &manifest {
        let repo = root.join(&spec.path);
        let patch = patches.join(&spec.patch);
        if action == "apply" && !repo.join(".git").exists() {
            fs::create_dir_all(repo.parent().context("repository parent")?)?;
            git(
                &root,
                &[
                    OsStr::new("clone"),
                    OsStr::new("--no-checkout"),
                    OsStr::new("--"),
                    OsStr::new(&spec.repository),
                    repo.as_os_str(),
                ],
                &[0],
            )?;
            command(&repo, &["checkout", "--detach", &spec.base])?;
        }
        check_base(&repo, spec)?;
        match action {
            "export" => fs::write(&patch, snapshot_patch(&repo, &spec.base)?)?,
            "apply" => apply(&repo, &patch)?,
            "verify" => {
                ensure!(
                    snapshot_patch(&repo, &spec.base)? == fs::read(&patch)?,
                    "{name}: patch is stale; run fux-xtask dependencies export"
                );
                let temporary = tempfile::Builder::new()
                    .prefix(&format!("fux-{name}-reconstruct-"))
                    .tempdir()?;
                let reconstructed = temporary.path().join(name);
                clone_at(&root, &repo, &reconstructed, &spec.base)?;
                apply(&reconstructed, &patch)?;
                ensure!(
                    source_files(&reconstructed)? == source_files(&repo)?,
                    "{name}: reconstructed source differs from its owning repository"
                );
            }
            _ => unreachable!(),
        }
        println!("{name}: {action} complete");
    }
    if build {
        verify_build(&root, &manifest, headless, resumes.first().copied())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_after_copy_cannot_be_fingerprinted_as_the_reconstructed_input() -> Result<()> {
        let root = tempfile::tempdir()?;
        let copy = tempfile::tempdir()?;
        command(root.path(), &["init", "-q"])?;
        fs::write(root.path().join("source.rs"), "original")?;
        fs::copy(root.path().join("source.rs"), copy.path().join("source.rs"))?;
        let repositories = [PathBuf::new()];
        let copied_paths = BTreeSet::from([PathBuf::from("source.rs")]);
        let before = gate_source(root.path(), copy.path(), &repositories, &copied_paths)?;
        fs::write(root.path().join("source.rs"), "edited after copy")?;
        assert!(gate_source(root.path(), copy.path(), &repositories, &copied_paths).is_err());
        fs::copy(root.path().join("source.rs"), copy.path().join("source.rs"))?;
        assert_ne!(
            before,
            gate_source(root.path(), copy.path(), &repositories, &copied_paths)?
        );
        fs::write(
            copy.path().join("source.rs"),
            "edited only in reconstruction",
        )?;
        assert!(gate_source(root.path(), copy.path(), &repositories, &copied_paths).is_err());
        fs::remove_file(root.path().join("source.rs"))?;
        assert!(gate_source(root.path(), copy.path(), &repositories, &copied_paths).is_err());
        Ok(())
    }

    #[test]
    fn headless_keeps_local_commands_and_only_skips_named_network_cases() -> Result<()> {
        let plan = checks()?;
        for command in &plan.full[..plan.full.len() - 2] {
            ensure!(
                plan.headless.contains(command),
                "missing local command: {command:?}"
            );
        }
        let gateways: Vec<_> = plan
            .headless
            .iter()
            .filter(|c| c.iter().any(|arg| arg == "gateway::"))
            .collect();
        assert_eq!(gateways.len(), 1);
        let command = gateways[0];
        let delimiter = command
            .iter()
            .position(|arg| arg == "--")
            .context("test args")?;
        let expected: Vec<_> = plan
            .deferred_r6_tests
            .iter()
            .flat_map(|name| ["--skip".to_owned(), name.clone()])
            .collect();
        assert_eq!(command[delimiter + 1..], expected);
        assert_eq!(plan.deferred_r6_tests.len(), 3);
        for required in [
            vec!["cargo", "test", "--locked", "--lib", "--bins"],
            vec![
                "cargo",
                "test",
                "--manifest-path",
                "zor/Cargo.toml",
                "--all-features",
                "--locked",
            ],
            vec![
                "cargo",
                "check",
                "--manifest-path",
                "zor/Cargo.toml",
                "--no-default-features",
                "--all-targets",
                "--locked",
            ],
            vec!["sh", "tests/verify/release-package.sh", "--allow-dirty"],
            vec![
                "cargo",
                "fmt",
                "--manifest-path",
                "tools/xtask/Cargo.toml",
                "--check",
            ],
            vec![
                "cargo",
                "clippy",
                "--manifest-path",
                "tools/xtask/Cargo.toml",
                "--all-targets",
                "--locked",
                "--",
                "-D",
                "warnings",
            ],
            vec![
                "cargo",
                "test",
                "--manifest-path",
                "tools/xtask/Cargo.toml",
                "--locked",
            ],
        ] {
            assert!(
                plan.headless
                    .contains(&required.into_iter().map(str::to_owned).collect())
            );
        }
        assert!(
            !plan
                .headless
                .iter()
                .any(|command| command.iter().any(|arg| arg == "gateway"))
        );
        assert!(
            plan.headless.contains(
                &[
                    "cargo",
                    "test",
                    "--manifest-path",
                    "zor/tools/xtask/Cargo.toml",
                    "--locked",
                    "--bin",
                    "zor-xtask",
                    "capture_startup"
                ]
                .map(str::to_owned)
                .to_vec()
            )
        );
        assert!(plan.headless.contains(&vec![
            "cargo".into(),
            "run".into(),
            "--manifest-path".into(),
            "zor/tools/xtask/Cargo.toml".into(),
            "--locked".into(),
            "--bin".into(),
            "zor-xtask".into(),
            "--".into(),
            "verify-opencode-events".into()
        ]));
        for owner in ["tools/xtask/Cargo.toml", "zor/tools/xtask/Cargo.toml"] {
            for check in ["fmt", "clippy", "test"] {
                ensure!(
                    plan.headless
                        .iter()
                        .any(|command| command.get(1).is_some_and(|arg| arg == check)
                            && command.iter().any(|arg| arg == owner)),
                    "missing {owner} {check}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn invalid_profiles_fail_before_repository_access() {
        for args in [
            vec!["export", "--headless"],
            vec!["verify", "--headless"],
            vec!["apply", "--build"],
            vec!["unknown"],
        ] {
            assert!(run(args.into_iter().map(str::to_owned).collect()).is_err());
        }
    }

    #[test]
    fn real_git_patch_reconstructs_new_deleted_and_modified_files_without_overwriting_divergence()
    -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let owner = temporary.path().join("owner");
        fs::create_dir(&owner)?;
        command(&owner, &["init", "-q"])?;
        fs::write(owner.join("changed"), "before\n")?;
        fs::write(owner.join("deleted"), "remove me\n")?;
        command(&owner, &["add", "."])?;
        command(
            &owner,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "fixture baseline",
            ],
        )?;
        let base = head(&owner)?;
        let mut spec = Spec {
            path: owner.clone(),
            repository: owner.to_string_lossy().into_owned(),
            base: base.clone(),
            patch: "source.patch".into(),
        };
        check_base(&owner, &spec)?;
        // A checkout at a different revision must fail before patch assembly,
        // including when CI supplies the checkout rather than the manifest cloner.
        spec.base = "0000000000000000000000000000000000000000".into();
        assert!(check_base(&owner, &spec).is_err());
        fs::write(owner.join("changed"), "after\n")?;
        fs::remove_file(owner.join("deleted"))?;
        fs::write(owner.join("new-file"), [0, 1, 2, 255])?;
        let patch = temporary.path().join("source.patch");
        fs::write(&patch, snapshot_patch(&owner, &base)?)?;
        let reconstructed = temporary.path().join("reconstructed");
        clone_at(&owner, &owner, &reconstructed, &base)?;
        apply(&reconstructed, &patch)?;
        assert_eq!(source_files(&owner)?, source_files(&reconstructed)?);
        apply(&reconstructed, &patch)?;
        fs::write(reconstructed.join("unrelated"), "user change")?;
        assert!(apply(&reconstructed, &patch).is_err());
        assert_eq!(
            fs::read_to_string(reconstructed.join("unrelated"))?,
            "user change"
        );
        Ok(())
    }
}
