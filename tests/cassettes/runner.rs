// Script cassettes (`tests/cassettes/README.md`): each
// `tests/cassettes/NAME.cassette` replayed on a fresh private server
// through fux's own interface. A replay checks the cassette's `=` lines
// and every step's status, then, whatever the cassette is for, the
// server's health, every call's status and code, the journal, and fux's
// observable promises, each reported held, broken or uncovered.
//
// `cargo test --test cassettes -- [--update] [--keep] [--dir PATH] [NAME…]`;
// `FUX_BIN` replays against another build, `FUX_BASE` names a second one
// for cassettes about two builds.
use fux_wire::code::Code;
use fux_wire::command::{Run, Version};
use fux_wire::events::Cursor;
use fux_wire::id::PaneId;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::iter::Peekable;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::str::Chars;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// A cassette file, and every cassette together.
const FILE_BYTES: u64 = 64 << 10;
const ALL_BYTES: u64 = 2 << 20;
const CASSETTES: usize = 32;
const STEPS: usize = 64;
/// `=` lines in a step, and in a cassette.
const STEP_CHECKS: usize = 16;
const CHECKS: usize = 48;
/// `~` lines kept for a step, their length, and for the journal.
const SEEN_LINES: usize = 40;
const SEEN_CHARS: usize = 200;
const JOURNAL_SEEN: usize = 256;
/// What a step may print on each stream, a snapshot or a batch may hold,
/// and the call log (each call's fields are cut to 4,000 characters).
const OUTPUT_BYTES: u64 = 1 << 20;
const LOG_BYTES: u64 = 2 << 20;
/// What the monitor holds: calls and journal events; and the entries of
/// any JSON array or object it reads (so 256 panes in a snapshot, 256
/// arguments in a call), each refused before it is kept.
const CALLS: usize = 1024;
const EVENTS: usize = 4096;
const ITEMS: usize = 256;
/// Seconds: a cassette's default and longest, and all of them together.
const DEFAULT_SECS: u64 = 10;
const MOST_SECS: u64 = 60;
const ALL_SECS: u64 = 60;
/// Seconds stopping the server may take: cleanup, bounded apart from the
/// replay's own time.
const AFTER_SECS: u64 = 10;
/// Cassettes replayed at once, at most.
const THREADS: usize = 4;
/// How deep the JSON fux prints nests: past any it makes.
const DEEPEST: usize = 32;

const REPO: &str = env!("CARGO_MANIFEST_DIR");

pub fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            say(&format!("cassettes: {e}"));
            ExitCode::FAILURE
        }
    }
}

fn say(text: &str) {
    let _ = writeln!(std::io::stdout().lock(), "{text}");
}

fn e(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// When a wait that starts now gives up.
fn after(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait).unwrap_or(now)
}

fn run() -> Result<bool, String> {
    let options = Options::read()?;
    let ctx = Ctx::new()?;
    let cassettes = load(&options)?;
    if cassettes.is_empty() {
        return Err(format!("no cassette in {} matches", options.dir.display()));
    }
    if options.update {
        let mut ok = true;
        for cassette in &cassettes {
            match update(cassette, &ctx, &options) {
                Ok(()) => say(&format!("recorded {}", cassette.name)),
                Err(why) => {
                    say(&format!("not recorded {}: {why}", cassette.name));
                    ok = false;
                }
            }
        }
        return Ok(ok);
    }
    let started = Instant::now();
    let suite = after(Duration::from_secs(ALL_SECS));
    let reports = replay_all(&cassettes, &ctx, &options, suite);
    let mut failed = 0usize;
    for report in &reports {
        say(&report.summary());
        if !report.problems.is_empty() {
            failed = failed.saturating_add(1);
        }
    }
    say(&coverage(&reports));
    let took = started.elapsed();
    if took > Duration::from_secs(ALL_SECS) {
        let slowest = reports.iter().max_by_key(|r| r.took);
        say(&format!(
            "the cassettes took {:.1}s, past the {ALL_SECS}s all may take; the slowest: {}",
            took.as_secs_f64(),
            slowest.map_or("none", |r| r.name.as_str())
        ));
        failed = failed.saturating_add(1);
    }
    say(&format!(
        "{} cassettes, {failed} failed, in {:.1}s",
        reports.len(),
        took.as_secs_f64()
    ));
    Ok(failed == 0)
}

struct Options {
    update: bool,
    keep: bool,
    dir: PathBuf,
    names: Vec<String>,
    threads: usize,
}

impl Options {
    fn read() -> Result<Options, String> {
        let mut options = Options {
            update: false,
            keep: false,
            dir: Path::new(REPO).join("tests/cassettes"),
            names: Vec::new(),
            threads: THREADS,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let threads = |n: Option<String>| -> Result<usize, String> {
                let n: usize = n
                    .and_then(|n| n.parse().ok())
                    .ok_or("--test-threads takes a number")?;
                Ok(n.clamp(1, THREADS))
            };
            match arg.as_str() {
                "--update" => options.update = true,
                "--keep" => options.keep = true,
                "--dir" => options.dir = args.next().ok_or("--dir takes a path")?.into(),
                "--test-threads" => options.threads = threads(args.next())?,
                // libtest's, which cargo may pass: they change nothing here.
                "--quiet" | "-q" | "--nocapture" => {}
                other => {
                    if let Some(n) = other.strip_prefix("--test-threads=") {
                        options.threads = threads(Some(n.to_owned()))?;
                    } else if other.starts_with('-') {
                        return Err(format!("no option {other}"));
                    } else {
                        options.names.push(other.to_owned());
                    }
                }
            }
        }
        Ok(options)
    }
}

/// What every replay shares: the builds, the shim, where replays live.
struct Ctx {
    fux: PathBuf,
    base: Option<OsString>,
    shim: PathBuf,
    scratch: PathBuf,
    path: OsString,
}

impl Ctx {
    fn new() -> Result<Ctx, String> {
        let fux = std::env::var_os("FUX_BIN")
            .map_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_fux")), PathBuf::from);
        Ok(Ctx {
            fux: std::fs::canonicalize(&fux).map_err(|why| format!("{}: {why}", fux.display()))?,
            base: std::env::var_os("FUX_BASE"),
            shim: Path::new(REPO).join("tests/cassettes/fux-record"),
            scratch: fuxix::file::scratch().map_err(e)?,
            path: std::env::var_os("PATH").unwrap_or_default(),
        })
    }
}

// The format.

#[derive(Clone)]
struct Cassette {
    name: String,
    /// The lines before the first step, as written.
    head: Vec<String>,
    timeout: Duration,
    /// The promises it is written to exercise (`# exercises:`).
    exercises: Vec<Promise>,
    steps: Vec<Step>,
    /// `@journal` and the comments before it, if it has one.
    journal: Option<Section>,
    /// Comments after everything else.
    tail: Vec<String>,
}

#[derive(Clone, Default)]
struct Section {
    /// Its comments, then its `$` and `>` lines (a step's) or `@journal`.
    text: Vec<String>,
    checks: Vec<String>,
    seen: Vec<Seen>,
}

#[derive(Clone)]
struct Step {
    command: String,
    lines: Section,
    /// `[N] CODE`: its status and code. None is status 0.
    status: Option<(i32, Option<String>)>,
}

/// A `~` line, or a `~?` one (noise: it differed between two recordings).
#[derive(Clone, PartialEq, Eq)]
struct Seen {
    noisy: bool,
    text: String,
}

fn load(options: &Options) -> Result<Vec<Cassette>, String> {
    let dir = &options.dir;
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|why| format!("{}: {why}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|x| x == "cassette"))
        .collect();
    paths.sort();
    if paths.len() > CASSETTES {
        return Err(format!(
            "{} holds {} cassettes, past the {CASSETTES} it may: delete the weakest, or fold it into a Rust test",
            dir.display(),
            paths.len()
        ));
    }
    let mut total = 0u64;
    let mut cassettes = Vec::new();
    for path in paths {
        let size = std::fs::metadata(&path).map_err(e)?.len();
        total = total.saturating_add(size);
        if size > FILE_BYTES || total > ALL_BYTES {
            return Err(format!(
                "{}: past {FILE_BYTES} bytes a cassette or {ALL_BYTES} in all",
                path.display()
            ));
        }
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = std::fs::read_to_string(&path).map_err(e)?;
        let cassette = parse(name, &text).map_err(|why| format!("{}: {why}", path.display()))?;
        if options.names.is_empty()
            || options
                .names
                .iter()
                .any(|n| cassette.name.contains(n.as_str()))
        {
            cassettes.push(cassette);
        }
    }
    Ok(cassettes)
}

fn parse(name: String, text: &str) -> Result<Cassette, String> {
    let mut lines = text.lines().zip(1usize..);
    if lines.next().map(|(line, _)| line) != Some("# fux cassette 1") {
        return Err("its first line is not `# fux cassette 1`".to_owned());
    }
    let mut cassette = Cassette {
        name,
        head: vec!["# fux cassette 1".to_owned()],
        timeout: Duration::from_secs(DEFAULT_SECS),
        exercises: Vec::new(),
        steps: Vec::new(),
        journal: None,
        tail: Vec::new(),
    };
    let mut guards = false;
    let mut pending: Vec<String> = Vec::new();
    for (line, n) in lines {
        let at = |why: &str| format!("line {n}: {why}");
        if let Some(command) = line.strip_prefix("$ ") {
            if cassette.journal.is_some() {
                return Err(at("a step after @journal"));
            }
            let mut text = std::mem::take(&mut pending);
            text.push(line.to_owned());
            cassette.steps.push(Step {
                command: command.to_owned(),
                lines: Section {
                    text,
                    ..Section::default()
                },
                status: None,
            });
        } else if let Some(more) = line.strip_prefix("> ").or((line == ">").then_some("")) {
            let step = cassette.steps.last_mut().filter(|s| {
                s.lines.checks.is_empty() && s.lines.seen.is_empty() && s.status.is_none()
            });
            let step = step.ok_or_else(|| at("a `>` line not right after its step"))?;
            step.command.push('\n');
            step.command.push_str(more);
            step.lines.text.push(line.to_owned());
        } else if line == "@journal" {
            let mut text = std::mem::take(&mut pending);
            text.push(line.to_owned());
            cassette.journal = Some(Section {
                text,
                ..Section::default()
            });
        } else if let Some(rest) = line.strip_prefix("  ") {
            let section = match (&mut cassette.journal, cassette.steps.last_mut()) {
                (Some(journal), _) => journal,
                (None, Some(step)) => {
                    if let Some(status) = rest.strip_prefix('[') {
                        step.status =
                            Some(status_line(status).ok_or_else(|| at("not `[N] CODE`"))?);
                        continue;
                    }
                    &mut step.lines
                }
                (None, None) => return Err(at("a check before any step")),
            };
            if let Some(check) = rest.strip_prefix("= ") {
                section.checks.push(check.to_owned());
            } else if let Some(text) = rest.strip_prefix("~? ").or((rest == "~?").then_some("")) {
                section.seen.push(Seen {
                    noisy: true,
                    text: text.to_owned(),
                });
            } else if let Some(text) = rest.strip_prefix("~ ").or((rest == "~").then_some("")) {
                section.seen.push(Seen {
                    noisy: false,
                    text: text.to_owned(),
                });
            } else {
                return Err(at("not a `=`, `~`, `~?` or `[N]` line"));
            }
        } else if line.is_empty() || line.starts_with('#') {
            if cassette.steps.is_empty() {
                cassette.head.push(line.to_owned());
                guards |= line.starts_with("# guards: ");
                if let Some(secs) = line.strip_prefix("# timeout: ") {
                    let secs: u64 = secs
                        .trim()
                        .parse()
                        .map_err(|_| at("a timeout is whole seconds"))?;
                    if secs == 0 || secs > MOST_SECS {
                        return Err(at(&format!("a timeout is 1 to {MOST_SECS} seconds")));
                    }
                    cassette.timeout = Duration::from_secs(secs);
                }
                if let Some(names) = line.strip_prefix("# exercises: ") {
                    for name in names.split_whitespace() {
                        let promise = Promise::ALL.into_iter().find(|p| p.name() == name);
                        cassette
                            .exercises
                            .push(promise.ok_or_else(|| at(&format!("no promise {name}")))?);
                    }
                }
            } else {
                pending.push(line.to_owned());
            }
        } else {
            return Err(at("not a line a cassette has"));
        }
    }
    cassette.tail = pending;
    if !guards {
        return Err("no `# guards:` line: the one property it protects".to_owned());
    }
    if cassette.steps.is_empty() || cassette.steps.len() > STEPS {
        return Err(format!(
            "{} steps: a cassette has 1 to {STEPS}",
            cassette.steps.len()
        ));
    }
    Ok(cassette)
}

/// `N] CODE` or `N]`.
fn status_line(text: &str) -> Option<(i32, Option<String>)> {
    let (status, code) = text.split_once(']')?;
    let code = code.trim();
    Some((
        status.parse().ok()?,
        (!code.is_empty()).then(|| code.to_owned()),
    ))
}

fn write(cassette: &Cassette) -> String {
    let mut lines = cassette.head.clone();
    let section = |lines: &mut Vec<String>, s: &Section| {
        lines.extend(s.text.iter().cloned());
        lines.extend(s.checks.iter().map(|c| format!("  = {c}")));
    };
    let seen = |lines: &mut Vec<String>, s: &Section| {
        for line in &s.seen {
            let mark = if line.noisy { "~?" } else { "~" };
            lines.push(format!("  {mark} {}", line.text).trim_end().to_owned());
        }
    };
    for step in &cassette.steps {
        section(&mut lines, &step.lines);
        if let Some((status, code)) = &step.status {
            lines.push(
                format!("  [{status}] {}", code.as_deref().unwrap_or(""))
                    .trim_end()
                    .to_owned(),
            );
        }
        seen(&mut lines, &step.lines);
    }
    if let Some(journal) = &cassette.journal {
        section(&mut lines, journal);
        seen(&mut lines, journal);
    }
    lines.extend(cassette.tail.iter().cloned());
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// What keeps a cassette from becoming a screenshot test, or a campaign.
fn lint(cassette: &Cassette) -> Vec<String> {
    let mut problems = Vec::new();
    let sections = (cassette
        .steps
        .iter()
        .map(|s| (&s.lines, s.command.as_str())))
    .chain(cassette.journal.iter().map(|j| (j, "")));
    let mut total = 0usize;
    for (section, command) in sections {
        total = total.saturating_add(section.checks.len());
        if section.checks.len() > STEP_CHECKS {
            problems.push(format!(
                "a step has {} `=` lines, past {STEP_CHECKS}",
                section.checks.len()
            ));
        }
        if !section.checks.is_empty() && command.split_whitespace().any(|w| w == "quiet") {
            problems.push(
                "a step that waits with `quiet` has `=` lines: wait for a text, a count or an exit"
                    .to_owned(),
            );
        }
        for check in &section.checks {
            let chars = check.chars().count();
            let spaces = check.chars().filter(|c| *c == ' ').count();
            if check
                .chars()
                .any(|c| ('\u{2500}'..='\u{257f}').contains(&c))
                || chars >= 10 && spaces.saturating_mul(10) > chars.saturating_mul(6)
            {
                problems.push(format!(
                    "`= {check}` is a screen row: print a fact, not a picture"
                ));
            }
            if check.contains("<pid>") || check.contains("#:R") {
                problems.push(format!("`= {check}` checks a pid or a cursor"));
            }
        }
    }
    if total > CHECKS {
        problems.push(format!("{total} `=` lines, past {CHECKS}"));
    }
    problems
}

// One replay.

/// The world one replay ran: what its steps printed, the fresh
/// observations between them, every fux call, and the journal.
struct Replay {
    steps: Vec<Ran>,
    /// `ls` before the first step, then after each.
    snapshots: Vec<Snapshot>,
    /// Each call through the shim, with the step it was in (from 1).
    calls: Vec<(usize, Call)>,
    events: Vec<Json>,
    problems: Vec<String>,
    took: Duration,
    /// Its `$DIR`.
    dir: String,
}

struct Ran {
    status: i32,
    out: String,
    err: String,
}

static COUNT: AtomicUsize = AtomicUsize::new(0);

/// Its directory (`$DIR`), the runner's own place in it, its socket.
struct Place {
    dir: PathBuf,
    own: PathBuf,
    socket: PathBuf,
}

impl Place {
    fn env(&self, ctx: &Ctx) -> Vec<(OsString, OsString)> {
        let mut path = OsString::from(self.own.join("bin"));
        path.push(":");
        path.push(&ctx.path);
        let pairs: [(&str, OsString); 13] = [
            ("DIR", self.dir.clone().into()),
            ("HOME", self.dir.clone().into()),
            ("XDG_CONFIG_HOME", self.dir.join(".config").into()),
            ("XDG_RUNTIME_DIR", self.own.clone().into()),
            ("FUX_SOCKET", self.socket.clone().into()),
            ("PATH", path),
            ("SHELL", "/bin/sh".into()),
            ("LANG", "C.UTF-8".into()),
            ("TMPDIR", self.own.join("tmp").into()),
            ("REPO", REPO.into()),
            ("FUX_REAL", ctx.fux.clone().into()),
            ("FUX_REC", self.own.join("calls.ndjson").into()),
            ("FUX_BASE", ctx.base.clone().unwrap_or_default()),
        ];
        pairs
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (k.into(), v))
            .collect()
    }

    /// fux itself, not through the shim, on this server: killed, with
    /// whatever it started, if it has not answered by `deadline`, or as
    /// soon as it prints past `OUTPUT_BYTES` on either stream.
    fn fux(&self, ctx: &Ctx, args: &[&str], deadline: Instant) -> Result<String, String> {
        let mut child = Command::new(&ctx.fux)
            .args(args)
            .env_clear()
            .envs(self.env(ctx))
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(e)?;
        let shown = || format!("fux {}", args.join(" "));
        // Each stream read on a thread of its own, which stops at the bound.
        let (sent, read) = std::sync::mpsc::channel();
        let pipes: [Option<Box<dyn Read + Send>>; 2] = [
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        ];
        for (n, pipe) in pipes.into_iter().enumerate() {
            let pipe = pipe.ok_or("its output")?;
            let sent = sent.clone();
            std::thread::spawn(move || sent.send((n, bounded(pipe, OUTPUT_BYTES, "its output"))));
        }
        drop(sent);
        let mut got: [Option<Result<String, String>>; 2] = [None, None];
        let mut status = None;
        let failed = loop {
            if status.is_none() {
                status = child.try_wait().ok().flatten();
            }
            if status.is_some() && got.iter().all(Option::is_some) {
                break None;
            }
            if got.iter().any(|g| matches!(g, Some(Err(_)))) {
                break Some(format!("printed past {OUTPUT_BYTES} bytes, so killed"));
            }
            if Instant::now() >= deadline {
                break Some("no answer in time, so killed".to_owned());
            }
            if let Ok((n, text)) = read.recv_timeout(Duration::from_millis(5))
                && let Some(slot) = got.get_mut(n)
            {
                *slot = Some(text);
            }
        };
        if let Some(why) = failed {
            stop_group(&mut child);
            return Err(format!("{}: {why}", shown()));
        }
        let [Some(Ok(out)), Some(Ok(err))] = got else {
            return Err(format!("{}: its output unread", shown()));
        };
        match status {
            Some(status) if status.success() => Ok(out),
            Some(status) => Err(format!("{}: {status}: {}", shown(), err.trim())),
            None => Err(format!("{}: no status", shown())),
        }
    }
}

/// Kills `child`'s process group, and it, and reaps it.
fn stop_group(child: &mut Child) {
    if let Some(group) = fuxix::process::Pid::of(child) {
        let _ = fuxix::process::kill_group(group, fuxix::process::Signal::Kill);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Waits for `child` until `deadline`; past it, kills its process group
/// and reaps it, and answers None.
fn finish(child: &mut Child, deadline: Instant) -> Option<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) | Err(_) => {
                stop_group(child);
                return None;
            }
        }
    }
}

fn bounded(from: impl Read, most: u64, what: &str) -> Result<String, String> {
    let mut bytes = Vec::new();
    from.take(most.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(e)?;
    if u64::try_from(bytes.len()).map_or(true, |n| n > most) {
        return Err(format!("{what} is past {most} bytes"));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_file(path: &Path, most: u64, what: &str) -> Result<String, String> {
    bounded(
        std::fs::File::open(path).map_err(|why| format!("{what}: {why}"))?,
        most,
        what,
    )
}

/// One replay: its server's start, its steps and the journal's read all end
/// by `deadline`; only the server's stop is bounded apart (`AFTER_SECS`).
fn replay(cassette: &Cassette, ctx: &Ctx, keep: bool, deadline: Instant) -> Result<Replay, String> {
    let started = Instant::now();
    let dir = ctx.scratch.join(format!(
        "c{}-{}",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::SeqCst)
    ));
    let own = dir.join(".cassette");
    let place = Place {
        socket: own.join("fux.sock"),
        dir,
        own,
    };
    let _ = std::fs::remove_dir_all(&place.dir);
    for sub in ["bin", "tmp", "steps"] {
        std::fs::create_dir_all(place.own.join(sub)).map_err(e)?;
    }
    std::fs::create_dir_all(place.dir.join(".config")).map_err(e)?;
    use std::os::unix::fs::PermissionsExt;
    for dir in [&place.dir, &place.own] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
    }
    std::os::unix::fs::symlink(&ctx.shim, place.own.join("bin/fux")).map_err(e)?;
    let log = std::fs::File::create(place.own.join("server.log")).map_err(e)?;
    let mut server = Command::new(&ctx.fux)
        .arg(fux::SERVE)
        .arg(&place.socket)
        .arg(fux::script::default_state().map_err(e)?.preferences.journal_entries.get().to_string())
        .env_clear()
        .envs(place.env(ctx))
        .env("PS1", "$ ")
        .env("ENV", "/dev/null")
        .current_dir(&place.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .map_err(e)?;
    let mut replay = drive(cassette, ctx, &place, &server, deadline);
    let panes = (replay.as_ref().ok())
        .and_then(|r| r.snapshots.last())
        .map(|s| s.panes.iter().filter_map(|p| p.pid).collect())
        .unwrap_or_default();
    let left = if keep {
        say(&format!(
            "kept {}: FUX_SOCKET={} (server pid {})",
            cassette.name,
            place.socket.display(),
            server.id()
        ));
        Vec::new()
    } else {
        stop(&mut server, ctx, &place, panes)
    };
    if let Err(why) = &mut replay {
        why.push_str(&format!("\n  its files: {}", place.dir.display()));
    }
    if let Ok(replay) = &mut replay {
        replay.problems.extend(left);
        replay.took = started.elapsed();
        if replay.problems.is_empty() && !keep {
            let _ = std::fs::remove_dir_all(&place.dir);
        } else {
            replay
                .problems
                .push(format!("its files: {}", place.dir.display()));
        }
    }
    replay
}

/// The steps, in one `sh` in a process group of its own, each step's
/// output in files, a fresh `ls` after each; then the journal.
fn drive(
    cassette: &Cassette,
    ctx: &Ctx,
    place: &Place,
    server: &Child,
    deadline: Instant,
) -> Result<Replay, String> {
    while std::os::unix::net::UnixStream::connect(&place.socket).is_err() {
        if Instant::now() > deadline {
            return Err(format!("its server (pid {}) did not start", server.id()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let quote = |path: &Path| format!("'{}'", path.display());
    let steps = place.own.join("steps");
    let file = |n: usize, what: &str| quote(&steps.join(format!("{n}.{what}")));
    let snapshot = |n: usize| format!("{} ls json >{} 2>&1\n", quote(&ctx.fux), file(n, "snap"));
    let calls = quote(&place.own.join("calls.ndjson"));
    let mut script = format!("cd {}\nulimit -f 4096\n: >{calls}\n", quote(&place.dir));
    script.push_str(&snapshot(0));
    for (n, step) in (1usize..).zip(&cassette.steps) {
        script.push_str(&format!(
            "{{ {}\n}} >{} 2>{} </dev/null\necho $? >{}\n",
            step.command,
            file(n, "out"),
            file(n, "err"),
            file(n, "status")
        ));
        script.push_str(&snapshot(n));
        script.push_str(&format!("wc -c <{calls} >{}\n", file(n, "calls")));
    }
    let script_path = place.own.join("script.sh");
    std::fs::write(&script_path, script).map_err(e)?;
    let mut sh = Command::new("/bin/sh")
        .arg(&script_path)
        .env_clear()
        .envs(place.env(ctx))
        .current_dir(&place.dir)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(place.own.join("sh.err")).map_err(e)?)
        .spawn()
        .map_err(e)?;
    let finished = finish(&mut sh, deadline).is_some();
    // Whatever a step left running in the background goes with it.
    if let Some(group) = fuxix::process::Pid::of(&sh) {
        let _ = fuxix::process::kill_group(group, fuxix::process::Signal::Kill);
    }
    let mut problems = Vec::new();
    let mut ran = Vec::new();
    let mut snapshots = vec![Snapshot::read(&read_file(
        &steps.join("0.snap"),
        OUTPUT_BYTES,
        "a snapshot",
    )?)?];
    let mut calls = Vec::new();
    let log = read_file(&place.own.join("calls.ndjson"), LOG_BYTES, "the call log")?;
    let mut from = 0usize;
    for n in 1..=cassette.steps.len() {
        let read = |what: &str| {
            read_file(
                &steps.join(format!("{n}.{what}")),
                OUTPUT_BYTES,
                &format!("step {n}'s {what}"),
            )
        };
        let Ok(status) = read("status") else {
            problems.push(if finished {
                let said = read_file(&place.own.join("sh.err"), OUTPUT_BYTES, "sh's stderr");
                format!(
                    "its shell ended in step {n}: {}",
                    said.unwrap_or_default().trim()
                )
            } else {
                format!(
                    "timed out in step {n}, past its {}s",
                    cassette.timeout.as_secs()
                )
            });
            break;
        };
        ran.push(Ran {
            status: status
                .trim()
                .parse()
                .map_err(|_| format!("step {n}'s status {status:?}"))?,
            out: read("out")?,
            err: read("err")?,
        });
        match Snapshot::read(&read("snap")?) {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(why) => {
                problems.push(format!("after step {n}, no fresh observation: {why}"));
                break;
            }
        }
        let to: usize = read("calls")?.trim().parse().map_err(e)?;
        keep_calls(&mut calls, n, log.get(from..to).unwrap_or_default())?;
        from = to;
    }
    let run = snapshots
        .first()
        .map(|s| s.run.to_string())
        .unwrap_or_default();
    let events = journal(ctx, place, &run, deadline).unwrap_or_else(|why| {
        problems.push(why);
        Vec::new()
    });
    Ok(Replay {
        steps: ran,
        snapshots,
        calls,
        events,
        problems,
        took: Duration::ZERO,
        dir: place.dir.display().to_string(),
    })
}

/// Keeps step `step`'s calls from the shim's log, refusing the one past
/// `CALLS` before it is read.
fn keep_calls(calls: &mut Vec<(usize, Call)>, step: usize, log: &str) -> Result<(), String> {
    for line in log.lines() {
        if calls.len() >= CALLS {
            return Err(format!("past the {CALLS} fux calls a cassette may make"));
        }
        calls.push((step, Call::read(line)?));
    }
    Ok(())
}

/// Keeps a batch's events, refusing a batch that would take the journal
/// past `EVENTS` before any of it is kept.
fn keep_events(events: &mut Vec<Json>, batch: &[Json]) -> Result<(), String> {
    if events.len().saturating_add(batch.len()) > EVENTS {
        return Err(format!(
            "the journal holds past {EVENTS} events: a cassette is small"
        ));
    }
    events.extend(batch.iter().cloned());
    Ok(())
}

/// Every event from the journal's start, in batches, `missed` refused, all
/// by `deadline`.
fn journal(ctx: &Ctx, place: &Place, run: &str, deadline: Instant) -> Result<Vec<Json>, String> {
    let mut cursor = format!("0:{run}");
    let mut events = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err("the journal not read whole: the replay's time had passed".to_owned());
        }
        let args = ["all", "str", &cursor, "0", "next-events", "json"];
        let batch = Json::read(&place.fux(ctx, &args, deadline)?, ITEMS)?;
        if batch.get("missed").map(Json::text).as_deref() != Some("0") {
            return Err(format!("the journal missed entries: {}", batch.render()));
        }
        let Some(Json::Array(got)) = batch.get("events") else {
            return Err(format!("a batch with no events: {}", batch.render()));
        };
        if got.is_empty() {
            return Ok(events);
        }
        keep_events(&mut events, got)?;
        cursor = batch
            .get("next")
            .map(Json::text)
            .ok_or("a batch with no next")?;
    }
}

/// Stops the server, which must have lived, and checks that no pane
/// program outlives it.
fn stop(server: &mut Child, ctx: &Ctx, place: &Place, panes: Vec<i32>) -> Vec<String> {
    let mut left = Vec::new();
    if let Ok(Some(status)) = server.try_wait() {
        left.push(format!("the server ended during the replay: {status}"));
    } else if let Err(why) = place.fux(
        ctx,
        &["kill-server"],
        after(Duration::from_secs(AFTER_SECS)),
    ) {
        left.push(format!("the server did not take kill-server: {why}"));
    }
    let deadline = Instant::now().checked_add(Duration::from_secs(5));
    while server.try_wait().ok().flatten().is_none() && deadline.is_some_and(|d| Instant::now() < d)
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    if server.try_wait().ok().flatten().is_none() {
        left.push(format!("the server (pid {}) did not stop", server.id()));
        let _ = server.kill();
    }
    let _ = server.wait();
    let log = read_file(
        &place.own.join("server.log"),
        OUTPUT_BYTES,
        "the server's log",
    )
    .unwrap_or_default();
    if log.contains("panicked") {
        left.push(format!("the server panicked: {}", log.trim()));
    }
    let deadline = Instant::now().checked_add(Duration::from_secs(3));
    for pid in panes.into_iter().filter_map(fuxix::process::Pid::from_raw) {
        while fuxix::process::exists(pid) && deadline.is_some_and(|d| Instant::now() < d) {
            std::thread::sleep(Duration::from_millis(10));
        }
        if fuxix::process::exists(pid) {
            left.push(format!("pane process {pid} outlived its server"));
        }
    }
    left
}

// What a replay printed, normalized.

/// One cassette's renaming: paths, server runs, ids by first appearance
/// within their own run, pids and cursors. Applied to every line in the
/// order the replay printed them.
struct Names {
    paths: Vec<(String, &'static str)>,
    runs: Vec<Run>,
    /// The run of a line that names none, or several: the last one printed.
    last: Option<usize>,
    /// (run, sigil, number) by first appearance; each sigil lettered apart.
    ids: Vec<(Option<usize>, char, String)>,
}

impl Names {
    fn new(dir: &str) -> Names {
        let mut paths = vec![(dir.to_owned(), "$DIR"), (REPO.to_owned(), "$REPO")];
        paths.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
        Names {
            paths,
            runs: Vec::new(),
            last: None,
            ids: Vec::new(),
        }
    }

    fn run(&mut self, run: Run) -> usize {
        match self.runs.iter().position(|r| *r == run) {
            Some(n) => n,
            None => {
                self.runs.push(run);
                self.runs.len().saturating_sub(1)
            }
        }
    }

    fn line(&mut self, raw: &str) -> String {
        let mut text = raw.to_owned();
        for (path, name) in &self.paths {
            if !path.is_empty() {
                text = text.replace(path.as_str(), name);
            }
        }
        // The line's runs first, so an id printed before its run (as in a
        // version) has it. A bare id is of the line's one run, else the
        // last run printed.
        let runs: Vec<usize> = scan(&text)
            .filter_map(|rest| run_at(rest))
            .map(|(run, _)| self.run(run))
            .collect();
        let line_run = match runs.as_slice() {
            [first, more @ ..] if more.iter().all(|r| r == first) => Some(*first),
            [..] => self.last,
        };
        self.last = runs.last().copied().or(self.last);
        let mut out = String::with_capacity(text.len());
        let mut rest = text.as_str();
        let mut prev: Option<char> = None;
        while let Some(c) = rest.chars().next() {
            if let Some((field, after)) = pid_at(rest) {
                out.push_str(field);
                out.push_str("<pid>");
                rest = after;
                prev = Some('>');
                continue;
            }
            if boundary(prev) {
                if let Some((run, after)) = run_at(rest) {
                    out.push_str(&format!("R{}", self.run(run).saturating_add(1)));
                    rest = after;
                    prev = Some('1');
                    continue;
                }
                let digits = rest
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len());
                if digits > 0
                    && let Some(after) = rest.get(digits..)
                    && after.strip_prefix(':').and_then(run_at).is_some()
                {
                    out.push('#');
                    rest = after;
                    prev = Some('#');
                    continue;
                }
                if let Some((sigil, number, after)) = id_at(rest) {
                    let word = rest
                        .split(|c: char| c.is_whitespace() || "\"',[]{}()".contains(c))
                        .next()
                        .unwrap_or_default()
                        .trim_end_matches([':', '.']);
                    let run = match word.parse::<Version>() {
                        Ok(version) => Some(self.run(version.run)),
                        Err(()) => line_run,
                    };
                    let key = (run, sigil, number.to_owned());
                    let same: Vec<_> = self.ids.iter().filter(|(_, s, _)| *s == sigil).collect();
                    let index = match same.iter().position(|id| **id == key) {
                        Some(n) => n,
                        None => {
                            let n = same.len();
                            self.ids.push(key);
                            n
                        }
                    };
                    out.push(sigil);
                    out.push_str(&letters(index));
                    rest = after;
                    prev = Some('a');
                    continue;
                }
            }
            out.push(c);
            rest = rest.get(c.len_utf8()..).unwrap_or_default();
            prev = Some(c);
        }
        out
    }
}

/// Whether a name may start after `prev`: not inside a word, a number, a
/// path or another name.
fn boundary(prev: Option<char>) -> bool {
    prev.is_none_or(|p| !(p.is_ascii_alphanumeric() || ".-_/%@+#".contains(p)))
}

/// Where in `text` a name may start, as the rest of the text from there.
fn scan(text: &str) -> impl Iterator<Item = &str> {
    let mut prev = None;
    text.char_indices().filter_map(move |(at, c)| {
        let starts = boundary(prev);
        prev = Some(c);
        starts.then(|| text.get(at..)).flatten()
    })
}

/// A field holding a process id, at the start of `text`: the field, and
/// what follows its digits.
fn pid_at(text: &str) -> Option<(&'static str, &str)> {
    ["\"pid\":", "\"foreground_group\":"]
    .into_iter()
    .find_map(|field| {
        let after = text.strip_prefix(field)?;
        let digits = after
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after.len());
        (digits > 0)
            .then(|| Some((field, after.get(digits..)?)))
            .flatten()
    })
}

/// A server run, `PID-HEX16`, at the start of `text`, and what follows it.
fn run_at(text: &str) -> Option<(Run, &str)> {
    let digits = text.find(|c: char| !c.is_ascii_digit())?;
    let hex = text.get(digits..)?.strip_prefix('-')?;
    let len = digits.checked_add(17)?;
    let tail = hex.get(16..)?;
    let is_hex = |c: char| c.is_ascii_digit() || ('a'..='f').contains(&c);
    let whole = digits > 0
        && hex.get(..16)?.chars().all(is_hex)
        && !tail.starts_with(|c: char| c.is_ascii_alphanumeric());
    let run = text.get(..len)?.parse().ok()?;
    whole.then_some((run, tail))
}

/// A pane, tab, workspace or client id (`%3`, `@2`, `+1`, `c1`).
fn id_at(text: &str) -> Option<(char, &str, &str)> {
    let sigil = text.chars().next().filter(|c| "%@+c".contains(*c))?;
    let rest = text.get(1..)?;
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let after = rest.get(digits..)?;
    (digits > 0 && !after.starts_with(|c: char| c.is_ascii_alphanumeric()))
        .then(|| Some((sigil, rest.get(..digits)?, after)))
        .flatten()
}

/// 0 is `a`, 25 `z`, 26 `aa`.
fn letters(n: usize) -> String {
    let mut out = Vec::new();
    let mut n = n;
    loop {
        out.push(char::from(b'a'.saturating_add(
            u8::try_from(n.checked_rem(26).unwrap_or(0)).unwrap_or(0),
        )));
        match n.checked_div(26).and_then(|d| d.checked_sub(1)) {
            Some(next) => n = next,
            None => break,
        }
    }
    out.iter().rev().collect()
}

/// Whether `pattern` (`*` any run of characters, `\*` a star) is `line`.
fn matches(pattern: &str, line: &str) -> bool {
    let mut parts = vec![String::new()];
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'*') => {
                chars.next();
                if let Some(part) = parts.last_mut() {
                    part.push('*');
                }
            }
            '*' => parts.push(String::new()),
            c => {
                if let Some(part) = parts.last_mut() {
                    part.push(c);
                }
            }
        }
    }
    let Some((first, rest)) = parts.split_first() else {
        return false;
    };
    let Some(mut left) = line.strip_prefix(first.as_str()) else {
        return false;
    };
    let Some((last, middle)) = rest.split_last() else {
        return left.is_empty();
    };
    for part in middle {
        let Some(at) = left.find(part.as_str()) else {
            return false;
        };
        left = left
            .get(at.saturating_add(part.len())..)
            .unwrap_or_default();
    }
    left.ends_with(last.as_str())
}

/// The `=` lines in order, as a subsequence of `printed`.
fn checked(expected: &[String], printed: &[String]) -> Result<(), String> {
    let mut printed = printed.iter();
    for want in expected {
        if !printed.by_ref().any(|line| matches(want, line)) {
            return Err(format!("no line after the previous check is `= {want}`"));
        }
    }
    Ok(())
}

/// The `~` lines a section keeps: at most `most`, each cut.
fn seen(lines: &[String], most: usize) -> Vec<Seen> {
    let mut kept: Vec<Seen> = (lines.iter().take(most))
        .map(|l| Seen {
            noisy: false,
            text: l.chars().take(SEEN_CHARS).collect(),
        })
        .collect();
    if lines.len() > most {
        let text = format!("… {} more lines", lines.len().saturating_sub(most));
        kept.push(Seen { noisy: false, text });
    }
    kept
}

/// The code of the last `fux: CODE: …` line, if any.
fn code(err: &str) -> Option<String> {
    let line = err.lines().rev().find_map(|l| l.strip_prefix("fux: "))?;
    Some(line.split_once(':')?.0.to_owned())
}

// Versions, snapshots, calls and the journal, as fux prints them, read with
// fux-wire's own parsers: one spelling each, so nothing malformed passes.

/// Every version in `text`, in order. A word that bears one (`%N:` and a
/// second `:`) but is not one is an error, never skipped. `cut`: the shim
/// cut the text at 4,000 characters; then a last word that no delimiter
/// ends may be part of one, and is answered apart, unchecked, if it may.
fn versions(text: &str, cut: bool) -> Result<(Vec<Version>, Option<String>), String> {
    let delimiter = |c: char| c.is_whitespace() || "\"',[]{}()".contains(c);
    let mut words: Vec<&str> = text.split(delimiter).filter(|w| !w.is_empty()).collect();
    let part = (cut && !text.ends_with(delimiter))
        .then(|| words.pop())
        .flatten();
    let may = |w: &str| {
        w.strip_prefix('%')
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    };
    let bears = |w: &&str| may(w) && w.matches(':').count() >= 2;
    let seen = (words.into_iter().filter(bears))
        .map(|w| w.trim_end_matches([':', '.']))
        .map(|w| w.parse().map_err(|()| format!("a malformed version {w:?}")))
        .collect::<Result<_, _>>()?;
    Ok((seen, part.filter(|w| may(w)).map(str::to_owned)))
}

/// An `ls`: a fresh, whole observation of every pane, and the journal's
/// position as it was taken.
struct Snapshot {
    run: Run,
    cursor: u64,
    panes: Vec<Pane>,
}

struct Pane {
    version: Version,
    /// `exited` and `signal`, as fux printed them.
    end: String,
    pid: Option<i32>,
}

impl Snapshot {
    fn read(text: &str) -> Result<Snapshot, String> {
        let shown = || text.trim().chars().take(300).collect::<String>();
        let json = Json::read(text, ITEMS).map_err(|why| format!("{why}: {}", shown()))?;
        let field = |key| {
            json.get(key)
                .map(Json::text)
                .ok_or(format!("an ls with no {key}: {}", shown()))
        };
        let run: Run = field("run")?
            .parse()
            .map_err(|()| format!("an ls's run: {}", shown()))?;
        let cursor = field("cursor")?;
        let cursor = (cursor.parse::<Cursor>().ok())
            .filter(|c| c.run == run)
            .ok_or(format!("an ls's cursor {cursor:?}, not of its run"))?;
        let Some(Json::Array(panes)) = json.get("panes") else {
            return Err(format!("an ls with no panes: {}", shown()));
        };
        let pane = |p: &Json| -> Result<Pane, String> {
            let version = p.get("version").map(Json::text).unwrap_or_default();
            Ok(Pane {
                version: (version.parse::<Version>().ok())
                    .filter(|v| v.run == run)
                    .ok_or(format!("an ls's version {version:?}"))?,
                end: [p.get("exited"), p.get("signal")]
                    .map(|j| j.map(Json::render).unwrap_or_default())
                    .join(" "),
                pid: p.get("pid").and_then(|pid| pid.text().parse().ok()),
            })
        };
        Ok(Snapshot {
            run,
            cursor: cursor.seq.0,
            panes: panes.iter().map(pane).collect::<Result<_, _>>()?,
        })
    }

    fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|p| p.version.pane == id)
    }
}

/// One fux call, as the shim logged it.
struct Call {
    argv: Vec<String>,
    stdin: String,
    status: i64,
    out: String,
    err: String,
    /// The shim kept only the first 4,000 characters.
    out_cut: bool,
    err_cut: bool,
}

/// What a call did, when its words establish it: one `expect`ed
/// `send-keys`, or one wait from a version, and nothing else, so the
/// call's status is that operation's own result.
#[derive(Debug, PartialEq, Eq)]
enum Operation {
    Expect(Version),
    Wait(Version),
}

impl Call {
    fn read(line: &str) -> Result<Call, String> {
        let json = Json::read(line, ITEMS).map_err(|why| format!("the call log: {why}"))?;
        let text = |key| json.get(key).map(Json::text).unwrap_or_default();
        let Some(Json::Array(argv)) = json.get("argv") else {
            return Err(format!("a call with no argv: {line}"));
        };
        Ok(Call {
            argv: argv.iter().map(Json::text).collect(),
            stdin: text("stdin"),
            status: text("status")
                .parse()
                .map_err(|_| format!("a call's status: {line}"))?,
            out: text("out"),
            err: text("err"),
            out_cut: text("out_cut") == "true",
            err_cut: text("err_cut") == "true",
        })
    }

    /// `str V expect` and keys (`str X text`, `str X key`), then
    /// `send-keys`; or `str V changed`, `N timeout` or not, then `wait`.
    /// Anything else (a program on stdin, a file, a `try`, a word that only
    /// pushes `expect` as text) establishes no operation.
    fn operation(&self) -> Option<Operation> {
        if !self.stdin.is_empty() {
            return None;
        }
        let words: Vec<&str> = self.argv.iter().map(String::as_str).collect();
        let ["str", version, rest @ ..] = words.as_slice() else {
            return None;
        };
        let version: Version = version.parse().ok()?;
        match rest {
            ["changed", "wait"] => Some(Operation::Wait(version)),
            ["changed", secs, "timeout", "wait"]
                if secs.parse::<u32>().is_ok() =>
            {
                Some(Operation::Wait(version))
            }
            // Keys fux takes, so it reaches the send: some text, or a key's
            // name.
            ["expect", keys @ .., "send-keys"] => {
                let (groups, left) = keys.as_chunks::<3>();
                let input = |[s, value, kind]: &[&str; 3]| {
                    *s == "str"
                        && match *kind {
                            "text" => !value.is_empty(),
                            "key" => value.parse::<fux_vt::keys::KeyPress>().is_ok(),
                            _ => false,
                        }
                };
                let sent = !groups.is_empty() && groups.iter().all(input) && left.is_empty();
                sent.then_some(Operation::Expect(version))
            }
            _ => None,
        }
    }
}

// fux's observable promises.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Promise {
    VersionsGrow,
    EndedFrozen,
    StaleExpect,
    WaitAcrossChange,
    Journal,
}

impl Promise {
    const ALL: [Promise; 5] = [
        Promise::VersionsGrow,
        Promise::EndedFrozen,
        Promise::StaleExpect,
        Promise::WaitAcrossChange,
        Promise::Journal,
    ];

    fn name(self) -> &'static str {
        match self {
            Promise::VersionsGrow => "versions-grow",
            Promise::EndedFrozen => "ended-frozen",
            Promise::StaleExpect => "stale-expect",
            Promise::WaitAcrossChange => "wait-across-change",
            Promise::Journal => "journal",
        }
    }

    /// As fux words it (README, `docs/agents.md`, `docs/json.md`), with
    /// what makes a replay exercise it.
    fn words(self) -> &'static str {
        match self {
            Promise::VersionsGrow => {
                "a version \"counts what only grows while its program runs\": per server run and program generation, between fresh observations; exercised when a count grew"
            }
            Promise::EndedFrozen => {
                "an exited program's status and ENDED count never change, and its output is frozen once its terminal is drained, while its pane may still be resized, have its history cleared or be renamed; a replay takes the terminal as drained from the first two observations with no output between them; exercised when an ended pane was observed twice"
            }
            Promise::StaleExpect => {
                "send-keys \"sends only if the pane is still at version SEEN as they are queued; else it sends none and fails stale\"; exercised by an expect at a version the pane had already moved past"
            }
            Promise::WaitAcrossChange => {
                "\"A wait waits from the version answer A holds, so a change between two steps is never missed\"; exercised by a wait from a version the pane had already moved past"
            }
            Promise::Journal => {
                "the journal: every event once, in order, \"missed counts exactly the entries evicted\"; nothing named before its created or after its closed, one exited per program; exercised by any event"
            }
        }
    }
}

/// Checks fux's promises on the raw replay, before any normalization;
/// returns the promises it exercised.
/// What the monitor could not check goes in `unchecked`: never a pass,
/// never a failure.
fn monitor(
    replay: &Replay,
    problems: &mut Vec<String>,
    unchecked: &mut Vec<String>,
) -> BTreeSet<Promise> {
    let mut exercised = BTreeSet::new();
    let Some(run) = replay.snapshots.first().map(|s| s.run) else {
        return exercised;
    };
    journal_monitor(&replay.events, run, problems, &mut exercised);
    // Ended panes, by program generation, seen with no output between two
    // observations: their terminal drained, their output is frozen.
    let mut drained = BTreeSet::new();
    let after = (replay.snapshots.iter())
        .zip(replay.snapshots.iter().skip(1))
        .zip(1usize..);
    for ((before, now), step) in after {
        if now.run != run {
            problems.push(format!(
                "after step {step}: the server is another run, {}",
                now.run
            ));
            continue;
        }
        for pane in &now.panes {
            let id = pane.version.pane;
            let Some(was) = before.pane(id) else {
                continue;
            };
            let (a, b) = (was.version.counts, pane.version.counts);
            if !b.had(a) {
                problems.push(format!(
                    "versions-grow: in step {step}, {id} went from {a} to {b}"
                ));
                continue;
            }
            if a.respawns != b.respawns {
                continue;
            }
            if a != b {
                exercised.insert(Promise::VersionsGrow);
            }
            if !a.ended {
                continue;
            }
            exercised.insert(Promise::EndedFrozen);
            // Each resize or history change between the two observations
            // is one revision of SCREEN; any more is output after the end.
            let explicit = (replay.events.iter())
                .filter(|ev| {
                    let at = ev.text_of("next").parse::<Cursor>().map(|c| c.seq.0);
                    at.is_ok_and(|at| at > before.cursor && at <= now.cursor)
                        && ev.text_of("pane") == id.to_string()
                        && ["resized", "history_cleared", "history_budget"]
                            .contains(&ev.text_of("event").as_str())
                })
                .count();
            let grew = b.screen.saturating_sub(a.screen);
            let spoke = b.bells != a.bells
                || b.done != a.done
                || u64::try_from(explicit).is_ok_and(|explicit| grew > explicit);
            let frozen = drained.contains(&(id, a.respawns));
            if !spoke {
                drained.insert((id, a.respawns));
            }
            if !b.ended || pane.end != was.end || spoke && frozen {
                problems.push(format!(
                    "ended-frozen: in step {step}, {id}'s ended program went from {a} ({}) to {b} ({}), with {explicit} resize or history events",
                    was.end, pane.end
                ));
            }
        }
    }
    for (step, call) in &replay.calls {
        let shown = call.argv.join(" ");
        // What fux answered in a step it saw before the next fresh
        // observation, which must have had it, in this run.
        match (
            versions(&call.out, call.out_cut),
            replay.snapshots.get(*step),
        ) {
            (Err(why), _) => problems.push(format!("step {step}: `fux {shown}` answered {why}")),
            (Ok((seen, part)), Some(now)) => {
                if let Some(part) = part {
                    unchecked.push(format!(
                        "step {step}: `fux {shown}` answered more than the shim keeps, ending in {part:?}, which may be part of a version"
                    ));
                }
                for v in seen {
                    if v.run != run {
                        problems.push(format!(
                            "step {step}: `fux {shown}` answered a version of another server run, {v}"
                        ));
                    } else if now
                        .pane(v.pane)
                        .is_some_and(|p| !p.version.counts.had(v.counts))
                    {
                        problems.push(format!(
                            "versions-grow: step {step}: `fux {shown}` answered {v}, which {} had not reached after it",
                            v.pane
                        ));
                    }
                }
            }
            (Ok(_), None) => unchecked.push(format!(
                "step {step}: `fux {shown}`: no fresh observation after it to check its answer by"
            )),
        }
        let code = code(&call.err);
        match (call.status, &code) {
            // 141: its stdout's reader went, said nowhere.
            (0 | 141, _) => {}
            (1 | 2 | 4 | 124, Some(name)) => match name.parse::<Code>() {
                Ok(c) if i64::from(c.status().get()) == call.status => {}
                Ok(_) => problems.push(format!(
                    "step {step}: `fux {shown}` ended {} with code {name}, whose status differs",
                    call.status
                )),
                Err(()) => problems.push(format!(
                    "step {step}: `fux {shown}` failed with {name}, no code of fux's"
                )),
            },
            // The shim keeps 4,000 characters of stderr.
            (1 | 2 | 4 | 124, None) if call.err_cut => {}
            (status, _) => problems.push(format!(
                "step {step}: `fux {shown}` ended {status} without `fux: CODE:`"
            )),
        }
        // Only a call whose words establish one operation and nothing else
        // tells that operation's result by its status.
        let Some(operation) = call.operation() else {
            continue;
        };
        let (Operation::Expect(v) | Operation::Wait(v)) = &operation;
        let moved = (replay.snapshots.get(step.saturating_sub(1)))
            .and_then(|before| before.pane(v.pane))
            .is_some_and(|p| {
                p.version.run == v.run
                    && p.version.counts != v.counts
                    && p.version.counts.had(v.counts)
            });
        if !moved {
            continue;
        }
        let ended = |what: &str| {
            format!(
                "step {step}: `fux {shown}` ended {} ({}), though {} had moved past {v} before it {what}",
                call.status,
                code.as_deref().unwrap_or("no code"),
                v.pane
            )
        };
        match operation {
            Operation::Expect(_) => {
                exercised.insert(Promise::StaleExpect);
                let stale = versions(&call.err, call.err_cut)
                    .map(|(v, _)| v)
                    .unwrap_or_default();
                let named = matches!(stale.as_slice(), [was, now, ..]
                    if was.pane == now.pane && was.counts != now.counts && now.counts.had(was.counts));
                if call.status != 1 || code.as_deref() != Some("stale") || !named {
                    problems.push(format!(
                        "stale-expect: {}, not refused stale naming where it was",
                        ended("ran")
                    ));
                }
            }
            Operation::Wait(_) => {
                exercised.insert(Promise::WaitAcrossChange);
                if call.status != 0 {
                    problems.push(format!("wait-across-change: {}", ended("began")));
                }
            }
        }
    }
    exercised
}

/// The journal, read whole from its start: each event once in order and of
/// this run, nothing named before its `created` or after its `closed`, one
/// `exited` per program, and an ended program silent until it is respawned.
fn journal_monitor(
    events: &[Json],
    run: Run,
    problems: &mut Vec<String>,
    exercised: &mut BTreeSet<Promise>,
) {
    let mut created: BTreeSet<String> = BTreeSet::new();
    let mut closed: BTreeSet<String> = BTreeSet::new();
    let mut ended: BTreeSet<String> = BTreeSet::new();
    let mut fail =
        |why: String, ev: &Json| problems.push(format!("journal: {why}: {}", ev.render()));
    for (ev, seq) in events.iter().zip(1u64..) {
        exercised.insert(Promise::Journal);
        let next = ev.text_of("next").parse::<Cursor>();
        if !next.is_ok_and(|next| next.seq.0 == seq && next.run == run) {
            fail(format!("event {seq} is not the next of this run"), ev);
        }
        let name = ev.text_of("event");
        // An unzoomed tab's `pane` is null: no pane's.
        let ids: Vec<String> = ["pane", "tab", "workspace"]
            .iter()
            .filter_map(|k| ev.get(k).filter(|v| **v != Json::Null).map(Json::text))
            .collect();
        if name == "created"
            && let Some(subject) = ids.first()
            && !created.insert(subject.clone())
        {
            fail(format!("{subject} created twice"), ev);
        }
        for id in &ids {
            if !created.contains(id) {
                fail(format!("{id} named before its created"), ev);
            }
            if closed.contains(id) {
                fail(format!("{id} named after its closed"), ev);
            }
        }
        if name == "closed"
            && let Some(subject) = ids.first()
        {
            closed.insert(subject.clone());
        }
        let Some(pane) = ev.get("pane").map(Json::text) else {
            continue;
        };
        match name.as_str() {
            "exited" => {
                if !ended.insert(pane.clone()) {
                    fail(format!("{pane}'s program exited twice"), ev);
                }
            }
            "respawned" => {
                ended.remove(&pane);
            }
            // What its terminal held is read on after its exit (D-90): the
            // snapshots check that it is frozen once that is drained.
            _ => {}
        }
    }
}

// Checking a replay.

/// What one replay of a cassette showed, normalized, and what failed.
struct Checked {
    problems: Vec<String>,
    /// Each step's `~` lines, then the journal's.
    steps: Vec<Vec<Seen>>,
    journal: Vec<Seen>,
    exercised: BTreeSet<Promise>,
    /// What the monitor could not check: never a pass, never a failure.
    unchecked: Vec<String>,
}

fn check(cassette: &Cassette, replay: &Replay, dir: &str) -> Checked {
    let mut problems = lint(cassette);
    problems.extend(replay.problems.iter().cloned());
    let mut unchecked = Vec::new();
    let exercised = monitor(replay, &mut problems, &mut unchecked);
    for promise in &cassette.exercises {
        if !exercised.contains(promise) {
            problems.push(format!(
                "{} is uncovered, and this cassette exercises it",
                promise.name()
            ));
        }
    }
    let mut names = Names::new(dir);
    let mut steps = Vec::new();
    for ((step, ran), n) in cassette.steps.iter().zip(&replay.steps).zip(1usize..) {
        let lines: Vec<String> = ran.out.lines().map(|l| names.line(l)).collect();
        if let Err(why) = checked(&step.lines.checks, &lines) {
            problems.push(format!("step {n}: {why}"));
        }
        let got = (
            ran.status,
            (ran.status != 0).then(|| code(&ran.err)).flatten(),
        );
        if step.status.clone().unwrap_or((0, None)) != got {
            let code = got.1.as_deref().unwrap_or("");
            problems.push(
                format!("step {n}: ended [{}] {code}: {}", got.0, ran.err.trim())
                    .trim_end()
                    .to_owned(),
            );
        }
        steps.push(seen(&lines, SEEN_LINES));
    }
    // The journal: `~` lines without the cursor, and fields to match.
    let mut lines = Vec::new();
    let mut events: Vec<(Option<String>, Fields)> = Vec::new();
    for ev in &replay.events {
        let Json::Object(fields) = ev else {
            continue;
        };
        let fields: Vec<(String, Json)> = fields
            .iter()
            .filter(|(k, _)| k != "next")
            .cloned()
            .collect();
        lines.push(names.line(&Json::Object(fields.clone()).render()));
        let fields: Vec<(String, String)> = fields
            .iter()
            .map(|(k, v)| (k.clone(), names.line(&v.text())))
            .collect();
        let pane = fields
            .iter()
            .find(|(k, _)| k == "pane")
            .map(|(_, v)| v.clone());
        events.push((pane, fields));
    }
    if let Some(journal) = &cassette.journal {
        // In order per pane; checks naming no pane, among events with none.
        let mut panes: Vec<Option<String>> = Vec::new();
        let pairs = |check: &String| -> Vec<(String, String)> {
            check
                .split_whitespace()
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect()
        };
        for check in &journal.checks {
            let pane = pairs(check)
                .into_iter()
                .find(|(k, _)| k == "pane")
                .map(|(_, v)| v);
            if !panes.contains(&pane) {
                panes.push(pane);
            }
        }
        for pane in panes {
            let mut candidates = events.iter().filter(|(p, _)| *p == pane);
            for check in journal.checks.iter().filter(|c| {
                pairs(c)
                    .into_iter()
                    .find(|(k, _)| k == "pane")
                    .map(|(_, v)| v)
                    == pane
            }) {
                let wanted = pairs(check);
                let found = candidates.by_ref().any(|(_, fields)| {
                    wanted
                        .iter()
                        .all(|(k, v)| fields.iter().any(|(fk, fv)| fk == k && matches(v, fv)))
                });
                if !found {
                    problems.push(format!(
                        "@journal: no event after the previous check is `= {check}`"
                    ));
                    break;
                }
            }
        }
    }
    Checked {
        problems,
        steps,
        journal: seen(&lines, JOURNAL_SEEN),
        exercised,
        unchecked,
    }
}

/// An event's top-level fields, normalized, each value as its text.
type Fields = Vec<(String, String)>;

/// Where this replay's `~` lines differ from the recording's (never a
/// failure): `~?` lines are noise and skipped.
fn drift(cassette: &Cassette, checked: &Checked) -> Vec<String> {
    let mut out = Vec::new();
    let sections = (cassette
        .steps
        .iter()
        .map(|s| &s.lines)
        .zip(&checked.steps)
        .zip(1usize..))
    .map(|((s, now), n)| (format!("step {n}"), &s.seen, now))
    .chain(
        cassette
            .journal
            .iter()
            .map(|j| ("journal".to_owned(), &j.seen, &checked.journal)),
    );
    for (label, was, now) in sections {
        let longest = was.len().max(now.len());
        for i in 0..longest {
            let (a, b) = (was.get(i), now.get(i));
            if a.is_some_and(|a| a.noisy) || a.map(|a| &a.text) == b.map(|b| &b.text) {
                continue;
            }
            let show = |s: Option<&Seen>| s.map_or("(none)".to_owned(), |s| s.text.clone());
            out.push(format!("  {label}: ~ {}   now   {}", show(a), show(b)));
        }
    }
    out.into_iter().take(20).collect()
}

/// What one cassette's replay came to.
struct Report {
    name: String,
    problems: Vec<String>,
    drift: Vec<String>,
    exercised: BTreeSet<Promise>,
    unchecked: Vec<String>,
    took: Duration,
}

impl Report {
    fn summary(&self) -> String {
        let promises: Vec<String> = Promise::ALL
            .iter()
            .map(|p| {
                let state = if self.exercised.contains(p) {
                    "held"
                } else {
                    "uncovered"
                };
                let broken = self.problems.iter().any(|why| why.starts_with(p.name()));
                format!("{} {}", p.name(), if broken { "BROKEN" } else { state })
            })
            .collect();
        let mut out = format!(
            "cassette {}: {} in {:.1}s; {}",
            self.name,
            if self.problems.is_empty() {
                "ok"
            } else {
                "FAILED"
            },
            self.took.as_secs_f64(),
            promises.join(", ")
        );
        for why in &self.problems {
            out.push_str(&format!("\n  {why}"));
        }
        if !self.unchecked.is_empty() {
            out.push_str(&format!(
                "\n  unchecked:\n  {}",
                self.unchecked.join("\n  ")
            ));
        }
        if !self.drift.is_empty() {
            out.push_str(&format!(
                "\n  drift (not checked):\n{}",
                self.drift.join("\n")
            ));
        }
        out
    }
}

fn coverage(reports: &[Report]) -> String {
    let mut out = "fux's promises over these replays:".to_owned();
    for promise in Promise::ALL {
        let by: Vec<&str> = (reports.iter().filter(|r| r.exercised.contains(&promise)))
            .map(|r| r.name.as_str())
            .collect();
        let state = if by.is_empty() {
            "UNCOVERED".to_owned()
        } else {
            format!("exercised by {}", by.join(", "))
        };
        out.push_str(&format!(
            "\n  {}: {state}\n    ({})",
            promise.name(),
            promise.words()
        ));
    }
    out
}

/// Replays a cassette, unless the suite's time has passed: then it is a
/// failure, not replayed. It may take its own time, but not past the suite's.
fn report(cassette: &Cassette, ctx: &Ctx, keep: bool, suite: Instant) -> Report {
    let started = Instant::now();
    let failed = |why: String| Report {
        name: cassette.name.clone(),
        problems: vec![why],
        drift: Vec::new(),
        exercised: BTreeSet::new(),
        unchecked: Vec::new(),
        took: started.elapsed(),
    };
    if started >= suite {
        return failed(format!(
            "not replayed: the {ALL_SECS}s all cassettes may take had passed"
        ));
    }
    let replay = match replay(cassette, ctx, keep, after(cassette.timeout).min(suite)) {
        Ok(replay) => replay,
        Err(why) => return failed(why),
    };
    let checked = check(cassette, &replay, &replay.dir);
    Report {
        name: cassette.name.clone(),
        drift: drift(cassette, &checked),
        problems: checked.problems,
        exercised: checked.exercised,
        unchecked: checked.unchecked,
        took: replay.took,
    }
}

fn replay_all(cassettes: &[Cassette], ctx: &Ctx, options: &Options, suite: Instant) -> Vec<Report> {
    let next = AtomicUsize::new(0);
    let threads = options.threads.min(cassettes.len()).max(1);
    let mut reports: Vec<Report> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    while let Some(cassette) = cassettes.get(next.fetch_add(1, Ordering::SeqCst)) {
                        done.push(report(cassette, ctx, options.keep, suite));
                    }
                    done
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_default())
            .collect()
    });
    reports.sort_by(|a, b| a.name.cmp(&b.name));
    reports
}

/// Records a cassette: two replays on two fresh servers; their `~` lines
/// where they agree, `~?` where they differ. Its `=` and `[N]` lines must
/// hold, and nothing secret may be saved.
fn update(cassette: &Cassette, ctx: &Ctx, options: &Options) -> Result<(), String> {
    let mut takes = Vec::new();
    for _ in 0..2 {
        let replay = replay(cassette, ctx, options.keep, after(cassette.timeout))?;
        let checked = check(cassette, &replay, &replay.dir);
        if !checked.problems.is_empty() {
            return Err(checked.problems.join("\n  "));
        }
        takes.push(checked);
    }
    let [one, two] = <[Checked; 2]>::try_from(takes).map_err(|_| "two recordings")?;
    let merge = |a: &[Seen], b: &[Seen]| -> Vec<Seen> {
        let longest = a.len().max(b.len());
        (0..longest)
            .filter_map(|i| {
                let (x, y) = (a.get(i), b.get(i));
                let text = x.or(y)?.text.clone();
                Some(Seen {
                    noisy: x.map(|x| &x.text) != y.map(|y| &y.text),
                    text,
                })
            })
            .collect()
    };
    let mut recorded = cassette.clone();
    for ((step, a), b) in recorded.steps.iter_mut().zip(&one.steps).zip(&two.steps) {
        step.lines.seen = merge(a, b);
    }
    if let Some(journal) = &mut recorded.journal {
        journal.seen = merge(&one.journal, &two.journal);
    }
    let text = write(&recorded);
    secrets(&text)?;
    std::fs::write(
        options.dir.join(format!("{}.cassette", cassette.name)),
        text,
    )
    .map_err(e)
}

/// Refuses to save what holds a secret of the caller's, never redacting it.
fn secrets(text: &str) -> Result<(), String> {
    for (name, value) in std::env::vars() {
        let secret = ["TOKEN", "KEY", "SECRET", "PASS", "AUTH"]
            .iter()
            .any(|s| name.contains(s));
        if secret
            && value.len() >= 8
            && let Some(at) = text.find(&value)
        {
            return Err(format!("it holds ${name}'s value at byte {at}: not saved"));
        }
    }
    for shape in ["ghp_", "github_pat_", "sk-", "AKIA", "-----BEGIN"] {
        if let Some(at) = text.find(shape) {
            return Err(format!(
                "it holds what looks like a secret ({shape}) at byte {at}: not saved"
            ));
        }
    }
    Ok(())
}

// JSON, as fux prints it: enough to read it, and to print it back.

#[derive(Clone, Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    /// `text`, each of its arrays and objects refused past `most`
    /// entries before it keeps another.
    fn read(text: &str, most: usize) -> Result<Json, String> {
        let mut chars = text.trim().chars().peekable();
        let value = Json::value(&mut chars, 0, most)?;
        match chars.next() {
            None => Ok(value),
            Some(c) => Err(format!("JSON with {c:?} after its value: {text}")),
        }
    }

    fn value(chars: &mut Peekable<Chars>, depth: usize, most: usize) -> Result<Json, String> {
        let bad = |what: &str| Err(format!("not JSON: {what}"));
        if depth > DEEPEST {
            return bad("nested too deep");
        }
        let deeper = depth.saturating_add(1);
        skip(chars);
        match chars.next() {
            Some('n') => word(chars, "ull", Json::Null),
            Some('t') => word(chars, "rue", Json::Bool(true)),
            Some('f') => word(chars, "alse", Json::Bool(false)),
            Some('"') => string(chars).map(Json::String),
            Some('[') => {
                let mut items = Vec::new();
                skip(chars);
                if chars.next_if_eq(&']').is_some() {
                    return Ok(Json::Array(items));
                }
                loop {
                    if items.len() >= most {
                        return bad(&format!("an array of more than {most} entries"));
                    }
                    items.push(Json::value(chars, deeper, most)?);
                    skip(chars);
                    match chars.next() {
                        Some(',') => {}
                        Some(']') => return Ok(Json::Array(items)),
                        _ => return bad("an array"),
                    }
                }
            }
            Some('{') => {
                let mut fields = Vec::new();
                skip(chars);
                if chars.next_if_eq(&'}').is_some() {
                    return Ok(Json::Object(fields));
                }
                loop {
                    if fields.len() >= most {
                        return bad(&format!("an object of more than {most} entries"));
                    }
                    skip(chars);
                    if chars.next() != Some('"') {
                        return bad("an object's key");
                    }
                    let key = string(chars)?;
                    skip(chars);
                    if chars.next() != Some(':') {
                        return bad("an object");
                    }
                    fields.push((key, Json::value(chars, deeper, most)?));
                    skip(chars);
                    match chars.next() {
                        Some(',') => {}
                        Some('}') => return Ok(Json::Object(fields)),
                        _ => return bad("an object"),
                    }
                }
            }
            Some(c) if c == '-' || c.is_ascii_digit() => {
                let mut n = String::from(c);
                while let Some(d) = chars.next_if(|d| d.is_ascii_digit() || "+-.eE".contains(*d)) {
                    n.push(d);
                }
                Ok(Json::Number(n))
            }
            _ => bad("a value"),
        }
    }

    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            Json::Null | Json::Bool(_) | Json::Number(_) | Json::String(_) | Json::Array(_) => None,
        }
    }

    /// A string's text, else the value as JSON.
    fn text(&self) -> String {
        match self {
            Json::String(s) => s.clone(),
            Json::Null | Json::Bool(_) | Json::Number(_) | Json::Array(_) | Json::Object(_) => {
                self.render()
            }
        }
    }

    fn text_of(&self, key: &str) -> String {
        self.get(key).map(Json::text).unwrap_or_default()
    }

    fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Number(n) => out.push_str(n),
            Json::String(s) => quoted(s, out),
            Json::Array(items) => {
                out.push('[');
                for (item, n) in items.iter().zip(0usize..) {
                    if n > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Object(fields) => {
                out.push('{');
                for ((key, value), n) in fields.iter().zip(0usize..) {
                    if n > 0 {
                        out.push(',');
                    }
                    quoted(key, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

fn skip(chars: &mut Peekable<Chars>) {
    while chars.next_if(|c| c.is_ascii_whitespace()).is_some() {}
}

fn word(chars: &mut Peekable<Chars>, rest: &str, value: Json) -> Result<Json, String> {
    for want in rest.chars() {
        if chars.next() != Some(want) {
            return Err("not JSON: a misspelt word".to_owned());
        }
    }
    Ok(value)
}

fn string(chars: &mut Peekable<Chars>) -> Result<String, String> {
    let mut out = String::new();
    loop {
        match chars.next() {
            None => return Err("not JSON: an unended string".to_owned()),
            Some('"') => return Ok(out),
            Some('\\') => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('b') => out.push('\u{8}'),
                Some('f') => out.push('\u{c}'),
                Some('u') => {
                    let high = hex4(chars)?;
                    let code = if (0xd800..0xdc00).contains(&high) {
                        if chars.next() != Some('\\') || chars.next() != Some('u') {
                            return Err("not JSON: half a surrogate pair".to_owned());
                        }
                        let low = hex4(chars)?;
                        (high.checked_sub(0xd800))
                            .and_then(|h| h.checked_mul(0x400))
                            .and_then(|h| h.checked_add(low.checked_sub(0xdc00)?))
                            .and_then(|c| c.checked_add(0x10000))
                            .unwrap_or(0xfffd)
                    } else {
                        high
                    };
                    out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                }
                Some(c) => out.push(c),
                None => return Err("not JSON: an unended escape".to_owned()),
            },
            Some(c) => out.push(c),
        }
    }
}

fn hex4(chars: &mut Peekable<Chars>) -> Result<u32, String> {
    let mut n = 0u32;
    for _ in 0..4 {
        let digit = chars
            .next()
            .and_then(|c| c.to_digit(16))
            .ok_or("not JSON: a bad \\u escape")?;
        n = n
            .checked_mul(16)
            .and_then(|n| n.checked_add(digit))
            .ok_or("not JSON: a bad \\u escape")?;
    }
    Ok(n)
}

fn quoted(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
}
