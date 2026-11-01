//! Every recipe of the cookbook (`docs/cookbook.md`, `fux help cookbook`)
//! runs as it stands, so a stale one fails here: its `sh` blocks in
//! order, each by a shell with `set -e -o pipefail` in one directory, on
//! a private server, so every command's status counts; its `fux` blocks,
//! which are configs, as a typing terminal runs one. Each must succeed: a
//! recipe says the failures it expects (`|| [ $? -eq 124 ]`). What they
//! print is never compared.
mod support;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use support::*;

const COOKBOOK: &str = include_str!("../docs/cookbook.md");

/// Programs a recipe may use beyond `sh` and fux; a recipe that names one
/// the machine lacks is skipped, saying so.
const NEEDS: &[&str] = &["jq", "timeout"];

/// How long one block may run before it fails as hung.
const HUNG: Duration = Duration::from_secs(120);

/// Each fenced block, in order: the heading it is under (`intro` before
/// the first), its language and its text. A fence this can't take, one
/// indented or never closed, is refused, so no block goes unrun.
fn blocks() -> Result<Vec<(&'static str, &'static str, String)>, String> {
    let (mut blocks, mut heading, mut open) = (Vec::new(), "intro", None);
    for line in COOKBOOK.lines() {
        match (&mut open, line.strip_prefix("```")) {
            (None, Some(lang)) => open = Some((lang, String::new())),
            (Some(_), Some("")) => {
                if let Some((lang, block)) = open.take() {
                    blocks.push((heading, lang, block));
                }
            }
            (Some((_, block)), None) => block.extend([line, "\n"]),
            _ if line.trim_start().starts_with("```") => {
                return Err(format!("{heading}: a fence the test can't take: {line:?}"));
            }
            _ => heading = line.strip_prefix("## ").unwrap_or(heading),
        }
    }
    match open {
        Some((lang, _)) => Err(format!("{heading}: a {lang:?} block never closed")),
        None => Ok(blocks),
    }
}

/// A shell that has `pipefail`: `sh` where it does, else `bash`.
fn shell() -> Result<&'static str, String> {
    let has = |sh: &str| {
        Command::new(sh)
            .args(["-c", "set -o pipefail"])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    (["sh", "bash"].into_iter().find(|sh| has(sh)))
        .ok_or_else(|| "no sh or bash with set -o pipefail to run the recipes".to_owned())
}

/// Whether `sh` finds `program`.
fn found(program: &str) -> bool {
    Command::new("sh")
        .args(["-c", "command -v \"$0\"", program])
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The servers recipes start in `dir` (each a `fux.sock` beneath it),
/// stopped when the test ends, however it ends.
struct Started(PathBuf);

impl Drop for Started {
    fn drop(&mut self) {
        let Ok(entries) = std::fs::read_dir(&self.0) else {
            return;
        };
        for entry in entries.flatten() {
            let socket = entry.path().join("fux.sock");
            if socket.exists() {
                let _ = stop_server(&socket);
            }
        }
    }
}

/// Runs `script` by `sh` with `-e -o pipefail` in `dir`, as a shell on
/// `server` with this build's fux first on the PATH, in a process group
/// of its own, which is killed once it ends or hangs, whatever it left
/// running: its exit status and what it said.
fn run(
    sh: &str,
    server: &Server,
    dir: &Path,
    i: usize,
    script: &str,
) -> Result<(i32, String), String> {
    let bin = Path::new(fux()).parent().ok_or("fux is in no directory")?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::iter::once(bin.to_owned()).chain(std::env::split_paths(&path));
    let said = dir.join(format!(".recipe-{i}.out"));
    let file = std::fs::File::create(&said).map_err(e)?;
    let mut child = Command::new(sh)
        .args(["-e", "-o", "pipefail", "-c", script])
        .current_dir(dir)
        .process_group(0)
        .env("PATH", std::env::join_paths(path).map_err(e)?)
        .env("FUX_SOCKET", &server.socket)
        .env("SHELL", "/bin/sh")
        .env("XDG_CONFIG_HOME", server.dir.join("config"))
        .env_remove("FUX_PANE")
        .env_remove("FUX_SERVER")
        .stdin(Stdio::null())
        .stdout(file.try_clone().map_err(e)?)
        .stderr(file)
        .spawn()
        .map_err(e)?;
    let deadline = after(HUNG);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(-1),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) | Err(_) => break -1,
        }
    };
    if let Some(group) = fuxix::process::Pid::of(&child) {
        let _ = fuxix::process::kill_group(group, fuxix::process::Signal::Kill);
    }
    let _ = child.wait();
    Ok((status, std::fs::read_to_string(&said).unwrap_or_default()))
}

#[test]
fn every_recipe_in_the_cookbook_runs() -> Outcome {
    let server = Server::start("")?;
    let dir = server.dir.join("recipes");
    std::fs::create_dir_all(&dir).map_err(e)?;
    let _started = Started(dir.clone());
    let sh = shell()?;
    let blocks = blocks()?;
    assert!(blocks.len() >= 10, "{} blocks", blocks.len());
    for (i, (heading, lang, block)) in blocks.into_iter().enumerate() {
        match lang {
            "sh" => {
                let named = block.split(|c: char| !c.is_alphanumeric());
                if let Some(missing) = named.into_iter().find(|w| NEEDS.contains(w) && !found(w)) {
                    eprintln!("skipped {heading:?}: it needs {missing}, which is missing");
                    continue;
                }
                let (status, said) = run(sh, &server, &dir, i, &block)?;
                assert_eq!(status, 0, "{heading}:\n{block}\n{said}");
            }
            "fux" => {
                let config = dir.join(format!("config-{i}.fux"));
                std::fs::write(&config, &block).map_err(e)?;
                let ran = fux::script::run_file(&config).map(drop);
                assert!(ran.is_ok(), "{heading}:\n{block}\n{ran:?}");
            }
            _ => return Err(format!("{heading}: a {lang:?} block, which nothing runs")),
        }
    }
    Ok(())
}
