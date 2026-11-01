//! Every recipe of the cookbook (`docs/cookbook.md`, `fux help cookbook`)
//! runs as it stands, so a stale one fails here: its `sh` blocks in
//! order, each by `sh -e` in one directory, on a private server; its
//! `fux` blocks, which are configs, as a typing terminal runs one. Each
//! must succeed. What they print is never compared.
mod support;
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

/// Each fenced block, in order: the heading it is under, its language and
/// its text.
fn blocks() -> Vec<(&'static str, &'static str, &'static str)> {
    let mut blocks = Vec::new();
    for section in COOKBOOK.split("\n## ").skip(1) {
        let heading = section.lines().next().unwrap_or_default();
        let mut rest = section;
        while let Some((_, from)) = rest.split_once("\n```") {
            let Some((lang, from)) = from.split_once('\n') else {
                break;
            };
            let Some((block, after)) = from.split_once("```") else {
                break;
            };
            blocks.push((heading, lang, block));
            rest = after;
        }
    }
    blocks
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

/// Runs `script` by `sh -e` in `dir`, as a shell on `server` with this
/// build's fux first on the PATH: its exit status and what it said.
fn run(server: &Server, dir: &Path, i: usize, script: &str) -> Result<(i32, String), String> {
    let bin = Path::new(fux()).parent().ok_or("fux is in no directory")?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::iter::once(bin.to_owned()).chain(std::env::split_paths(&path));
    let said = dir.join(format!(".recipe-{i}.out"));
    let file = std::fs::File::create(&said).map_err(e)?;
    let mut child = Command::new("sh")
        .args(["-ec", script])
        .current_dir(dir)
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
        if let Some(status) = child.try_wait().map_err(e)? {
            break status.code().unwrap_or(-1);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break -1;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok((status, std::fs::read_to_string(&said).unwrap_or_default()))
}

#[test]
fn every_recipe_in_the_cookbook_runs() -> Outcome {
    let server = Server::start("")?;
    let dir = server.dir.join("recipes");
    std::fs::create_dir_all(&dir).map_err(e)?;
    let _started = Started(dir.clone());
    let blocks = blocks();
    assert!(blocks.len() >= 10, "{} blocks", blocks.len());
    for (i, (heading, lang, block)) in blocks.into_iter().enumerate() {
        match lang {
            "sh" => {
                let named = block.split(|c: char| !c.is_alphanumeric());
                if let Some(missing) = named.into_iter().find(|w| NEEDS.contains(w) && !found(w)) {
                    eprintln!("skipped {heading:?}: it needs {missing}, which is missing");
                    continue;
                }
                let (status, said) = run(&server, &dir, i, block)?;
                assert_eq!(status, 0, "{heading}:\n{block}\n{said}");
            }
            "fux" => {
                let config = dir.join(format!("config-{i}.fux"));
                std::fs::write(&config, block).map_err(e)?;
                let ran = fux::script::run_file(&config).map(drop);
                assert!(ran.is_ok(), "{heading}:\n{block}\n{ran:?}");
            }
            _ => return Err(format!("{heading}: a {lang:?} block, which nothing runs")),
        }
    }
    Ok(())
}
