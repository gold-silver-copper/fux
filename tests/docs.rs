//! Every example the docs show runs as it stands, or says why not, so a
//! stale one fails here. The docs are the source: each fenced block is
//! read from the page and done as its fence says.
//!
//! - `sh`: run by a shell with `set -e -o pipefail`, the page's blocks in
//!   order in one directory, on a private server, so every command's
//!   status counts. A block says the failures it expects (`|| [ $? -eq
//!   124 ]`).
//! - `grift`: a program, run by `fux --file` there; `grift config`, a
//!   config, as a typing terminal loads one.
//! - `text`: a record as fux prints it: the fields it shows, in its order,
//!   are fields of a record `docs/records.txt` lists (the code's list), in
//!   that order. `...` leaves fields out.
//! - `LANG run by tests/agents.rs`: run there, under its heading.
//! - `LANG not run here: WHO`: run by a harness outside cargo test (agentbench
//!   on CI), named with its task.
//! - `LANG not run: WHY`: needs what a test can't have (a person's TUI in
//!   pane 2, another machine); the names its programs use are bound.
//!
//! A block of any other kind fails. The cookbook (`(explain "cookbook")`),
//! the README, the guide's topics (`(explain "guide")`), and
//! `docs/records.md` with `docs/data.md` each run on a server of their own,
//! which starts with a shell in pane 1. The agent page (`(explain
//! "agents")`) runs as an agent types it, in one bash with no server: it
//! starts its own, and stops it. None has `XDG_RUNTIME_DIR`, and each has
//! its `TMPDIR` in its directory. What a program prints is never compared.
//!
//! Every program the docs and help show as `fux '…'`, and every `grift`
//! block (a page's later block may use what an earlier one defined), names
//! only what exists. records.md's field tables are records.txt's lists.
// They script `/bin/sh` over a Unix socket (WINDOWS.md, "What PR 8 found").
#![cfg(unix)]
mod support;
#[path = "support/unbound.rs"]
mod unbound;
use std::collections::BTreeSet;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use support::*;

const COOKBOOK: &str = include_str!("../docs/cookbook.md");
const README: &str = include_str!("../README.md");
const AGENTS: &str = include_str!("../docs/agents.md");
const RECORDS: &str = include_str!("../docs/records.md");
const DATA: &str = include_str!("../docs/data.md");

/// Every page with examples, and the help text around the words'.
const PAGES: &[(&str, &str)] = &[
    ("docs/cookbook.md", COOKBOOK),
    ("README.md", README),
    ("docs/agents.md", AGENTS),
    ("docs/records.md", RECORDS),
    ("docs/data.md", DATA),
    ("WINDOWS.md", include_str!("../WINDOWS.md")),
    ("src/help-usage.txt", include_str!("../src/help-usage.txt")),
    (
        "src/help-running.txt",
        include_str!("../src/help-running.txt"),
    ),
    (
        "src/help-settings.txt",
        include_str!("../src/help-settings.txt"),
    ),
];

/// The tests a block may be run by instead, and their source.
const ELSEWHERE: &[(&str, &str)] = &[("tests/agents.rs", include_str!("agents.rs"))];

/// The agent page, `(explain "agents")`: `docs/agents.md` before its first
/// topic; and its topics.
fn agent_page() -> (&'static str, &'static str) {
    AGENTS.split_once("\n## ").unwrap_or((AGENTS, ""))
}

/// Programs an example may use beyond `sh` and fux; one that names one
/// the machine lacks is skipped, saying so.
const NEEDS: &[&str] = &["timeout", "ps", "pgrep", "prlimit", "python3"];

/// How long one block may run before it fails as hung.
const HUNG: Duration = Duration::from_secs(120);

/// A fenced block: the heading it is under (`intro` before the first), its
/// fence's words after the backticks, and its text, its fence's indent
/// taken off.
struct Block {
    heading: &'static str,
    info: &'static str,
    text: String,
}

/// What a block's fence says to do with it.
enum How {
    Shell,
    Program,
    Config,
    Shape,
    NotRun,
    Elsewhere,
}

impl Block {
    fn how(&self) -> Result<How, String> {
        let (lang, note) = self.info.split_once(' ').unwrap_or((self.info, ""));
        if (note.strip_prefix("not run: "))
            .or_else(|| note.strip_prefix("not run here: "))
            .is_some_and(|why| !why.is_empty())
        {
            return Ok(How::NotRun);
        }
        if let Some(test) = note.strip_prefix("run by ") {
            let (_, source) = (ELSEWHERE.iter().find(|(name, _)| *name == test))
                .ok_or_else(|| format!("{}: run by {test:?}, no test here", self.heading))?;
            // The test finds it by its heading: `## topic:`, or `### title`.
            let key = match self.heading.starts_with("### ") {
                true => self.heading,
                false => self.heading.split_inclusive(':').next().unwrap_or_default(),
            };
            return match source.contains(key) {
                true => Ok(How::Elsewhere),
                false => Err(format!("{key:?}: run by {test}, which never names it")),
            };
        }
        match (lang, note) {
            ("sh", "") => Ok(How::Shell),
            ("grift", "") => Ok(How::Program),
            ("grift", "config") => Ok(How::Config),
            ("text", "") => Ok(How::Shape),
            _ => Err(format!(
                "{}: a {:?} block, which nothing runs or checks",
                self.heading, self.info
            )),
        }
    }
}

/// Each fenced block of `doc`, in order. A fence this can't take, one
/// never closed or closed at another indent, is refused, so no block goes
/// unread.
fn blocks(doc: &'static str) -> Result<Vec<Block>, String> {
    let (mut blocks, mut heading) = (Vec::new(), "intro");
    let mut open: Option<(&str, &str, String)> = None;
    for line in doc.lines() {
        let fence = line.trim_start();
        let indent = line.strip_suffix(fence).unwrap_or_default();
        match (&mut open, fence.strip_prefix("```")) {
            (None, Some(info)) => open = Some((indent, info, String::new())),
            (Some((at, _, _)), Some("")) if *at == indent => {
                if let Some((_, info, text)) = open.take() {
                    blocks.push(Block {
                        heading,
                        info,
                        text,
                    });
                }
            }
            (Some(_), Some(_)) => {
                return Err(format!("{heading}: a fence the test can't take: {line:?}"));
            }
            (Some((at, _, text)), None) => {
                text.extend([line.strip_prefix(*at).unwrap_or(line), "\n"])
            }
            (None, None) if line.starts_with("## ") || line.starts_with("### ") => heading = line,
            (None, None) => {}
        }
    }
    match open {
        Some((_, info, _)) => Err(format!("{heading}: a {info:?} block never closed")),
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
/// `server` (or on none, when `cold`) with this build's fux first on the
/// PATH, in a process group of its own, which is killed once it ends or
/// hangs, whatever it left running: its exit status and what it said.
fn run(
    sh: &str,
    cold: bool,
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
    let mut command = Command::new(sh);
    match cold {
        true => command.env_remove("FUX_SOCKET"),
        false => command.env("FUX_SOCKET", &server.socket),
    };
    let mut child = command
        .args(["-e", "-o", "pipefail", "-c", script])
        .current_dir(dir)
        .process_group(0)
        .env("PATH", std::env::join_paths(path).map_err(e)?)
        .env("TMPDIR", dir)
        .env_remove("XDG_RUNTIME_DIR")
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
    if let Some(group) = i32::try_from(child.id())
        .ok()
        .and_then(fuxix::process::Pid::from_raw)
    {
        let _ = fuxix::process::kill_group(group, fuxix::process::Signal::Kill);
    }
    let _ = child.wait();
    Ok((status, std::fs::read_to_string(&said).unwrap_or_default()))
}

#[test]
fn every_recipe_in_the_cookbook_runs() -> Outcome {
    let blocks = blocks(COOKBOOK)?;
    assert!(blocks.len() >= 10, "{} blocks", blocks.len());
    for skipped in runs(shell()?, blocks, false)? {
        eprintln!("{skipped}");
    }
    Ok(())
}

/// The page whole, whose `jobs` workspace outlives the agent's, so its
/// stop kills the server; and its server, tab and stop alone, where the
/// agent's workspace is the server's last, so the server has ended. By
/// bash, as an agent's shell is: its `kill` fails on an empty pid, where
/// bash as `sh` says so and goes on.
#[test]
fn every_command_on_the_agent_page_runs() -> Outcome {
    let mut page = Vec::new();
    for block in blocks(agent_page().0)? {
        match block.how()? {
            How::Shell => page.push(block.text),
            How::Program | How::Config | How::Shape | How::NotRun | How::Elsewhere => {
                return Err(format!(
                    "{}: a {:?} block on the page",
                    block.heading, block.info
                ));
            }
        }
    }
    let (Some(start), Some(tab), Some(stop)) = (page.first(), page.get(1), page.last()) else {
        return Err(format!("{} blocks on the page", page.len()));
    };
    let alone = [start.as_str(), tab, stop].concat();
    let block = |heading, text| Block {
        heading,
        info: "sh",
        text,
    };
    let runs = runs(
        "bash",
        vec![
            block("the agent page", page.concat()),
            block("its stop alone", alone),
        ],
        true,
    )?;
    for skipped in runs {
        eprintln!("{skipped}");
    }
    Ok(())
}

#[test]
fn every_command_in_the_readme_runs() -> Outcome {
    for skipped in runs(shell()?, blocks(README)?, false)? {
        eprintln!("{skipped}");
    }
    Ok(())
}

#[test]
fn every_example_in_the_guide_s_topics_runs() -> Outcome {
    for skipped in runs(shell()?, blocks(agent_page().1)?, false)? {
        eprintln!("{skipped}");
    }
    Ok(())
}

#[test]
fn every_example_of_records_and_data_runs() -> Outcome {
    let mut both = blocks(RECORDS)?;
    both.extend(blocks(DATA)?);
    for skipped in runs(shell()?, both, false)? {
        eprintln!("{skipped}");
    }
    Ok(())
}

/// Runs `blocks` in order on a private server with a shell in pane 1, or
/// on none when `cold`. What it skipped, saying why.
fn runs(sh: &str, blocks: Vec<Block>, cold: bool) -> Result<Vec<String>, String> {
    let server = Server::start("")?;
    if !cold {
        server.ok(&["(new-tab (size (in (options) \"docs\") 80 24))"])?;
    }
    let dir = server.dir.join("recipes");
    std::fs::create_dir_all(&dir).map_err(e)?;
    // Recipes name the recipe files where a checkout of fux has them.
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("recipes");
    std::os::unix::fs::symlink(checkout, dir.join("recipes")).map_err(e)?;
    let _started = Started(dir.clone());
    let records = records_txt();
    let mut skipped = Vec::new();
    for (i, block) in blocks.into_iter().enumerate() {
        let Block { heading, text, .. } = &block;
        let script = match block.how()? {
            How::Shell => text.clone(),
            How::Program => {
                let file = format!(".block-{i}.grift");
                std::fs::write(dir.join(&file), text).map_err(e)?;
                format!("fux --file {file}\n")
            }
            How::Config => {
                let config = dir.join(format!("config-{i}.grift"));
                std::fs::write(&config, text).map_err(e)?;
                let ran = fux::script::run_file(&config).map(drop);
                assert!(ran.is_ok(), "{heading}:\n{text}\n{ran:?}");
                continue;
            }
            How::Shape => {
                for mut shown in shown_records(text) {
                    // A record holding a server run prints it last, as
                    // `run`, which records.txt lists only where it is a field.
                    let run = shown.last().is_some_and(|last| last == "run");
                    let listed = |shown: &[String]| {
                        records.iter().any(|(_, fields)| in_order(shown, fields))
                    };
                    let listed = listed(&shown) || (run && shown.pop().is_some() && listed(&shown));
                    assert!(
                        listed,
                        "{heading}: records.txt lists no record of {shown:?}"
                    );
                }
                continue;
            }
            How::NotRun | How::Elsewhere => continue,
        };
        let named = text.split(|c: char| !c.is_alphanumeric());
        if let Some(missing) = named.into_iter().find(|w| NEEDS.contains(w) && !found(w)) {
            skipped.push(format!(
                "skipped {heading:?}: it needs {missing}, which is missing"
            ));
            continue;
        }
        let (status, said) = run(sh, cold, &server, &dir, i, &script)?;
        assert_eq!(status, 0, "{heading}:\n{text}\n{said}");
    }
    Ok(skipped)
}

/// The names any program sees: fux's, its libraries' and grift's.
fn known(name: &str) -> bool {
    fux::script::Word::named(name).is_some()
        || fux::script::docs().is_ok_and(|docs| docs.iter().any(|doc| doc.name == name))
        || fux::script::language().is_ok_and(|names| names.iter().any(|n| n == name))
        || fux::script::HOST.iter().any(|(host, _)| *host == name)
}

/// Every program the docs and help show, as `fux '…'` or a `grift` block,
/// reads, and names only what it binds or fux has; a page's `grift` blocks
/// read as one, as a later one may use what an earlier one defined.
#[test]
fn every_program_shown_names_what_exists() -> Outcome {
    let mut unbound_names = Vec::new();
    // `...` in a shown program leaves out what doesn't matter there, and a
    // name in capitals (`PANE`, `ARGUMENT...`) stands for what you give.
    let placeholder = |name: &str| {
        let name = name.trim_end_matches("...");
        name.is_empty() || name.chars().all(|c| c.is_ascii_uppercase() || c == '-')
    };
    let known = |name: &str| placeholder(name) || known(name);
    for (page, text) in PAGES {
        let mut shown = String::new();
        for block in blocks(text)? {
            if block.info.split(' ').next() == Some("grift") {
                let required = required(&block.text).ok_or(block.heading)?;
                shown.push_str(&required);
                shown.push_str(&block.text);
                let unbound = unbound::unbound(&shown, &known)?;
                if !unbound.is_empty() {
                    unbound_names.push(format!("{page}: {}: {unbound:?}", block.heading));
                }
            }
        }
        for program in text.split("fux '").skip(1) {
            let program = program.split_once('\'').map_or(program, |(p, _)| p);
            let Some(required) = required(program) else {
                continue;
            };
            let program = required + program;
            let unbound = unbound::unbound(&program, &known).map_err(|e| format!("{page}: {e}"))?;
            if !unbound.is_empty() {
                unbound_names.push(format!("{page}: {program}: {unbound:?}"));
            }
        }
    }
    assert!(unbound_names.is_empty(), "{unbound_names:#?}");
    Ok(())
}

/// What `program` requires (`(require! "recipes/…")`), from fux's checkout;
/// none where it requires a file a page writes first, which its run checks.
fn required(program: &str) -> Option<String> {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"));
    let paths = program.split("(require! \"").skip(1);
    let paths = paths.map(|rest| rest.split_once('"').map_or(rest, |(path, _)| path));
    paths
        .map(|path| std::fs::read_to_string(checkout.join(path)).ok())
        .collect()
}

/// Each record `docs/records.txt` lists: where it is, and its fields in
/// order.
fn records_txt() -> Vec<(&'static str, Vec<&'static str>)> {
    let lines = include_str!("../docs/records.txt").lines();
    (lines.filter(|line| !line.starts_with('#')))
        .filter_map(|line| line.split_once(": "))
        .map(|(at, fields)| (at, fields.split(' ').collect()))
        .collect()
}

/// Whether every one of `shown` is in `fields`, in the same order.
fn in_order(shown: &[String], fields: &[&str]) -> bool {
    let mut fields = fields.iter();
    !shown.is_empty() && shown.iter().all(|s| fields.any(|f| f == s))
}

/// The fields each record a shape shows holds, in order: a record is a
/// list at the shape's top, and a field a list in it, named by its first
/// word.
fn shown_records(shape: &str) -> Vec<Vec<String>> {
    let (mut records, mut depth, mut named) = (Vec::<Vec<String>>::new(), 0_usize, false);
    let mut chars = shape.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '(' => {
                depth = depth.saturating_add(1);
                named = depth == 2;
                if depth == 1 {
                    records.push(Vec::new());
                }
            }
            ')' => {
                depth = depth.saturating_sub(1);
                named = false;
            }
            '"' => {
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => {}
                    }
                }
                named = false;
            }
            c if c.is_whitespace() => {}
            c => {
                let mut word = String::from(c);
                while let Some(c) = chars.next_if(|c| !c.is_whitespace() && !"()\"".contains(*c)) {
                    word.push(c);
                }
                if std::mem::take(&mut named)
                    && let Some(record) = records.last_mut()
                {
                    record.push(word);
                }
            }
        }
    }
    records
}

/// records.md's tables of fields are records.txt's: each table, or each
/// part of one named for a field (`terminal`'s `…`), lists a record's
/// fields, all of them in order; its table of events, every event.
#[test]
fn the_records_page_lists_the_code_s_fields() -> Outcome {
    let records = records_txt();
    // Each table's first column: its head, then its rows.
    let (mut tables, mut table) = (Vec::new(), Vec::new());
    for line in RECORDS.lines() {
        match line.strip_prefix("| ").and_then(|l| l.split(" |").next()) {
            Some(cell) => table.push(cell),
            None if !table.is_empty() => tables.push(std::mem::take(&mut table)),
            None => {}
        }
    }
    let names = |cell: &'static str| -> Vec<(&'static str, &'static str)> {
        let fields = cell.split(", ").filter_map(|field| {
            let (of, name) = field.split_once("'s ").unwrap_or(("", field));
            let name = name.strip_prefix('`')?.strip_suffix('`')?;
            Some((of.trim_matches('`'), name))
        });
        fields.collect()
    };
    let (mut fields, mut events) = (0, 0);
    for table in tables {
        let (head, rows) = (table.first().copied(), table.into_iter().skip(2));
        match head.unwrap_or_default() {
            "Field" => {
                let mut parts: Vec<(&str, Vec<&str>)> = Vec::new();
                for (of, name) in rows.flat_map(names) {
                    match parts.last_mut() {
                        Some((at, names)) if *at == of => names.push(name),
                        _ => parts.push((of, vec![name])),
                    }
                }
                for (of, names) in parts {
                    let ours = |at: &str| of.is_empty() || at.ends_with(&format!(".{of}"));
                    let listed = records.iter().any(|(at, f)| ours(at) && *f == names);
                    assert!(listed, "records.txt lists no record of just {of} {names:?}");
                    fields += 1;
                }
            }
            "`event`" => {
                let shown: BTreeSet<&str> = rows.flat_map(names).map(|(_, n)| n).collect();
                let listed: BTreeSet<&str> = (records.iter())
                    .filter_map(|(at, _)| at.strip_prefix("events.events[]["))
                    .filter_map(|at| at.strip_suffix(']'))
                    .collect();
                assert_eq!(shown, listed, "records.md's events, records.txt's");
                events += 1;
            }
            _ => {}
        }
    }
    assert!(
        fields >= 4 && events == 1,
        "{fields} field tables, {events} of events"
    );
    Ok(())
}
