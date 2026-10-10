//! esctest2 (`references/xterm/esctest2`), xterm's conformance suite by
//! George Nachman and Thomas E. Dickey, run against fux-vt: each test
//! writes to its terminal and reads the terminal's reports back, the cells
//! by DECRQCRA rectangle checksums, the cursor by DSR, modes by DECRQM.
//!
//! esctest runs on a PTY, one Python process for each area (a test class,
//! one file of `esctest/tests/`), in order within it as esctest runs them,
//! so every area starts on a fresh terminal. The terminal is a fux-vt
//! parser set up as fux's panes are (`fux::pane::OPTIONS`) with the
//! reports fux-vt keeps off in panes that esctest needs (see [`options`]):
//! it reads what esctest writes, and its replies are written back. Each
//! read esctest makes waits `--timeout` for its reply, so a report fux-vt
//! does not give fails that test alone; an area that runs past `--limit`
//! is stopped, and its unfinished tests fail.
//!
//! With `--in-fux`, each area also runs in a pane of a real fux server of
//! its own (its own socket and directory), with a client attached on a
//! PTY, whose terminal is a fux-vt parser that answers what fux asks of
//! it. The results are set beside the direct run's: a test passing in
//! one and failing in the other points at fux, its pane replies or its
//! painting. Tests that read cells cannot run there: fux's panes do not
//! answer DECRQCRA, so they are not counted.
//!
//! With `--xterm`, each area also runs in a real xterm under Xvfb, the
//! reference: what xterm passes and fux-vt fails is what fux-vt lacks.
//!
//! Results are checked against `esctest-expected.txt`, every test that
//! fails with its reason: the run fails on a test failing that is not
//! listed, and on one listed that passes, so the list never goes stale.
//!
//! With `--terminal NAME`, the terminal under test is another engine in
//! this process instead of fux-vt (Ghostty's core, for one), answering as
//! [`crate::answering`] says: its replies go back, and DECRQCRA, which only
//! fux-vt answers, is answered from its screen. Its results are checked
//! against its own list, `esctest-expected-NAME.txt`. With `--beside NAME`,
//! another terminal runs every area too, checked against its own list, and
//! the two are compared area by area, with the tests one passes and the
//! other fails, each with its feature and where the feature is specified.
use crate::answering::Answering;
use crate::engine::{ENGINES, Setup};
use crate::escape;
use fuxix::poll::{Events, PollFd};
use fuxix::process::{Pid, Signal};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// esctest's terminal: it resizes to 25 by 80 at the start of every test
/// (`esctest.py`, `reset`), which fux-vt, refusing window operations from
/// programs, does not do, so the terminal starts at that size.
const ROWS: u16 = 25;
const COLS: u16 = 80;

/// `--expected-terminal`: esctest adjusts what it expects to the terminal
/// it is told it runs in, xterm or iTerm2. fux-vt follows xterm, so xterm:
/// blanks are spaces, checksums are DEC's (negated), and xterm's own known
/// bugs (`knownBug(terminal="xterm")`) are expected to fail.
const EXPECTED_TERMINAL: &str = "xterm";

/// `--max-vt-level`: 4, a VT420, xterm's default (`decTerminalID`) and the
/// level esctest's README runs a vanilla xterm at. Level 4 has DECRQCRA,
/// which reads the cells back, and the rectangle operations; level 5
/// (VT520) tests are skipped.
const VT_LEVEL: &str = "4";

/// The usage of `esctest`.
pub const USAGE: &str = "\
usage: fux-vt-compare esctest [--terminal NAME] [--beside NAME] [--in-fux] [--xterm]
                              [--jobs N] [--timeout SECONDS] [--limit SECONDS]
                              [--expected FILE] [--json FILE] [--logs DIR]
                              [--replays] [--show] [FILTER]

Runs esctest2 (references/xterm/esctest2, from references/fetch.sh) against
fux-vt set up as fux's panes are, with the reports it needs; FILTER is a
Python regular expression a test's name (CLASS.test_name) must contain.
Results are checked against esctest-expected.txt: exit 1 on a failure not
listed there, or a test listed that passes. --in-fux also runs every area in
a pane of a real fux server (building fux), --xterm in a real xterm under Xvfb,
and each lists where it differs from fux-vt alone. --timeout is esctest's: how
long a read waits for its reply (default 1); --limit how long an area may run
(default 120). --logs keeps esctest's logs (AREA.log, AREA.fux.log,
AREA.xterm.log) in DIR. --replays prints, for each failure, what the test wrote
(without its queries) as a run.sh replay. --show prints each failure's message.
--json writes every result, as fux-vt/compare's README says. --terminal runs
against another engine in this process (ghostty) instead of fux-vt, checked
against esctest-expected-NAME.txt; --beside also runs NAME (fux-vt, or an
engine), checked against its own list, and compares the two: each area's pass
rates, and the tests one passes and the other fails, with their features.";

/// The terminal esctest runs against directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Subject {
    /// fux-vt, set up as fux's panes are with the reports esctest needs
    /// ([`options`]): it answers every query itself, DECRQCRA among them.
    FuxVt,
    /// An engine in this process (by its index in [`ENGINES`]), answering
    /// as [`crate::answering`] says.
    Engine(usize),
}

impl Subject {
    /// The terminal `name` names: fux-vt, or an engine in this process
    /// that can run here.
    fn named(name: &str) -> Result<Subject, String> {
        if name == "fux-vt" {
            return Ok(Subject::FuxVt);
        }
        let i = crate::engine::find(name).ok_or(format!("no engine {name}"))?;
        let kind = ENGINES.get(i).ok_or(format!("no engine {name}"))?;
        if !kind.in_process {
            return Err(format!(
                "{name} runs in a process of its own; esctest's terminal must run in this one"
            ));
        }
        (kind.available)().map_err(|e| format!("{name}: {e}"))?;
        Ok(Subject::Engine(i))
    }

    fn name(self) -> &'static str {
        match self {
            Subject::FuxVt => "fux-vt",
            Subject::Engine(i) => ENGINES.get(i).map_or("?", |k| k.name),
        }
    }

    /// Its list of expected failures: `esctest-expected.txt` for fux-vt,
    /// `esctest-expected-NAME.txt` for an engine.
    fn expected(self) -> PathBuf {
        let file = match self {
            Subject::FuxVt => "esctest-expected.txt".to_owned(),
            Subject::Engine(_) => format!("esctest-expected-{}.txt", self.name()),
        };
        Path::new(env!("CARGO_MANIFEST_DIR")).join(file)
    }

    /// The engine set up as esctest's terminal: Ghostty's core told its
    /// size ([`crate::engines::ghostty::answering`]), any other as the
    /// comparisons make it.
    fn engine(i: usize) -> Result<Box<dyn crate::engine::Engine>, String> {
        let kind = ENGINES.get(i).ok_or("no such engine")?;
        if kind.name == "ghostty" {
            return crate::engines::ghostty::answering(ROWS, COLS);
        }
        (kind.make)(&Setup {
            rows: ROWS,
            cols: COLS,
            history: 10_000,
            reflow: false,
        })
    }
}

/// What the command was asked.
struct Request {
    filter: Option<String>,
    /// The terminal under test.
    terminal: Subject,
    /// A terminal run beside it, and compared with it.
    beside: Option<Subject>,
    in_fux: bool,
    /// Also run each area in a real xterm, as the reference.
    xterm: bool,
    jobs: usize,
    /// esctest's own `--timeout`: how long a read waits for its reply.
    timeout: f64,
    /// How long an area may run before it is stopped.
    limit: Duration,
    expected: PathBuf,
    json: Option<PathBuf>,
    /// Where esctest's logs are kept, rather than in a directory removed
    /// at the end.
    logs: Option<PathBuf>,
    replays: bool,
    show: bool,
}

/// The request `argv` makes; `None` if it asks for help.
fn request(argv: &[String]) -> Result<Option<Request>, String> {
    let mut out = Request {
        filter: None,
        terminal: Subject::FuxVt,
        beside: None,
        in_fux: false,
        xterm: false,
        jobs: std::thread::available_parallelism().map_or(4, std::num::NonZero::get),
        timeout: 1.0,
        limit: Duration::from_secs(120),
        expected: PathBuf::new(),
        json: None,
        logs: None,
        replays: false,
        show: false,
    };
    let mut expected = None;
    let mut words = argv.iter();
    while let Some(word) = words.next() {
        let mut value = |name: &str| words.next().ok_or(format!("{name} needs a value"));
        let number = |name: &str, text: &str| {
            text.parse::<f64>()
                .ok()
                .filter(|n| n.is_finite() && *n > 0.0)
                .ok_or(format!("{name}: {text:?} is not a positive number"))
        };
        match word.as_str() {
            "--in-fux" => out.in_fux = true,
            "--xterm" => out.xterm = true,
            "--replays" => out.replays = true,
            "--show" => out.show = true,
            "--jobs" => {
                let text = value("--jobs")?;
                out.jobs = text
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or(format!("--jobs: {text:?} is not a positive number"))?;
            }
            "--timeout" => out.timeout = number("--timeout", value("--timeout")?)?,
            "--limit" => {
                out.limit = Duration::try_from_secs_f64(number("--limit", value("--limit")?)?)
                    .map_err(|e| format!("--limit: {e}"))?;
            }
            "--expected" => expected = Some(value("--expected")?.into()),
            "--terminal" => out.terminal = Subject::named(value("--terminal")?)?,
            "--beside" => out.beside = Some(Subject::named(value("--beside")?)?),
            "--json" => out.json = Some(value("--json")?.into()),
            "--logs" => out.logs = Some(value("--logs")?.into()),
            "-h" | "--help" => return Ok(None),
            other if other.starts_with("--") => return Err(format!("unknown option {other}")),
            other => {
                if out.filter.replace(other.to_owned()).is_some() {
                    return Err("esctest takes one FILTER".into());
                }
            }
        }
    }
    // A fux pane runs fux-vt, and is set beside fux-vt's direct run.
    if out.in_fux && out.terminal != Subject::FuxVt {
        return Err("--in-fux runs fux-vt in a fux pane: it goes with --terminal fux-vt".into());
    }
    if out.beside == Some(out.terminal) {
        return Err(format!(
            "--beside {0}: {0} is the terminal under test",
            out.terminal.name()
        ));
    }
    out.expected = expected.unwrap_or_else(|| out.terminal.expected());
    Ok(Some(out))
}

/// How the terminal under test is set up: as fux's panes are, with what
/// esctest needs that panes leave off.
pub fn options() -> fux_vt::Options {
    fux::pane::OPTIONS
        .with(fux_vt::Feature::RectangleChecksums)
        .with(fux_vt::Feature::ExtendedReplies)
}

/// Where esctest is: `references/xterm/esctest2/esctest`, which
/// `references/fetch.sh` clones.
fn suite() -> Result<PathBuf, String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../references/xterm/esctest2/esctest")
        .canonicalize()
        .map_err(|_| "esctest is not in references/xterm/esctest2: run references/fetch.sh")?;
    Ok(dir)
}

/// The first `name` on `PATH`, as a full path: the panes of a fux server
/// are given it so.
fn which(name: &str) -> Result<PathBuf, String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
        .ok_or(format!("no {name} on PATH"))
}

/// Lists esctest's tests, in the order it runs them, whose names contain
/// `filter`: esctest has no listing of its own, so its test classes are
/// imported as `esctest.py` imports them.
const LIST: &str = r#"
import inspect, re, sys
import esc, escargs, esccmd
escargs.args = escargs.parser.parse_args([])
import tests
pattern = re.compile(sys.argv[1]) if len(sys.argv) > 1 else None
for c in tests.tests:
    for n, _ in inspect.getmembers(c(), predicate=inspect.ismethod):
        name = c.__name__ + "." + n
        if n.startswith("test_") and (pattern is None or pattern.search(name)):
            print(name)
"#;

/// An area: one test class, and its tests, in order.
#[derive(Clone, Debug)]
struct Area {
    name: String,
    tests: Vec<String>,
}

fn areas(python: &Path, suite: &Path, filter: Option<&str>) -> Result<Vec<Area>, String> {
    let mut command = Command::new(python);
    command.arg("-c").arg(LIST).current_dir(suite);
    if let Some(filter) = filter {
        command.arg(filter);
    }
    let out = command
        .output()
        .map_err(|e| format!("listing esctest's tests: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "listing esctest's tests: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let mut areas: Vec<Area> = Vec::new();
    for name in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((class, _)) = name.split_once('.') else {
            continue;
        };
        match areas.last_mut() {
            Some(area) if area.name == class => area.tests.push(name.to_owned()),
            _ => areas.push(Area {
                name: class.to_owned(),
                tests: vec![name.to_owned()],
            }),
        }
    }
    Ok(areas)
}

/// Each test of an area, by name, and how it came out, in order.
type Tests = Vec<(String, Outcome)>;

/// How a test came out.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    /// Its assertions held. `beyond_xterm`: esctest expected it to fail,
    /// as it does in xterm (a known bug of xterm's, or an option xterm
    /// needs), and said "Should have failed".
    Pass { beyond_xterm: bool },
    /// It failed, with what esctest said.
    Fail(String),
    /// Not run: it needs a VT level above the one chosen, or esctest does
    /// not try it in xterm.
    Skip(String),
}

impl Outcome {
    fn passed(&self) -> bool {
        matches!(self, Outcome::Pass { .. })
    }
    fn failed(&self) -> bool {
        matches!(self, Outcome::Fail(_))
    }
}

/// The last part of a traceback, the exception and its message, and where
/// it was raised: the functions it went through, from the test's own on,
/// as `(in test_X > GetChecksumOfRect > ReadDCS > read)`, which says what
/// report a timeout waited for.
fn exception(traceback: &[&str]) -> String {
    // After the last frame (`  File …`) and its source lines (indented
    // four), which a message's own lines, a screen's among them, may look
    // like.
    let last_frame = traceback
        .iter()
        .rposition(|line| line.starts_with("  File "))
        .map_or(0, |i| i.saturating_add(1));
    let start = traceback
        .iter()
        .enumerate()
        .skip(last_frame)
        .find(|(_, line)| !line.starts_with("    "))
        .map_or(traceback.len(), |(i, _)| i);
    let message = traceback
        .get(start..)
        .unwrap_or_default()
        .iter()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned();
    let calls: Vec<&str> = traceback
        .iter()
        .filter_map(|line| line.strip_prefix("  File ")?.rsplit_once(", in "))
        .map(|(_, function)| function.trim())
        .filter(|f| !matches!(*f, "RunTest" | "func_wrapper" | "<module>"))
        .collect();
    if message.is_empty() || calls.is_empty() {
        return message;
    }
    match message.split_once('\n') {
        Some((first, rest)) => format!("{first} (in {})\n{rest}", calls.join(" > ")),
        None => format!("{message} (in {})", calls.join(" > ")),
    }
}

/// Each test's outcome, from esctest's log: `Run test: NAME`, then
/// `Passed.`, `Fails as expected: …`, `Skipped because …`, or `*** TEST
/// NAME FAILED:` and its traceback.
fn outcomes(log: &str) -> BTreeMap<String, Outcome> {
    let mut out = BTreeMap::new();
    let mut lines = log.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(name) = line.strip_prefix("Run test: ") else {
            continue;
        };
        let mut body: Vec<&str> = Vec::new();
        while let Some(next) = lines.peek() {
            if next.starts_with("Run test: ") || next.starts_with("*** ") && next.ends_with(" ***")
            {
                break;
            }
            body.push(next);
            lines.next();
        }
        // A test may log lines of its own before its outcome
        // (ChangeSpecialColorTests logs each colour it reads, `Read: …`).
        let start = body
            .iter()
            .position(|l| {
                *l == "Passed."
                    || l.starts_with("Fails as expected: ")
                    || l.starts_with("Skipped because ")
                    || l.starts_with("*** TEST ")
            })
            .unwrap_or(0);
        let body = body.get(start..).unwrap_or_default();
        let outcome = if body.first().is_some_and(|l| *l == "Passed.") {
            Outcome::Pass {
                beyond_xterm: false,
            }
        } else if let Some(reason) = body
            .first()
            .and_then(|l| l.strip_prefix("Fails as expected: "))
        {
            // Then the traceback of how it failed, each line marked.
            let traceback: Vec<&str> = body
                .iter()
                .filter_map(|l| {
                    l.strip_prefix("KNOWN BUG: ")
                        .or_else(|| l.strip_prefix("EXPECTED FAILURE (MISSING OPTION): "))
                })
                .collect();
            let how = exception(&traceback);
            if reason.ends_with("(not trying)") {
                Outcome::Skip(format!("esctest does not try it in xterm: {reason}"))
            } else if how.is_empty() {
                Outcome::Fail(format!("fails as in xterm: {reason}"))
            } else {
                Outcome::Fail(format!("fails as in xterm: {reason}: {how}"))
            }
        } else if let Some(reason) = body
            .first()
            .and_then(|l| l.strip_prefix("Skipped because "))
        {
            Outcome::Skip(reason.to_owned())
        } else {
            let traceback = body
                .iter()
                .skip_while(|l| !l.starts_with("*** TEST "))
                .skip(1)
                .copied()
                .collect::<Vec<_>>();
            let message = exception(&traceback);
            if message.starts_with("esctypes.InternalError: Should have failed") {
                Outcome::Pass { beyond_xterm: true }
            } else if message.is_empty() {
                Outcome::Fail("no result in esctest's log".into())
            } else {
                Outcome::Fail(message)
            }
        };
        out.insert(name.to_owned(), outcome);
    }
    out
}

/// The arguments esctest is run with for `area`, logging to `log`, with
/// each test's bytes in `cases` if asked.
fn esctest_argv(
    python: &Path,
    suite: &Path,
    area: &Area,
    timeout: f64,
    log: &Path,
    cases: Option<&Path>,
) -> Vec<String> {
    let names: Vec<&str> = area
        .tests
        .iter()
        .filter_map(|t| t.split_once('.').map(|(_, n)| n))
        .collect();
    let mut argv = vec![
        python.display().to_string(),
        suite.join("esctest.py").display().to_string(),
        format!("--expected-terminal={EXPECTED_TERMINAL}"),
        format!("--max-vt-level={VT_LEVEL}"),
        format!("--timeout={timeout}"),
        format!("--logfile={}", log.display()),
        "--no-print-logs".into(),
        format!("--include=^{}\\.(?:{})$", area.name, names.join("|")),
    ];
    if let Some(dir) = cases {
        argv.push(format!("--test-case-dir={}", dir.display()));
    }
    argv
}

/// Spawns are made one at a time.
static SPAWN: Mutex<()> = Mutex::new(());

/// Starts `argv` on a PTY of `rows` by `cols` (`record::start`), with
/// `env` besides `TERM` and `PATH`.
fn launch(
    argv: &[String],
    rows: u16,
    cols: u16,
    env: &[(&str, &Path)],
) -> Result<(File, Child), String> {
    let _guard = SPAWN.lock().map_err(|_| "a spawn panicked")?;
    crate::record::start(rows, cols, argv, |command| {
        command
            .env_clear()
            .env("TERM", "xterm-256color")
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .envs(env.iter().copied());
    })
}

/// Ends `child` and its process group.
fn stop(child: &mut Child) {
    if let Some(pid) = Pid::of(child) {
        let _ = fuxix::process::kill_group(pid, Signal::Kill);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// A terminal's replies, collected for one read.
#[derive(Default)]
struct Replies(Vec<u8>);

impl fux_vt::Sink for Replies {
    fn reply(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }
}

/// The client's terminal, for `--in-fux`: it answers what fux asks of a
/// terminal it attaches to (`src/outer.rs`), its colours as white on black,
/// as the recorder's stand-in terminal does.
#[derive(Default)]
struct Terminal(Vec<u8>);

impl fux_vt::Sink for Terminal {
    fn reply(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }
    fn event(&mut self, event: fux_vt::Event<'_>) {
        if let fux_vt::Event::ColorQuery { number, bel } = event {
            let colour = match number {
                10 => "rgb:ffff/ffff/ffff",
                11 => "rgb:0000/0000/0000",
                _ => return,
            };
            let end = if bel { "\x07" } else { "\x1b\\" };
            self.0
                .extend_from_slice(format!("\x1b]{number};{colour}{end}").as_bytes());
        }
    }
}

/// What `parser` answers to `bytes`, its replies collected by a sink `S`
/// and taken by `take`.
fn parse<S: fux_vt::Sink + Default>(
    parser: &mut fux_vt::Parser,
    bytes: &[u8],
    take: impl Fn(&mut S) -> Vec<u8>,
) -> Vec<u8> {
    let mut sink = S::default();
    // The parser refuses only allocations past its limits, and output goes
    // on regardless, as in fux.
    let _ = parser.process_with(bytes, &mut sink);
    take(&mut sink)
}

/// A terminal answering a program: what it answers to what the program
/// wrote.
type Answer<'a> = dyn FnMut(&[u8]) -> Result<Vec<u8>, String> + 'a;

/// Reads `master` until it hangs up or `done` says so, giving what is read
/// to `answer` and writing what it answers back; past `deadline`, gives up.
/// Whether it finished before the deadline.
fn pump(
    master: &mut File,
    answer: &mut Answer<'_>,
    deadline: Instant,
    done: impl Fn() -> bool,
) -> Result<bool, String> {
    let mut buf = vec![0u8; 1 << 16];
    loop {
        if Instant::now() >= deadline {
            return Ok(false);
        }
        if done() {
            return Ok(true);
        }
        let mut fds = [PollFd::new(&*master, Events::IN)];
        let ready = fuxix::poll::poll(&mut fds, Some(Duration::from_millis(50)))
            .map_err(|e| format!("polling the PTY: {e}"))?;
        if ready == 0 {
            continue;
        }
        match master.read(&mut buf) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                let replies = answer(buf.get(..n).unwrap_or_default())?;
                if !replies.is_empty() {
                    master
                        .write_all(&replies)
                        .map_err(|e| format!("writing a reply: {e}"))?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            // The PTY hangs up (EIO on Linux) once no one has it open.
            Err(_) => return Ok(true),
        }
    }
}

/// What running an area gave: esctest's log, and whether it was stopped.
struct Ran {
    log: String,
    stopped: bool,
}

/// Runs `area` against `subject` directly: fux-vt (its log `AREA.log`), or
/// an engine (`AREA.NAME.log`). Only the terminal under test keeps each
/// test's bytes, for `--replays`.
fn direct(job: &Job<'_>, subject: Subject, area: &Area, scratch: &Path) -> Result<Ran, String> {
    let log = match subject {
        Subject::FuxVt => scratch.join(format!("{}.log", area.name)),
        Subject::Engine(_) => scratch.join(format!("{}.{}.log", area.name, subject.name())),
    };
    let cases = if subject == job.request.terminal {
        job.cases_dir(scratch, area)?
    } else {
        None
    };
    let argv = esctest_argv(
        job.python,
        job.suite,
        area,
        job.request.timeout,
        &log,
        cases.as_deref(),
    );
    let mut answer: Box<Answer<'_>> = match subject {
        Subject::FuxVt => {
            let mut parser =
                fux_vt::Parser::with_options(crate::vt_size(ROWS, COLS)?, 10_000, options())
                    .map_err(|e| format!("fux-vt: {e}"))?;
            Box::new(move |bytes| {
                Ok(parse(&mut parser, bytes, |s: &mut Replies| {
                    std::mem::take(&mut s.0)
                }))
            })
        }
        Subject::Engine(i) => {
            let mut terminal = Answering::new(Subject::engine(i)?);
            Box::new(move |bytes| terminal.take(bytes))
        }
    };
    let (mut master, mut child) = launch(&argv, ROWS, COLS, &[])?;
    let deadline = deadline(job.request.limit);
    let finished = pump(&mut master, &mut *answer, deadline, || false);
    stop(&mut child);
    let finished = finished?;
    Ok(Ran {
        log: std::fs::read_to_string(&log).unwrap_or_default(),
        stopped: !finished,
    })
}

fn deadline(limit: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(limit).unwrap_or(now)
}

/// A fux server of its own, for `--in-fux`: its directory and its child.
struct Server {
    dir: PathBuf,
    socket: PathBuf,
    child: Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

static SERVERS: AtomicUsize = AtomicUsize::new(0);

/// Starts a fux server whose panes run `argv`, in a directory of its own
/// under `/tmp` (a socket's path must stay under 104 bytes), as fux's own
/// tests start one (`tests/support/mod.rs`); never the user's.
fn server(fux: &Path, argv: &[String]) -> Result<Server, String> {
    let dir = Path::new("/tmp").join(format!(
        "fux-esctest-{}-{}",
        std::process::id(),
        SERVERS.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let quoted: Vec<String> = argv.iter().map(|w| format!("'{w}'")).collect();
    let config = dir.join("fux.conf");
    std::fs::write(&config, format!("set shell {}\n", quoted.join(" ")))
        .map_err(|e| format!("{}: {e}", config.display()))?;
    let socket = dir.join("s").join("fux.sock");
    let log = File::create(dir.join("server.log")).map_err(|e| format!("server.log: {e}"))?;
    let child = {
        let _guard = SPAWN.lock().map_err(|_| "a spawn panicked")?;
        Command::new(fux)
            .arg("server")
            .arg("--socket")
            .arg(&socket)
            .arg("--config")
            .arg(&config)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &dir)
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .map_err(|e| format!("starting {}: {e}", fux.display()))?
    };
    let server = Server { dir, socket, child };
    let deadline = deadline(Duration::from_secs(10));
    while std::os::unix::net::UnixStream::connect(&server.socket).is_err() {
        if Instant::now() >= deadline {
            let log = std::fs::read_to_string(server.dir.join("server.log")).unwrap_or_default();
            return Err(format!("the fux server did not start: {log}"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(server)
}

/// Runs `area` in a pane of a fux server of its own, with a client
/// attached on a PTY one row taller than esctest's terminal, for fux's bar.
fn in_fux(job: &Job<'_>, fux: &Path, area: &Area, scratch: &Path) -> Result<Ran, String> {
    let log = scratch.join(format!("{}.fux.log", area.name));
    let argv = esctest_argv(job.python, job.suite, area, job.request.timeout, &log, None);
    let server = server(fux, &argv)?;
    let rows = ROWS.saturating_add(1);
    let terminal = fux_vt::Options::new()
        .with(fux_vt::Feature::ExtendedReplies)
        .with(fux_vt::Feature::Events);
    let mut parser = fux_vt::Parser::with_options(crate::vt_size(rows, COLS)?, 0, terminal)
        .map_err(|e| format!("fux-vt: {e}"))?;
    let attach = vec![fux.display().to_string(), "attach".to_owned()];
    let (mut master, mut client) = launch(
        &attach,
        rows,
        COLS,
        &[("FUX_SOCKET", &server.socket), ("HOME", &server.dir)],
    )?;
    let deadline = deadline(job.request.limit);
    let ended = || {
        std::fs::read_to_string(&log).is_ok_and(|text| {
            text.lines()
                .any(|l| l.starts_with("*** ") && l.ends_with(" ***"))
        })
    };
    let mut answer = |bytes: &[u8]| {
        Ok(parse(&mut parser, bytes, |s: &mut Terminal| {
            std::mem::take(&mut s.0)
        }))
    };
    let finished = pump(&mut master, &mut answer, deadline, ended);
    stop(&mut client);
    drop(server);
    let finished = finished?;
    Ok(Ran {
        log: std::fs::read_to_string(&log).unwrap_or_default(),
        stopped: !finished,
    })
}

/// What every area's run shares.
struct Job<'a> {
    request: &'a Request,
    python: &'a Path,
    suite: &'a Path,
}

impl Job<'_> {
    /// Where esctest writes each test's bytes, with `--replays`.
    fn cases_dir(&self, scratch: &Path, area: &Area) -> Result<Option<PathBuf>, String> {
        if !self.request.replays {
            return Ok(None);
        }
        let dir = scratch.join(&area.name);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Some(dir))
    }
}

/// Each test of `area`, from its run.
fn results(area: &Area, ran: &Ran, limit: Duration) -> Tests {
    let found = outcomes(&ran.log);
    area.tests
        .iter()
        .map(|name| {
            let outcome = found.get(name).cloned().unwrap_or_else(|| {
                Outcome::Fail(if ran.stopped {
                    format!("no result: the area was stopped after {}s", limit.as_secs())
                } else {
                    "no result: esctest did not run it".into()
                })
            });
            (name.clone(), outcome)
        })
        .collect()
}

/// An area's results, or why it could not run.
type AreaResult = Result<Tests, String>;

/// Runs every area, `jobs` at a time, with `run`; each area's results, in
/// the areas' order.
fn run_all(
    areas: &[Area],
    jobs: usize,
    limit: Duration,
    run: &(dyn Fn(&Area) -> Result<Ran, String> + Sync),
) -> Result<Vec<Tests>, String> {
    // The largest first, so that the last to finish is small.
    let mut order: Vec<usize> = (0..areas.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(areas.get(i).map_or(0, |a| a.tests.len())));
    let next = AtomicUsize::new(0);
    let done: Mutex<BTreeMap<usize, AreaResult>> = Mutex::new(BTreeMap::new());
    std::thread::scope(|scope| {
        for _ in 0..jobs.min(areas.len()) {
            scope.spawn(|| {
                while let Some(&i) = order.get(next.fetch_add(1, Ordering::SeqCst)) {
                    let Some(area) = areas.get(i) else {
                        break;
                    };
                    let result = run(area).map(|ran| results(area, &ran, limit));
                    if let Ok(mut done) = done.lock() {
                        done.insert(i, result);
                    }
                }
            });
        }
    });
    let done = done.into_inner().map_err(|_| "a run panicked")?;
    (0..areas.len())
        .map(|i| {
            done.get(&i)
                .cloned()
                .unwrap_or(Err("an area did not run".into()))
        })
        .collect()
}

/// Where a listed failure applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    /// Both directly and in a fux pane.
    Both,
    /// Directly only: in a pane, fux does what fux-vt alone does not.
    Direct,
    /// In a fux pane only: fux changes what fux-vt does.
    Fux,
}

/// Why a listed test fails: the first word of its reason.
const REASONS: &[&str] = &[
    // fux-vt's README, "Departures from the references".
    "departure:",
    // A choice a spec makes, or leaves to the terminal, that esctest's
    // expectation (xterm's) does not share.
    "spec:",
    // No real program in the corpus sends it, or none needs the answer.
    "not-implemented:",
    // xterm fails it too: esctest marks it a known bug of xterm's, or as
    // needing an option xterm is not run with.
    "xterm-too:",
    // A bug, to fix in a later fux-vt (or fux) PR, with its replay.
    "bug:",
];

/// A listed failure.
#[derive(Clone, Debug)]
struct Listed {
    scope: Scope,
    reason: String,
}

/// Reads the list of expected failures: one test a line, `NAME
/// [direct|fux] REASON`, `#` starting a comment.
fn expected(text: &str) -> Result<BTreeMap<String, Listed>, String> {
    let mut out = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let at = |e: &str| format!("esctest-expected.txt line {}: {e}", n.saturating_add(1));
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, rest) = line
            .split_once(char::is_whitespace)
            .ok_or(at("a test with no reason"))?;
        let rest = rest.trim_start();
        let (scope, reason) = if let Some(r) = rest.strip_prefix("[direct]") {
            (Scope::Direct, r.trim_start())
        } else if let Some(r) = rest.strip_prefix("[fux]") {
            (Scope::Fux, r.trim_start())
        } else {
            (Scope::Both, rest)
        };
        if !REASONS.iter().any(|r| reason.starts_with(r)) {
            return Err(at(&format!(
                "the reason must start with one of {}",
                REASONS.join(" ")
            )));
        }
        let listed = Listed {
            scope,
            reason: reason.to_owned(),
        };
        if out.insert(name.to_owned(), listed).is_some() {
            return Err(at(&format!("{name} is listed twice")));
        }
    }
    Ok(out)
}

/// Whether `listed` says the test fails in a run `in_fux` or not.
fn listed_fails(listed: Option<&Listed>, in_fux: bool) -> bool {
    match listed.map(|l| l.scope) {
        None => false,
        Some(Scope::Both) => true,
        Some(Scope::Direct) => !in_fux,
        Some(Scope::Fux) => in_fux,
    }
}

/// Where `results` and the list disagree: a failure not listed, or a test
/// listed that passes.
fn mismatches(
    results: &[(String, Outcome)],
    list: &BTreeMap<String, Listed>,
    in_fux: bool,
) -> Vec<String> {
    let mut out = Vec::new();
    for (name, outcome) in results {
        let listed = listed_fails(list.get(name), in_fux);
        match outcome {
            Outcome::Fail(message) if !listed => {
                let first = message.lines().next().unwrap_or_default();
                out.push(format!("{name}: fails, not listed: {first}"));
            }
            Outcome::Pass { .. } if listed => {
                out.push(format!("{name}: passes, but is listed: remove it"));
            }
            Outcome::Pass { .. } | Outcome::Fail(_) | Outcome::Skip(_) => {}
        }
    }
    out
}

/// Counts of a set of results.
#[derive(Clone, Copy, Default)]
struct Tally {
    passed: usize,
    beyond_xterm: usize,
    failed: usize,
    skipped: usize,
}

impl Tally {
    fn of<'a>(results: impl IntoIterator<Item = &'a Outcome>) -> Tally {
        let mut t = Tally::default();
        for outcome in results {
            match outcome {
                Outcome::Pass { beyond_xterm } => {
                    t.passed = t.passed.saturating_add(1);
                    if *beyond_xterm {
                        t.beyond_xterm = t.beyond_xterm.saturating_add(1);
                    }
                }
                Outcome::Fail(_) => t.failed = t.failed.saturating_add(1),
                Outcome::Skip(_) => t.skipped = t.skipped.saturating_add(1),
            }
        }
        t
    }
    /// The share of the tests run that passed, in percent.
    fn rate(self) -> f64 {
        let run = self.passed.saturating_add(self.failed);
        if run == 0 {
            return 0.0;
        }
        f64::from(u32::try_from(self.passed).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(run).unwrap_or(u32::MAX))
            * 100.0
    }
    fn json(self) -> serde_json::Value {
        serde_json::json!({
            "passed": self.passed,
            "passed_beyond_xterm": self.beyond_xterm,
            "failed": self.failed,
            "skipped": self.skipped,
            "pass_rate": (self.rate() * 10.0).round() / 10.0,
        })
    }
}

/// A test's result, with the reason the list gives for its failure.
fn outcome_json(outcome: &Outcome, listed: Option<&Listed>) -> serde_json::Value {
    let mut value = match outcome {
        Outcome::Pass { beyond_xterm } => {
            serde_json::json!({"outcome": "pass", "beyond_xterm": beyond_xterm})
        }
        Outcome::Fail(message) => serde_json::json!({"outcome": "fail", "message": message}),
        Outcome::Skip(why) => serde_json::json!({"outcome": "skip", "message": why}),
    };
    if let (Some(listed), Some(object)) = (listed, value.as_object_mut()) {
        object.insert("listed".into(), serde_json::json!(listed.reason));
    }
    value
}

/// The replay of a test's bytes, as esctest wrote them to its test-case
/// file (what it wrote, without its report requests).
fn replay(scratch: &Path, name: &str) -> Option<String> {
    let (area, _) = name.split_once('.')?;
    let bytes = std::fs::read(scratch.join(area).join(format!("{name}.txt"))).ok()?;
    Some(format!(
        "run.sh replay --size {ROWS}x{COLS} '{}'",
        escape::escape(&bytes)
    ))
}

/// The areas' table, and the total, for `runs` (directly, and in fux and
/// xterm if asked): each area's passes of the tests run, `+N` skipped, a
/// few areas a line so that it fits a screen.
fn table(areas: &[Area], runs: &[(&str, &[Tests])]) -> String {
    let cell = |t: Tally| {
        let run = t.passed.saturating_add(t.failed);
        let skipped = if t.skipped > 0 {
            format!("+{}", t.skipped)
        } else {
            String::new()
        };
        format!("{:>3}/{:<3}{skipped:<3}", t.passed, run)
    };
    let entries: Vec<String> = areas
        .iter()
        .enumerate()
        .map(|(i, area)| {
            let name = area.name.strip_suffix("Tests").unwrap_or(&area.name);
            let mut entry = format!("{name:<24}");
            for (_, results) in runs {
                let tally = Tally::of(results.get(i).into_iter().flatten().map(|(_, o)| o));
                entry.push_str(&cell(tally));
            }
            entry
        })
        .collect();
    let per_line = if runs.len() > 1 { 2 } else { 3 };
    let mut out = String::new();
    if runs.len() > 1 {
        let labels: Vec<&str> = runs.iter().map(|(label, _)| *label).collect();
        let _ = writeln!(
            out,
            "each area: passed/run (+skipped) {}",
            labels.join(", ")
        );
    } else {
        out.push_str("each area: passed/run (+skipped)\n");
    }
    let mut total = format!("{:<24}", "ALL");
    for (_, results) in runs {
        let tally = Tally::of(results.iter().flatten().map(|(_, o)| o));
        let _ = write!(
            total,
            "{}/{} ({:.1}%) +{}  ",
            tally.passed,
            tally.passed.saturating_add(tally.failed),
            tally.rate(),
            tally.skipped
        );
    }
    let mut line = String::new();
    for (i, entry) in entries.into_iter().enumerate() {
        line.push_str(&entry);
        if i.saturating_add(1).checked_rem(per_line) == Some(0) {
            out.push_str(line.trim_end());
            out.push('\n');
            line.clear();
        } else {
            line.push_str("  ");
        }
    }
    if !line.is_empty() {
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.push_str(total.trim_end());
    out.push('\n');
    out
}

/// The listed failures among `results` by the kind of their reason, and
/// those that are bugs, in full.
fn reasons(results: &[(String, Outcome)], list: &BTreeMap<String, Listed>) -> String {
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    let mut bugs = Vec::new();
    for (name, outcome) in results {
        if !outcome.failed() {
            continue;
        }
        let Some(listed) = list.get(name) else {
            continue;
        };
        let kind = listed.reason.split(':').next().unwrap_or_default();
        let count = kinds.entry(kind).or_default();
        *count = count.saturating_add(1);
        if kind == "bug" {
            bugs.push(format!("  {name} {}", listed.reason));
        }
    }
    let mut out = String::from("failures listed, by reason:");
    for (kind, count) in kinds {
        let _ = write!(out, " {kind} {count}");
    }
    out.push('\n');
    for bug in bugs {
        out.push_str(&bug);
        out.push('\n');
    }
    out
}

/// Where an area runs: against a terminal directly (the one under test, or
/// the one beside it), in a fux pane, or in xterm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    Direct(Subject),
    Fux,
    Xterm,
}

impl Place {
    /// How the place is named in the table and the summary, with
    /// `terminal` the terminal under test: fux-vt's direct run is
    /// `directly`, an engine's or a terminal's beside it is its name.
    fn label(self, terminal: Subject) -> String {
        match self {
            Place::Direct(Subject::FuxVt) if terminal == Subject::FuxVt => "directly".into(),
            Place::Direct(subject) => subject.name().into(),
            Place::Fux => "in fux".into(),
            Place::Xterm => "in xterm".into(),
        }
    }
    /// Its key in the JSON: the terminal under test's is `direct`, the one
    /// beside it is its name (`fux_vt`).
    fn key(self, terminal: Subject) -> String {
        match self {
            Place::Direct(subject) if subject == terminal => "direct".into(),
            Place::Direct(subject) => subject.name().replace('-', "_"),
            Place::Fux => "in_fux".into(),
            Place::Xterm => "xterm".into(),
        }
    }
}

/// One place's results: each area's, in the areas' order, and how long
/// they took; how it is named, and its JSON key ([`Place::label`],
/// [`Place::key`]).
struct Column {
    place: Place,
    label: String,
    key: String,
    results: Vec<Tests>,
    time: Duration,
}

impl Column {
    fn flat(&self) -> Tests {
        self.results.iter().flatten().cloned().collect()
    }
    fn tally(&self) -> Tally {
        Tally::of(self.results.iter().flatten().map(|(_, o)| o))
    }
}

/// In a fux pane, a test that reads cells waits for a DECRQCRA reply the
/// pane never gives: fux's panes leave rectangle checksums off (see
/// [`options`]). It says nothing of fux, so it is not counted.
fn unanswered_checksum(outcome: Outcome) -> Outcome {
    match outcome {
        Outcome::Fail(message)
            if message.contains("Timeout waiting to read")
                && message.contains("GetChecksumOfRect") =>
        {
            Outcome::Skip("reads cells by DECRQCRA, which fux's panes do not answer".into())
        }
        other @ (Outcome::Pass { .. } | Outcome::Fail(_) | Outcome::Skip(_)) => other,
    }
}

/// For an engine answering as [`crate::answering`] says, a test that reads
/// cells in origin mode waits for a DECRQCRA reply that is never given:
/// there the rectangle is relative to the margins, which the engine's
/// snapshot does not give. Its cells cannot be read, so it is not counted.
fn unread_checksum(outcome: Outcome, name: &str) -> Outcome {
    match outcome {
        Outcome::Fail(message)
            if message.contains("Timeout waiting to read")
                && message.contains("GetChecksumOfRect") =>
        {
            Outcome::Skip(format!(
                "cannot read: reads cells by DECRQCRA in origin mode, where {name}'s margins \
                 cannot be read"
            ))
        }
        other @ (Outcome::Pass { .. } | Outcome::Fail(_) | Outcome::Skip(_)) => other,
    }
}

/// Each outcome of `results`, as `f` reads it again.
fn reread(results: &mut [Tests], f: impl Fn(Outcome) -> Outcome) {
    for (_, outcome) in results.iter_mut().flatten() {
        let taken = std::mem::replace(outcome, Outcome::Skip(String::new()));
        *outcome = f(taken);
    }
}

/// The list of expected failures in `path`.
fn read_list(path: &Path) -> Result<BTreeMap<String, Listed>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    expected(&text)
}

/// How `outcome` reads in a list of differences.
fn say(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Pass { .. } => "passes".to_owned(),
        Outcome::Fail(m) => format!("fails ({})", m.lines().next().unwrap_or_default()),
        Outcome::Skip(_) => "skipped".to_owned(),
    }
}

/// The tests run in both `a` and `b` that pass in one and fail in the
/// other.
fn differences(a: &Column, b: &Column) -> Vec<String> {
    a.flat()
        .iter()
        .zip(b.flat())
        .filter(|((_, x), (_, y))| (x.passed() && y.failed()) || (x.failed() && y.passed()))
        .map(|((name, x), (_, y))| {
            format!("{name}: {} {}, {} {}", a.label, say(x), b.label, say(&y))
        })
        .collect()
}

/// `esctest`: see the module documentation and [`USAGE`].
pub fn run(argv: &[String]) -> Result<bool, String> {
    let Some(request) = request(argv)? else {
        println!("{USAGE}");
        return Ok(true);
    };
    let suite = suite()?;
    let python = which("python3")?;
    let areas = areas(&python, &suite, request.filter.as_deref())?;
    if areas.is_empty() {
        return Err("no esctest test matches".into());
    }
    let count: usize = areas.iter().map(|a| a.tests.len()).sum();
    let list = read_list(&request.expected)?;
    let beside = match request.beside {
        Some(subject) => {
            let path = subject.expected();
            Some((subject, read_list(&path)?, path))
        }
        None => None,
    };
    let scratch = request.logs.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("fux-vt-esctest-{}", std::process::id()))
    });
    std::fs::create_dir_all(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let job = Job {
        request: &request,
        python: &python,
        suite: &suite,
    };
    println!(
        "esctest: {count} tests in {} areas, --expected-terminal={EXPECTED_TERMINAL} \
         --max-vt-level={VT_LEVEL} --timeout={}, {} at a time",
        areas.len(),
        request.timeout,
        request.jobs
    );
    let terminal = request.terminal;
    if let Subject::Engine(_) = terminal {
        println!(
            "terminal: {}, answering as src/answering.rs says (DECRQCRA from its screen)",
            terminal.name()
        );
    }
    let fux = if request.in_fux {
        Some(build_fux()?)
    } else {
        None
    };
    let mut places = vec![Place::Direct(terminal)];
    if let Some((subject, _, _)) = &beside {
        places.push(Place::Direct(*subject));
    }
    if let Some(fux) = &fux {
        places.push(Place::Fux);
        println!("fux: {}", fux.display());
    }
    if request.xterm {
        places.push(Place::Xterm);
    }
    let mut columns = Vec::new();
    for place in places {
        let at = Instant::now();
        let mut results = run_all(
            &areas,
            request.jobs,
            request.limit,
            &|area| match (place, &fux) {
                (Place::Fux, Some(fux)) => in_fux(&job, fux, area, &scratch),
                (Place::Xterm, _) => in_xterm(&job, area, &scratch),
                (Place::Direct(subject), _) => direct(&job, subject, area, &scratch),
                (Place::Fux, None) => direct(&job, terminal, area, &scratch),
            },
        )?;
        match place {
            Place::Fux => reread(&mut results, unanswered_checksum),
            Place::Direct(subject @ Subject::Engine(_)) => {
                reread(&mut results, |o| unread_checksum(o, subject.name()));
            }
            Place::Direct(Subject::FuxVt) | Place::Xterm => {}
        }
        columns.push(Column {
            place,
            label: place.label(terminal),
            key: place.key(terminal),
            results,
            time: at.elapsed(),
        });
    }
    let runs: Vec<(&str, &[Tests])> = columns
        .iter()
        .map(|c| (c.label.as_str(), c.results.as_slice()))
        .collect();
    print!("{}", table(&areas, &runs));
    let mut problems = Vec::new();
    let mut json = serde_json::json!({
        "suite": "esctest2",
        "expected_terminal": EXPECTED_TERMINAL,
        "max_vt_level": VT_LEVEL,
        "timeout": request.timeout,
        "filter": request.filter,
        "tests": count,
    });
    if let (Subject::Engine(_), Some(object)) = (terminal, json.as_object_mut()) {
        object.insert("terminal".into(), serde_json::json!(terminal.name()));
    }
    // Each column's list: the beside terminal's own, the list given for
    // every other.
    let list_of = |place: Place| match (&beside, place) {
        (Some((subject, list, _)), Place::Direct(s)) if s == *subject => list,
        _ => &list,
    };
    for column in &columns {
        let flat = column.flat();
        let first = column.place == Place::Direct(terminal);
        if column.place != Place::Xterm {
            let found = mismatches(&flat, list_of(column.place), column.place == Place::Fux);
            problems.extend(found.iter().map(|p| {
                if first {
                    p.clone()
                } else {
                    format!("{}: {p}", column.label)
                }
            }));
        }
        if first && (request.show || request.replays) {
            for (name, outcome) in &flat {
                if let Outcome::Fail(message) = outcome {
                    if request.show {
                        println!("-- {name}\n{message}");
                    }
                    if request.replays
                        && let Some(replay) = replay(&scratch, name)
                    {
                        println!("{name}: {replay}");
                    }
                }
            }
        }
        if let Some(object) = json.as_object_mut() {
            object.insert(
                column.key.clone(),
                run_json(&areas, &column.results, column.time, list_of(column.place)),
            );
        }
    }
    let alone = match terminal {
        Subject::FuxVt => "fux-vt alone",
        Subject::Engine(_) => terminal.name(),
    };
    if let Some(direct) = columns.first() {
        // The terminal beside is compared below, feature by feature.
        for other in columns
            .iter()
            .skip(1)
            .filter(|c| !matches!(c.place, Place::Direct(_)))
        {
            let differ = differences(direct, other);
            // xterm, the reference, passes many tests fux-vt does not
            // implement: they are counted, and listed with --show.
            if other.place == Place::Xterm && !request.show {
                let fails = format!("{} fails", direct.label);
                let direct_fails = differ.iter().filter(|d| d.contains(&fails)).count();
                println!(
                    "in xterm: {direct_fails} tests pass that fail {}, {} the other way \
                     (--show lists them)",
                    direct.label,
                    differ.len().saturating_sub(direct_fails)
                );
            } else if !differ.is_empty() {
                println!("where {} differs from {alone}:", other.label);
                for line in &differ {
                    println!("  {line}");
                }
            }
            if let Some(object) = json.as_object_mut() {
                object.insert(
                    format!("differences_{}", other.key),
                    serde_json::json!(differ),
                );
            }
        }
    }
    if let (Some(direct), Some((subject, other_list, _))) = (columns.first(), &beside)
        && let Some(other) = columns.iter().find(|c| c.place == Place::Direct(*subject))
    {
        let compared = Comparison::of(
            &areas,
            [
                (terminal.name(), direct, &list),
                (subject.name(), other, other_list),
            ],
        );
        print!("{}", compared.text());
        if let Some(object) = json.as_object_mut() {
            object.insert("comparison".into(), compared.json());
        }
    }
    if let Some(object) = json.as_object_mut() {
        object.insert("mismatches".into(), serde_json::json!(problems));
    }
    if let Some(path) = &request.json {
        let mut text =
            serde_json::to_string_pretty(&json).map_err(|e| format!("the results: {e}"))?;
        text.push('\n');
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if request.logs.is_none() {
        let _ = std::fs::remove_dir_all(&scratch);
    }
    if let Some(direct) = columns.first() {
        print!("{}", reasons(&direct.flat(), &list));
    }
    for problem in &problems {
        println!("MISMATCH {problem}");
    }
    let mut summary = String::from("esctest:");
    for (i, column) in columns.iter().enumerate() {
        let t = column.tally();
        let _ = write!(
            summary,
            "{} {} {} of {} pass ({:.1}%), {} skipped, in {:.1}s",
            if i == 0 { "" } else { ";" },
            column.label,
            t.passed,
            t.passed.saturating_add(t.failed),
            t.rate(),
            t.skipped,
            column.time.as_secs_f64()
        );
    }
    let _ = write!(
        summary,
        "; {} mismatches with {}",
        problems.len(),
        request.expected.display()
    );
    if let Some((_, _, path)) = &beside {
        let _ = write!(summary, " and {}", path.display());
    }
    println!("{summary}");
    Ok(problems.is_empty())
}

/// A test one terminal passes and the other fails.
struct Only {
    test: String,
    /// The failing terminal's message, its first line.
    failure: String,
    /// The reason the failing terminal's list gives: what the feature is.
    reason: String,
}

impl Only {
    fn json(&self) -> serde_json::Value {
        let (kind, feature) = self
            .reason
            .split_once(": ")
            .unwrap_or(("", self.reason.as_str()));
        serde_json::json!({
            "test": self.test,
            "kind": kind,
            "feature": feature,
            "source": source(&self.reason),
            "failure": self.failure,
        })
    }
}

/// Two terminals' results side by side, for `--beside`: each area's pass
/// rate, and the tests one passes and the other fails, each with what its
/// feature is (the failing terminal's list says) and where it is specified
/// ([`source`]).
struct Comparison {
    names: [&'static str; 2],
    /// Each area's tallies, the first terminal's then the second's.
    areas: Vec<(String, [Tally; 2])>,
    totals: [Tally; 2],
    /// The tests only the first passes, then those only the second does.
    only: [Vec<Only>; 2],
    /// The tests run in one and not the other: what each did.
    one_ran: Vec<(String, [String; 2])>,
}

impl Comparison {
    fn of(
        areas: &[Area],
        sides: [(&'static str, &Column, &BTreeMap<String, Listed>); 2],
    ) -> Comparison {
        let [(a_name, a, a_list), (b_name, b, b_list)] = sides;
        let tallies = |c: &Column, i: usize| {
            Tally::of(c.results.get(i).into_iter().flatten().map(|(_, o)| o))
        };
        let per_area = areas
            .iter()
            .enumerate()
            .map(|(i, area)| (area.name.clone(), [tallies(a, i), tallies(b, i)]))
            .collect();
        let only = |name: &str, outcome: &Outcome, list: &BTreeMap<String, Listed>| Only {
            test: name.to_owned(),
            failure: match outcome {
                Outcome::Fail(m) => m.lines().next().unwrap_or_default().to_owned(),
                Outcome::Pass { .. } | Outcome::Skip(_) => String::new(),
            },
            reason: list
                .get(name)
                .map_or_else(|| "unlisted".to_owned(), |l| l.reason.clone()),
        };
        let mut first = Vec::new();
        let mut second = Vec::new();
        let mut one_ran = Vec::new();
        for ((name, x), (_, y)) in a.flat().iter().zip(b.flat()) {
            if x.passed() && y.failed() {
                first.push(only(name, &y, b_list));
            } else if x.failed() && y.passed() {
                second.push(only(name, x, a_list));
            } else if matches!(x, Outcome::Skip(_)) != matches!(y, Outcome::Skip(_)) {
                let what = |o: &Outcome| match o {
                    Outcome::Skip(why) => format!("skipped: {why}"),
                    Outcome::Pass { .. } | Outcome::Fail(_) => say(o),
                };
                one_ran.push((name.clone(), [what(x), what(&y)]));
            }
        }
        Comparison {
            names: [a_name, b_name],
            areas: per_area,
            totals: [a.tally(), b.tally()],
            only: [first, second],
            one_ran,
        }
    }

    /// The comparison as text: a line per area, then each list.
    fn text(&self) -> String {
        let [a, b] = self.names;
        let rate = |t: Tally| {
            let run = t.passed.saturating_add(t.failed);
            format!("{:>3}/{:<3} {:>5.1}%", t.passed, run, t.rate())
        };
        let mut out = format!(
            "{a} beside {b}, each area's pass rate (passed/run):\n  {:<24}{a:<16}{b}\n",
            "AREA"
        );
        for (name, [x, y]) in self
            .areas
            .iter()
            .chain(std::iter::once(&("ALL".to_owned(), self.totals)))
        {
            let name = name.strip_suffix("Tests").unwrap_or(name);
            let mark = match x.passed.cmp(&y.passed) {
                std::cmp::Ordering::Greater => {
                    format!("  {a} +{}", x.passed.saturating_sub(y.passed))
                }
                std::cmp::Ordering::Less => format!("  {b} +{}", y.passed.saturating_sub(x.passed)),
                std::cmp::Ordering::Equal => String::new(),
            };
            let _ = writeln!(out, "  {name:<24}{:<16}{}{mark}", rate(*x), rate(*y));
        }
        let [first, second] = &self.only;
        for (list, pass, fail) in [(first, a, b), (second, b, a)] {
            let _ = writeln!(out, "passes in {pass}, fails in {fail}: {}", list.len());
            for only in list {
                let _ = writeln!(out, "  {}: {}", only.test, only.reason);
                if let Some(source) = source(&only.reason) {
                    let _ = writeln!(out, "      source: {source}");
                }
            }
        }
        if !self.one_ran.is_empty() {
            let _ = writeln!(
                out,
                "run in one only: {} (the JSON's comparison lists them)",
                self.one_ran.len()
            );
        }
        out
    }

    fn json(&self) -> serde_json::Value {
        let [a, b] = self.names;
        let mut areas = serde_json::Map::new();
        for (name, [x, y]) in &self.areas {
            areas.insert(name.clone(), serde_json::json!({a: x.json(), b: y.json()}));
        }
        let [first, second] = &self.only;
        let [x, y] = self.totals;
        serde_json::json!({
            "terminals": [a, b],
            "total": {a: x.json(), b: y.json()},
            "areas": areas,
            "passes_only_in": {
                a: first.iter().map(Only::json).collect::<Vec<_>>(),
                b: second.iter().map(Only::json).collect::<Vec<_>>(),
            },
            "run_in_one_only": self.one_ran.iter().map(|(test, [x, y])| {
                serde_json::json!({"test": test, a: x, b: y})
            }).collect::<Vec<_>>(),
        })
    }
}

/// Where the feature a list's `reason` names is specified: xterm's
/// ctlseqs, DEC STD 070 (the 1990 revision of chapter 5 in
/// `references/dec/`), the VT510 manual (`references/dec/vt100.net/
/// vt510-rm/`, a page a function), ECMA-48. Matched by the first entry
/// whose words the reason contains, so the narrower come first; every
/// reason in both lists has one (a test says so).
const SOURCES: &[(&str, &str)] = &[
    (
        "DECSTR",
        "DEC STD 070 ch. 4, Soft Terminal Reset (p. 4-37); VT510 DECSTR; ctlseqs CSI ! p",
    ),
    (
        "BS without",
        "DEC STD 070 §5.4.4, Back Space, and §5.4.3, Set Left and Right Margins; ECMA-48 BS",
    ),
    (
        "CHA, HPR and VPR",
        "DEC STD 070 §5.4.3, Set/Reset Origin Mode, and §5.4.4, Horizontal/Vertical Position; \
         ECMA-48 CHA, HPR, VPR; VT510 DECOM",
    ),
    (
        "RIS leaves",
        "DEC STD 070 appendix 0 §0.2, Reset to Initial State, and §5.4.6, Set/Reset Column \
         Mode; ctlseqs ESC c, CSI ? 3 h, CSI ? 40 h",
    ),
    (
        "TBC",
        "ECMA-48 TBC (Ps 0, the default: the stop at the active position); DEC STD 070 §5.4.5, \
         Tabulation Clear; ctlseqs CSI Ps g",
    ),
    (
        "needs window operations",
        "ctlseqs CSI Ps t (XTWINOPS: 20 and 21 t report the titles) and OSC 52 (Manipulate \
         Selection Data), with xterm's allowWindowOps resource",
    ),
    (
        "reads the title back",
        "ctlseqs CSI 20 t and CSI 21 t (XTWINOPS), with xterm's disallowedWindowOps resource",
    ),
    (
        "ISO-protected",
        "ECMA-48 SPA, EPA and ERM; ctlseqs CSI ? Ps J and CSI ? Ps K (DECSED, DECSEL), ESC V and \
         ESC W; DEC STD 070 §5.11.1.2, Selectively Erasable Character Attribute",
    ),
    (
        "xterm always returns 4",
        "DEC STD 070 §5.13.2, Request Mode, and ch. 6 §6.6.3, Auto Repeat Mode; VT510 DECRQM, \
         DECARM; ctlseqs CSI ? Ps $ p",
    ),
    (
        "DECSERA (",
        "DEC STD 070 §5.12.1, Selective Erase Rectangular Area; VT510 DECSERA; ctlseqs \
         CSI Pt ; Pl ; Pb ; Pr $ {",
    ),
    (
        "protected cells (DECSCA",
        "DEC STD 070 §5.11.1.2, Selectively Erasable Character Attribute (DECSCA, DECSED, DECSEL), \
         and §5.12.1 (DECSERA); ECMA-48 SPA, EPA; ctlseqs CSI Ps \" q, CSI ? Ps J, CSI ? Ps K",
    ),
    (
        "rectangle operations",
        "DEC STD 070 §5.12.1, Rectangular Area Operations; VT510 DECCRA, DECFRA, DECERA, \
         DECCARA, DECRARA, DECSACE; ctlseqs CSI … $ v, $ x, $ z, $ r, $ t, * x",
    ),
    (
        "DECIC and DECDC",
        "DEC STD 070 §5.11, Insert Column and Delete Column; VT510 DECIC, DECDC; ctlseqs \
         CSI Pn ' } and CSI Pn ' ~",
    ),
    (
        "DECBI and DECFI",
        "DEC STD 070 §5.4.3, Back Index and Forward Index; VT510 DECBI, DECFI; ctlseqs ESC 6, \
         ESC 9",
    ),
    (
        "left and right margins",
        "DEC STD 070 §5.4.3, Set Left and Right Margins and Set/Reset Left Right Margin Mode; \
         VT510 DECSLRM, DECLRMM; ctlseqs CSI ? 69 h, CSI Pl ; Pr s",
    ),
    (
        "reverse wraparound",
        "ctlseqs CSI ? 45 h (reverse-wraparound mode) and CSI ? 1045 h (extended \
         reverse-wraparound mode)",
    ),
    (
        "device-independent colour spaces",
        "ctlseqs OSC 4 and OSC 10 to 19, whose colours are XParseColor's; Xlib manual, Color \
         Strings (CIEXYZ, CIEuvY, CIExyY, CIELab, CIELuv, TekHVC)",
    ),
    (
        "#RGB",
        "Xlib manual, Color Strings (#RGB: the digits are the high bits); ctlseqs OSC 4 and \
         OSC 10 to 19",
    ),
    (
        "rgbi:",
        "Xlib manual, Color Strings (RGBi); ctlseqs OSC 4 and OSC 10 to 19",
    ),
    (
        "special colours",
        "ctlseqs OSC 5 and OSC 105 (special colours), OSC 4 past the palette, XTGETTCAP Co",
    ),
    (
        "colour palette",
        "ctlseqs OSC 4, OSC 5, OSC 10 to 19, OSC 104, OSC 105, OSC 110 to 119",
    ),
    (
        "DECRQSS",
        "DEC STD 070 §5.13.2, Request Selection or Setting; VT510 DECRQSS; ctlseqs \
         DCS $ q Pt ST",
    ),
    (
        "DECRQM",
        "DEC STD 070 §5.13.2, Request Mode and Report Mode; VT510 DECRQM; ctlseqs CSI Ps $ p \
         and CSI ? Ps $ p",
    ),
    (
        "DECXCPR",
        "DEC STD 070 §5.4.4, Extended Cursor Position Report; VT510 DSR-XCPR; ctlseqs \
         CSI ? 6 n",
    ),
    (
        "DSR reports of DEC's other devices",
        "DEC STD 070 ch. 4, Device Status Report (p. 4-41), and §5.12.2, Memory Checksum DSR; \
         VT510 DSR-PP, DSR-UDK, DSR-KBD, DSR-DECCKSR, DSR-DIR, DSR-MSR; ctlseqs CSI ? Ps n",
    ),
    (
        "DA1",
        "DEC STD 070 ch. 4, Device Attributes (Primary, p. 4-22, and Secondary, p. 4-29); \
         VT510 DA1, DA2; ctlseqs CSI Ps c, CSI > Ps c",
    ),
    (
        "window operations",
        "ctlseqs CSI Ps ; Ps ; Ps t (XTWINOPS), CSI ? 40 h; DEC STD 070 §5.4.6, Set/Reset \
         Column Mode and Set Lines Per Page; VT510 DECCOLM, DECSLPP",
    ),
    (
        "8-bit controls",
        "ctlseqs, C1 (8-Bit) Control Characters, and S8C1T (ESC SP G); ECMA-43, C1 in 8-bit \
         codes",
    ),
    (
        "conformance levels",
        "DEC STD 070 ch. 4, Select Conformance Level (p. 4-27); VT510 DECSCL; ctlseqs \
         CSI Pl ; Pc \" p",
    ),
    (
        "DECALN",
        "DEC STD 070 appendix 0 §0.8, Screen Alignment; VT510 DECALN; ctlseqs ESC # 8",
    ),
    (
        "DECID",
        "DEC STD 070 ch. 4, Identify Terminal (p. 4-33); VT100 User Guide, DECID; ctlseqs ESC Z",
    ),
    (
        "XTSAVE",
        "ctlseqs CSI ? Pm s (XTSAVE) and CSI ? Pm r (XTRESTORE)",
    ),
    (
        "LNM",
        "ECMA-48 LNM; DEC STD 070 §5.4.4, Set/Reset New Line Mode; ctlseqs CSI 20 h",
    ),
    ("more(1) fix", "ctlseqs CSI ? 41 h (more(1) fix)"),
    (
        "known xterm bug",
        "esctest's own marking of xterm (escutil.py, knownBug) on the test",
    ),
];

/// The source of the feature `reason` names ([`SOURCES`]).
fn source(reason: &str) -> Option<&'static str> {
    SOURCES
        .iter()
        .find(|(words, _)| reason.contains(words))
        .map(|(_, source)| *source)
}

/// Runs `area` in a real xterm, under the harness's Xvfb (`engines::xterm`),
/// set up as esctest's README says: 80 by 25, UTF-8 as fux is, a VT420
/// (`decTerminalID`, xterm's default), and DECRQCRA allowed, which xterm's
/// default `disallowedWindowOps` forbids; the other window operations stay
/// as xterm keeps them by default. Its checksum counts a cell nothing was
/// written to as a space (`checksumExtension` 8, "do not skip uninitialized
/// cells", ctlseqs' XTCHECKSUM), as esctest expects of a DEC terminal by
/// default: xterm's own default counts it as nothing, which esctest reads
/// as no character at all, failing every test that reads an empty cell.
fn in_xterm(job: &Job<'_>, area: &Area, scratch: &Path) -> Result<Ran, String> {
    let log = scratch.join(format!("{}.xterm.log", area.name));
    let argv = esctest_argv(job.python, job.suite, area, job.request.timeout, &log, None);
    let display = crate::engines::xterm::display()?;
    let resources = [
        "XTerm*locale: false",
        "XTerm*utf8: 2",
        "XTerm*decTerminalID: 420",
        "XTerm*checksumExtension: 8",
        "XTerm*disallowedWindowOps: GetIconTitle,GetWinTitle,SetSelection,GetSelection,SetXprop",
    ];
    let mut command = Command::new("xterm");
    command
        .arg("-display")
        .arg(format!(":{display}"))
        .arg("-geometry")
        .arg(format!("{COLS}x{ROWS}"))
        .args(["-T", "", "-ut"]);
    for resource in resources {
        command.arg("-xrm").arg(resource);
    }
    let mut child = {
        let _guard = SPAWN.lock().map_err(|_| "a spawn panicked")?;
        command
            .arg("-e")
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("xterm: {e}"))?
    };
    let deadline = deadline(job.request.limit);
    let mut finished = false;
    while Instant::now() < deadline {
        if !matches!(child.try_wait(), Ok(None)) {
            finished = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    stop(&mut child);
    Ok(Ran {
        log: std::fs::read_to_string(&log).unwrap_or_default(),
        stopped: !finished,
    })
}

/// A place's results: the tally, the time, each area's tally and each
/// test's result.
fn run_json(
    areas: &[Area],
    results: &[Tests],
    time: Duration,
    list: &BTreeMap<String, Listed>,
) -> serde_json::Value {
    let mut by_area = serde_json::Map::new();
    let mut tests = serde_json::Map::new();
    for (area, results) in areas.iter().zip(results) {
        by_area.insert(
            area.name.clone(),
            Tally::of(results.iter().map(|(_, o)| o)).json(),
        );
        for (name, outcome) in results {
            tests.insert(name.clone(), outcome_json(outcome, list.get(name)));
        }
    }
    let mut total = Tally::of(results.iter().flatten().map(|(_, o)| o)).json();
    if let Some(object) = total.as_object_mut() {
        object.insert(
            "seconds".into(),
            serde_json::json!((time.as_secs_f64() * 10.0).round() / 10.0),
        );
        object.insert("areas".into(), serde_json::Value::Object(by_area));
        object.insert("tests".into(), serde_json::Value::Object(tests));
    }
    total
}

/// Builds fux (`cargo build --release --bin fux` in the repository) for
/// `--in-fux`; its binary.
fn build_fux() -> Result<PathBuf, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--quiet",
            "--bin",
            "fux",
            "--manifest-path",
        ])
        .arg(root.join("Cargo.toml"))
        .status()
        .map_err(|e| format!("building fux: {e}"))?;
    if !status.success() {
        return Err("building fux failed".into());
    }
    root.join("target/release/fux")
        .canonicalize()
        .map_err(|e| format!("fux's binary: {e}"))
}

#[cfg(test)]
mod tests {
    use super::{Outcome, Scope};

    const LOG: &str = "\
Run test: CUPTests.test_CUP_Default
Passed.

Run test: DECRQMTests.test_DECRQM
*** TEST DECRQMTests.test_DECRQM FAILED:
Traceback (most recent call last):
  File \"esctest.py\", line 97, in RunTest
    method()
esctypes.InternalError: Timeout waiting to read.

Run test: DECSETTests.test_DECSET_Allow80To132
Fails as expected: xterm bug (not trying)

Run test: DECSCLTests.test_DECSCL_Level2DoesntSupportDECRQM
Skipped because terminal lacks requisite capability:

Run test: ECHTests.test_ECH_doesNotRespectDECPRotection
*** TEST ECHTests.test_ECH_doesNotRespectDECPRotection FAILED:
Traceback (most recent call last):
  File \"escutil.py\", line 1, in func_wrapper
    raise esctypes.InternalError(\"Should have failed\")
esctypes.InternalError: Should have failed

Run test: ELTests.test_EL_Basic
*** TEST ELTests.test_EL_Basic FAILED:
Traceback (most recent call last):
  File \"escutil.py\", line 1, in x
    Raise(esctypes.ChecksumException(errorLocations, actual, expected))
esctypes.ChecksumException: Checksum failed at the following locations:
At Point(x=1, y=1) expected 'a' (0x61) but got ' ' (0x20)
Actual:


Expected:
a

*** 1 test passed, 2 known bugs, 2 TESTS FAILED ***
";

    /// esctest's log gives each test's outcome: a pass, a failure with its
    /// exception, a skip, and a test xterm fails that passes.
    #[test]
    fn the_log_gives_each_outcome() -> Result<(), String> {
        let found = super::outcomes(LOG);
        let get = |n: &str| found.get(n).cloned();
        assert_eq!(
            get("CUPTests.test_CUP_Default"),
            Some(Outcome::Pass {
                beyond_xterm: false
            })
        );
        assert_eq!(
            get("DECRQMTests.test_DECRQM"),
            Some(Outcome::Fail(
                "esctypes.InternalError: Timeout waiting to read.".into()
            ))
        );
        assert!(matches!(
            get("DECSETTests.test_DECSET_Allow80To132"),
            Some(Outcome::Skip(_))
        ));
        assert!(matches!(
            get("DECSCLTests.test_DECSCL_Level2DoesntSupportDECRQM"),
            Some(Outcome::Skip(_))
        ));
        assert_eq!(
            get("ECHTests.test_ECH_doesNotRespectDECPRotection"),
            Some(Outcome::Pass { beyond_xterm: true })
        );
        let Some(Outcome::Fail(message)) = get("ELTests.test_EL_Basic") else {
            return Err("EL fails".into());
        };
        assert!(message.starts_with("esctypes.ChecksumException: Checksum failed"));
        assert!(message.ends_with("Expected:\na"));
        assert_eq!(found.len(), 6);
        Ok(())
    }

    /// Every reason in fux-vt's list and in Ghostty's says where its
    /// feature is specified, so each test of `--beside`'s lists has one.
    #[test]
    fn every_listed_reason_has_a_source() -> Result<(), String> {
        for file in ["esctest-expected.txt", "esctest-expected-ghostty.txt"] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
            for (name, listed) in super::read_list(&path)? {
                assert!(
                    super::source(&listed.reason).is_some(),
                    "{file}: {name}: no source for {:?}",
                    listed.reason
                );
            }
        }
        Ok(())
    }

    /// Two terminals' results side by side: each area's tallies, the
    /// tests only one passes with the failing one's reason, and those run
    /// in one only.
    #[test]
    fn a_comparison_lists_what_one_passes_and_the_other_fails() -> Result<(), String> {
        let areas = vec![super::Area {
            name: "ATests".into(),
            tests: vec!["A.t1".into(), "A.t2".into(), "A.t3".into(), "A.t4".into()],
        }];
        let pass = Outcome::Pass {
            beyond_xterm: false,
        };
        let fail = Outcome::Fail("esctypes.TestFailure: x\nmore".into());
        let skip = Outcome::Skip("cannot read".into());
        let column = |results: Vec<Outcome>| super::Column {
            place: super::Place::Xterm,
            label: String::new(),
            key: String::new(),
            results: vec![
                areas
                    .iter()
                    .flat_map(|a| a.tests.iter().cloned())
                    .zip(results)
                    .collect(),
            ],
            time: std::time::Duration::ZERO,
        };
        let a = column(vec![pass.clone(), fail.clone(), skip, pass.clone()]);
        let b = column(vec![fail.clone(), pass.clone(), fail, pass]);
        let a_list = super::expected("A.t2 bug: a's\n")?;
        let b_list = super::expected("A.t1 not-implemented: left and right margins\n")?;
        let compared = super::Comparison::of(&areas, [("a", &a, &a_list), ("b", &b, &b_list)]);
        let json = compared.json();
        let at = |pointer: &str| json.pointer(pointer).cloned().unwrap_or_default();
        assert_eq!(at("/passes_only_in/a/0/test"), "A.t1");
        assert_eq!(at("/passes_only_in/a/0/kind"), "not-implemented");
        assert_eq!(at("/passes_only_in/a/0/feature"), "left and right margins");
        assert_eq!(at("/passes_only_in/a/0/failure"), "esctypes.TestFailure: x");
        assert!(
            at("/passes_only_in/a/0/source")
                .as_str()
                .is_some_and(|s| s.contains("DECSLRM"))
        );
        assert_eq!(at("/passes_only_in/b/0/test"), "A.t2");
        assert_eq!(at("/passes_only_in/b/0/kind"), "bug");
        assert_eq!(at("/run_in_one_only/0/test"), "A.t3");
        assert_eq!(at("/areas/ATests/a/passed"), 2);
        assert_eq!(at("/areas/ATests/b/passed"), 2);
        assert_eq!(at("/areas/ATests/a/skipped"), 1);
        let text = compared.text();
        assert!(text.contains("passes in a, fails in b: 1\n  A.t1: not-implemented"));
        assert!(text.contains("passes in b, fails in a: 1\n  A.t2: bug: a's"));
        Ok(())
    }

    /// A test's own log lines before its outcome do not hide it.
    #[test]
    fn a_tests_own_log_lines_come_before_its_outcome() {
        let found = super::outcomes(
            "Run test: A.test_pass\nRead: ;17;rgb:8080/0000/0000\nPassed.\n\n\
             Run test: A.test_fail\nRead: x\n*** TEST A.test_fail FAILED:\n\
             Traceback (most recent call last):\n  File \"a.py\", line 1, in test_fail\n    \
             f()\nesctypes.InternalError: Timeout waiting to read.\n\n",
        );
        assert_eq!(
            found.get("A.test_pass"),
            Some(&Outcome::Pass {
                beyond_xterm: false
            })
        );
        assert_eq!(
            found.get("A.test_fail"),
            Some(&Outcome::Fail(
                "esctypes.InternalError: Timeout waiting to read. (in test_fail)".into()
            ))
        );
    }

    /// The list of expected failures: a scope, a reason of a known kind,
    /// each test once; a listed test that passes, or a failure not listed,
    /// is a mismatch.
    #[test]
    fn the_list_is_checked_both_ways() -> Result<(), String> {
        let list = super::expected(
            "# comment\nA.t1 bug: x\nA.t2 [fux] spec: y\nA.t3  [direct] xterm-too: z\n",
        )?;
        assert_eq!(list.get("A.t2").map(|l| l.scope), Some(Scope::Fux));
        assert_eq!(list.get("A.t3").map(|l| l.scope), Some(Scope::Direct));
        assert!(super::expected("A.t1 because\n").is_err());
        assert!(super::expected("A.t1 bug: x\nA.t1 bug: y\n").is_err());
        let fail = Outcome::Fail("e".into());
        let pass = Outcome::Pass {
            beyond_xterm: false,
        };
        let results = vec![
            ("A.t1".to_owned(), pass.clone()),
            ("A.t2".to_owned(), fail.clone()),
            ("A.t3".to_owned(), fail),
            ("A.t4".to_owned(), pass),
        ];
        let direct = super::mismatches(&results, &list, false);
        assert_eq!(direct.len(), 2, "{direct:?}");
        assert!(direct.iter().any(|m| m.starts_with("A.t1: passes")));
        assert!(direct.iter().any(|m| m.starts_with("A.t2: fails")));
        let fux = super::mismatches(&results, &list, true);
        assert_eq!(fux.len(), 2, "{fux:?}");
        assert!(fux.iter().any(|m| m.starts_with("A.t3: fails")));
        Ok(())
    }
}
