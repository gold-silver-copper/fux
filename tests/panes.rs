//! The program itself, driven the way a person drives it: with files.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, clippy::disallowed_methods)]

use fux_vt::sixel;
use std::cell::RefCell;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// How long a test has, all of it: two minutes (the whole of them takes a third of one), or the
/// seconds `FUX_TEST_LIMIT` says. Every wait in a test ends at the one moment its first session set
/// (`deadline`): a command that has not ended by then is killed and named, a FIFO nobody reads is an
/// error at once, and nothing a test does waits without that end.
fn limit() -> Duration {
    let seconds = std::env::var("FUX_TEST_LIMIT").ok().and_then(|seconds| seconds.parse().ok());
    Duration::from_secs(seconds.unwrap_or(120))
}
/// A test that is not over this long after its deadline is held where no deadline is asked (a call that
/// waits by itself): its watcher ends every session of the tests and the tests with them.
fn grace() -> Duration {
    limit() / 6
}

/// The test on a thread: how many sessions it has, when it must be over, and the word to its
/// watcher that it is.
type Scenario = Option<(usize, Instant, Arc<AtomicBool>)>;
thread_local! {
    static SCENARIO: RefCell<Scenario> = const { RefCell::new(None) };
}
/// The sessions of the tests that run now: what a watcher ends.
static SESSIONS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
/// (Taken whatever became of a test that had it: a session that ends must not fail at its end, which
/// in a test that is failing would stop every test where it is, its panes left running.)
fn sessions() -> std::sync::MutexGuard<'static, Vec<PathBuf>> {
    SESSIONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// When the test on this thread must be over (one with no session: `limit` from now).
fn deadline() -> Instant {
    SCENARIO
        .with(|scenario| scenario.borrow().as_ref().map(|scenario| scenario.1))
        .unwrap_or_else(|| Instant::now() + limit())
}

/// Ends a session. Each pane's holder leads a session of its own, and its program another: neither
/// goes with the test, nor at once with the directory (a holder looks every two seconds, and a program
/// may ignore the hangup). So those of every pane whose holder lives are killed, the program with
/// its process group; then the directory goes.
fn end(session: &Path) {
    let folders = |folder: &Path| fs::read_dir(folder).into_iter().flatten().flatten().map(|entry| entry.path());
    for pane in folders(session).flat_map(|workspace| folders(&workspace).collect::<Vec<_>>()) {
        // (Whether its holder lives, the lock says. Opened without waiting: a test may have put a
        // FIFO where a pane would be.)
        let held = fuxix::open(&pane, false).is_ok_and(|folder| folder.try_lock_shared().is_err());
        let status = read(&pane.join("status"));
        let number = |name: &str| status.lines().find_map(|line| line.strip_prefix(name)?.parse::<u32>().ok());
        let (Some(holder @ 2..), Some(program @ 2..), true) = (number("holder "), number("pid "), held) else {
            continue;
        };
        // A number in a file is not yet a process of ours (a pane started again has its lock before
        // its new `status`): the holder must be this fux holding, and the program its child.
        let mut ps = Command::new("ps");
        ps.args(["-o", "pid=,ppid=,args=", "-p", &format!("{holder},{program}")]).stdout(Stdio::piped());
        let mut processes = String::new();
        if let Ok(mut ps) = ps.stderr(Stdio::null()).spawn() {
            let _ = ps.stdout.take().map(|mut said| said.read_to_string(&mut processes));
            let _ = ps.wait();
        }
        let is = |pid: u32, parent: Option<u32>, runs: &str| {
            processes.lines().any(|line| {
                let mut words = line.split_whitespace();
                let numbers = (words.next().and_then(|n| n.parse().ok()), words.next().and_then(|n| n.parse().ok()));
                numbers.0 == Some(pid) && parent.is_none_or(|parent| numbers.1 == Some(parent)) && line.contains(runs)
            })
        };
        let ours = is(holder, None, concat!(env!("CARGO_BIN_EXE_fux"), " hold "));
        let targets =
            [(ours && is(program, Some(holder), "")).then(|| format!("-{program}")), ours.then(|| holder.to_string())];
        if targets.iter().any(Option::is_some) {
            let mut kill = Command::new("kill");
            kill.args(["-KILL", "--"]).args(targets.into_iter().flatten()).stdout(Stdio::null()).stderr(Stdio::null());
            let _ = kill.spawn().and_then(|mut kill| kill.wait());
        }
    }
    // (A holder may write a file while its folder is being removed: then once more, so that
    // no pane of a test outlives it.)
    for _ in 0..20 {
        if fs::remove_dir_all(session).is_ok() || !session.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A session directory of the test's own. Dropped, it is ended: its panes with it.
struct Session(PathBuf);
impl Drop for Session {
    fn drop(&mut self) {
        end(&self.0);
        sessions().retain(|session| *session != self.0);
        // (The test's last session: its watcher is told that it is over.)
        SCENARIO.with(|scenario| {
            let mut scenario = scenario.borrow_mut();
            if let Some((sessions, _, over)) = scenario.as_mut() {
                *sessions -= 1;
                if *sessions == 0 {
                    over.store(true, Ordering::Relaxed);
                    *scenario = None;
                }
            }
        });
    }
}

impl Session {
    fn new(name: &str) -> Session {
        // Short, as a deep path would not fit a Unix path limit on macOS.
        let dir = std::env::temp_dir().join(format!("fux-{name}-{}", std::process::id()));
        // (A name is one test's: two that run at once in one directory end each other's panes.)
        let mine = {
            let mut sessions = sessions();
            let mine = !sessions.contains(&dir);
            sessions.push(dir.clone());
            mine
        };
        assert!(mine, "two tests at once have a session called {name}");
        let _ = fs::remove_dir_all(&dir);
        // A test's first session sets its deadline, and starts the watcher of it.
        SCENARIO.with(|scenario| match &mut *scenario.borrow_mut() {
            Some((sessions, ..)) => *sessions += 1,
            scenario @ None => {
                let (end, over) = (Instant::now() + limit(), Arc::new(AtomicBool::new(false)));
                *scenario = Some((1, end, over.clone()));
                watch(name.to_string(), end + grace(), over);
            }
        });
        Session(dir)
    }
    fn fux(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fux"));
        command.env("FUX_DIR", &self.0).env_remove("COLUMNS").env_remove("LINES");
        // Run in a pane of fux, the suite must not take that pane's workspace or keys for its own.
        command.env_remove("FUX_PANE").env_remove("FUX_WS").env_remove("FUX_KEYS");
        command
    }
    fn start(&self, name: &str, program: &[&str]) -> PathBuf {
        let out = self.fux().arg("start").arg(name).arg("--").args(program).run();
        assert!(out.status.success(), "start failed: {}", String::from_utf8_lossy(&out.stderr));
        PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
    }
}

/// Watches a test from a thread of its own: one that is not `over` at `end` waits where nothing
/// asks its deadline. It is named, every session of the tests is ended, and the tests stop.
fn watch(test: String, last: Instant, over: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while !over.load(Ordering::Relaxed) {
            if Instant::now() >= last {
                eprintln!(
                    "the test of the session `{test}` was not over {:?} after its deadline: the tests stop",
                    grace()
                );
                sessions().iter().for_each(|session| end(session));
                std::process::exit(124);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });
}

/// What a test runs ends by the test's deadline, or is killed there and named.
trait Bounded {
    /// As `output`: what the command printed, and how it ended. It is given nothing to read, unless
    /// `from` is a file for it.
    fn run_from(&mut self, from: Option<fs::File>) -> Output;
    fn run(&mut self) -> Output {
        self.run_from(None)
    }
    /// As `status`: how the command ended; what it prints is the test's own output.
    fn ended(&mut self) -> ExitStatus;
}

impl Bounded for Command {
    fn run_from(&mut self, from: Option<fs::File>) -> Output {
        let what = format!("{self:?}");
        self.stdin(from.map_or(Stdio::null(), Stdio::from)).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = self.spawn().unwrap_or_else(|e| panic!("{what}: {e}"));
        // What it prints is read meanwhile, by threads: one that prints more than a pipe holds ends too.
        let reader = |pipe: Option<Box<dyn Read + Send>>| {
            let (read, bytes) = mpsc::channel();
            std::thread::spawn(move || {
                let mut all = Vec::new();
                let _ = pipe.map(|mut pipe| pipe.read_to_end(&mut all));
                let _ = read.send(all);
            });
            bytes
        };
        let stdout = reader(child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
        let stderr = reader(child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
        let status = wait(&mut child, &what);
        // (Something the command started may still hold its pipes: that too is waited for no longer.)
        let all = |bytes: mpsc::Receiver<Vec<u8>>| {
            let left = deadline().saturating_duration_since(Instant::now());
            bytes.recv_timeout(left).unwrap_or_else(|_| panic!("{what}: what it started still holds its output"))
        };
        Output { status, stdout: all(stdout), stderr: all(stderr) }
    }
    fn ended(&mut self) -> ExitStatus {
        let what = format!("{self:?}");
        let mut child = self.stdin(Stdio::null()).spawn().unwrap_or_else(|e| panic!("{what}: {e}"));
        wait(&mut child, &what)
    }
}

/// Waits for a command a test started, until the test's deadline: one that has not ended by then is
/// killed, and the test fails with its command line.
fn wait(child: &mut Child, what: &str) -> ExitStatus {
    let end = deadline();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            panic!("not ended by the test's deadline, and killed: {what}");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Writes to a FIFO (a pane's `in` or `ctl`, a viewer's own) as a client that never waits does:
/// one that nobody reads fails the test at once, and one that stays full fails it at its deadline.
fn send(fifo: &Path, bytes: impl AsRef<[u8]>) {
    let (mut bytes, end) = (bytes.as_ref(), deadline());
    let mut file = fuxix::open(fifo, true).unwrap_or_else(|e| panic!("{}: {e}", fifo.display()));
    while !bytes.is_empty() {
        match file.write(bytes) {
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(e) => panic!("{}: {e}", fifo.display()),
        }
    }
}

/// A file's text, read without waiting: a FIFO of that name has none.
fn read(path: &Path) -> String {
    let mut text = String::new();
    let _ = fuxix::open(path, false).and_then(|mut file| file.read_to_string(&mut text));
    text
}

/// Styled text without its colours (SGR codes).
fn bare(styled: &str) -> String {
    let parts = styled.split("\x1b[").enumerate();
    parts.map(|(i, part)| if i == 0 { part } else { part.split_once('m').map_or("", |(_, text)| text) }).collect()
}

#[test]
fn what_a_test_runs_and_writes_to_ends_at_the_tests_one_deadline() {
    // The session of a test ended: a program that ignores the hangup, in a session of its own, is
    // gone with it, and so is its holder (removing the directory ends neither at once).
    let session = Session::new("bounded");
    let pane = session.start("deaf", &["sh", "-c", "trap '' HUP; echo r''eady; while :; do sleep 1; done"]);
    until("the program ignores the hangup", || read(&pane.join("shown/screen")).contains("ready"));
    let status = read(&pane.join("status"));
    let numbers: Vec<&str> = status.lines().take(2).filter_map(|line| line.split(' ').nth(1)).collect();
    let alive = |pid: &str| Command::new("kill").args(["-0", pid]).stderr(Stdio::null()).ended().success();
    assert!(numbers.len() == 2 && numbers.iter().all(|pid| alive(pid)), "{status}");
    end(&session.0);
    until("the holder and the program are gone", || !numbers.iter().any(|pid| alive(pid)));
    assert!(!session.0.exists());

    // From here the test has three seconds. A command that would run for ten minutes is
    // killed at that moment, and the failure names it.
    let hurried = Instant::now() + Duration::from_secs(3);
    SCENARIO.with(|scenario| scenario.borrow_mut().as_mut().unwrap().1 = hurried);
    let said = |failed: std::thread::Result<()>| *failed.unwrap_err().downcast::<String>().unwrap();
    let sleeps = said(std::panic::catch_unwind(|| drop(Command::new("sleep").arg("600").run())));
    assert!(sleeps.contains("\"sleep\" \"600\"") && Instant::now() >= hurried, "{sleeps}");
    // It is one deadline, not one for each wait: whatever waits after it is over at once (all of it
    // in less than the three seconds one more wait would have).
    let after = Instant::now();
    let waits = said(std::panic::catch_unwind(|| until("something that never is", || false)));
    let sleeps = said(std::panic::catch_unwind(|| _ = Command::new("sleep").arg("600").ended()));
    assert!(waits.contains("something that never is") && sleeps.contains("\"sleep\" \"600\""), "{waits} {sleeps}");
    assert!(after.elapsed() < Duration::from_secs(3), "{:?}", after.elapsed());
    // A FIFO nobody reads holds no test: writing to it fails, and reading it finds nothing.
    fs::create_dir_all(&session.0).unwrap();
    let fifo = session.0.join("fifo");
    assert!(Command::new("mkfifo").arg(&fifo).spawn().unwrap().wait().unwrap().success());
    let refused = said(std::panic::catch_unwind(|| send(&fifo, "typed")));
    assert!(refused.contains("fifo") && read(&fifo).is_empty(), "{refused}");
    assert!(after.elapsed() < Duration::from_secs(3), "{:?}", after.elapsed());
}

#[test]
fn a_test_held_where_nothing_asks_its_deadline_is_named_and_the_tests_stop() {
    // The tests themselves, run for the one test below with two seconds to its name: it waits for
    // good, so its watcher ends its session (the pane's program with it) and the run.
    let dir = std::env::temp_dir().join(format!("fux-held-{}", std::process::id()));
    let mut tests = Command::new(std::env::current_exe().unwrap());
    tests.args(["--exact", "a_test_that_opens_a_fifo_nobody_writes", "--nocapture"]);
    let began = Instant::now();
    let out = tests.env("FUX_TEST_LIMIT", "2").env("FUX_TEST_HELD", &dir).run();
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(124), "{said}");
    assert!(said.contains("the test of the session `held` was not over"), "{said}");
    assert!(
        began.elapsed() < Duration::from_secs(30) && !dir.exists(),
        "{:?}, and its session is there",
        began.elapsed()
    );
}

#[test]
fn a_test_that_opens_a_fifo_nobody_writes() {
    // (It waits for good, where the test above tells it to: that one runs it.)
    let Some(dir) = std::env::var_os("FUX_TEST_HELD").map(PathBuf::from) else { return };
    let session = Session(dir);
    sessions().push(session.0.clone());
    let (end, over) = (Instant::now() + limit(), Arc::new(AtomicBool::new(false)));
    SCENARIO.with(|scenario| *scenario.borrow_mut() = Some((1, end, over.clone())));
    watch("held".to_string(), end + grace(), over);
    let pane = session.start("p", &["sleep", "600"]);
    assert!(Command::new("mkfifo").arg(pane.join("fifo")).ended().success());
    // (To open a FIFO to read is to wait for its writer: nothing here has a deadline.)
    drop(fs::File::open(pane.join("fifo")));
}

#[test]
fn no_test_waits_for_a_command_or_a_fifo_but_through_the_helpers() {
    // What waits by itself, a test calls through `run`, `ended`, `wait` and `send`, which ask the
    // test's deadline: none of these is in this file but there.
    let source = include_str!("panes.rs");
    for call in [[".out", "put()"], [".sta", "tus()"], ["wait_with_", "output"], ["fs::write(pane.join(\"i", "n\")"]] {
        let call = call.concat();
        let found = source.lines().enumerate().filter(|(_, line)| line.contains(&call));
        let lines: Vec<usize> = found.map(|(n, _)| n + 1).collect();
        assert!(lines.is_empty(), "`{call}` at lines {lines:?}: it waits without the test's deadline");
    }
}

#[test]
fn history_is_put_aside_where_a_line_ends_however_the_lines_come() {
    // Lines of 252 bytes, three rows each, in bursts: more than twice what `history` holds, so
    // it is put aside twice. No line is cut by that: `history.1` ends with a whole line, and
    // `history` goes on with the next one. (Each line's number has a colour.)
    let session = Session::new("turns");
    let prints = "awk 'BEGIN { pad = sprintf(\"%245s\", \"\"); gsub(/ /, \"x\", pad); \
        for (i = 0; i < 36000; i++) { printf \"\\033[3%dm%06d\\033[m%s\\n\", i % 8, i, pad; if (i % 700 == 699) { fflush(); system(\"sleep 0.01\") } } }'";
    let mut start = session.fux();
    start.env("COLUMNS", "120").env("LINES", "36").args(["start", "lines", "--", "sh", "-c", prints]);
    let pane = PathBuf::from(String::from_utf8(start.run().stdout).unwrap().trim());
    let mut turns = Vec::new();
    until("the program has ended", || {
        // (Each `history.1` is looked at as it appears: its last line is whole.)
        // (Told apart by the number its first line begins with: two may be of one size.)
        let aside = fs::read(pane.join("history.1")).unwrap_or_default();
        let first = aside.get(..6).map(<[u8]>::to_vec);
        if aside.len() > 252 && turns.last().map(|(_, _, first)| first) != Some(&first) {
            turns.push((aside.len(), aside.ends_with(b"\n") && aside.len() % 252 == 0, first));
        }
        fact(&pane, "state") == "exited 0"
    });
    assert!(turns.len() >= 2 && turns.iter().all(|(size, whole, _)| *whole && *size < 5 << 20), "{turns:?}");
    let (aside, history) = (read(&pane.join("history.1")), read(&pane.join("history")));
    let number = |line: Option<&str>| line.and_then(|line| line.get(..6)?.parse::<usize>().ok());
    let (last, next) = (number(aside.lines().last()), number(history.lines().next()));
    assert!(last.is_some() && next == last.map(|n| n + 1), "{last:?} {next:?}");
    assert!(history.lines().take(100).all(|line| line.len() == 251));
    // `history.styled` has the same lines with their colours, and was put aside with `history`
    // each time, at the same line.
    let (styled_aside, styled) = (read(&pane.join("history.styled.1")), read(&pane.join("history.styled")));
    assert!(styled_aside.starts_with("\x1b[0;3") && styled.lines().all(|line| line.starts_with("\x1b[0;3")));
    assert!(bare(&styled_aside) == aside && bare(&styled) == history);
}

#[test]
fn the_two_histories_keep_their_bounds_and_the_same_lines() {
    let session = Session::new("pair");
    let size = |pane: &Path, name: &str| fs::metadata(pane.join(name)).map_or(0, |file| file.len());
    // Rows that are a colour and nothing else are a byte in `history` and a row's worth in
    // `history.styled`: what waits to be written is counted by the longer, and so is a file.
    // (The program says it is drawing, so nothing is published for half a second.)
    let bar = "printf '\\033[?2026h\\033[44m'; yes '' | head -n 200000; printf '\\033[?2026l\\033[mdone'; sleep 600";
    let mut start = session.fux();
    start.env("COLUMNS", "100").env("LINES", "24").args(["start", "bar", "--", "sh", "-c", bar]);
    let pane = PathBuf::from(String::from_utf8(start.run().stdout).unwrap().trim());
    let mut most = 0;
    until("the bars are printed", || {
        most = most.max(size(&pane, "history.styled")).max(size(&pane, "history.styled.1"));
        read(&pane.join("shown/screen")).contains("done")
    });
    assert!(most < 6 << 20 && size(&pane, "history.styled.1") > 4 << 20, "{most}");
    // A file is not put aside while its last rows are a resize's to take back: they are taken
    // back from it. (One line of 700,000 characters in rows of two, 4.2 MiB with its colours,
    // then a row the smaller screen has no room for.)
    let long = "printf '\\033[31m'; head -c 700000 /dev/zero | tr '\\0' a; printf 'b\\nZ'; sleep 600";
    let mut start = session.fux();
    start.env("COLUMNS", "2").env("LINES", "2").args(["start", "long", "--", "sh", "-c", long]);
    let pane = PathBuf::from(String::from_utf8(start.run().stdout).unwrap().trim());
    let texts = |pane: &Path| (read(&pane.join("shown/screen")), read(&pane.join("history")));
    until("the line is printed", || texts(&pane).0 == "b\nZ\n" && size(&pane, "history") == 700_000);
    assert!(size(&pane, "history.styled") > 4 << 20);
    send(&pane.join("ctl"), "resize 2 1\n");
    until("the row is in history", || texts(&pane).0 == "Z\n" && texts(&pane).1.ends_with("ab\n"));
    send(&pane.join("ctl"), "resize 2 2\n");
    until("the row is back", || texts(&pane).0 == "b\nZ\n" && size(&pane, "history") == 700_000);
    let styled = read(&pane.join("history.styled"));
    assert!(!pane.join("history.1").exists() && !pane.join("history.styled.1").exists());
    assert!(bare(&styled) == texts(&pane).1 && styled.ends_with("\x1b[0;31maa\x1b[m"), "{}", styled.len());
    // When one of the two cannot be written, the other is written all the same and the log
    // says what was lost: the file that could not be written lacks those lines, and no others.
    let lines = "p() { i=0; while [ $i -lt 40 ]; do printf '\\033[3%dm%s\\033[m\\n' $((i % 8)) $1$i; i=$((i + 1)); done; }; \
        w() { until [ -e \"$FUX_PANE/$1\" ]; do sleep 0.05; done; }; p one; w go1; p two; w go2; p three; sleep 600";
    let pane = session.start("lost", &["sh", "-c", lines]);
    let mode = |mode| {
        fs::set_permissions(pane.join("history.styled"), std::os::unix::fs::PermissionsExt::from_mode(mode)).unwrap()
    };
    until("the first lines are in history", || texts(&pane).1.contains("one10\n"));
    mode(0o000);
    fs::write(pane.join("go1"), "").unwrap();
    until("the log says what was lost", || {
        texts(&pane).1.contains("two10\n") && read(&pane.join("log")).contains("history.styled was lost")
    });
    mode(0o600);
    fs::write(pane.join("go2"), "").unwrap();
    until("both are written again", || {
        let (plain, styled) = (texts(&pane).1, bare(&read(&pane.join("history.styled"))));
        plain.ends_with("three16\n") && styled.ends_with("three16\n")
    });
    let (plain, styled) = (texts(&pane).1, bare(&read(&pane.join("history.styled"))));
    assert!(plain.starts_with("one0\none1\n") && plain.contains("one39\ntwo0\n") && plain.contains("two39\nthree0\n"));
    assert!(
        styled.starts_with("one0\none1\n") && styled.contains("one10\n") && !styled.contains("two10\n"),
        "{styled}"
    );
}

#[test]
fn a_history_that_cannot_be_put_aside_begins_anew_and_the_other_keeps_its_lines() {
    // `history.styled.1` is a directory: `history.styled` cannot be put aside when the two are
    // past their size. It begins anew, and the log says so; left as it was, it would be past its
    // size at every later round, and `history` put aside each time, over the lines it kept.
    // (56,000 lines of 80 bytes: past 4 MiB once, and not twice.)
    let session = Session::new("aside");
    let prints = "mkdir \"$FUX_PANE/history.styled.1\"; awk 'BEGIN { for (i = 0; i < 56000; i++) printf \"%079d\\n\", i }'; printf done; sleep 600";
    let pane = session.start("p", &["sh", "-c", prints]);
    until("the lines are printed", || read(&pane.join("shown/screen")).contains("done"));
    let (aside, history) = (read(&pane.join("history.1")), read(&pane.join("history")));
    let lines = aside.lines().count() + history.lines().count();
    assert!(aside.starts_with("0000") && lines == 56000 - 23, "{lines} lines, {} put aside", aside.len());
    let (styled, log) = (fs::metadata(pane.join("history.styled")).unwrap().len(), read(&pane.join("log")));
    assert!(styled < 4 << 20 && log.contains("history.styled was lost"), "{styled} {log}");
}

#[test]
fn a_pane_moved_while_it_is_started_is_held_where_it_went_and_its_name_is_anothers_to_start() {
    // A start has locked the pane's folder and handed the lock to its holder, which waits before its
    // first screen (its `history` is a FIFO nobody reads yet: that the holder is there, its `status`
    // says, and nobody asks the lock). The folder is moved, and a second start takes the name. The
    // first holder keeps the folder that was locked for it, under its new name, and the second the
    // new one: never two in one.
    let session = Session::new("twostarts");
    let (pane, old) = (session.0.join("main/p"), session.0.join("main/old"));
    fs::create_dir_all(&pane).unwrap();
    assert!(Command::new("mkfifo").arg(pane.join("history")).ended().success());
    let mut first = session.fux().args(["start", "p", "--", "sleep", "600"]).stdout(Stdio::null()).spawn().unwrap();
    until("the first holder has the pane and waits", || read(&pane.join("status")).contains("command sleep 600"));
    assert!(first.try_wait().unwrap().is_none(), "the start did not wait for its holder's first screen");
    let held = read(&pane.join("status"));
    fs::rename(&pane, &old).unwrap();
    assert_eq!(session.start("p", &["sleep", "600"]), pane);
    let second = read(&pane.join("status"));
    // (Read from now on, without waiting: the first holder goes on to its first screen.)
    let history = fuxix::open(old.join("history"), false).unwrap();
    assert!(wait(&mut first, "the first start").success());
    until("the first holder has its screen where its folder went", || fact(&old, "state") == "running");
    assert_eq!((read(&old.join("status")), read(&pane.join("status"))), (held.clone(), second.clone()));
    assert_ne!(held, second);
    drop(history);
}

#[test]
fn every_panes_text_is_searched_and_ended_panes_are_started_again_as_they_were() {
    let session = Session::new("recipes");
    fs::create_dir_all(&session.0).unwrap();
    let keys = session.0.join(".keys");
    assert!(
        Command::new("cp").arg("-R").arg(concat!(env!("CARGO_MANIFEST_DIR"), "/keys")).arg(&keys).ended().success()
    );
    let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
    let path = format!("{bin}:{}", std::env::var("PATH").unwrap());
    // Two panes with text: one whose line has scrolled into history, one still running. And an
    // editor for the test, which notes the line it is to open at and the line it finds there.
    let old = session.start("w/old", &["sh", "-c", "echo 'a needle in old'; seq 1 60; exec sleep 600"]);
    // (The other's arguments have no blank in them, and what a shell would expand.)
    let other = session.start("v/other", &["printf", "%s\\n", "*", "the-needle;$HOME"]);
    until("both have printed", || {
        read(&old.join("history")).contains("needle") && read(&other.join("shown/screen")).contains("$HOME")
    });
    fs::write(session.0.join("ed"), "#!/bin/sh\nsed -n \"${1#+}p\" \"$2\" > \"$OUT\"\n").unwrap();
    Command::new("chmod").arg("+x").arg(session.0.join("ed")).ended();
    let out = session.0.join("out");
    // The search, in a pane as the key starts it: the words, then the first line of the list.
    let mut search = session.fux();
    search.env("PATH", &path).env("FUX_KEYS", &keys).env("OUT", &out).env("SHELL", "/bin/sh");
    search
        .env("FUX_EDIT", format!("{} \"+$2\" \"$1\"", session.0.join("ed").display()))
        .env("PATH", format!("{}:{path}", session.0.join("none").display()));
    let found = search.args(["start", "w/find", "--"]).arg(keys.join(".search")).run();
    let find = PathBuf::from(String::from_utf8(found.stdout).unwrap().trim());
    until("it asks", || read(&find.join("shown/screen")).contains("search every pane for:"));
    send(&find.join("in"), "NEEDLE\r");
    // (With a finder installed the list is its own; without, the lines are numbered.)
    if ["fzf", "sk", "fzy"].iter().all(|finder| {
        Command::new("sh")
            .args(["-c", &format!("command -v {finder}")])
            .run()
            .status
            .code()
            .is_some_and(|code| code != 0)
    }) {
        until("the lines found are listed", || read(&find.join("shown/screen")).contains("w/old 1:a needle in old"));
        assert!(read(&find.join("shown/screen")).contains("v/other 2:the-needle;$HOME"));
        send(&find.join("in"), "1\r");
        until("the editor was opened at the line", || read(&out) == "the-needle;$HOME\n");
    }
    let _ = fs::remove_dir_all(&find);
    // A pane whose program has ended is started again by `.restore`, from the command line in
    // its `status` (no other file says what it ran): the words of it as they are, none of them
    // expanded, and what it had printed kept beside it. The one that runs is left alone.
    assert!(session.fux().arg("wait").arg(&other).ended().success());
    let (was, runs) = (read(&other.join("status")), read(&old.join("status")));
    assert!(was.ends_with("\ncommand printf %s\\n * the-needle;$HOME\n"), "{was}");
    assert!(!other.join("argv").exists(), "a pane has no argv file: its command is in status");
    // (The session is named from where the script is run, as a login may name it: it is the same
    // session after the script has gone to the pane's directory, here the home it is given.)
    let (home, named) = (session.0.join("home"), session.0.file_name().unwrap());
    fs::create_dir_all(&home).unwrap();
    let mut restore = Command::new("sh");
    restore.arg(keys.join(".restore")).env("PATH", &path).env("FUX_DIR", named).env("HOME", &home);
    let restored = restore.current_dir(session.0.parent().unwrap()).run();
    assert_eq!(String::from_utf8_lossy(&restored.stdout), "started v/other again\n");
    assert!(!home.join(named).exists(), "the pane was started in a session of that name under its own directory");
    until("it has run again", || {
        read(&other.join("status")) != was && read(&other.join("shown/screen")).starts_with("*\nthe-needle;$HOME\n")
    });
    assert!(read(&other.join("before")).contains("the-needle;$HOME") && !other.join("argv").exists());
    assert_eq!(read(&old.join("status")), runs);
}

#[test]
fn each_screen_has_a_folder_of_its_own_and_shown_leads_to_the_one_that_is_drawn() {
    let session = Session::new("covered");
    let shows = "printf '\\033[32mgreen\\033[m and a line of forty characters or so\\n\\033[?1049hfull'; read x; \
        printf '\\033[?1049l'; read x";
    let pane = session.start("p", &["sh", "-c", shows]);
    let file = |name: &str| read(&pane.join(name));
    let shown = || fs::read_link(pane.join("shown")).unwrap_or_default();
    // `alt` is filled before `shown` is turned to it: whoever finds the link there finds the screen.
    until("the program has the alternate screen", || shown() == Path::new("alt"));
    assert!(file("alt/screen").contains("\nfull\n") && file("shown/screen") == file("alt/screen"));
    // The main screen has the names it always has, and is told as before it was covered. The
    // alternate screen is in no text: its lines are counted from its own first row.
    let (main, styled) = (file("main/screen"), file("main/screen.styled"));
    assert!(main.starts_with("green and a line of forty characters or so\n") && main.lines().count() == 24, "{main:?}");
    assert!(styled.starts_with("\x1b[0;32mgreen\x1b[") && bare(&styled) == main, "{styled:?}");
    assert!(!file("alt/screen").contains("green") && file("alt/lines") == "0\n\n");
    let folder = |name: &str| {
        let mut names: Vec<_> = fs::read_dir(pane.join(name)).unwrap().flatten().map(|file| file.file_name()).collect();
        names.sort();
        names
    };
    assert_eq!(folder("alt"), folder("main"), "the same names in each screen's folder");
    // The pane's text, as the keys make it, is the shell's while the program is open.
    let text = session.0.join("text");
    let mut script = Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/keys/.text"));
    assert!(script.arg(&text).env("FUX_PANE", &pane).ended().success());
    assert!(read(&text).starts_with("green and a line of forty characters or so\n") && !read(&text).contains("full"));
    assert_eq!(folder("main"), ["lines", "links", "pictures", "screen", "screen.styled"]);
    assert!(!pane.join("screen").exists() && !pane.join("lines").exists() && !pane.join("screen.main").exists());
    // What the program draws meanwhile is not the main screen's: its files are not written again.
    use std::os::unix::fs::MetadataExt;
    let stamp = |name: &str| fs::metadata(pane.join(name)).map(|file| (file.ino(), file.mtime_nsec())).ok();
    let written = (stamp("main/screen"), stamp("main/screen.styled"));
    send(&pane.join("in"), "more");
    until("the program's screen has changed", || file("alt/screen").contains("\nfullmore\n"));
    assert_eq!((stamp("main/screen"), stamp("main/screen.styled")), written);
    // A resize wraps the main screen anew under the program, and its files say so.
    send(&pane.join("ctl"), "resize 20 24\n");
    until("the covered screen is wrapped anew", || {
        file("main/screen").starts_with("green and a line of \nforty characters or \nso\n")
            && bare(&file("main/screen.styled")).starts_with("green and a line of \nforty")
    });
    until("its rows that go on are told", || file("main/lines") == "0\n1 2\n" && file("alt/lines") == "0\n\n");
    // The program leaves: `shown` is turned back, `alt` goes, and the main screen is what its files said.
    let covered = file("main/screen");
    send(&pane.join("in"), "\n");
    until("the main screen is shown again", || shown() == Path::new("main") && !pane.join("alt").exists());
    assert!(file("shown/screen") == covered && file("main/lines") == "0\n1 2\n");
}

/// Gives a session's `main` workspace a layout, as an editor that saves in one step does: the
/// file is whole at every moment (the viewer reads it, and writes it too, at any time).
fn lay(session: &Path, text: &str) {
    let layout = session.join("main/layout");
    fs::create_dir_all(session.join("main")).unwrap();
    fs::write(session.join("main/.layout.test"), text).unwrap();
    fs::rename(session.join("main/.layout.test"), layout).unwrap();
}

/// Waits for something to become true, until the test's deadline.
fn until(what: &str, ready: impl FnMut() -> bool) {
    until_or(what, String::new, ready);
}

/// The same, and when it does not become true, says what `more` has to say of it.
fn until_or(what: &str, more: impl Fn() -> String, mut ready: impl FnMut() -> bool) {
    let end = deadline();
    while !ready() {
        assert!(Instant::now() < end, "at the test's deadline, still waiting until {what}{}", more());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Which file a pane has of a name, and when it was written.
fn stamp_of(pane: &Path, name: &str) -> Option<(u64, i64)> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(pane.join(name)).map(|file| (file.ino(), file.mtime_nsec())).ok()
}

/// One of a pane's facts: the text of its file, without the line end.
fn fact(pane: &Path, name: &str) -> String {
    read(&pane.join(name)).trim_end().to_string()
}

#[test]
fn what_is_written_to_in_is_typed_and_what_the_program_shows_is_in_screen() {
    let session = Session::new("io");
    let pane = session.start("build", &["env", "PS1=ready$ ", "sh"]);
    assert_eq!(pane, session.0.join("main/build"));
    // (What is typed before the shell's first prompt is answered on that prompt's row.)
    until("the shell's prompt shows", || read(&pane.join("shown/screen")).contains("ready$"));
    send(&pane.join("in"), "echo h''i; stty size\n");
    until("the answers show", || read(&pane.join("shown/screen")).contains("\nhi\n24 80\n"));
    // A resize reaches the program and the `size` file; the screen is its rows and nothing else.
    send(&pane.join("ctl"), "resize 91 17\nfly\n");
    send(&pane.join("in"), "stty size\n");
    until("the new size shows", || read(&pane.join("shown/screen")).contains("\n17 91\n"));
    assert_eq!(fact(&pane, "size"), "91 17");
    assert_eq!(read(&pane.join("shown/screen")).lines().count(), 17, "one line a row");
    // Every fact has its file: where the cursor is, how input is sent, whether the program runs, who it is.
    let cursor: Vec<String> = fact(&pane, "cursor").split(' ').map(str::to_string).collect();
    assert!(cursor.len() == 4 && cursor[2] == "shown", "{cursor:?}");
    assert_eq!((fact(&pane, "state").as_str(), read(&pane.join("main/lines")).as_str()), ("running", "0\n\n"));
    assert!(read(&pane.join("status")).lines().nth(1).unwrap().starts_with("pid "));
    until("the refusal is logged", || read(&pane.join("log")).contains("fly: not a command"));
    send(&pane.join("ctl"), "kill \x1b]0;now\x07\n");
    until("a refused line is logged without its control codes", || read(&pane.join("log")).contains("kill ?]0;now?"));
    // A pane whose program cannot start leaves its folder, with a log that says why; started
    // again under that name, it runs.
    assert!(!session.fux().args(["start", "none", "--", "/no/such/program"]).run().status.success());
    assert!(read(&session.0.join("main/none/log")).contains("cannot start /no/such/program"));
    assert_eq!(fact(&session.start("none", &["sleep", "600"]), "state"), "running");
    // A second start of a live pane is refused; the first is not disturbed.
    assert!(!session.fux().args(["start", "build", "--", "sh"]).run().status.success());
    send(&pane.join("in"), "exit 3\n");
    let waited = session.fux().arg("wait").arg(&pane).run();
    assert!(String::from_utf8_lossy(&waited.stdout).contains("state exited 3"));
    assert!(!pane.join("in").exists() && !pane.join("ctl").exists(), "the FIFOs go with the holder");
    // A start in place that fails leaves what the last program left.
    assert!(!session.fux().args(["start", "build", "--", "/no/such/program"]).run().status.success());
    assert!(fact(&pane, "state") == "exited 3" && read(&pane.join("shown/screen")).contains("exit 3"));
    assert!(!session.fux().args(["start", "layout"]).run().status.success(), "the layout file's name is taken");
    // A workspace's name that is none is refused: it is not taken to mean `main`.
    let odd = session.fux().env("FUX_WS", "main/").arg("layout").run();
    assert!(!odd.status.success() && String::from_utf8_lossy(&odd.stderr).contains("main/"), "{odd:?}");
    // Nor is one that is no text.
    let none = <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(b"\xff");
    let odd = session.fux().env("FUX_WS", none).arg("layout").run();
    assert!(!odd.status.success() && String::from_utf8_lossy(&odd.stderr).contains("workspace"), "{odd:?}");
}

#[test]
fn a_pane_knows_where_it_is_and_gets_no_name_that_is_not_its_own() {
    let session = Session::new("names");
    let mut start = session.fux();
    // What a key script would leave in the environment is not this pane's; another program's names are not ours to judge.
    // (How the editor is run is a setting for every viewer, one started in a pane too: it stays.)
    start.env("FUX_WS", "main").env("FUX_VIEW", "/nowhere").env("FUX_SOCKET", "/another").env("COLUMNS", "90");
    start.env("FUX_EDIT", "ed \"$1\"").env("FUX_ROW", "40x24").env("FUX_COL", "80x12");
    assert!(start.args(["start", "w2/p", "--", "sh"]).ended().success());
    let pane = session.0.join("w2/p");
    let me = env!("CARGO_BIN_EXE_fux");
    let typed = format!(
        "env | grep -E '^(FUX_|TERM=|TERM_PROGRAM|COLUMNS)' | LC_ALL=C sort > \"$FUX_PANE/env\"; '{me}' layout > \"$FUX_PANE/here\"\n"
    );
    send(&pane.join("in"), typed);
    // A command typed in a pane means its own workspace, not `main`.
    until("the layout of its own workspace is printed", || read(&pane.join("here")) == "* p focus=p\n");
    let expected = format!(
        "FUX_DIR={}\nFUX_EDIT=ed \"$1\"\nFUX_PANE={}\nFUX_SOCKET=/another\nTERM=xterm-256color\nTERM_PROGRAM=fux\nTERM_PROGRAM_VERSION={}\n",
        session.0.display(),
        pane.display(),
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(read(&pane.join("env")), expected);
    assert_eq!(fact(&pane, "size"), "90 24");
    // A size that is set and is none is refused: the pane is not started at a size made up for it.
    let odd = session.fux().env("COLUMNS", "wide").args(["start", "w2/odd", "--", "sh"]).run();
    assert!(
        !odd.status.success() && String::from_utf8_lossy(&odd.stderr).contains("COLUMNS is not a number"),
        "{odd:?}"
    );
    // A pane named with its workspace is started there, whatever the starter's own is called.
    let named = session.fux().env("FUX_WS", "none/").args(["start", "w2/q", "--", "sleep", "600"]).run();
    assert!(named.status.success(), "{named:?}");
    // A session named by a path from where the starter stands is its whole path for the pane's
    // program, wherever that goes.
    let mut start = session.fux();
    start.current_dir(session.0.parent().unwrap()).env("FUX_DIR", session.0.file_name().unwrap());
    let asks = format!("cd / && '{me}' dir > \"$FUX_PANE/dir\"; sleep 600");
    assert!(start.args(["start", "w2/r", "--", "sh", "-c", &asks]).ended().success());
    let said = || fs::canonicalize(read(&session.0.join("w2/r/dir")).trim()).ok();
    until("the pane's program is told where the session is", || said().is_some());
    assert_eq!(said(), fs::canonicalize(&session.0).ok());
}

#[test]
fn kill_hangs_up_the_program_and_removing_the_directory_ends_the_pane() {
    let session = Session::new("end");
    let pane = session.start(
        "a",
        &["sh", "-c", "trap 'echo hup > \"$FUX_PANE/saw\"; exit 0' HUP; echo r''eady; while :; do sleep 1; done"],
    );
    until("the program has set its trap", || read(&pane.join("shown/screen")).contains("ready"));
    send(&pane.join("ctl"), "kill\n");
    until("the program saw the hangup", || read(&pane.join("saw")) == "hup\n");
    until("its end is in `state`", || fact(&pane, "state") == "exited 0");

    // A program that ends without a word: `state` changes and the text does not. `generation` is
    // written again all the same, an even number, after the fact: who watches that one file finds the end.
    use std::os::unix::fs::MetadataExt;
    let quiet = session.start("q", &["sh", "-c", "sleep 0.4"]);
    let stamp = || fs::metadata(quiet.join("generation")).map(|file| (file.ino(), file.mtime_nsec())).ok();
    let (running, first, text) = (fact(&quiet, "state"), stamp(), stamp_of(&quiet, "main/screen.styled"));
    until("its end is written", || {
        stamp() != first && fact(&quiet, "generation").parse::<u64>().is_ok_and(|n| n % 2 == 0)
    });
    assert_eq!((running.as_str(), fact(&quiet, "state").as_str()), ("running", "exited 0"));
    assert_eq!(stamp_of(&quiet, "main/screen.styled"), text, "a screen that did not change is not written again");
    // `generation` is written last of all: in a pane that has ended, no file is younger than it.
    let written = |name: &str| fs::metadata(quiet.join(name)).unwrap().modified().unwrap();
    let wait = session.fux().arg("wait").arg(&quiet).run();
    assert!(String::from_utf8_lossy(&wait.stdout).ends_with("state exited 0\n"));
    for name in [
        "size",
        "cursor",
        "modes",
        "state",
        "status",
        "main/lines",
        "main/pictures",
        "main/screen",
        "main/screen.styled",
    ] {
        assert!(written(name) <= written("generation"), "{name} was written after generation");
    }

    // Whoever starts a pane may ignore signals; its program does not inherit that.
    let start = format!("trap '' INT HUP; exec '{}' start c -- sleep 600", env!("CARGO_BIN_EXE_fux"));
    assert!(Command::new("sh").args(["-c", &start]).env("FUX_DIR", &session.0).ended().success());
    let interrupted = session.0.join("main/c");
    until("the program runs", || fact(&interrupted, "state") == "running");
    send(&interrupted.join("in"), "\x03");
    until("Ctrl-C ended it", || fact(&interrupted, "state") == "killed 2");

    let other = session.start("b", &["sh", "-c", "echo $$ > \"$FUX_PANE/pid\"; exec sleep 600"]);
    until("the program runs", || !read(&other.join("pid")).is_empty());
    let pid = read(&other.join("pid")).trim().to_string();
    let alive = || Command::new("kill").args(["-0", &pid]).run().status.success();
    assert!(alive());
    fs::remove_dir_all(&other).unwrap();
    until("the program is gone with its directory", || !alive());
}

#[test]
fn what_is_written_to_in_after_a_kill_goes_nowhere_and_its_writer_does_not_wait() {
    // A program that ignores the hangup lives on with no terminal. What is then written to `in`
    // is read and dropped: more than a FIFO holds is written at once, by a process of its own.
    let session = Session::new("afterkill");
    let pane = session.start("a", &["sh", "-c", "trap '' HUP; echo r''eady; exec sleep 20"]);
    until("the program ignores the hangup", || read(&pane.join("shown/screen")).contains("ready"));
    send(&pane.join("ctl"), "kill\n");
    until("the pty is closed", || fact(&pane, "state") == "hungup");
    let mut writer =
        Command::new("sh").args(["-c", "head -c 100000 /dev/zero > in"]).current_dir(&pane).spawn().unwrap();
    let start = Instant::now();
    let done = loop {
        match writer.try_wait().unwrap() {
            None if start.elapsed() < Duration::from_secs(5) => std::thread::sleep(Duration::from_millis(20)),
            done => break done,
        }
    };
    let _ = writer.kill();
    let _ = writer.wait();
    assert!(done.is_some_and(|status| status.success()), "100,000 bytes to `in` after kill: {done:?} after 5 seconds");
    assert_eq!(fact(&pane, "state"), "hungup");
}

#[test]
fn a_pane_made_narrow_and_wide_again_is_as_it_was_and_loses_no_text_on_the_way() {
    let session = Session::new("resize");
    let lines: Vec<String> = (1..=8).map(|n| format!("line{n}-{}", "abcdefghij".repeat(n % 4 + 1))).collect();
    let script = format!("printf '%s\\n' {}; printf ready; sleep 600", lines.join(" "));
    let pane = session.start("p", &["sh", "-c", &script]);
    let screen = || read(&pane.join("shown/screen"));
    let rows = screen;
    until("the program has printed", || screen().contains("\nready\n"));
    let history = || (read(&pane.join("history")), read(&pane.join("history.styled")));
    let (before, kept) = (rows(), history());
    // Narrow: lines wrap, rows leave by the top into `history`, and every line is whole in
    // `history` and `screen` together (the screen's rows that go on are joined to tell).
    for size in ["12 24", "7 24", "40 5", "80 24"] {
        send(&pane.join("ctl"), format!("resize {size}\n"));
        // (The size is written a moment before the text that was wrapped for it.)
        until("the pane has the size and every line whole", || {
            // (No line here has a colour: the two files are the same, once both are written.)
            let (plain, styled) = history();
            fact(&pane, "size") == size
                && plain == styled
                && (plain + &rows()).replace('\n', "") == lines.concat() + "ready"
        });
    }
    // Back at the size it had, with nothing printed between: the screen as it was, and
    // `history` has given back what it was sent, and `history.styled` too.
    assert_eq!((rows(), history()), (before, kept));
    // A size that is refused changes nothing and is in the log.
    send(&pane.join("ctl"), "resize 0 5\n");
    until("the log says so", || read(&pane.join("log")).contains("resize"));
    assert_eq!(fact(&pane, "size"), "80 24");
}

#[test]
fn a_flood_is_published_a_bounded_number_of_times_and_loses_no_line() {
    let session = Session::new("flood");
    let pane = session.start("yes", &["sh", "-c", "yes | head -n 200000; echo done; sleep 600"]);
    // Each round of writing ends with the next even number in `generation`: how many there were is
    // read from the numbers, however often the file was looked at.
    let rounds = || fact(&pane, "generation").parse::<f64>().map_or(0.0, |n| (n / 2.0).floor());
    let (start, first) = (Instant::now(), rounds());
    while !read(&pane.join("shown/screen")).contains("\ndone\n") {
        assert!(start.elapsed() < Duration::from_secs(20), "the flood did not end");
        std::thread::sleep(Duration::from_millis(1));
    }
    let publishes = rounds() - first;
    let seconds = start.elapsed().as_secs_f64() + 0.1;
    assert!(publishes >= 2.0, "the flood was watched");
    assert!(publishes <= 41.0 * seconds + 2.0, "{publishes} publishes in {seconds} s is over 40 a second");
    let history = read(&pane.join("history.1")) + &read(&pane.join("history")) + &read(&pane.join("shown/screen"));
    assert_eq!(history.lines().filter(|line| *line == "y").count(), 200_000);
}

#[test]
fn a_program_that_draws_frame_after_frame_without_a_pause_is_shown_while_it_does() {
    let session = Session::new("frames");
    // Each frame says that it is being drawn and that it is done (mode 2026), and the next
    // follows at once, faster than it is read. The frame's number is on the first row, which
    // stays while the rest of the frame scrolls under it.
    let frames = "f=$(head -c 200000 /dev/zero | tr '\\0' x); printf 'start\\033[?1049h\\033[2;24r'; i=0; while :; do i=$((i + 1)); printf '\\033[?2026h\\033[H%s\\033[2;1H%s\\033[?2026l' $i \"$f\"; done";
    let pane = session.start("frames", &["sh", "-c", frames]);
    let frame = || read(&pane.join("shown/screen")).lines().next().and_then(|n| n.parse::<u32>().ok());
    // (Not `until`: a pane that is never written again says so in a few seconds, not a minute.)
    let (start, mut first) = (Instant::now(), None);
    while first.is_none() || frame() == first {
        first = first.or_else(frame);
        assert!(start.elapsed() < Duration::from_secs(6), "no later frame than {first:?} was shown");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_viewer_whose_panes_do_not_change_does_no_work_and_draws_the_next_change() {
    // The viewer on a terminal of 500 by 500 cells: putting a frame of that size together is
    // work that shows in its processor time, which `ps` tells (in whole seconds, on Linux).
    let (outer, inner) = (Session::new("iouter"), Session::new("iinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    let one = inner.start("one", &["sh", "-c", "echo r''eady; exec cat"]);
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let mut start = outer.fux();
    start.env("COLUMNS", "500").env("LINES", "500").args(["start", "view", "--"]).args(command);
    let started = start.args([env!("CARGO_BIN_EXE_fux"), "view"]).run();
    let view = PathBuf::from(String::from_utf8(started.stdout).unwrap().trim());
    until("the pane has its tile's size and is drawn", || {
        fact(&one, "size") == "500 500" && read(&view.join("shown/screen")).starts_with("ready\n")
    });
    let pid = read(&view.join("status")).lines().find_map(|line| Some(line.strip_prefix("pid ")?.to_string())).unwrap();
    let worked = || {
        let told = Command::new("ps").args(["-o", "time=", "-p", &pid]).run().stdout;
        let told = String::from_utf8(told).unwrap();
        told.trim().split(':').fold(0.0, |seconds, part| seconds * 60.0 + part.parse::<f64>().unwrap())
    };
    // Six seconds in which nothing changes: no frame is put together. (One that were put together
    // each time the viewer looks would take all of them here; however slow the machine, a viewer
    // that does nothing uses none.)
    let before = worked();
    std::thread::sleep(Duration::from_secs(6));
    let work = worked() - before;
    assert!(work <= 2.0, "the viewer worked for {work} s of 6 in which nothing changed");
    // And what changes next is drawn.
    send(&one.join("in"), "next\n");
    until("the change is drawn", || read(&view.join("shown/screen")).starts_with("ready\nnext\nnext\n"));
}

#[test]
fn a_workspace_with_no_pane_gets_a_shell_when_it_is_first_shown() {
    // The viewer in a pane of an outer session, with no keys directory yet: it writes the
    // default keys, and `.empty` among them starts a shell in the workspace it finds empty.
    let (outer, inner) = (Session::new("eouter"), Session::new("einner"));
    let keys = inner.0.join(".keys");
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let viewer = ["env", "SHELL=/bin/sh", env!("CARGO_BIN_EXE_fux"), "view"];
    let view = outer.start("view", &[&command[..], &viewer[..]].concat());
    let first = inner.0.join("main/1");
    until("the first pane runs a shell", || {
        fact(&first, "state") == "running" && read(&first.join("status")).contains(".keys/.shell")
    });
    // What is typed to the viewer is the shell's, and the viewer shows it.
    until("the shell's prompt is shown", || {
        read(&view.join("shown/screen")).lines().next().is_some_and(|row| row.ends_with(['$', '#']))
    });
    send(&view.join("in"), "echo $((6*7))\r");
    until("the shell answered", || read(&view.join("shown/screen")).contains("\n42\n"));
    // The shell ends, and its pane with it: the workspace stays empty, and the viewer says how
    // to go on. Nothing is started again: not then, and not when the viewer draws anew.
    send(&view.join("in"), "exit\r");
    let seen = || format!(": the viewer shows {:?}", read(&view.join("shown/screen")).trim());
    until_or("the viewer says so", seen, || {
        read(&view.join("shown/screen")).contains("no panes in main: C-b t n starts a shell")
    });
    send(&view.join("ctl"), "resize 70 20\n");
    until("the viewer drew at the new size", || {
        fact(&view, "size") == "70 20" && read(&view.join("shown/screen")).contains("no panes")
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(fs::read_dir(inner.0.join("main")).unwrap().flatten().all(|entry| !entry.path().is_dir()));
    // A workspace the viewer is taken to runs the keys' own `.empty`, once: here one that only
    // notes that it ran.
    fs::write(keys.join(".empty"), "#!/bin/sh\necho \"$FUX_WS\" >> \"$FUX_KEYS/ran\"\n").unwrap();
    fs::create_dir_all(inner.0.join("other")).unwrap();
    let ctl =
        fs::read_dir(&inner.0).unwrap().flatten().find(|file| file.file_name().to_string_lossy().starts_with(".view."));
    let ctl = ctl.unwrap().path();
    send(&ctl, "workspace other\n");
    until("the other workspace is shown", || read(&view.join("shown/screen")).contains("no panes in other"));
    assert_eq!(read(&keys.join("ran")), "other\n");
    // The viewer keeps no workspace but the one it shows: `-` is no name, and names none.
    send(&ctl, "workspace -\n");
    until("the viewer says that it is none", || read(&view.join("shown/screen")).contains("no workspace -"));
    assert_eq!(read(&keys.join("ran")), "other\n");
}

#[test]
fn the_bar_covers_the_whole_last_row_of_a_wide_terminal() {
    // A terminal 300 columns wide, and a pane whose last row is text from end to end: while the
    // prefix is held the bar is on that row, and none of the text shows through it.
    let (outer, inner) = (Session::new("wouter"), Session::new("winner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b")).unwrap();
    let wide = |session: &Session, name: &str, program: &[&str]| {
        let mut start = session.fux();
        start.env("COLUMNS", "300").env("LINES", "20").args(["start", name, "--"]).args(program);
        PathBuf::from(String::from_utf8(start.run().stdout).unwrap().trim())
    };
    let fills = "printf '\\033[20;1H%s' \"$(head -c 300 /dev/zero | tr '\\0' x)\"; sleep 600";
    wide(&inner, "full", &["sh", "-c", fills]);
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = wide(&outer, "view", &[&command[..], &[env!("CARGO_BIN_EXE_fux"), "view"]].concat());
    let last = || read(&view.join("shown/screen")).lines().nth(19).unwrap_or_default().to_string();
    until("the pane's last row is shown", || last() == "x".repeat(300));
    send(&view.join("in"), "\x02");
    until("the bar is shown", || last().starts_with(" main "));
    assert!(!last().contains('x'), "{}", last());
}

#[test]
fn the_viewer_shows_panes_as_the_layout_says_and_runs_a_key_script() {
    // The viewer needs a terminal: it runs in a pane of an outer session.
    let (outer, inner) = (Session::new("outer"), Session::new("inner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b")).unwrap();
    // The split key starts its pane at the size the viewer says the new tile will have; the
    // program notes the size it found when it started, before the pane has a tile at all.
    let script = "#!/bin/sh\n# split\n# group: Panes\np=$(COLUMNS=${FUX_ROW%x*} LINES=${FUX_ROW#*x} fux start \"$FUX_WS/two\" -- sh -c 'stty size > \"$FUX_PANE/first\"; exec sh') && until [ -s \"$p/first\" ]; do sleep 0.01; done && fux layout split row two\necho made $FUX_KEYS\n";
    fs::write(keys.join("C-b/v"), script).unwrap();
    fs::write(keys.join("C-b/e"), "#!/bin/sh\nsleep 0.3\nprintf 'echo ' 1<>\"$FUX_PANE/in\"\n").unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/v")).arg(keys.join("C-b/e")).ended();
    let one = inner.start("one", &["sh"]);
    // The terminal the viewer runs in says what its colours are: here the outer session's file does.
    fs::create_dir_all(&outer.0).unwrap();
    fs::write(outer.0.join("colours"), "foreground #102030\nbackground #fdf6e3\ncursor #0a0b0c\n4 #268bd2\n").unwrap();
    let viewer = [env!("CARGO_BIN_EXE_fux"), "view"];
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &viewer[..]].concat());
    let screen = || read(&view.join("shown/screen"));
    send(&view.join("in"), "echo o''ne\r");
    until("what is typed reaches the pane and its screen the viewer", || screen().contains("\none\n"));
    assert!(read(&one.join("shown/screen")).contains("\none\n"));
    // Keys typed after a key with a script wait for the script: here it types `echo `.
    send(&view.join("in"), "\x02ein or''der\r");
    until("the script's typing came first", || screen().contains("\nin order\n"));
    // The prefix opens the menu the directory gives; the key runs the script.
    send(&view.join("in"), "\x02");
    until("the menu and the bar show", || {
        screen().contains(" e\n")
            && screen().contains(" Panes\n")
            && screen().contains("v  split")
            && screen().contains(" main  one ")
            && screen().contains("C-b …")
    });
    send(&view.join("in"), "v");
    until("the layout file has the split", || read(&inner.0.join("main/layout")) == "* row(one two) focus=two\n");
    until("both panes are drawn with a line between", || {
        screen().lines().filter(|line| line.contains('│')).count() >= 23
    });
    until("the script's words are shown", || screen().contains("made "));
    send(&view.join("in"), "echo t''wo\r");
    until("the new pane has the keys", || read(&inner.0.join("main/two/shown/screen")).contains("\ntwo\n"));
    // Each pane was asked once to be the size of its tile.
    let width = |pane: &str| Some(fact(&inner.0.join("main").join(pane), "size").replace(' ', "x"));
    until("the panes have their tiles' sizes", || {
        width("one") == Some("40x24".into()) && width("two") == Some("39x24".into())
    });
    // And the new pane's program started at its tile's size: it was never drawn at another.
    assert_eq!(read(&inner.0.join("main/two/first")), "24 39\n");
    // A character the terminal outside may draw wider than fux counts it (three people who are one
    // family here) is not drawn where that could reach past its tile's edge, unless the terminal
    // says that it joins characters as fux does (the viewer asks: mode 2027). This one, a pane of
    // fux, says so: the family is drawn at the very edge, as a wide character is.
    let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
    send(&inner.0.join("main/one/in"), format!("clear; printf '\\033[9;39H{family}\\033[11;39H\u{6f22}\\n'\r"));
    until("the family is in its pane and in the viewer", || {
        read(&inner.0.join("main/one/shown/screen")).contains(family)
            && screen().contains(&format!("{family}│"))
            && screen().contains("\u{6f22}│")
    });
    // The viewer asked its terminal once what the colours are and wrote them down for the
    // session: a program in a pane that asks is told the same, viewer or no viewer.
    let told = "foreground #102030\nbackground #fdf6e3\ncursor #0a0b0c\n4 #268bd2\n";
    until("the session knows its colours", || read(&inner.0.join("colours")) == told);
    send(&view.join("in"), "clear; printf '\\033]11;?\\007\\033]4;4;?\\007\\033]4;5;?\\007'; cat -v\r");
    let answers = || read(&inner.0.join("main/two/shown/screen")).replace('\n', "");
    until("the program is told", || answers().contains("^[]11;rgb:fdfd/f6f6/e3e3^G^[]4;4;rgb:2626/8b8b/d2d2^G"));
    assert!(!answers().contains("]4;5;rgb"));
    // (The interrupt empties what was typed after it: the next line waits until it is through.)
    send(&view.join("in"), "\x03");
    until("the program is interrupted", || answers().contains("^G^C"));
    // A colour a program sets is its pane's: its cells are drawn in it (direct colour: the whole
    // tile in the background it set), and the terminal's cursor has the colour it set while its
    // pane has the keys. The terminal's own colours are never set: the same palette number in
    // the pane beside it is drawn as that number, on no background.
    let set = "\\033]4;1;#123456\\007\\033]11;#654321\\007\\033]12;#abcdef\\007";
    // (A wide character that has no room in its row's last cell leaves that cell empty.)
    send(&view.join("in"), format!("printf '{set}\\033[31mr''ed\\033[m\\n%38s\u{754c}\\n' ''\r"));
    fs::write(inner.0.join("main/one/in"), "clear; printf '\\033[31mpl''ain\\033[m\\n'\r").unwrap();
    let styled = || read(&view.join("shown/screen.styled"));
    let drawn = |text: &str| styled().lines().find_map(|line| Some(line.split_once(text)?.0.to_string()));
    until_or("the pane is drawn in its colours, and the one beside it is not", styled, || {
        let own = drawn("red").is_some_and(|before| before.ends_with("\x1b[0;38;2;18;52;86;48;2;101;67;33m"));
        own && drawn("plain").is_some_and(|before| before.ends_with("\x1b[0;31m") && !before.contains("48;2"))
    });
    // (Every row of its tile, from the line between the panes to the tile's end: 39 cells, the
    // one a wide character left empty among them.)
    let cells = |row: &str| bare(row).chars().map(|c| 1 + usize::from(c == '\u{754c}')).sum::<usize>();
    let whole = |row: &str| row.contains("48;2;101;67;33m") && !row.contains("\x1b[0m") && cells(row) == 39;
    assert!(
        read(&inner.0.join("main/two/shown/screen.styled")).contains(&format!("{}\u{200b} \x1b[m\n", " ".repeat(38)))
    );
    assert!(styled().lines().all(|line| line.split_once('│').is_some_and(|(_, row)| whole(row))), "{}", styled());
    assert_eq!(fact(&inner.0.join("main/two"), "cursor").split(' ').nth(4), Some("#abcdef"));
    let cursor = || fact(&view, "cursor").split(' ').nth(4).map(str::to_string);
    until("the terminal's cursor has the pane's colour", || cursor() == Some("#abcdef".into()));
    lay(&inner.0, "* row(one two) focus=one\n");
    until("the cursor has its own colour when the keys are another pane's", || cursor().is_none());
    lay(&inner.0, "* row(one two) focus=two\n");
    until("and the pane's when it has them again", || cursor() == Some("#abcdef".into()));
    send(&view.join("in"), "\x02");
    until("its own while a menu is open", || cursor().is_none() && screen().contains("C-b …"));
    send(&view.join("in"), "\x1b");
    until("the pane's after it", || cursor() == Some("#abcdef".into()));
    // Given back, the colours are the terminal's again, for what is on the screen already too.
    send(&view.join("in"), "printf '\\033]104\\007\\033]111\\007\\033]112\\007'\r");
    until_or("the pane is drawn as it was written", styled, || {
        cursor().is_none()
            && !styled().contains("48;2")
            && drawn("red").is_some_and(|before| before.ends_with("\x1b[0;31m"))
    });
    // A program that asks for the mouse gets it, counted from its own tile's corner (the tile starts at column 42).
    send(&view.join("in"), "printf '\\033[?1000h\\033[?1006h'; cat -v\r");
    until("the pane's modes say so", || fact(&inner.0.join("main/two"), "modes").contains("mousesgr mouse1000"));
    until("the viewer asked its own terminal", || fact(&view, "modes").contains("mousesgr mouse1000"));
    send(&view.join("in"), "\x1b[<0;50;3M\r");
    until("the click arrives translated", || read(&inner.0.join("main/two/shown/screen")).contains("^[[<0;9;3M"));
    // What a paste and a click are sent as is what the program asks now, whatever the viewer last
    // drew: here it has just been shown another workspace and this one again, in one go, with
    // nothing on the screen changed by it.
    send(&view.join("in"), "\x03");
    until("the program is interrupted", || read(&inner.0.join("main/two/shown/screen")).matches("^C").count() >= 2);
    send(&view.join("in"), "printf '\\033[?2004hre''ady\\n'; cat -v\r");
    until("the program asks for pastes marked", || read(&inner.0.join("main/two/shown/screen")).contains("\nready\n"));
    until("the pane's modes say so", || fact(&inner.0.join("main/two"), "modes").contains("paste"));
    fs::create_dir(inner.0.join("other")).unwrap();
    let mut own = fs::read_dir(&inner.0).unwrap().flatten().map(|entry| entry.path());
    let own = own.find(|path| path.file_name().unwrap().to_string_lossy().starts_with(".view.")).unwrap();
    send(&own, b"workspace other\nworkspace main\n");
    std::thread::sleep(Duration::from_millis(300));
    send(&view.join("in"), "\x1b[200~pasted\x1b[201~\x1b[<0;51;4M\r");
    until_or(
        "the paste arrives marked and the click translated",
        || read(&inner.0.join("main/two/shown/screen")),
        || {
            let screen = read(&inner.0.join("main/two/shown/screen"));
            screen.contains("^[[200~pasted^[[201~") && screen.contains("^[[<0;10;4M")
        },
    );
    send(&view.join("in"), "\x03");
    lay(&inner.0, "* one focus=one\n  two focus=two\n");
    until("an edit of the file by hand is followed", || width("one") == Some("80x24".into()));
    // A folder with a screen of someone's own making: what it says reaches the terminal as text only.
    fs::create_dir(inner.0.join("main/odd")).unwrap();
    fs::write(inner.0.join("main/odd/state"), "x\x1b]0;T\x07\x1b[2Jy\n").unwrap();
    fs::write(inner.0.join("main/odd/cursor"), "99 99 shown 7 #abcdef\n").unwrap();
    fs::write(inner.0.join("main/odd/size"), "5 9\n").unwrap();
    fs::create_dir(inner.0.join("main/odd/main")).unwrap();
    fs::write(inner.0.join("main/odd/main/screen.styled"), "h\x1b[1mi\x1b]0;T\x07\x1b[?1049h ho-hum\n").unwrap();
    std::os::unix::fs::symlink("main", inner.0.join("main/odd/shown")).unwrap();
    // The viewer gives the new folder a tab; a hand's edit comes after that, not across it.
    until("the viewer has placed the folder", || read(&inner.0.join("main/layout")).contains("odd"));
    lay(&inner.0, "* row(one odd) focus=odd\n");
    until("its title is drawn with the codes as marks", || screen().contains(" x?]0;T??[2Jy\n"));
    // (No program lives there: the terminal's cursor is not given the colour its file names.)
    assert_eq!(fact(&view, "cursor").split(' ').count(), 4);
    // Its row is text and colours and nothing more, cut where it says the pane ends (5 columns).
    assert!(screen().contains("│hi?]0\n") || screen().contains("│hi?]0 "), "{}", screen());
}

#[test]
fn keys_reach_a_program_as_it_asked_for_them_and_the_viewer_still_finds_its_own() {
    // The viewer runs in a pane of an outer session whose terminal is said to take the kitty
    // keyboard protocol; what is written to that pane's `in` is what such a terminal would send.
    let (outer, inner) = (Session::new("kouter"), Session::new("kinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b")).unwrap();
    fs::write(keys.join("C-b/o"), "#!/bin/sh\n# other\nfux layout focus next\n").unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/o")).ended();
    fs::create_dir_all(&outer.0).unwrap();
    fs::write(outer.0.join("keyboard"), "5\n").unwrap();
    // A program asks to have keys told apart (on its alternate screen, as an editor does) before
    // any viewer has said what its terminal takes: it is told that nothing is in force.
    let asking = "stty raw -echo; printf '\\033[?1049h\\033[>1u\\033[?u'; exec cat -vt";
    let early = inner.start("early", &["sh", "-c", asking]);
    let plain = inner.start("plain", &["sh", "-c", "stty raw -echo; exec cat -vt"]);
    until("the first answer", || read(&early.join("shown/screen")).contains("^[[?0u"));
    // The layout is changed as a person changes it, by `fux layout`, which takes the lock the viewer takes.
    let layout = |edits: &[&[&str]]| {
        for edit in edits {
            let out = inner.fux().arg("layout").args(*edit).run();
            assert!(out.status.success(), "layout {edit:?}: {}", String::from_utf8_lossy(&out.stderr));
        }
        read(&inner.0.join("main/layout"))
    };
    assert_eq!(
        layout(&[&["split", "row", "plain", "beside", "early"], &["pane", "early"]]),
        "* row(early plain) focus=early\n"
    );
    // The terminal the viewer finds has flags of its own on its alternate screen (4), and after
    // the viewer it is asked what it has.
    let view = format!(
        "printf '\\033[?1049h\\033[>4u\\033[?1049l'; FUX_DIR={} FUX_KEYS={} {} view; stty raw -echo; printf '\\033[?1049h\\033[?u'; exec cat -vt",
        inner.0.display(),
        keys.display(),
        env!("CARGO_BIN_EXE_fux")
    );
    let view = outer.start("view", &["sh", "-c", &view]);
    let modes = |pane: &Path| fact(pane, "modes");
    let shown = |pane: &Path| read(&pane.join("shown/screen")).replace('\n', "");
    // (When the viewer is waited for in vain, what its pane says is printed.)
    let seen = || {
        format!(
            ": the viewer's pane has modes {:?}, state {:?} and shows\n{}",
            modes(&view),
            fact(&view, "state"),
            read(&view.join("shown/screen"))
        )
    };
    // The viewer learns what its terminal takes and says so for the session. A program that
    // asks from then on has its flags in force, on its own `modes` and, asked of the terminal,
    // on the viewer's.
    until("the session knows", || read(&inner.0.join("keyboard")) == "5\n");
    let asks = inner.start("asks", &["sh", "-c", asking]);
    until("the second answer", || shown(&asks).contains("^[[?1u") && modes(&asks).contains("keys1"));
    let edits: [&[&str]; 4] =
        [&["split", "row", "asks", "beside", "plain"], &["swap", "prev"], &["pane", "early"], &["break"]];
    assert_eq!(layout(&edits), "- row(asks plain) focus=asks\n* early focus=early\n");
    assert_eq!(layout(&[&["pane", "asks"]]), "* row(asks plain) focus=asks\n- early focus=early\n");
    // (And has drawn the last of the edits: the pane has its half of the terminal.)
    until_or("the viewer asked its terminal for them", seen, || {
        modes(&view).contains("keys1") && fact(&asks, "size") == "40 24"
    });
    // Ctrl-I is not Tab and Shift-Enter is not Enter: each reaches the program as it was sent.
    send(&view.join("in"), "a\x1b[105;5ub\tc\x1b[13;2ud\r");
    until("the keys arrive apart", || shown(&asks).ends_with("^[[?1ua^[[105;5ub^Ic^[[13;2ud^M"));
    // The prefix is found in that form too, its key runs the script, and neither is typed.
    send(&view.join("in"), "\x1b[98;5uo");
    until("the other pane has the keys", || read(&inner.0.join("main/layout")).contains("focus=plain"));
    // That program asked for nothing: the terminal is put back to telling keys the old way.
    until_or("the viewer took the flags back", seen, || !modes(&view).contains("keys"));
    send(&view.join("in"), "x\ty");
    until("plain keys arrive", || shown(&plain) == "x^Iy");
    assert!(shown(&asks).ends_with("d^M"), "{}", shown(&asks));
    // Back on the pane that asked, they are in force again.
    send(&view.join("in"), "\x02o");
    until_or("the flags are asked for again", seen, || modes(&view).contains("keys1"));
    // The program that was told "none" before the viewer came still gets keys the old way: what
    // it was told stays true.
    assert!(!modes(&early).contains("keys") && shown(&early) == "^[[?0u", "{}", modes(&early));
    // When the viewer leaves, the terminal has the flags it had: 4, on its alternate screen.
    let ctl = fs::read_dir(&inner.0)
        .unwrap()
        .flatten()
        .find(|entry| entry.file_name().to_string_lossy().starts_with(".view."));
    send(&ctl.unwrap().path(), "quit\n");
    until("the terminal is as it was found", || shown(&view).contains("^[[?4u"));
}

#[test]
fn a_pane_says_where_its_program_is_and_what_it_wants_noticed_and_the_viewer_passes_it_on() {
    let (outer, inner) = (Session::new("gouter"), Session::new("ginner"));
    // The keys as they come with fux.
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&inner.0).unwrap();
    let copied = Command::new("cp").arg("-R").arg(concat!(env!("CARGO_MANIFEST_DIR"), "/keys")).arg(&keys).ended();
    assert!(copied.success());
    let raw = "stty raw -echo; printf '\\033[?1004h'; exec cat -vt";
    let (p1, p2) = (inner.start("p1", &["sh", "-c", raw]), inner.start("p2", &["sh", "-c", raw]));
    lay(&inner.0, "* p1 focus=p1\n  p2 focus=p2\n");
    let viewer = [env!("CARGO_BIN_EXE_fux"), "view"];
    let path = format!("PATH={}", std::env::var("PATH").unwrap());
    let command = ["env", &path, &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &viewer[..]].concat());
    let shown = |pane: &Path| read(&pane.join("shown/screen")).replace('\n', "");
    // The pane that has the keys is told so, having asked (mode 1004), and the viewer asks its
    // own terminal to tell it of the window's focus.
    until("the first pane is told it has the keys", || shown(&p1) == "^[[I");
    until("the viewer asked its terminal", || fact(&view, "modes").contains("focus"));
    // Where a program says it is (OSC 7) is the pane's `cwd`; a place on another machine is no place here.
    let print = inner.start("print", &["sh", "-c", "printf '\\033]7;file:///usr/share\\007'; sleep 600"]);
    until("the directory is told", || read(&print.join("cwd")) == "/usr/share\n");
    let far = inner.start(
        "far",
        &["sh", "-c", "printf '\\033]7;file:///usr\\007\\033]7;file://elsewhere.example/home/x\\007'; sleep 600"],
    );
    until("the far pane runs", || {
        fact(&far, "state") == "running" && read(&far.join("shown/screen.styled")).is_empty().eq(&false)
    });
    assert!(!far.join("cwd").exists());
    // Anyone may have something noticed through `ctl`; a program rings the bell. Each is the
    // pane's `notice`, with a number that grows. The viewer passes a new one on to its own
    // terminal once and marks the tab until its pane has had the keys.
    until("the viewer has looked at the panes once", || fact(&view, "modes").contains("focus"));
    std::thread::sleep(Duration::from_millis(600));
    send(&p2.join("ctl"), "notify the build is done\n");
    until("the notice is written", || read(&p2.join("notice")) == "1 the build is done\n");
    until("the viewer passed it on", || read(&view.join("notice")).ends_with(" p2: the build is done\n"));
    send(&view.join("in"), "\x02");
    until("the tab is marked", || read(&view.join("shown/screen")).contains(" p2! "));
    // The terminal's window loses and gains the keys with the menu open: the menu stays, and
    // the pane that asked is told.
    send(&view.join("in"), "\x1b[O\x1b[I");
    until("the pane is told of the window", || shown(&p1) == "^[[I^[[O^[[I");
    assert!(read(&view.join("shown/screen")).contains(" p2! "));
    // The next tab: the pane has the keys, the mark goes, and the panes are told who has them.
    send(&view.join("in"), "n");
    until("the second pane is told", || shown(&p2) == "^[[I" && shown(&p1) == "^[[I^[[O^[[I^[[O");
    send(&view.join("in"), "\x02");
    until("the mark is gone", || {
        read(&view.join("shown/screen")).contains(" p2 ") && !read(&view.join("shown/screen")).contains("p2!")
    });
    send(&view.join("in"), "\x1b");
    // A program that stops asking (it ends, and a shell is there again) is told nothing more:
    // not when it stops, and not when its pane loses the keys.
    let asks_then_stops = "stty raw -echo; printf '\\033[?1004h'; until [ -e \"$FUX_PANE/go\" ]; do sleep 0.05; done; printf '\\033[?1004l'; exec cat -vt";
    let quits = inner.start("quits", &["sh", "-c", asks_then_stops]);
    until("it asks, and has its place", || {
        fact(&quits, "modes") == "focus" && read(&inner.0.join("main/layout")).contains("quits")
    });
    lay(&inner.0, "  p1 focus=p1\n  p2 focus=p2\n* quits focus=quits\n");
    until("the second pane lost the keys", || shown(&p2).ends_with("^[[I^[[O"));
    std::thread::sleep(Duration::from_millis(300));
    fs::write(quits.join("go"), "").unwrap();
    let seen = || {
        format!(
            ": it has {:?}, modes {:?}, layout {:?}",
            shown(&quits),
            fact(&quits, "modes"),
            read(&inner.0.join("main/layout"))
        )
    };
    until_or("it was told it had the keys, and stops asking", seen, || {
        shown(&quits) == "^[[I" && fact(&quits, "modes").is_empty()
    });
    std::thread::sleep(Duration::from_millis(300));
    lay(&inner.0, "  p1 focus=p1\n* p2 focus=p2\n  quits focus=quits\n");
    until("the second pane has the keys again", || shown(&p2).ends_with("^[[O^[[I"));
    assert_eq!(shown(&quits), "^[[I");
    let bell = inner.start("bell", &["sh", "-c", "sleep 0.7; printf '\\007'; sleep 600"]);
    until("the bell is a notice with no words", || read(&bell.join("notice")) == "1\n");
    // A pane that is new, and has something to notice at once: that is new too.
    inner.start("quick", &["sh", "-c", "printf '\\033]9;done at once\\007'; sleep 600"]);
    until("the viewer passed it on", || read(&view.join("notice")).ends_with(" quick: done at once\n"));
    // A program in the middle of drawing (mode 2026) is not shown until it is done: half a
    // frame is never seen. One that never says it is done is shown after half a second, however
    // often it says that it draws.
    let draws = inner.start("draws", &["sh", "-c", "printf '\\033[?2026hhalf'; sleep 0.3; printf ' whole\\033[?2026l'; sleep 0.3; printf '\\033[?2026h stuck'; while sleep 0.1; do printf '\\033[?2026h'; done"]);
    let start = Instant::now();
    while !read(&draws.join("shown/screen")).contains("stuck") {
        assert!(!read(&draws.join("shown/screen")).starts_with("half\n"), "half a frame was shown");
        assert!(start.elapsed() < Duration::from_secs(10), "a program that never ends its drawing froze its pane");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(start.elapsed() > Duration::from_millis(900));
    // The find key lists the panes in a pane of its own, with whatever finder there is (a
    // numbered list if none): the pane chosen gets the keys.
    lay(&inner.0, "* p1 focus=p1\n  p2 focus=p2\n");
    until("the first tab is shown", || shown(&p1).ends_with("^[[I"));
    send(&view.join("in"), "\x02fp");
    until("the list is shown", || {
        read(&view.join("shown/screen")).contains("p2") && read(&view.join("shown/screen")).contains("draws")
    });
    std::thread::sleep(Duration::from_millis(300));
    // (A finder takes a moment to narrow its list to what was typed.)
    send(&view.join("in"), "p2");
    std::thread::sleep(Duration::from_millis(500));
    send(&view.join("in"), "\r");
    until("the pane chosen has the keys", || read(&inner.0.join("main/layout")).contains("* p2 focus=p2"));
}

#[test]
fn the_marks_of_a_shell_reach_into_history_and_the_keys_open_an_output_and_a_prompt() {
    let session = Session::new("marks");
    fs::create_dir_all(&session.0).unwrap();
    let keys = session.0.join(".keys");
    assert!(
        Command::new("cp").arg("-R").arg(concat!(env!("CARGO_MANIFEST_DIR"), "/keys")).arg(&keys).ended().success()
    );
    // A program that marks as a shell does: two commands (the first with more output than the
    // screen has rows, and a line that wraps), then a prompt.
    let shell = "m() { printf '\\033]133;%s\\007' \"$1\"; }; \
        m A; printf '$ '; m B; echo first; m C; seq 101 112; printf '\\033[32ma-long-line-\\033[mthat-wraps-in-twelve\\n'; m 'D;0'; \
        m A; printf '$ '; m B; echo second; m C; echo alpha; echo beta; m 'D;1'; m A; printf '$ '; m B; sleep 600";
    let mut start = session.fux();
    start.env("COLUMNS", "12").env("LINES", "5").args(["start", "sh", "--", "sh", "-c", shell]);
    let pane = PathBuf::from(String::from_utf8(start.run().stdout).unwrap().trim());
    until("the marks are there", || read(&pane.join("marks")).lines().count() == 10);
    let marks = read(&pane.join("marks"));
    let (first, last) =
        ("0 0 prompt\n0 2 command\n1 0 output\n14 0 end 0\n14 0 prompt\n", "17 0 end 1\n17 0 prompt\n17 2 command\n");
    assert!(marks.starts_with(first) && marks.ends_with(last), "{marks}");
    // One file says how the screen's rows are lines of the pane's text: the number of the line
    // its first row is of (the wrapped one, 13 lines in), then the rows that go on from the row
    // above (the first, from history). No other file says either.
    assert_eq!(
        read(&pane.join("main/lines")),
        "13\n0\n",
        "the number of the first row's line, then the rows that go on"
    );
    assert!(!pane.join("top").exists() && !pane.join("wraps").exists());
    // An editor for the test: it is called `vim`, so it is given a line number; it notes that
    // and copies the file it was to open. (The copy has its name only when it is whole, and the
    // note is written before it: a result that is there is all there, and of this key.)
    let out = session.0.join("out");
    let fake = |words: &str| {
        format!(
            "#!/bin/sh\nfor a; do f=$a; done\necho \"{words}\" > \"$OUT.at\"\ncp \"$f\" \"$OUT.new\" && mv \"$OUT.new\" \"$OUT\"\n"
        )
    };
    fs::write(session.0.join("vim"), fake("$1")).unwrap();
    Command::new("chmod").arg("+x").arg(session.0.join("vim")).ended();
    let clear = || {
        let _ = fs::remove_file(&out);
        let _ = fs::remove_file(session.0.join("out.at"));
    };
    let key = |name: &str| {
        clear();
        let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
        let mut script = Command::new(keys.join("C-b").join(name));
        script.env("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap())).env("EDITOR", session.0.join("vim"));
        script.env("OUT", &out).env("FUX_DIR", &session.0).env("FUX_KEYS", &keys).env("FUX_WS", "main");
        script.env_remove("FUX_EDIT");
        assert!(script.env("FUX_PANE", &pane).ended().success());
        until("the editor was given the file", || out.exists() && session.0.join("out.at").exists());
        (read(&out), read(&session.0.join("out.at")).trim().trim_start_matches('+').parse::<usize>().unwrap())
    };
    // The last command's output, and nothing else.
    assert_eq!(key("S-c"), ("alpha\nbeta\n".to_string(), 1));
    // The whole text at the prompt before this one; its first command's output is whole, the
    // wrapped line one line, though most of it is in history.
    let (text, line) = key("p");
    assert_eq!(text.lines().nth(line - 1), Some("$ second"), "{text}");
    assert!(
        text.starts_with("$ first\n101\n")
            && text.contains("\n112\na-long-line-that-wraps-in-twelve\n$ second\nalpha\n"),
        "{text}"
    );
    // History in the editor opens at the text's end.
    let (text, line) = key("c");
    assert_eq!((text.lines().count(), text.lines().last()), (line, Some("$")));
    // History in colour is the same text with its colours, in `less -R` at its end (a `less`
    // for the test: the same script).
    fs::write(session.0.join("less"), fake("$*")).unwrap();
    Command::new("chmod").arg("+x").arg(session.0.join("less")).ended();
    clear();
    let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
    let mut script = Command::new(keys.join("C-b").join("M-c"));
    script.env("PATH", format!("{}:{bin}:{}", session.0.display(), std::env::var("PATH").unwrap()));
    script.env("OUT", &out).env("FUX_DIR", &session.0).env("FUX_KEYS", &keys).env("FUX_WS", "main");
    assert!(script.env("FUX_PANE", &pane).ended().success());
    until("less was given the file", || bare(&read(&out)) == text);
    assert!(read(&out).contains("\n\x1b[0;32ma-long-line-\x1b[mthat-wraps-in-twelve\n$ second\n"), "{:?}", read(&out));
    let asked = read(&session.0.join("out.at"));
    assert!(asked.starts_with("-+F -R +G /") && asked.ends_with("/text\n"), "{asked}");
    // The rows left out at the screen's end are those with no text: a colour alone is none.
    // And the cell a wide character left empty at a row's end is no text: the rows are joined without it.
    let blue = session.start("blue", &["sh", "-c", "printf 'x\\033[44m\\033[J%78s\u{754c}\\033[m' ''; sleep 600"]);
    until("the screen is blue", || read(&blue.join("shown/screen.styled")).matches("\x1b[0;44m").count() == 24);
    let mut text = Command::new(keys.join(".text"));
    assert!(text.arg(&out).arg("styled").env("FUX_PANE", &blue).run().status.success());
    assert!(
        read(&blue.join("shown/screen.styled")).starts_with(&format!("x\x1b[0;44m{}\u{200b} \x1b[m\n", " ".repeat(78)))
    );
    let joined = format!("x\x1b[0;44m{}\x1b[m\x1b[0;44m\u{754c} ", " ".repeat(78));
    assert!(read(&out).starts_with(&joined) && read(&out).lines().count() == 1, "{:?}", read(&out));
    // A command that prints nothing has no output to open, and the key says so; one whose
    // output does not end its line has that output, every character of it whole, without the
    // prompt that follows on the line. (And the editor is run as `FUX_EDIT` says.)
    let quiet = "m() { printf '\\033]133;%s\\007' \"$1\"; }; \
        m A; printf '$ '; m B; echo printf; m C; printf 'e\\314\\201\\347\\225\\214'; m 'D;0'; \
        m A; printf '$ '; m B; until [ -e \"$FUX_PANE/go\" ]; do sleep 0.05; done; echo true; m C; m 'D;0'; m A; printf '$ '; m B; sleep 600";
    let pane = session.start("quiet", &["sh", "-c", quiet]);
    until("the first command is done", || read(&pane.join("marks")).lines().count() == 6);
    let run_in = |name: &str, pane: &Path, edit: String| {
        let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
        let mut script = Command::new(keys.join("C-b").join(name));
        script.env("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap())).env("FUX_EDIT", edit);
        script.env("OUT", &out).env("FUX_DIR", &session.0).env("FUX_KEYS", &keys).env("FUX_WS", "main");
        String::from_utf8(script.env("FUX_PANE", pane).run().stdout).unwrap()
    };
    let run = |name: &str| run_in(name, &pane, format!("{} \"line-$2\" \"$1\"", session.0.join("vim").display()));
    clear();
    assert_eq!(run("S-c"), "");
    until("the editor was given the output", || read(&out) == "e\u{301}界\n");
    assert_eq!(read(&session.0.join("out.at")), "line-1\n");
    fs::write(pane.join("go"), "").unwrap();
    until("the second command is done", || read(&pane.join("marks")).lines().count() == 10);
    assert_eq!(
        run("S-c"),
        "nothing of the last command's output is here (it printed none, or the screen was cleared)\n"
    );
    // An editor still open when its pane is closed with the close key: the file it was given is
    // in the pane's own folder, and goes with it.
    assert_eq!(run_in("c", &pane, "tail -f \"$1\"".into()), "");
    let editor =
        || fs::read_dir(session.0.join("main")).unwrap().flatten().find(|pane| pane.path().join("text").exists());
    until("the editor has its file", || editor().is_some());
    let closing = editor().unwrap().path();
    assert_eq!(run_in("x/y", &closing, String::new()), "");
    until("the pane and its file are gone", || {
        !closing.exists()
            && fs::read_dir(&session.0).unwrap().flatten().all(|file| {
                let name = file.file_name().to_string_lossy().into_owned();
                !name.starts_with(".text.") && !name.starts_with(".closed.")
            })
    });
}

#[test]
fn a_shell_that_ends_with_127_closes_its_pane_and_a_command_that_cannot_be_run_stays_to_be_read() {
    // `.shell`'s "it could not be run" is for a program that could not be started. A shell that
    // was started and ends with the status of its last command (127 after a mistyped one) has
    // run: its pane closes as after any other exit.
    let session = Session::new("could");
    fs::create_dir_all(&session.0).unwrap();
    let wrapper = concat!(env!("CARGO_MANIFEST_DIR"), "/keys/.shell");
    fs::write(session.0.join("sh127"), "#!/bin/sh\nexit 127\n").unwrap();
    assert!(Command::new("chmod").arg("+x").arg(session.0.join("sh127")).ended().success());
    let mut start = session.fux();
    start.env("SHELL", session.0.join("sh127")).args(["start", "ran", "--", wrapper]);
    let ran = PathBuf::from(String::from_utf8(start.run().stdout).unwrap().trim());
    let seen = || format!(": its screen is {:?}", read(&ran.join("shown/screen")).trim());
    until_or("the pane of a shell that ended with 127 is gone", seen, || !ran.exists());
    // A command that is not there could not be run: that is said, and stays until Enter.
    let none = session.start("none", &[wrapper, "/no/such/program"]);
    until("it says that it could not be run", || {
        read(&none.join("shown/screen")).contains("it could not be run: Enter")
    });
    send(&none.join("in"), "\r");
    until("Enter closed the pane", || !none.exists());
}

#[test]
fn the_workspace_chosen_is_told_to_the_viewer_that_is_there_and_no_file_is_made_for_one_that_has_gone() {
    // `C-b w g`'s picker writes its choice to the FIFO of the viewer that started it. A viewer
    // that has gone by then (detached, and attached again under another number) has none: the
    // choice is lost, and no plain file of the FIFO's name is left in the session.
    let session = Session::new("chosen");
    let keys = session.0.join(".keys");
    fs::create_dir_all(session.0.join("main")).unwrap();
    assert!(
        Command::new("cp").arg("-R").arg(concat!(env!("CARGO_MANIFEST_DIR"), "/keys")).arg(&keys).ended().success()
    );
    let view = session.0.join(".view.0");
    let choose = || {
        let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
        let mut script = Command::new(keys.join("C-b/w/g"));
        script.env("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap())).env("FUX_VIEW", &view);
        script.env("FUX_DIR", &session.0).env("FUX_KEYS", &keys).env("FUX_WS", "main").env_remove("FUX_PANE");
        assert!(script.ended().success());
        let pane = session.0.join("main/1");
        until("the list is shown", || read(&pane.join("shown/screen")).contains("main"));
        // (A finder takes a moment to narrow its list to what was typed; with none it is the name.)
        std::thread::sleep(Duration::from_millis(300));
        send(&pane.join("in"), "main");
        std::thread::sleep(Duration::from_millis(500));
        send(&pane.join("in"), "\r");
        until("the picker's pane is gone", || !pane.exists());
    };
    // A viewer that is there is told (here the test holds its FIFO, and reads it).
    assert!(Command::new("mkfifo").arg(&view).ended().success());
    let mut fifo = fs::OpenOptions::new().read(true).write(true).open(&view).unwrap();
    choose();
    let (told, tells) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = [0; 64];
        let n = std::io::Read::read(&mut fifo, &mut line).unwrap();
        told.send(String::from_utf8_lossy(&line[..n]).into_owned())
    });
    assert_eq!(tells.recv_timeout(Duration::from_secs(60)).as_deref(), Ok("workspace main\n"));
    fs::remove_file(&view).unwrap();
    choose();
    assert!(!view.exists(), "a file was made in place of the FIFO of a viewer that has gone");
}

#[test]
fn an_editor_that_ends_while_its_key_runs_has_taken_its_pane_and_the_key_says_nothing() {
    // The editor's pane removes its own folder when the editor ends. When that is before the
    // key's script asks the layout to show the pane, there is none to show: no error of the
    // key's. (An editor that ends when told, and a `fux` first in PATH whose `layout` tells it
    // and waits until the pane has gone.)
    let session = Session::new("atonce");
    let (keys, bin) = (session.0.join(".keys"), session.0.join("bin"));
    fs::create_dir_all(&bin).unwrap();
    assert!(
        Command::new("cp").arg("-R").arg(concat!(env!("CARGO_MANIFEST_DIR"), "/keys")).arg(&keys).ended().success()
    );
    let fux = env!("CARGO_BIN_EXE_fux");
    let scripts = [
        ("ed", "#!/bin/sh\nuntil [ -e \"$FUX_DIR/go\" ]; do sleep 0.02; done\n".to_string()),
        (
            "fux",
            format!(
                "#!/bin/sh\nif [ \"$1 $2\" = 'layout pane' ]; then : > \"$FUX_DIR/go\"; while [ -d \"$FUX_DIR/main/$3\" ]; do sleep 0.02; done; fi\nexec {fux} \"$@\"\n"
            ),
        ),
    ];
    for (name, text) in scripts {
        fs::write(bin.join(name), text).unwrap();
        assert!(Command::new("chmod").arg("+x").arg(bin.join(name)).ended().success());
    }
    fs::write(session.0.join("file"), "text\n").unwrap();
    let mut script = Command::new(keys.join(".edit"));
    script.arg(session.0.join("file")).env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()));
    script.env("FUX_EDIT", "ed \"$1\"").env("FUX_DIR", &session.0).env("FUX_KEYS", &keys).env("FUX_WS", "main");
    let out = script.env_remove("FUX_PANE").run();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success() && said.is_empty(), "{:?}: {said}", out.status);
    assert!(!session.0.join("main/1").exists() && !session.0.join("file").exists());
}

#[test]
fn a_key_the_viewer_took_is_its_own_to_its_release_and_a_release_follows_its_press() {
    // As the test before: the viewer in a pane of an outer session whose terminal is said to
    // take every flag of the kitty keyboard protocol.
    let (outer, inner) = (Session::new("router"), Session::new("rinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b")).unwrap();
    fs::write(keys.join("C-b/o"), "#!/bin/sh\n# other\nfux layout focus next\n").unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/o")).ended();
    fs::create_dir_all(&outer.0).unwrap();
    fs::write(outer.0.join("keyboard"), "31\n").unwrap();
    let raw = "stty raw -echo; exec cat -vt";
    let plain = inner.start("plain", &["sh", "-c", raw]);
    let viewer = [env!("CARGO_BIN_EXE_fux"), "view"];
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &viewer[..]].concat());
    until("the session knows what the terminal takes", || read(&inner.0.join("keyboard")) == "31\n");
    // A program asks for escape codes told apart, presses and releases, and every key as a code.
    let asking = "stty raw -echo; printf '\\033[>11u\\033[?u'; exec cat -vt";
    let asks = inner.start("asks", &["sh", "-c", asking]);
    let shown = |pane: &Path| read(&pane.join("shown/screen")).replace('\n', "");
    until("it is told they are in force", || shown(&asks) == "^[[?11u" && fact(&asks, "modes") == "keys11");
    lay(&inner.0, "* row(asks plain) focus=asks\n");
    until("the viewer asked its terminal for them", || fact(&view, "modes").contains("keys11"));
    // A key typed: its press and its release reach the program. One held down stays down for now.
    send(&view.join("in"), "\x1b[97;1:1u\x1b[97;1:3u\x1b[120;1:1u");
    until("the keys arrive", || shown(&asks) == "^[[?11u^[[97;1:1u^[[97;1:3u^[[120;1:1u");
    // Ctrl goes down (a terminal that tells of every key tells of that too: the pane has the keys
    // and gets it), then the prefix, held and let go; Shift by itself, down and up, with the menu
    // open; Ctrl is let go. The menu stays open through all of it; Ctrl's release follows its
    // press to the pane, and the pane hears of nothing else.
    let ctrl = "\x1b[57442;5:1u\x1b[98;5:1u\x1b[98;5:2u\x1b[98;5:3u\x1b[57441;2:1u\x1b[57441;1:3u\x1b[57442;1:3u";
    send(&view.join("in"), ctrl);
    until("the menu is open, and stays", || read(&view.join("shown/screen")).contains("o  other"));
    let held = "^[[?11u^[[97;1:1u^[[97;1:3u^[[120;1:1u^[[57442;5:1u^[[57442;1:3u";
    until("Ctrl went down and up in the pane", || shown(&asks) == held);
    assert!(read(&view.join("shown/screen")).contains("o  other"));
    // The menu's key and its release: the script runs once and the other pane has the keys.
    send(&view.join("in"), "\x1b[111;1:1u\x1b[111;1:3u");
    until("the other pane has the keys", || read(&inner.0.join("main/layout")).contains("focus=plain"));
    until("the terminal is asked for no flags", || !fact(&view, "modes").contains("keys"));
    // The keys left the first pane with a key of its still down: it is told there that the key
    // is let go, at once (this terminal, asked for no flags now, would never tell of it). What
    // the terminal still tells of that key reaches no pane, nor does a release of a key nobody
    // pressed. What is typed here arrives here.
    until("the first pane's key is let go there", || shown(&asks) == format!("{held}^[[120;1:3u"));
    send(&view.join("in"), "\x1b[120;1:2u\x1b[120;1:3u\x1b[121;1:3uz");
    until("the plain pane has what was typed there", || shown(&plain) == "z");
    assert_eq!(shown(&asks), format!("{held}^[[120;1:3u"));
    let held = format!("{held}^[[120;1:3u");
    // A key the viewer took whose release never came (the terminal tells of none without the
    // flag) does not take a later press of that key for its own: the prefix, then the menu's key,
    // and `b` typed in the pane that has the keys again, down and up.
    send(&view.join("in"), "\x1b[98;5:1u");
    until("the menu is open", || read(&view.join("shown/screen")).contains("o  other"));
    send(&view.join("in"), "\x1b[111;1:1u\x1b[111;1:3u");
    until("the first pane has the keys", || fact(&view, "modes").contains("keys11"));
    send(&view.join("in"), "\x1b[98;1:1u\x1b[98;1:3u");
    until("b went down and up", || shown(&asks) == format!("{held}^[[98;1:1u^[[98;1:3u"));
    // Only a press makes a key someone's: a repeat with no press goes nowhere, and so does the
    // release after it. A sign sent as itself when pressed and as a sequence when let go (the
    // number of its key, then its own) is one key.
    send(&view.join("in"), "\x1b[119;1:2u\x1b[119;1:3u!\x1b[49:33;2:3u");
    let held = format!("{held}^[[98;1:1u^[[98;1:3u!^[[49:33;2:3u");
    until("the sign went down and up", || shown(&asks) == held);
    // The viewer leaves with a key down in the pane: the pane is told that it is let go. Text
    // with no key (number 0) was passed on before it, and is no key that is let go.
    send(&view.join("in"), "\x1b[0;;229u\x1b[99;1:1u");
    let held = format!("{held}^[[0;;229u");
    until("c is down", || shown(&asks) == format!("{held}^[[99;1:1u"));
    let ctl = fs::read_dir(&inner.0)
        .unwrap()
        .flatten()
        .find(|entry| entry.file_name().to_string_lossy().starts_with(".view."));
    send(&ctl.unwrap().path(), "quit\n");
    until("c is let go", || shown(&asks) == format!("{held}^[[99;1:1u^[[99;1:3u"));
    assert_eq!(shown(&plain), "z");
}

#[test]
fn a_held_key_does_again_what_its_press_did_and_there_only_and_a_program_started_anew_gets_none_of_it() {
    let (outer, inner) = (Session::new("houter"), Session::new("hinner"));
    let keys = inner.0.join(".keys");
    // Two menus that stay open, with the same key in both.
    for menu in ["r", "m"] {
        fs::create_dir_all(keys.join("C-b").join(menu)).unwrap();
        fs::write(keys.join("C-b").join(menu).join(".repeat"), "").unwrap();
        let script = keys.join("C-b").join(menu).join("h");
        fs::write(&script, format!("#!/bin/sh\n# act\necho {menu} >> \"$FUX_KEYS/ran\"\n")).unwrap();
        Command::new("chmod").arg("+x").arg(script).ended();
    }
    fs::create_dir_all(&outer.0).unwrap();
    fs::write(outer.0.join("keyboard"), "31\n").unwrap();
    fs::create_dir_all(&inner.0).unwrap();
    fs::write(inner.0.join("keyboard"), "31\n").unwrap();
    let asking = ["sh", "-c", "stty raw -echo; printf '\\033[>11u\\033[?u'; exec cat -vt"];
    let (first, second) = (inner.start("first", &asking), inner.start("second", &asking));
    let layout = |edit: &[&str]| assert!(inner.fux().arg("layout").args(edit).ended().success());
    layout(&["pane", "first"]);
    let viewer = [env!("CARGO_BIN_EXE_fux"), "view"];
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &viewer[..]].concat());
    let shown = |pane: &Path| read(&pane.join("shown/screen")).replace('\n', "");
    until("the viewer asked its terminal for the flags", || fact(&view, "modes").contains("keys11"));
    // A key (down and up) for each of: the prefix, a menu, Escape; and `h` down, held, up.
    let tap = |key: &str| format!("\x1b[{key}:1u\x1b[{key}:3u");
    let (prefix, escape, h) = (tap("98;5"), tap("27;1"), ["\x1b[104;1:1u", "\x1b[104;1:2u", "\x1b[104;1:3u"]);
    let ran = || read(&keys.join("ran"));
    // Held in the one menu, the key acts again there.
    send(&view.join("in"), format!("{prefix}{}{}{}", tap("114;1"), h[0], h[1]));
    until("the key acted twice", || ran() == "r\nr\n");
    // Still held when that menu is closed and the other opened, it does nothing in the other:
    // not what it did before, and not what a press would do there. Pressed there, it does.
    send(&view.join("in"), format!("{escape}{prefix}{}{}{}{}{}", tap("109;1"), h[1], h[2], h[0], h[2]));
    until("the key acted in the other menu", || ran().ends_with("m\n"));
    // The prefix pressed in a menu that stays open starts over: its own menu is open again, and
    // the next keys are that menu's.
    send(&view.join("in"), format!("{prefix}{}{}{}", tap("114;1"), h[0], h[2]));
    until("the first menu's key acted again", || ran() == "r\nr\nm\nr\n");
    // A key goes down in the first pane, and the other tab is shown: the key is let go there,
    // and a key typed right after the command that showed it is the second pane's.
    send(&view.join("in"), format!("{escape}\x1b[97;1:1u"));
    until("the key is down", || shown(&first) == "^[[?11u^[[97;1:1u");
    assert_eq!(ran(), "r\nr\nm\nr\n");
    // (What the terminal still tells of the held key right after that command, before the
    // viewer has drawn again, reaches neither pane.)
    layout(&["tab", "next"]);
    send(&view.join("in"), format!("\x1b[97;1:2u{}", tap("122;1")));
    until("the second pane has the keys", || shown(&second) == "^[[?11u^[[122;1:1u^[[122;1:3u");
    until("the key is let go in the first", || shown(&first) == "^[[?11u^[[97;1:1u^[[97;1:3u");
    // The first pane's program ends and another is started under its name, in the tab that is
    // not shown: what the terminal tells of the key after that is nothing to the new program.
    send(&first.join("ctl"), "kill\n");
    until("the program has ended", || fact(&first, "state") != "running");
    inner.start("first", &asking);
    until("the new program is told its flags", || {
        fact(&first, "state") == "running" && shown(&first).ends_with("^[[?11u")
    });
    let new = shown(&first);
    send(&view.join("in"), format!("\x1b[97;1:2u\x1b[97;1:3u{}", tap("121;1")));
    until("the second pane has the next key", || shown(&second).ends_with("^[[121;1:1u^[[121;1:3u"));
    assert_eq!(shown(&first), new);
}

#[test]
fn a_link_a_program_makes_is_a_line_of_links_and_the_viewer_draws_it_inside_its_tile() {
    // The viewer in a pane of an outer session: what it draws as a link is in that pane's `links`.
    let (outer, inner) = (Session::new("louter"), Session::new("linner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    let address = "https://example.org/a?b=c#d";
    // At columns 3 to 6 of its second row, and again where it runs past the tile's right edge.
    // Above them two characters a terminal would join into one (a flag's two halves, the right
    // one printed first), each a link of its own, and in the third row's last cell before the
    // tile's edge a character that takes the next one into itself (U+0600), a link too.
    let prints = format!(
        "printf '\\033[1;2H\\033]8;;{address}2\\033\\\\🇧\\033]8;;\\007\\033[1;1H\\033]8;;{address}1\\033\\\\🇦\\033]8;;\\007\\033[3;40H\\033]8;;{address}3\\033\\\\\u{600}\\033]8;;\\007\\033[1;3H\\n  \\033]8;;{address}\\033\\\\link\\033]8;;\\033\\\\ plain\\n\\033[4;35H\\033]8;;{address}\\033\\\\0123456789\\033]8;;\\007'; sleep 600"
    );
    let left = inner.start("left", &["sh", "-c", &prints]);
    inner.start("right", &["sh", "-c", "echo right; sleep 600"]);
    let above = format!("0 0 1 {address}1\n0 1 1 {address}2\n1 2 4 {address}\n2 39 1 {address}3\n");
    until("the link is the pane's", || read(&left.join("shown/links")) == format!("{above}3 34 10 {address}\n"));
    assert!(!read(&left.join("shown/screen.styled")).contains("\x1b]"));
    fs::create_dir_all(inner.0.join("main")).unwrap();
    lay(&inner.0, "* row(left right) focus=right\n");
    // (Whatever ran before the viewer left a link open on its terminal: no cell of the viewer's is part of it.)
    let viewer = format!("printf '\\033]8;;http://open/\\033\\\\'; exec {} view", env!("CARGO_BIN_EXE_fux"));
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &["sh", "-c", &viewer]].concat());
    // The tile is 40 columns wide and the pane takes that size: the second link's text wraps,
    // and the link is drawn on its cells in both rows, up to the tile's edge and not on the
    // line between the tiles. Each cell has its own address and no other: the flag's halves are
    // two characters still, and the line between the tiles is no part of the character before it.
    until("the pane has its tile's size", || {
        read(&left.join("shown/links")) == format!("{above}3 34 10 {address}\n") && fact(&left, "size") == "40 24"
    });
    let drawn = format!("{above}3 34 6 {address}\n4 0 4 {address}\n");
    // (A pane's facts are written before its screen: the one may be read a round ahead of the other.)
    until("the viewer drew the links", || {
        read(&view.join("shown/links")) == drawn
            && read(&view.join("shown/screen")).lines().nth(3).is_some_and(|row| row.contains("012345│"))
    });
    // The pane's text scrolls into history and its first link with it; the viewer's terminal
    // then has that one no more.
    send(&left.join("ctl"), "resize 40 2\n");
    until("the first link is in history", || read(&left.join("shown/links")) == format!("3 34 10 {address}\n"));
    until("the viewer follows", || read(&view.join("shown/links")) == format!("0 34 6 {address}\n1 0 4 {address}\n"));
}

#[test]
fn a_picture_that_does_not_fit_its_tile_is_drawn_as_the_part_of_it_inside_the_tile() {
    // The viewer in a pane of an outer session, whose terminal (fux) says it draws sixel: what
    // the viewer draws is that pane's picture, a file and a line in `pictures`.
    let (outer, inner) = (Session::new("pouter"), Session::new("pinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b")).unwrap();
    fs::write(keys.join("C-b/x"), "#!/bin/sh\n").unwrap();
    // 600 by 60 pixels, 60 columns and 3 rows of an 80 column pane: red, the last 205 columns
    // blue, and green for red from the 31st row of pixels on. At a line typed to it the program
    // scrolls its screen by one row.
    let bands: Vec<String> = (0..10).map(|band| format!("#{}!395~#1!205~", if band < 5 { 0 } else { 2 })).collect();
    let picture = format!("q\"1;1;600;60#0;2;100;0;0#1;2;0;0;100#2;2;0;100;0{}", bands.join("-"));
    let prints = format!(
        "stty -echo; printf '\\033P%s\\033\\\\' '{picture}'; read line; set -- $(stty size); i=3; while [ $i -lt $1 ]; do echo; i=$((i + 1)); done; sleep 600"
    );
    let left = inner.start("left", &["sh", "-c", &prints]);
    inner.start("right", &["sh", "-c", "echo right; sleep 600"]);
    until("the picture is the pane's", || fact(&left, "shown/pictures") == "pic-1.sixel 0 0 60 3 10 20");
    let whole = fs::read(left.join("pic-1.sixel")).unwrap();
    assert_eq!(whole, format!("\x1bP{picture}\x1b\\").as_bytes());
    lay(&inner.0, "* row(left right) focus=right\n");
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &[env!("CARGO_BIN_EXE_fux"), "view"]].concat());
    // The picture the viewer drew last: where it is and its file; and a picture's pixels in a
    // rectangle of it, each with its colour.
    let drawn = || {
        let pictures = fact(&view, "shown/pictures");
        let (name, place) = pictures.lines().last().and_then(|line| line.split_once(' ')).unwrap_or_default();
        let place: Vec<&str> = place.split(' ').take(4).collect();
        (place.join(" "), fs::read(view.join(name)).unwrap_or_default())
    };
    let pixels = |picture: &[u8], rectangle: [usize; 4]| {
        let inside = picture.strip_prefix(b"\x1bP").and_then(|rest| rest.strip_suffix(b"\x1b\\")).unwrap_or_default();
        let part = sixel::read(inside, rectangle).ok()?;
        let pen = |x: usize, y: usize| usize::from(part.pixels[y * part.stride + x]).checked_sub(1);
        let colours = (0..part.h).flat_map(|y| (0..part.w).map(move |x| pen(x, y))).map(|pen| Some(part.colours[pen?]));
        Some((part.w, part.h, colours.collect::<Vec<_>>()))
    };
    let [red, green, blue] = [[100, 0, 0], [0, 100, 0], [0, 0, 100]].map(|colour| Some(sixel::Colour::Rgb(colour)));
    // The tile is 40 columns wide and the pane takes that size, with its picture whole. The
    // viewer draws the 40 columns of it that are in the tile, 400 pixels of each row: the line
    // between the tiles and the pane beside it are as they were.
    until("the part in the tile is drawn", || fact(&left, "size") == "40 24" && drawn().0 == "0 0 40 3");
    let (_, part) = drawn();
    let (w, h, colours) = pixels(&part, [0, 0, 1000, 100]).expect("the viewer drew a sixel picture");
    assert_eq!((w, h, colours[394], colours[395], colours[399], colours[30 * 400]), (400, 60, red, blue, blue, green));
    assert!(Some((w, h, colours)) == pixels(&whole, [0, 0, 400, 60]));
    let screen = read(&view.join("shown/screen"));
    assert!(screen.starts_with(&format!("{:40}│right\n{:40}│\n{:40}│\n", "", "", "")), "{screen}");
    assert_eq!(fact(&left, "shown/pictures"), "pic-1.sixel 0 0 60 3 10 20");
    assert_eq!(fs::read(left.join("pic-1.sixel")).unwrap(), whole);
    // In a tile wide enough the picture is whole again: the program's own bytes.
    lay(&inner.0, "* col(left right) focus=right\n");
    until("the picture is drawn whole", || drawn() == ("0 0 60 3".to_string(), whole.clone()));
    // A menu that covers no cell of it leaves it drawn.
    send(&view.join("in"), "\x02");
    until("the menu is open", || read(&view.join("shown/screen")).contains("C-b …"));
    assert_eq!(drawn(), ("0 0 60 3".to_string(), whole.clone()));
    send(&view.join("in"), "\x1b");
    // Scrolled one row off its pane's top, it is drawn from its 21st row of pixels on.
    send(&left.join("in"), "\n");
    until("the pane has it one row up", || fact(&left, "shown/pictures") == "pic-1.sixel -1 0 60 3 10 20");
    until("the rest of it is drawn", || drawn().0 == "0 0 60 2");
    let (w, h, colours) = pixels(&drawn().1, [0, 0, 1000, 100]).expect("the viewer drew a sixel picture");
    assert_eq!((w, h, colours[9 * 600], colours[10 * 600], colours[599]), (600, 40, red, green, blue));
    assert!(Some((w, h, colours)) == pixels(&whole, [0, 20, 600, 40]));
    // The file written anew under its name is drawn anew: all of it blue.
    let anew = |picture: String| {
        fs::write(left.join("anew"), format!("\x1bP{picture}\x1b\\")).unwrap();
        fs::rename(left.join("anew"), left.join("pic-1.sixel")).unwrap();
    };
    anew(format!("q\"1;1;600;60#1;2;0;0;100{}", ["!600~"; 10].join("-")));
    let all_blue = |(w, h, colours): (usize, usize, Vec<_>)| (w, h) == (600, 40) && colours.iter().all(|c| *c == blue);
    until("the new picture is drawn", || pixels(&drawn().1, [0, 0, 1000, 100]).is_some_and(all_blue));
    // One that cannot be cut is not drawn, and the viewer's corner says why. (It paints every
    // band in a register past the last: what a picture paints outside its part counts for nothing.)
    anew(format!("q\"1;1;600;60#256{}", ["!600~"; 10].join("-")));
    let why = "left: a picture is not drawn: it names a colour register over 255";
    until("the corner says why", || {
        read(&view.join("shown/screen")).contains(why) && fact(&view, "shown/pictures").is_empty()
    });
}

#[test]
fn a_picture_is_cut_by_the_cells_it_was_laid_out_in_whatever_the_viewers_are() {
    // A pane told that a cell is 10 by 30 pixels: a picture of 600 by 60, red above and green from
    // its 31st row of pixels on, is two rows of it. Its program scrolls it one row up and ends:
    // the list says one row of it is above the screen, and of what cells.
    let (outer, inner) = (Session::new("couter"), Session::new("cinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    let bands: Vec<String> = (0..10).map(|band| format!("#{}!600~", if band < 5 { 0 } else { 2 })).collect();
    let picture = format!("q\"1;1;600;60#0;2;100;0;0#2;2;0;100;0{}", bands.join("-"));
    let prints = format!(
        "stty -echo; read line; printf '\\033P%s\\033\\\\' '{picture}'; set -- $(stty size); i=2; while [ $i -lt $1 ]; do echo; i=$((i + 1)); done"
    );
    let pane = inner.start("tall", &["sh", "-c", &prints]);
    send(&pane.join("ctl"), "resize 80 24 10 30\n");
    send(&pane.join("in"), "\n");
    assert!(inner.fux().arg("wait").arg(&pane).ended().success());
    assert_eq!(fact(&pane, "main/pictures"), "pic-1.sixel -1 0 60 2 10 30");
    // A viewer whose terminal has cells of 10 by 20 draws what is below that row: 30 rows of
    // pixels are above the screen, not 20, and what it draws is all green.
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &[env!("CARGO_BIN_EXE_fux"), "view"]].concat());
    until("the viewer drew a picture", || !fact(&view, "shown/pictures").is_empty());
    let (name, _) = fact(&view, "shown/pictures").split_once(' ').map(|(name, _)| (name.to_string(), ())).unwrap();
    let drawn = fs::read(view.join(name)).unwrap();
    let inside = drawn.strip_prefix(b"\x1bP").and_then(|rest| rest.strip_suffix(b"\x1b\\")).unwrap_or_default();
    let part = sixel::read(inside, [0, 0, 1000, 100]).expect("the viewer drew a sixel picture");
    let green = sixel::Colour::Rgb([0, 100, 0]);
    let colours: Vec<_> =
        part.pixels.iter().filter_map(|pen| usize::from(*pen).checked_sub(1)).map(|pen| part.colours[pen]).collect();
    assert!((part.w, part.h) == (600, 30) && colours.len() == 600 * 30 && colours.iter().all(|c| *c == green));
}

#[test]
fn a_click_gives_a_tile_the_keys_also_between_programs_that_ask_for_presses_only() {
    // Two programs that ask for the oldest mouse reports (mode 9: presses, no releases), side by side.
    let (outer, inner) = (Session::new("mouter"), Session::new("minner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    let asking = ["sh", "-c", "stty raw -echo; printf '\\033[?9h'; exec cat -v"];
    let (a, b) = (inner.start("a", &asking), inner.start("b", &asking));
    until("both ask for presses", || fact(&a, "modes") == "mouse9" && fact(&b, "modes") == "mouse9");
    lay(&inner.0, "* row(a b) focus=a\n");
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &[env!("CARGO_BIN_EXE_fux"), "view"]].concat());
    // The viewer asks its own terminal for releases too: a click that only moves the focus ends
    // with its release, which a terminal asked for presses alone would never tell of.
    until("the viewer asked its terminal for the mouse", || fact(&view, "modes").contains("mouse"));
    assert!(fact(&view, "modes").contains("mousesgr mouse1000"), "{}", fact(&view, "modes"));
    let focus = || read(&inner.0.join("main/layout"));
    for (click, tile) in [("50;3", "b"), ("5;3", "a"), ("50;3", "b")] {
        send(&view.join("in"), format!("\x1b[<0;{click}M\x1b[<0;{click}m"));
        until_or("the click moved the focus", focus, || focus().contains(&format!("focus={tile}")));
    }
    // The program that has the keys is told of presses and of nothing else, as it asked: in the
    // old form, counted from its tile's corner (which is at column 42).
    until("the pane is the size of its tile", || fact(&b, "size") == "39 24");
    send(&view.join("in"), "\x1b[<0;50;3M\x1b[<0;50;3m\x1b[<0;51;4M\x1b[<0;51;4m");
    until("the presses arrive", || read(&b.join("shown/screen")).contains("^[[M )#^[[M *$"));
    assert_eq!(
        (read(&b.join("shown/screen")).trim_end(), read(&a.join("shown/screen")).trim_end()),
        ("^[[M )#^[[M *$", "")
    );
}

#[test]
fn a_click_whose_release_the_terminal_never_told_of_does_not_take_the_next_click() {
    // A program that asks for the mouse beside one that does not. A click on the second gives it
    // the keys, and the terminal is then asked for no reports: the release of that click is
    // never told of. Back on the first, a click is that program's.
    let (outer, inner) = (Session::new("zouter"), Session::new("zinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    let a = inner.start("a", &["sh", "-c", "stty raw -echo; printf '\\033[?1000h\\033[?1006h'; exec cat -v"]);
    inner.start("b", &["sh", "-c", "stty raw -echo; exec cat -v"]);
    until("the first asks for the mouse", || fact(&a, "modes") == "mousesgr mouse1000");
    lay(&inner.0, "* row(a b) focus=a\n");
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &[env!("CARGO_BIN_EXE_fux"), "view"]].concat());
    until("the viewer asked its terminal for the mouse", || fact(&view, "modes").contains("mouse1000"));
    send(&view.join("in"), "\x1b[<0;50;3M");
    let layout = || read(&inner.0.join("main/layout"));
    until_or("the click moved the focus", layout, || layout().contains("focus=b"));
    until("the viewer asks for the mouse no more", || !fact(&view, "modes").contains("mouse1000"));
    assert!(inner.fux().env("FUX_WS", "main").args(["layout", "pane", "a"]).ended().success());
    until("the viewer asks for the mouse again", || fact(&view, "modes").contains("mouse1000"));
    send(&view.join("in"), "\x1b[<0;5;3M\x1b[<0;5;3m");
    let start = Instant::now();
    until_or(
        "the click is the program's",
        || read(&a.join("shown/screen")),
        || {
            assert!(
                start.elapsed() < Duration::from_secs(8),
                "the click went nowhere: {:?}",
                read(&a.join("shown/screen"))
            );
            read(&a.join("shown/screen")).contains("^[[<0;5;3M^[[<0;5;3m")
        },
    );
}

#[test]
fn a_press_after_the_program_asked_for_no_mouse_for_a_while_is_a_new_click() {
    // A program that asks for the mouse gets a press, asks for no reports before the button is
    // let go (the release is never told of) and then asks again. A press on the other tile is a
    // new click: it gives that tile the keys, and is not the first program's.
    let (outer, inner) = (Session::new("xouter"), Session::new("xinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    let asks = "printf '\\033[?1000h\\033[?1006h'; until [ -e \"$FUX_PANE/off\" ]; do sleep 0.05; done; \
                printf '\\033[?1000l'; until [ -e \"$FUX_PANE/on\" ]; do sleep 0.05; done; \
                printf '\\033[?1000h'; sleep 600";
    let a = inner.start("a", &["sh", "-c", asks]);
    inner.start("b", &["sh", "-c", "stty raw -echo; exec cat -v"]);
    until("the first asks for the mouse", || fact(&a, "modes") == "mousesgr mouse1000");
    lay(&inner.0, "* row(a b) focus=a\n");
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &[env!("CARGO_BIN_EXE_fux"), "view"]].concat());
    until("the viewer asked its terminal for the mouse", || fact(&view, "modes").contains("mouse1000"));
    send(&view.join("in"), "\x1b[<0;5;3M");
    until("the press is the program's", || read(&a.join("shown/screen")).contains("<0;5;3M"));
    fs::write(a.join("off"), "").unwrap();
    until("the viewer asks for the mouse no more", || !fact(&view, "modes").contains("mouse1000"));
    fs::write(a.join("on"), "").unwrap();
    until("the viewer asks for the mouse again", || fact(&view, "modes").contains("mouse1000"));
    send(&view.join("in"), "\x1b[<0;50;3M\x1b[<0;50;3m");
    let start = Instant::now();
    let layout = || read(&inner.0.join("main/layout"));
    until_or("the click moved the focus", layout, || {
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "the press was the first program's: {:?}",
            read(&a.join("shown/screen"))
        );
        layout().contains("focus=b")
    });
}

#[test]
fn a_menu_on_a_terminal_of_one_row_leaves_the_bar_to_be_read() {
    // One row is the bar's while the prefix is in use: the keys that can follow have no row above
    // it, and are not drawn over it.
    let (outer, inner) = (Session::new("oouter"), Session::new("oinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b")).unwrap();
    fs::write(keys.join("C-b/v"), "#!/bin/sh\n# says\necho said\n").unwrap();
    inner.start("one", &["sleep", "600"]);
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let mut start = outer.fux();
    start.env("COLUMNS", "80").env("LINES", "1").args(["start", "view", "--"]).args(command);
    start.args([env!("CARGO_BIN_EXE_fux"), "view"]);
    let view = PathBuf::from(String::from_utf8(start.run().stdout).unwrap().trim());
    let screen = || read(&view.join("shown/screen"));
    until("the viewer draws", || !fact(&view, "modes").is_empty() || !screen().trim().is_empty());
    send(&view.join("in"), "\x02");
    until_or("the bar is shown", screen, || screen().starts_with(" main "));
    assert!(screen().contains("C-b …") && !screen().contains("says"), "{:?}", screen());
}

#[test]
fn a_menu_with_no_room_for_its_count_shows_no_part_of_a_number() {
    // 26 keys and their heading on a terminal of 4 columns and 3 rows: one line is shown and 26
    // are not, and ` ▼ 26 more` has no room. A count cut to its first digit would say 2.
    let (outer, inner) = (Session::new("cnouter"), Session::new("cninner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b")).unwrap();
    for key in 'a'..='z' {
        fs::write(keys.join("C-b").join(key.to_string()), "#!/bin/sh\n# abcdefghij\n").unwrap();
    }
    inner.start("one", &["sleep", "600"]);
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let mut start = outer.fux();
    start.env("COLUMNS", "4").env("LINES", "3").args(["start", "view", "--"]).args(command);
    start.args([env!("CARGO_BIN_EXE_fux"), "view"]);
    let view = PathBuf::from(String::from_utf8(start.run().stdout).unwrap().trim());
    let screen = || read(&view.join("shown/screen"));
    until("the viewer draws", || !fact(&view, "modes").is_empty() || !screen().trim().is_empty());
    send(&view.join("in"), "\x02");
    until_or("the menu is shown", screen, || screen().contains('▼'));
    assert!(!screen().contains(|c: char| c.is_ascii_digit()), "{:?}", screen());
}

#[test]
fn a_menu_opens_whatever_kind_of_file_a_key_is() {
    // A FIFO where a key's script would be, and one where a menu's caption would be: nobody
    // writes to either, and the menu opens all the same.
    let (outer, inner) = (Session::new("fouter"), Session::new("finner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b/sub")).unwrap();
    assert!(Command::new("mkfifo").arg(keys.join("C-b/f")).arg(keys.join("C-b/sub/.about")).ended().success());
    fs::write(keys.join("C-b/v"), "#!/bin/sh\n# says\necho said\n").unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/v")).ended();
    inner.start("one", &["sh"]);
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &[env!("CARGO_BIN_EXE_fux"), "view"]].concat());
    let screen = || read(&view.join("shown/screen"));
    until("the pane is shown", || screen().contains('$') || screen().contains('#'));
    send(&view.join("in"), "\x02");
    until_or("the menu shows", screen, || screen().contains("v  says") && screen().contains("sub  …"));
    send(&view.join("in"), "v");
    until_or("the viewer still answers", screen, || screen().contains(" said"));
}

#[test]
fn a_workspace_whose_layout_is_a_fifo_is_drawn_and_left_all_the_same() {
    // A FIFO where the workspace's layout would be, which nobody writes to: the viewer does not
    // wait for it. It draws the pane that is there and still takes its keys, the one to leave too.
    let (outer, inner) = (Session::new("youter"), Session::new("yinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b")).unwrap();
    fs::write(keys.join("C-b/d"), "#!/bin/sh\necho quit\n").unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/d")).ended();
    inner.start("one", &["sh"]);
    assert!(Command::new("mkfifo").arg(inner.0.join("main/layout")).ended().success());
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &[env!("CARGO_BIN_EXE_fux"), "view"]].concat());
    let screen = || read(&view.join("shown/screen"));
    // (A few seconds, not `until`'s minute: a viewer that waits for the FIFO waits for good.)
    let soon = |what: &str, ready: &dyn Fn() -> bool| {
        let start = Instant::now();
        while !ready() {
            assert!(start.elapsed() < Duration::from_secs(8), "after 8 s the viewer had not {what}: {:?}", screen());
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    soon("shown the pane", &|| screen().contains('$') || screen().contains('#'));
    // (It read nothing there, and wrote the layout it shows in the FIFO's place.)
    assert_eq!(read(&inner.0.join("main/layout")), "* one focus=one\n");
    send(&view.join("in"), "\x02d");
    soon("left", &|| fact(&view, "state").starts_with("exited"));
}

#[test]
fn keys_typed_after_the_key_that_leaves_are_not_typed_into_the_pane() {
    // The key to leave and a letter after it, in one read. The key's script stops the viewer,
    // says `quit` and ends, and the viewer goes on a moment later: it finds its script ended and
    // `quit` said at once. It leaves, and the letter is nobody's.
    let (outer, inner) = (Session::new("qouter"), Session::new("qinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b")).unwrap();
    let leaves = "#!/bin/sh\nkill -STOP $PPID\n(sleep 0.3; kill -CONT $PPID) > /dev/null 2>&1 &\necho quit\n";
    fs::write(keys.join("C-b/d"), leaves).unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/d")).ended();
    let one = inner.start("one", &["cat"]);
    send(&one.join("in"), "shown\n");
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &[env!("CARGO_BIN_EXE_fux"), "view"]].concat());
    until("the pane is shown", || read(&view.join("shown/screen")).contains("shown\nshown"));
    send(&view.join("in"), "\x02dZ");
    until("the viewer has left", || fact(&view, "state").starts_with("exited"));
    send(&one.join("in"), "END\n");
    // (What was typed is echoed, and `cat` says it again a moment later: both are waited for.)
    until("the pane has what was typed", || read(&one.join("shown/screen")).contains("END\nEND\n"));
    assert_eq!(read(&one.join("shown/screen")).trim_end(), "shown\nshown\nEND\nEND");
}

#[test]
fn a_clipboard_set_while_a_workspace_with_no_pane_was_shown_is_not_sent_on_the_way_back() {
    // A pane that copies each word typed to it (OSC 52), shown by a viewer whose keys have no
    // `.empty`: a workspace with no pane stays empty.
    let (outer, inner) = (Session::new("bouter"), Session::new("binner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    fs::create_dir_all(inner.0.join("other")).unwrap();
    let copies = "stty -echo; while read -r word; do printf '\\033]52;c;%s\\007did %s\\n' \"$word\" \"$word\"; done";
    let pane = inner.start("a", &["sh", "-c", copies]);
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &[env!("CARGO_BIN_EXE_fux"), "view"]].concat());
    let (screen, sent) = (|| read(&view.join("shown/screen")), || read(&view.join("clipboard")));
    // What the pane copies while it has the keys reaches the viewer's own terminal.
    until("the viewer has looked at the pane", || fact(&pane, "size") == "80 24");
    std::thread::sleep(Duration::from_millis(300));
    send(&pane.join("in"), "b25l\n");
    until("the first copy is passed on", || sent() == "one");
    // What it copies while the viewer shows a workspace with no pane is old when the viewer
    // comes back: it is not sent.
    let ctl =
        fs::read_dir(&inner.0).unwrap().flatten().find(|file| file.file_name().to_string_lossy().starts_with(".view."));
    let ctl = ctl.unwrap().path();
    send(&ctl, "workspace other\n");
    until("the workspace with no pane is shown", || screen().contains("no panes in other"));
    send(&pane.join("in"), "dHdv\n");
    until("the pane copied again", || read(&pane.join("clipboard")) == "two");
    send(&ctl, "workspace main\n");
    until("the pane is shown again", || screen().contains("did dHdv"));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(sent(), "one", "a clipboard set while another workspace was shown was sent on the way back");
    send(&pane.join("in"), "dGhyZWU=\n");
    until("the next copy is passed on", || sent() == "three");
}

#[test]
fn a_fifo_where_a_named_pane_would_be_is_refused_at_once() {
    // A FIFO nobody reads or writes, under a pane's name: it is no directory to start a pane
    // in, and the start says so without waiting for its other end.
    let session = Session::new("fifoname");
    fs::create_dir_all(session.0.join("main")).unwrap();
    assert!(Command::new("mkfifo").arg(session.0.join("main/p")).ended().success());
    let mut start = session.fux().args(["start", "p", "--", "sleep", "600"]).spawn().unwrap();
    // (A few seconds, not `until`'s minute: a start that waits for the FIFO waits for good.)
    let begun = Instant::now();
    let status = loop {
        if let Some(status) = start.try_wait().unwrap() {
            break status;
        }
        if begun.elapsed() > Duration::from_secs(4) {
            start.kill().unwrap();
            start.wait().unwrap();
            panic!("after 4 s the start was still waiting for the FIFO");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!status.success());
}

#[test]
fn a_fifo_where_a_workspace_would_be_is_refused_at_once() {
    // A FIFO nobody reads or writes, under a workspace's name: it is no folder of panes, and
    // `fux layout` says so without waiting for its other end.
    let session = Session::new("fifoworkspace");
    fs::create_dir_all(&session.0).unwrap();
    assert!(Command::new("mkfifo").arg(session.0.join("main")).ended().success());
    let mut layout = session.fux().arg("layout").stdout(std::process::Stdio::null()).spawn().unwrap();
    let begun = Instant::now();
    let status = loop {
        if let Some(status) = layout.try_wait().unwrap() {
            break status;
        }
        if begun.elapsed() > Duration::from_secs(4) {
            layout.kill().unwrap();
            layout.wait().unwrap();
            panic!("after 4 s the layout was still waiting for the FIFO");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!status.success());
}

#[test]
fn a_link_where_a_file_is_written_anew_is_not_written_through() {
    // Links where the holder makes its files anew: where `state` and a picture's file are put
    // together, and under the picture's own name. Each leads to a file that is none of the pane's.
    // The holder writes none of them: what it writes is its own file, made anew.
    let session = Session::new("nolink");
    let draws =
        "trap '' HUP; echo r''eady; read x; printf '\\033Pq\"1;1;10;6#0;2;100;0;0#0!10~\\033\\\\drawn\\n'; sleep 600";
    let pane = session.start("p", &["sh", "-c", draws]);
    until("the program is there", || read(&pane.join("shown/screen")).contains("ready"));
    let kept = |name: &str| session.0.join(name);
    for (link, file) in
        [(".state.new", "state-kept"), ("pic-1.sixel", "picture-kept"), (".pic-1.sixel.new", "new-kept")]
    {
        fs::write(kept(file), "not the pane's\n").unwrap();
        std::os::unix::fs::symlink(kept(file), pane.join(link)).unwrap();
    }
    send(&pane.join("in"), "\n");
    until("the picture is drawn", || fact(&pane, "main/pictures").starts_with("pic-1.sixel "));
    send(&pane.join("ctl"), "kill\n");
    until("the program is hung up", || fact(&pane, "state") == "hungup");
    for file in ["state-kept", "picture-kept", "new-kept"] {
        assert_eq!(read(&kept(file)), "not the pane's\n", "{file} was written through a link");
    }
    let is_file = |name: &str| fs::symlink_metadata(pane.join(name)).is_ok_and(|file| file.is_file());
    assert!(is_file("state") && is_file("pic-1.sixel"), "a link was given the file's name");
    assert!(read(&pane.join("pic-1.sixel")).starts_with("\x1bPq\"1;1;10;6"));
    // A file that cannot be given its name (here a folder has it) does not stay under the name it
    // was put together under: the round fails, and nothing is left of it.
    let titled = session.start("t", &["sh", "-c", "read x; printf '\\033]0;a title\\007'; sleep 600"]);
    fs::create_dir(titled.join("title")).unwrap();
    fs::write(titled.join("title/kept"), "").unwrap();
    send(&titled.join("in"), "\n");
    until("the round has failed", || read(&titled.join("log")).contains("cannot write the pane's files"));
    let left: Vec<_> = fs::read_dir(&titled).unwrap().flatten().map(|file| file.file_name()).collect();
    assert!(!left.iter().any(|name| name.to_string_lossy().ends_with(".new")), "{left:?}");
    // The name a file is put together under starts with a dot, in whichever folder the file is: a
    // folder of that name stops the file being written, and one of the name without the dot does not.
    for (folder, stops) in [("main/screen.new", false), ("main/.screen.new", true)] {
        let pane =
            session.start(if stops { "stopped" } else { "written" }, &["sh", "-c", "read x; echo shown; sleep 600"]);
        fs::create_dir(pane.join(folder)).unwrap();
        send(&pane.join("in"), "\n");
        until("the round is over", || {
            read(&pane.join("main/screen")).contains("shown") != stops
                && read(&pane.join("log")).contains("cannot write the pane's files") == stops
        });
    }
    // Nor is the name of a layout being written one that a pane may not have.
    assert!(session.fux().args(["start", "layout.new", "--", "sleep", "600"]).run().status.success());
    assert!(session.fux().arg("layout").run().status.success() && session.0.join("main/layout.new/state").exists());
}

#[test]
fn rounds_of_writing_that_fail_do_not_leave_their_pictures_for_good() {
    // A program that draws a picture, erases it and draws the next, while the list of them cannot
    // be written (a directory where its new file would be made): a round that fails still removes
    // the files of pictures no screen has any more, so they do not grow in number while it lasts.
    let session = Session::new("picfail");
    let draws = "until [ -e \"$FUX_PANE/go\" ]; do sleep 0.05; done; i=0; while [ $i -lt 12 ]; do i=$((i + 1)); \
        printf '\\033[2J\\033[H\\033Pq\"1;1;10;6#0;2;100;0;0#0!10~\\033\\\\'; sleep 0.1; done; : > \"$FUX_PANE/drawn\"; sleep 600";
    let pane = session.start("p", &["sh", "-c", draws]);
    fs::create_dir(pane.join("main/.pictures.new")).unwrap();
    fs::write(pane.join("go"), "").unwrap();
    let files = || {
        fs::read_dir(&pane)
            .unwrap()
            .flatten()
            .filter(|file| file.file_name().as_encoded_bytes().starts_with(b"pic-"))
            .count()
    };
    until("the pictures are drawn in rounds that failed", || {
        pane.join("drawn").exists() && read(&pane.join("log")).matches("cannot write the pane's files").count() >= 6
    });
    // (A few seconds for the last round, not `until`'s minute: files that stay, stay for good.)
    let begun = Instant::now();
    while files() > 2 {
        assert!(begun.elapsed() < Duration::from_secs(3), "{} picture files after 12 pictures, one at a time", files());
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_line_of_ctl_is_measured_in_the_bytes_that_were_written() {
    // 507 bytes that are no text (each would be three as text) are a line under the bound of
    // 1,280: the notice is taken, whatever its bytes are shown as.
    let session = Session::new("ctlbytes");
    let pane = session.start("p", &["sleep", "600"]);
    send(&pane.join("ctl"), [&b"notify "[..], &[0xff; 500], b"\n"].concat());
    until("the line is taken or refused", || !fact(&pane, "notice").is_empty() || !read(&pane.join("log")).is_empty());
    assert!(fact(&pane, "notice").starts_with("1 \u{fffd}"), "{}", read(&pane.join("log")));
    // A line over the bound is dropped whole, however it arrives, and the next one is taken.
    send(&pane.join("ctl"), [&[b'x'; 3000][..], b"\nnotify next\n"].concat());
    until("the next line is taken", || fact(&pane, "notice") == "2 next");
    assert_eq!(read(&pane.join("log")), "ctl: a line over 1280 bytes was dropped\n");
}

#[test]
fn the_windows_focus_is_not_told_to_a_program_that_asks_no_more() {
    // A program that asked to be told of the focus (mode 1004) and was told it has the keys. It
    // stops asking while the viewer is stopped, and the terminal says in that time that its window
    // lost the focus: the viewer, going on, reads that before it draws, and tells the program nothing.
    let (outer, inner) = (Session::new("focusout"), Session::new("focusin"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    let asks_then_stops = "stty raw -echo; printf '\\033[?1004h'; until [ -e \"$FUX_PANE/go\" ]; do sleep 0.05; done; printf '\\033[?1004l'; exec cat -vt";
    let one = inner.start("one", &["sh", "-c", asks_then_stops]);
    until("it asks", || fact(&one, "modes") == "focus");
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &[env!("CARGO_BIN_EXE_fux"), "view"]].concat());
    until("the viewer asks its terminal for the focus", || fact(&view, "modes").contains("focus"));
    std::thread::sleep(Duration::from_millis(500));
    let pid = view_fifo(&inner.0).extension().unwrap().to_string_lossy().into_owned();
    assert!(Command::new("kill").args(["-STOP", &pid]).ended().success());
    fs::write(one.join("go"), "").unwrap();
    until("it asks no more", || fact(&one, "modes").is_empty());
    send(&view.join("in"), "\x1b[O");
    std::thread::sleep(Duration::from_millis(300));
    assert!(Command::new("kill").args(["-CONT", &pid]).ended().success());
    std::thread::sleep(Duration::from_millis(300));
    send(&one.join("in"), "END");
    until("the pane has what was typed", || read(&one.join("shown/screen")).contains("END"));
    assert_eq!(read(&one.join("shown/screen")).trim_end(), "^[[IEND");
}

/// The FIFO of the one viewer of a session (`.view.PID`).
fn view_fifo(session: &Path) -> PathBuf {
    let files = fs::read_dir(session).unwrap().flatten().map(|entry| entry.path());
    files.into_iter().find(|path| path.file_name().unwrap().to_string_lossy().starts_with(".view.")).unwrap()
}

#[test]
fn a_name_that_is_a_number_of_no_line_is_chosen_without_a_word_of_complaint() {
    // With no finder, the list is numbered and a number or a name is asked for. `0` is no line's
    // number: it is the name of the second line here, and that is all that is said.
    let folder = Session::new("pickzero");
    let (bin, pane, list) = (folder.0.join("bin"), folder.0.join("pane"), folder.0.join("list"));
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&pane).unwrap();
    let links = "for tool in mv awk head; do ln -s \"$(command -v $tool)\" \"$0/$tool\"; done";
    assert!(Command::new("sh").arg("-c").arg(links).arg(&bin).ended().success());
    fs::write(&list, "one\tthe first\n0\tnought\n").unwrap();
    fs::write(folder.0.join("typed"), "0\n").unwrap();
    let mut pick = Command::new("/bin/sh");
    pick.arg(concat!(env!("CARGO_MANIFEST_DIR"), "/keys/.pick")).arg(&list).args(["echo", "chose"]);
    pick.env("PATH", &bin).env("FUX_PANE", &pane);
    let out = pick.run_from(Some(fs::File::open(folder.0.join("typed")).unwrap()));
    let (said, wrong) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(said.ends_with("chose 0\n") && wrong.is_empty(), "{said:?} {wrong:?}");
}

#[test]
fn a_shell_named_by_nothing_is_no_shell_named() {
    // `SHELL=` (set, and empty) is as if it were not set: `/bin/sh`. So is `FUX_WS=`: `main`.
    let session = Session::new("noshell");
    let out = session.fux().env("SHELL", "").env("FUX_WS", "").args(["start", "e"]).run();
    assert!(out.status.success(), "start with SHELL and FUX_WS empty: {}", String::from_utf8_lossy(&out.stderr));
    assert!(read(&session.0.join("main/e/status")).ends_with("command /bin/sh\n"));
}

#[test]
fn a_pane_started_again_that_never_came_up_leaves_no_state_of_the_run_before() {
    // A pane whose program ended with 37 is started again, and the new holder cannot write its
    // first screen (`.marks.new` is a directory). Its `status` has the new program's number by
    // then: the `state` beside it is not the old program's, so `fux wait` has no end to tell of.
    let session = Session::new("wrongrun");
    let pane = session.start("p", &["sh", "-c", "exit 37"]);
    let wait = || session.fux().arg("wait").arg(&pane).run();
    assert!(String::from_utf8_lossy(&wait().stdout).ends_with("state exited 37\n"));
    fs::create_dir(pane.join(".marks.new")).unwrap();
    let again = session.fux().args(["start", "p", "--", "sleep", "600"]).run();
    assert!(String::from_utf8_lossy(&again.stderr).contains("cannot write the screen"), "{again:?}");
    let (waited, state) = (wait(), read(&pane.join("state")));
    assert!(!waited.status.success() && waited.stdout.is_empty() && state.is_empty(), "{waited:?} {state:?}");
}

#[test]
fn a_program_that_writes_its_cwd_as_soon_as_it_runs_keeps_it() {
    // The program writes where it is (`pwd > "$FUX_PANE/cwd"`, as the README says a shell may)
    // before its holder has come up: here the holder waits meanwhile, before its first screen, at a
    // `history` that is a FIFO nobody reads yet. What an earlier holder left went before the program started.
    let session = Session::new("owncwd");
    let pane = session.0.join("main/p");
    fs::create_dir_all(&pane).unwrap();
    assert!(Command::new("mkfifo").arg(pane.join("history")).ended().success());
    fs::write(pane.join("cwd"), "/left/by/an/earlier/holder\n").unwrap();
    let says = "pwd > \"$FUX_PANE/cwd.mine\" && mv \"$FUX_PANE/cwd.mine\" \"$FUX_PANE/cwd\"; exec sleep 600";
    let mut start = session.fux().args(["start", "p", "--", "sh", "-c", says]).stdout(Stdio::null()).spawn().unwrap();
    let here = format!("{}\n", std::env::current_dir().unwrap().display());
    until("the program has said where it is", || read(&pane.join("cwd")) == here);
    until("its holder has said what it runs", || read(&pane.join("status")).contains("command sh -c pwd"));
    assert!(start.try_wait().unwrap().is_none(), "the holder was up before the test let it");
    // (Read from now on, without waiting: the holder goes on to its first screen.)
    let history = fuxix::open(pane.join("history"), false).unwrap();
    assert!(wait(&mut start, "the start").success());
    assert_eq!((fact(&pane, "state").as_str(), read(&pane.join("cwd"))), ("running", here));
    drop(history);
}

#[test]
fn a_line_of_ctl_that_has_not_ended_writes_no_screen() {
    // The first half of a line is no command yet: who watches `generation` of a quiet pane is
    // not woken for it. The line's end is what has the notice written, and `generation` after it.
    let session = Session::new("halfline");
    let pane = session.start("p", &["sleep", "600"]);
    let first = stamp_of(&pane, "generation");
    send(&pane.join("ctl"), "notify the first");
    std::thread::sleep(Duration::from_millis(300));
    let now = (stamp_of(&pane, "generation"), fact(&pane, "notice"));
    assert_eq!(now, (first, String::new()), "a round was written for half a line");
    send(&pane.join("ctl"), " half\n");
    until("the whole line is taken", || fact(&pane, "notice") == "1 the first half");
    until("the round has ended", || stamp_of(&pane, "generation") != first && fact(&pane, "generation") == "4");
}

#[test]
fn a_key_script_is_told_the_terminals_size_before_the_first_frame_and_none_where_there_is_none() {
    // A key typed before the viewer has drawn anything: it is waiting on the terminal when the viewer
    // starts (`dd` took the byte before it). Its script is told the terminal's size, not 0.
    let (outer, inner) = (Session::new("souter"), Session::new("sinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    fs::write(keys.join("x"), "#!/bin/sh\necho \"$COLUMNS $LINES\" > \"$FUX_DIR/told\"\n").unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("x")).ended();
    inner.start("one", &["sleep", "600"]);
    let views = |session: &Session, keys: &Path, first: &str| {
        let (dir, keys, fux) = (session.0.display(), keys.display(), env!("CARGO_BIN_EXE_fux"));
        format!("{first}; SHELL=/bin/sh FUX_DIR='{dir}' FUX_KEYS='{keys}' exec '{fux}' view")
    };
    let view =
        outer.start("view", &["sh", "-c", &views(&inner, &keys, "stty raw -echo; dd bs=1 count=1 >/dev/null 2>&1")]);
    send(&view.join("in"), "gx");
    until("the key's script ran", || read(&inner.0.join("told")).ends_with('\n'));
    assert_eq!(read(&inner.0.join("told")), "80 24\n");
    // A terminal that says it has no rows (a pty whose size nobody set) has told no size: the workspace's
    // first shell is started all the same, at 80 by 24, as `fux start` has it when it is told none.
    let (outer, inner) = (Session::new("tsouter"), Session::new("tsinner"));
    let view = outer.start("view", &["sh", "-c", &views(&inner, &inner.0.join(".keys"), "stty rows 0")]);
    let first = inner.0.join("main/1");
    let log = || format!(": its log says {:?}, the viewer's {:?}", read(&first.join("log")), read(&view.join("log")));
    until_or("the first pane was started", log, || {
        fact(&first, "state") == "running" || !read(&first.join("log")).is_empty()
    });
    assert_eq!((fact(&first, "state").as_str(), fact(&first, "size").as_str()), ("running", "80 24"), "{}", log());
}

#[test]
fn what_is_typed_right_after_the_new_workspace_key_goes_to_its_shell() {
    // The new workspace's shell runs by the time the key's script tells the viewer of the workspace:
    // the viewer keeps what is typed after a key until its script ends, and it is then that shell's.
    let (outer, inner) = (Session::new("nwouter"), Session::new("nwinner"));
    let keys = inner.0.join(".keys");
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let viewer = ["env", "SHELL=/bin/sh", env!("CARGO_BIN_EXE_fux"), "view"];
    let view = outer.start("view", &[&command[..], &viewer[..]].concat());
    until("the first workspace has its shell", || fact(&inner.0.join("main/1"), "state") == "running");
    // (The script by itself, as the viewer runs it: `fux` is the one beside the viewer.)
    let mut path = std::ffi::OsString::from(Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap());
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap());
    let mut script = Command::new(keys.join("C-b/w/n"));
    script.env("FUX_DIR", &inner.0).env("FUX_KEYS", &keys).env("FUX_WS", "main").env("PATH", path);
    let said = script.env("FUX_PANE", inner.0.join("main/1")).env("SHELL", "/bin/sh").run();
    let state = fact(&inner.0.join("w1/1"), "state");
    assert_eq!((String::from_utf8_lossy(&said.stdout).as_ref(), state.as_str()), ("workspace w1\n", "running"));
    // And the key with a letter after it in one write: the letter is typed to the new shell.
    send(&view.join("in"), "\x02wnZ");
    let shell = inner.0.join("w2/1");
    let seen = || {
        format!(
            ": the new shell shows {:?}, the viewer {:?}",
            read(&shell.join("shown/screen")),
            read(&view.join("shown/screen"))
        )
    };
    until_or("the letter is typed to the new shell", seen, || read(&shell.join("shown/screen")).contains('Z'));
}

#[test]
fn a_picture_below_a_shortened_alternate_screen_is_not_listed_until_its_row_is_back() {
    // A picture on row 3 of the alternate screen, which is then two rows high: `pictures` has a line
    // for each picture that has a row on the screen, and this one has none until the screen is higher again.
    let session = Session::new("altpic");
    let draws = "printf '\\033[?1049h\\033[4;1H\\033Pq\"1;1;10;20#0;2;100;0;0#0!10~-!10~-!10~\\033\\\\'; sleep 600";
    let pane = session.start("p", &["sh", "-c", draws]);
    until("the picture is listed", || fact(&pane, "shown/pictures").starts_with("pic-1.sixel 3 "));
    let listed = fact(&pane, "shown/pictures");
    send(&pane.join("ctl"), "resize 10 2\n");
    until("the screen is two rows", || fact(&pane, "size") == "10 2");
    assert_eq!(fact(&pane, "shown/pictures"), "");
    send(&pane.join("ctl"), "resize 80 24\n");
    until("the picture is listed again", || fact(&pane, "shown/pictures") == listed);
}

#[test]
fn a_restore_that_failed_keeps_the_text_and_a_pane_started_again_is_told_the_keys() {
    let session = Session::new("unrestored");
    let keys = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/keys"));
    let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
    // Two ended panes: one whose holder cannot write its first screen when it is started again (a
    // directory where it puts `marks` together), and one that prints where the keys are.
    let lost = session.start("w/lost", &["seq", "1", "60"]);
    let told = session.start("w/told", &["printenv", "FUX_KEYS"]);
    for pane in [&lost, &told] {
        assert!(session.fux().arg("wait").arg(pane).ended().success());
    }
    assert!(read(&lost.join("history")).contains("\n17\n") && read(&told.join("shown/screen")).trim().is_empty());
    fs::create_dir(lost.join(".marks.new")).unwrap();
    // (Run as a login runs it: by the script's path, with no FUX_KEYS.)
    // (A CDPATH that leads to the script's folder makes `cd` print where it went: the keys are still one path.)
    let mut restore = Command::new("sh");
    restore.arg("keys/.restore").env("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap()));
    restore.current_dir(env!("CARGO_MANIFEST_DIR")).env("CDPATH", env!("CARGO_MANIFEST_DIR"));
    restore.env("FUX_DIR", &session.0).env_remove("FUX_KEYS").env_remove("FUX_PANE").env_remove("FUX_WS");
    let restored = restore.run();
    assert_eq!(String::from_utf8_lossy(&restored.stdout), "started w/told again\n");
    until_or(
        "the pane started again says where the keys are",
        || read(&told.join("shown/screen")),
        || read(&told.join("shown/screen")).replace('\n', "") == keys.display().to_string(),
    );
    // The start that failed had begun the pane's files anew: what it had printed is in `before`.
    assert!(!read(&lost.join("history")).contains("\n17\n"), "the start did not get as far as the test means");
    assert!(read(&lost.join("before")).contains("\n17\n"), "before: {:?}", read(&lost.join("before")));
}

#[test]
fn a_paste_ends_marked_as_it_began_whatever_the_program_asks_by_then() {
    // A program that asks for pastes marked, reads the start mark and one letter (seven bytes), asks no
    // more and reads on: the paste began marked, and its end mark follows the rest of it.
    let (outer, inner) = (Session::new("p2outer"), Session::new("p2inner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    let reads =
        "stty raw -echo; printf '\\033[?2004h'; dd bs=1 count=7 >/dev/null 2>&1; printf '\\033[?2004l'; exec cat -v";
    let pane = inner.start("a", &["sh", "-c", reads]);
    until("the program asks for pastes marked", || fact(&pane, "modes").contains("paste"));
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &[env!("CARGO_BIN_EXE_fux"), "view"]].concat());
    until("the viewer has looked at the pane", || fact(&pane, "size") == "80 24");
    // (A terminal says what it is like before anything is pasted into it: its answers are not part of a paste.)
    until("the terminal has said what it is like", || inner.0.join("keyboard").exists());
    send(&view.join("in"), "\x1b[200~a");
    until("the program asks for them no more", || !fact(&pane, "modes").contains("paste"));
    send(&view.join("in"), "b\x1b[201~Z");
    until("the rest arrives", || read(&pane.join("shown/screen")).contains('Z'));
    assert_eq!(
        read(&pane.join("shown/screen")).trim_end(),
        "b^[[201~Z",
        "a paste that began marked did not end marked"
    );
}

#[test]
fn a_key_script_is_told_the_size_the_terminal_has_when_the_key_is_typed() {
    // The terminal is made larger and a key typed at once, before the viewer has looked at its size
    // again (the holder reads `ctl` before `in`): the key's script is told the size the terminal has now.
    let (outer, inner) = (Session::new("szouter"), Session::new("szinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    let tells = "#!/bin/sh\necho \"$COLUMNS $LINES $FUX_ROW $FUX_COL\" > \"$FUX_DIR/told\"\n";
    fs::write(keys.join("x"), tells).unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("x")).ended();
    let pane = inner.start("one", &["sleep", "600"]);
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &[env!("CARGO_BIN_EXE_fux"), "view"]].concat());
    until("the viewer has looked at the pane", || fact(&pane, "size") == "80 24");
    // (At rest the viewer looks at its files, and at its terminal's size, every 50 ms.)
    std::thread::sleep(Duration::from_millis(3200));
    send(&view.join("ctl"), "resize 83 27\n");
    send(&view.join("in"), "x");
    until("the key's script ran", || read(&inner.0.join("told")).ends_with('\n'));
    assert_eq!(read(&inner.0.join("told")), "83 27 41x27 83x13\n");
}

#[test]
fn what_is_typed_before_a_workspace_with_no_pane_is_first_drawn_goes_to_its_shell() {
    // A letter that waits on the terminal when the viewer starts in a workspace with no pane (`dd`
    // took the byte before it): it is typed to the shell that workspace gets, not to nobody.
    let (outer, inner) = (Session::new("fzouter"), Session::new("fzinner"));
    let (dir, keys, fux) = (inner.0.display(), inner.0.join(".keys"), env!("CARGO_BIN_EXE_fux"));
    let views = format!(
        "stty raw -echo; dd bs=1 count=1 >/dev/null 2>&1; SHELL=/bin/sh FUX_DIR='{dir}' FUX_KEYS='{}' exec '{fux}' view",
        keys.display()
    );
    let view = outer.start("view", &["sh", "-c", &views]);
    send(&view.join("in"), "gZ");
    let shell = inner.0.join("main/1");
    until("the workspace has its shell", || fact(&shell, "state") == "running");
    let seen = || {
        format!(
            ": the shell shows {:?}, the viewer {:?}",
            read(&shell.join("shown/screen")),
            read(&view.join("shown/screen"))
        )
    };
    until_or("the letter is typed to the shell", seen, || read(&shell.join("shown/screen")).contains('Z'));
}

#[test]
fn what_is_typed_after_a_key_that_shows_a_workspace_with_no_pane_goes_to_its_shell() {
    // The key's script names the workspace and is still running when `.empty` is started for it,
    // and has ended before `.empty` has made the pane: the letter typed after the key waits for
    // `.empty`, whose workspace it is typed in, and not only for the script that chose it.
    let (outer, inner) = (Session::new("wlouter"), Session::new("wlinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b/w")).unwrap();
    fs::write(keys.join("C-b/w/l"), "#!/bin/sh\necho workspace other\nsleep 0.2\n").unwrap();
    let empty = "#!/bin/sh\nsleep 1\np=$(fux start \"$FUX_WS/1\" -- cat) && fux layout pane 1\n";
    fs::write(keys.join(".empty"), empty).unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/w/l")).arg(keys.join(".empty")).ended();
    inner.start("one", &["sh", "-c", "echo READY; exec cat"]);
    fs::create_dir_all(inner.0.join("other")).unwrap();
    let viewer = [env!("CARGO_BIN_EXE_fux"), "view"];
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &viewer[..]].concat());
    until("the viewer shows the first workspace", || read(&view.join("shown/screen")).contains("READY"));
    send(&view.join("in"), "\x02wlZ");
    let shell = inner.0.join("other/1");
    until("the workspace has its shell", || fact(&shell, "state") == "running");
    let seen = || {
        format!(
            ": the shell shows {:?}, the viewer {:?}",
            read(&shell.join("shown/screen")),
            read(&view.join("shown/screen"))
        )
    };
    until_or("the letter is typed to the shell", seen, || read(&shell.join("shown/screen")).contains('Z'));
}

#[test]
fn what_is_typed_after_a_key_whose_script_makes_the_pane_waits_for_it_though_empty_has_ended() {
    // The key's script names a workspace with no pane and starts its pane a second later; `.empty`,
    // started for that workspace meanwhile, ends at once: the letter typed after the key waits for
    // every script started before it, the key's own among them, and not only for the last.
    let (outer, inner) = (Session::new("wkouter"), Session::new("wkinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b/w")).unwrap();
    let key = "#!/bin/sh\necho workspace other\nsleep 1\nexec fux start other/1 -- cat >/dev/null\n";
    fs::write(keys.join("C-b/w/l"), key).unwrap();
    fs::write(keys.join(".empty"), "#!/bin/sh\nexit 0\n").unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/w/l")).arg(keys.join(".empty")).ended();
    inner.start("one", &["sh", "-c", "echo READY; exec cat"]);
    fs::create_dir_all(inner.0.join("other")).unwrap();
    let viewer = [env!("CARGO_BIN_EXE_fux"), "view"];
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &viewer[..]].concat());
    until("the viewer shows the first workspace", || read(&view.join("shown/screen")).contains("READY"));
    send(&view.join("in"), "\x02wlZ");
    let shell = inner.0.join("other/1");
    until("the workspace has its pane", || fact(&shell, "state") == "running");
    until("the viewer shows it", || read(&inner.0.join("other/layout")).contains("focus=1"));
    // (A letter the viewer had dropped before the pane was there never comes: a second is given.)
    std::thread::sleep(Duration::from_secs(1));
    assert!(read(&shell.join("shown/screen")).contains('Z'), "the pane shows {:?}", read(&shell.join("shown/screen")));
}

#[test]
fn a_restore_that_cannot_copy_what_a_pane_printed_does_not_start_it_again() {
    // `.restore` starts a pane in place, which begins its files anew: what it had printed is copied
    // first, and a pane whose text cannot be copied (a `screen` that cannot be read) is left as it is.
    let session = Session::new("unreadable");
    let keys = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/keys"));
    let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
    let pane = session.start("w/kept", &["echo", "KEEP"]);
    assert!(session.fux().arg("wait").arg(&pane).ended().success());
    let mode = |mode: &str| assert!(Command::new("chmod").arg(mode).arg(pane.join("shown/screen")).ended().success());
    mode("000");
    if fs::read(pane.join("shown/screen")).is_ok() {
        return; // (Whoever may read every file, root, has no file that cannot be copied.)
    }
    let mut restore = Command::new("sh");
    restore.arg(keys.join(".restore")).env("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap()));
    restore.env("FUX_DIR", &session.0).env_remove("FUX_KEYS").env_remove("FUX_PANE").env_remove("FUX_WS");
    let restored = restore.run();
    mode("600");
    let (said, kept) = (String::from_utf8_lossy(&restored.stdout).into_owned(), read(&pane.join("shown/screen")));
    let before = fs::read_to_string(pane.join("before")).ok();
    assert!(said.is_empty() && kept.contains("KEEP") && before.is_none(), "{said:?} {kept:?} {before:?}");
    assert!(String::from_utf8_lossy(&restored.stderr).contains("screen"), "cat says which file it could not read");
}

#[test]
fn a_restore_leaves_what_a_pane_that_is_alive_has_kept_as_it_is() {
    // A pane that runs, with a `before` of its own and a `log` of a later time than the copy's (a holder
    // that logged a line since has one too): the start is refused, and `before` is as it was.
    let session = Session::new("leftalone");
    let keys = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/keys"));
    let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
    let pane = session.start("w/alive", &["sleep", "600"]);
    fs::write(pane.join("before"), "saved from earlier\n").unwrap();
    assert!(Command::new("touch").args(["-t", "203701010000"]).arg(pane.join("log")).ended().success());
    let runs = read(&pane.join("status"));
    let mut restore = Command::new("sh");
    restore.arg(keys.join(".restore")).env("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap()));
    restore.env("FUX_DIR", &session.0).env_remove("FUX_KEYS").env_remove("FUX_PANE").env_remove("FUX_WS");
    assert!(restore.run().stdout.is_empty());
    assert_eq!(read(&pane.join("before")), "saved from earlier\n");
    assert_eq!(read(&pane.join("status")), runs);
    assert!(
        session.fux().arg("start").arg("w/alive").run().status.code().is_some_and(|code| code != 0),
        "it still runs"
    );
}

#[test]
fn a_restore_that_cannot_make_the_copy_of_what_a_pane_printed_does_not_start_it_again() {
    // A directory of the copy's name (which ends with the number of the shell that runs the script):
    // the copy cannot be opened for writing, so there is none, and the pane is left as it is.
    let session = Session::new("nocopy");
    let keys = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/keys"));
    let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
    let pane = session.start("w/kept", &["seq", "1", "60"]);
    assert!(session.fux().arg("wait").arg(&pane).ended().success());
    let was = read(&pane.join("history"));
    let mut restore = Command::new("sh");
    restore.args(["-c", r#"mkdir "$1/before.$$" && exec sh "$0""#]).arg(keys.join(".restore")).arg(&pane);
    restore.env("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap()));
    restore.env("FUX_DIR", &session.0).env_remove("FUX_KEYS").env_remove("FUX_PANE").env_remove("FUX_WS");
    let said = String::from_utf8_lossy(&restore.run().stdout).into_owned();
    let kept = read(&pane.join("history"));
    assert!(said.is_empty() && kept == was && kept.contains("\n17\n"), "{said:?} {kept:?}");
    assert!(!pane.join("before").exists());
}

#[test]
fn the_focus_goes_to_the_pane_that_is_beside_it_in_the_terminal_a_key_is_pressed_in() {
    // Of `row(col(a b:99) col(c d:9999))` in 80 columns and 24 rows, a and c have the first row
    // each: c is the pane to the right of a, and a key's script is told the terminal's size.
    let session = Session::new("diag");
    ["a", "b", "c", "d"].iter().for_each(|pane| fs::create_dir_all(session.0.join("main").join(pane)).unwrap());
    lay(&session.0, "* row(col(a b:99) col(c d:9999)) focus=a\n");
    let mut edit = session.fux();
    edit.env("FUX_WS", "main").env("COLUMNS", "80").env("LINES", "24").args(["layout", "focus", "right"]);
    assert!(edit.ended().success());
    assert_eq!(read(&session.0.join("main/layout")), "* row(col(a b:99) col(c d:9999)) focus=c\n");
}

#[test]
fn a_new_workspace_key_that_another_got_ahead_of_takes_the_next_name() {
    // Two viewers' `C-b w n` at once: the name one found free is the other's by the time it makes
    // the folder. The folder made is the claim, so it takes the next. (The other key is a `mkdir`
    // put first on PATH here, which makes the folder asked for just before, once.)
    let session = Session::new("nwrace");
    let keys = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/keys"));
    let shim = session.0.join(".bin");
    fs::create_dir_all(&shim).unwrap();
    let real = String::from_utf8(Command::new("sh").args(["-c", "command -v mkdir"]).run().stdout).unwrap();
    let ahead =
        format!("#!/bin/sh\n[ -e \"$0.done\" ] || {{ : > \"$0.done\"; {0} \"$1\"; }}\nexec {0} \"$@\"\n", real.trim());
    fs::write(shim.join("mkdir"), ahead).unwrap();
    assert!(Command::new("chmod").arg("+x").arg(shim.join("mkdir")).ended().success());
    let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
    let mut script = Command::new(keys.join("C-b/w/n"));
    script.env("PATH", format!("{}:{bin}:{}", shim.display(), std::env::var("PATH").unwrap()));
    script.env("FUX_DIR", &session.0).env("FUX_KEYS", &keys).env("FUX_WS", "main").env("FUX_PANE", "");
    let said = script.env("SHELL", "/bin/sh").env_remove("COLUMNS").env_remove("LINES").run();
    let (out, wrong) = (String::from_utf8_lossy(&said.stdout), String::from_utf8_lossy(&said.stderr));
    assert_eq!(
        (out.as_ref(), fact(&session.0.join("w2/1"), "state").as_str()),
        ("workspace w2\n", "running"),
        "{wrong}"
    );
    assert!(!session.0.join("w1/1").exists(), "the other key's workspace is the other's");
}

#[test]
fn the_blank_lines_a_command_ended_with_are_in_its_output() {
    // `A` and two blank lines, and then the end mark: the screen's text is taken without the blank
    // rows at its end, and the lines up to the end mark are the output's all the same.
    let session = Session::new("blanks");
    let keys = concat!(env!("CARGO_MANIFEST_DIR"), "/keys");
    let shell = "printf '\\033]133;C\\007A\\n\\n\\n\\033]133;D;0\\007'; sleep 600";
    let pane = session.start("sh", &["sh", "-c", shell]);
    // (The holder writes `marks` before `screen`: the key is pressed when both are there.)
    until("the marks and the output are there", || {
        read(&pane.join("marks")).lines().count() == 2 && read(&pane.join("shown/screen")).starts_with('A')
    });
    let out = session.0.join("out");
    let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
    let mut script = Command::new(Path::new(keys).join("C-b/S-c"));
    script.env("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap())).env("OUT", &out);
    script.env("FUX_EDIT", "cat \"$1\" > \"$OUT.new\" && mv \"$OUT.new\" \"$OUT\"");
    script.env("FUX_DIR", &session.0).env("FUX_KEYS", keys).env("FUX_WS", "main").env("FUX_PANE", &pane);
    let said = script.env_remove("COLUMNS").env_remove("LINES").run();
    assert_eq!(String::from_utf8_lossy(&said.stdout), "", "marks: {:?}", read(&pane.join("marks")));
    until("the editor was given the output", || out.exists());
    assert_eq!(read(&out), "A\n\n\n");
}

#[test]
fn a_notice_that_cannot_be_opened_is_old_when_it_can_be() {
    // What is there at the first look only marks its tab, also when it cannot be opened then: made
    // readable later (a `chmod` alone), it is the same old notice and is not rung.
    let (outer, inner) = (Session::new("shutout"), Session::new("shutin"));
    let keys = concat!(env!("CARGO_MANIFEST_DIR"), "/keys");
    let (_p1, p2) = (inner.start("p1", &["sh", "-c", "echo SHOWN; sleep 600"]), inner.start("p2", &["sleep", "600"]));
    lay(&inner.0, "* p1 focus=p1\n  p2 focus=p2\n");
    send(&p2.join("ctl"), "notify old\n");
    until("the notice is written", || read(&p2.join("notice")) == "1 old\n");
    let mode = |mode: &str| assert!(Command::new("chmod").arg(mode).arg(p2.join("notice")).ended().success());
    mode("000");
    if fs::read(p2.join("notice")).is_ok() {
        return; // (Whoever may read every file, root, has no notice that cannot be opened.)
    }
    let path = format!("PATH={}", std::env::var("PATH").unwrap());
    let command = ["env", &path, &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={keys}")];
    let view = outer.start("view", &[&command[..], &[env!("CARGO_BIN_EXE_fux"), "view"]].concat());
    until("the viewer shows the first pane", || read(&view.join("shown/screen")).contains("SHOWN"));
    std::thread::sleep(Duration::from_millis(600));
    send(&view.join("in"), "\x02");
    until("the tabs are shown", || read(&view.join("shown/screen")).contains(" p2"));
    let marked = read(&view.join("shown/screen")).contains(" p2! ");
    mode("600");
    std::thread::sleep(Duration::from_millis(600));
    send(&p2.join("ctl"), "notify new\n");
    until("the viewer passed the new one on", || read(&view.join("notice")).ends_with(" p2: new\n"));
    assert_eq!((marked, read(&view.join("notice")).as_str()), (true, "1 p2: new\n"), "the old notice was rung");
}

#[test]
fn a_text_or_a_list_that_cannot_be_moved_into_its_pane_is_removed() {
    // The editor's text and the picker's list are moved into their pane's folder and go with it. A
    // pane that is gone by then (closed under them) takes nothing: the file is removed, not left in the session.
    let session = Session::new("unmoved");
    fs::create_dir_all(&session.0).unwrap();
    let (text, list, enter) = (session.0.join(".text.case"), session.0.join(".pick.case"), session.0.join("enter"));
    fs::write(&text, "private transcript\n").unwrap();
    fs::write(&list, "a\tpane a\n").unwrap();
    fs::write(&enter, "\n").unwrap();
    for (script, args) in
        [(".edit", vec!["--in", text.to_str().unwrap(), "1"]), (".pick", vec![list.to_str().unwrap(), "echo"])]
    {
        let mut helper = Command::new("sh");
        helper.arg(format!("{}/keys/{script}", env!("CARGO_MANIFEST_DIR"))).args(args);
        helper.env("FUX_PANE", session.0.join("gone"));
        let out = helper.run_from(Some(fs::File::open(&enter).unwrap()));
        assert!(String::from_utf8_lossy(&out.stderr).contains("gone"), "{script}: mv says why");
    }
    assert!(!text.exists() && !list.exists(), "left behind: text {} list {}", text.exists(), list.exists());
}

#[test]
fn a_start_from_a_directory_that_is_gone_claims_no_name() {
    // What can refuse a start before a folder is made refuses it before: no name is taken for nothing.
    let session = Session::new("nowhere");
    let gone = session.0.join("gone");
    fs::create_dir_all(&gone).unwrap();
    let mut start = Command::new("sh");
    start.args(["-c", r#"rmdir "$PWD" && exec "$0" start w/ -- true"#, env!("CARGO_BIN_EXE_fux")]);
    let out = start.env("FUX_DIR", &session.0).env_remove("FUX_PANE").env_remove("FUX_WS").current_dir(&gone).run();
    assert!(!out.status.success() && !session.0.join("w/1").exists(), "{out:?}");
}

#[test]
fn two_files_of_a_pane_that_floods_are_read_as_of_one_moment() {
    // Line n of the pane's text (from 0) is the number n + 1: the screen's first row reads one more
    // than the lines `main/lines` says are before it, if the two files are of one round of writing.
    // They are when `generation` is the same even number before and after they are read.
    let session = Session::new("moment");
    let pane = session.start("seq", &["sh", "-c", "seq 1 100000000"]);
    let number = |name: &str| read(&pane.join(name)).lines().next().and_then(|line| line.parse::<u64>().ok());
    until("the flood scrolls", || number("main/lines") > Some(1000));
    let (start, mut kept, mut apart) = (Instant::now(), 0, 0);
    while start.elapsed() < Duration::from_secs(2) && apart == 0 {
        let (before, top, first) = (number("generation"), number("main/lines"), number("main/screen"));
        if before.is_some_and(|n| n % 2 == 0) && number("generation") == before {
            kept += 1;
            apart += u32::from(top.map(|top| top + 1) != first);
        }
    }
    assert!(kept > 100 && apart == 0, "of {kept} reads kept, {apart} were of two rounds of writing");
}

#[test]
fn a_mark_in_a_line_that_never_ends_is_found_in_the_text_after_history_was_put_aside() {
    // One line of 1,200,000 characters in rows of one, each row with its looks (16 bytes of
    // `history.styled` for a character): that file is past twice its size twice, so the line's start
    // is in no file any more. Then a command's output with its marks, in that same line.
    let session = Session::new("endless");
    let prints = "printf '\\033[1;31;44m'; head -c 1200000 /dev/zero | tr '\\0' x; \
        printf '\\033]133;C\\007MARKED-TAIL\\033]133;D;0\\007yy\\r\\nafter\\r\\n'";
    let mut start = session.fux();
    start.env("COLUMNS", "1").env("LINES", "2").args(["start", "line", "--", "sh", "-c", prints]);
    let pane = PathBuf::from(String::from_utf8(start.run().stdout).unwrap().trim());
    until("the program has ended", || fact(&pane, "state") == "exited 0");
    assert!(session.fux().arg("wait").arg(&pane).ended().success());
    // (The line was ended where history was put aside, and `log` says so each time.)
    assert!(
        pane.join("history.styled.1").exists() && read(&pane.join("log")).matches("was ended in history").count() == 2
    );
    // The pane's text as the keys make it, and the marks' places in it.
    let text = session.0.join("text");
    let mut script = Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/keys/.text"));
    let add = script.arg(&text).env("FUX_PANE", &pane).run().stdout;
    let add: i64 = String::from_utf8(add).unwrap().trim().parse().unwrap();
    let marks = read(&pane.join("marks"));
    let place = |kind: &str| {
        let mark = marks.lines().find(|mark| mark.split(' ').nth(2) == Some(kind)).unwrap_or_default();
        let mut numbers = mark.split(' ').map(|n| n.parse::<i64>().unwrap_or(-1));
        (numbers.next().unwrap_or(-1), numbers.next().unwrap_or(-1) as usize)
    };
    let ((line, from), (last, to)) = (place("output"), place("end"));
    assert_eq!(line, last, "{marks}");
    let text = fs::read(&text).unwrap();
    let marked = text.split(|byte| *byte == b'\n').nth((line + add - 1) as usize).unwrap_or_default();
    let marked = String::from_utf8_lossy(marked.get(from..to).unwrap_or_default());
    assert_eq!(marked, "MARKED-TAIL", "the marks ({marks:?}) are not where the text has what was marked");
}

#[test]
fn nothing_of_a_pane_changes_under_an_even_generation_also_when_the_odd_one_cannot_be_written() {
    // A directory where `generation` is written anew: the holder cannot say that it is writing, so
    // it writes nothing, neither the screens nor the history, which waits (over a megabyte of it).
    let session = Session::new("even");
    let prints = "read x; seq 1 200000; : > \"$FUX_PANE/printed\"; echo done; sleep 600";
    let pane = session.start("p", &["sh", "-c", prints]);
    let files = |pane: &Path| (fact(pane, "generation"), read(&pane.join("history")), read(&pane.join("main/screen")));
    let before = files(&pane);
    fs::create_dir(pane.join(".generation.new")).unwrap();
    send(&pane.join("in"), "\n");
    until("the program has printed and the rounds fail", || {
        pane.join("printed").exists() && read(&pane.join("log")).matches("cannot write the pane's files").count() >= 2
    });
    assert!(files(&pane) == before, "a file changed while `generation` said {}", before.0);
    // When it can be written again, the next round is written whole and ends even, and no line is lost.
    fs::remove_dir(pane.join(".generation.new")).unwrap();
    send(&pane.join("ctl"), "notify again\n");
    let even = |pane: &Path| fact(pane, "generation").parse::<u64>().is_ok_and(|n| n % 2 == 0 && n > 2);
    until("a round is written", || even(&pane) && files(&pane).2.contains("\ndone\n"));
    let text = files(&pane).1 + &files(&pane).2;
    assert_eq!(text.lines().filter(|line| line.parse::<u32>().is_ok()).count(), 200_000);
    assert!(!read(&pane.join("log")).contains("history"), "{}", read(&pane.join("log")));
}

#[test]
fn the_readme_and_the_agents_page_list_the_capabilities_the_emulator_answers() {
    // The README's table is this table, row for row, and the agents' page lists the same names.
    let (readme, page) = (include_str!("../README.md"), include_str!("../src/help.txt"));
    let rows: Vec<String> = fux_vt::CAPS
        .iter()
        .map(|(name, value)| match value.replace('\x1b', "\\E").as_str() {
            "" => format!("| `{name}` | yes |"),
            shown => format!("| `{name}` | `{shown}` |"),
        })
        .collect();
    assert!(
        readme.contains(&format!("| Name | Value |\n|---|---|\n{}\n\n", rows.join("\n"))),
        "the README's table of capabilities"
    );
    let names: Vec<&str> = fux_vt::CAPS.iter().map(|(name, _)| *name).collect();
    assert!(
        page.split_whitespace().collect::<Vec<_>>().join(" ").contains(&format!("{}.", names.join(" "))),
        "the agents' page"
    );
}

#[test]
fn a_menu_shades_its_group_headings() {
    let (outer, inner) = (Session::new("mghouter"), Session::new("mghinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b")).unwrap();
    fs::write(keys.join("C-b/v"), "#!/bin/sh\n# split side by side\n# group: Panes\n").unwrap();
    fs::write(keys.join("C-b/n"), "#!/bin/sh\n# next tab\n# group: Tabs\n").unwrap();
    inner.start("one", &["sleep", "600"]);
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let mut start = outer.fux();
    start.env("COLUMNS", "80").env("LINES", "12").args(["start", "view", "--"]).args(command);
    start.args([env!("CARGO_BIN_EXE_fux"), "view"]);
    let view = PathBuf::from(String::from_utf8(start.run().stdout).unwrap().trim());
    let screen = || read(&view.join("shown/screen"));
    until("the viewer draws", || !fact(&view, "modes").is_empty() || !screen().trim().is_empty());
    send(&view.join("in"), "\x02");
    until_or("the menu shows", screen, || screen().contains("Panes") && screen().contains("Tabs"));
    let styled = read(&view.join("shown/screen.styled"));
    let row = |needle: &str| styled.lines().find(|line| line.contains(needle)).unwrap_or_default();
    let (title, panes, split, tabs, next) = (row("C-b"), row("Panes"), row("split"), row("Tabs"), row("next"));
    assert!(
        panes.contains("38;5;255;48;5;235m Panes\x1b[0;48;5;235m")
            && tabs.contains("38;5;255;48;5;235m Tabs\x1b[0;48;5;235m"),
        "headings not shaded across box width:\n{styled}"
    );
    assert!(
        !title.contains("48;5;235") && !split.contains("48;5;235") && !next.contains("48;5;235"),
        "non-headings shaded:\n{styled}"
    );
}

#[test]
fn hold_refuses_running_without_its_pane_directory_on_stdin() {
    let session = Session::new("holdguard");
    let pane = session.start("p", &["sleep", "60"]);
    let project = session.0.join(".project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("history"), "precious\n").unwrap();
    fs::write(project.join("state"), "keep\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fux"))
        .args(["hold", ".", "true"])
        .current_dir(&project)
        .env("FUX_PANE", &pane)
        .stdin(std::process::Stdio::null())
        .run();
    assert!(!out.status.success(), "bare fux hold should refuse to run");
    assert_eq!(read(&project.join("history")), "precious\n");
    assert_eq!(read(&project.join("state")), "keep\n");
}

#[test]
fn workspace_prev_next_wrap_skip_invalid_names_and_new_workspace_is_0700() {
    use std::os::unix::fs::PermissionsExt;
    let session = Session::new("wswrap");
    let keys = Path::new(env!("CARGO_MANIFEST_DIR")).join("keys");
    for name in ["Alpha", "beta", "bad space", "colours"] {
        fs::create_dir_all(session.0.join(name)).unwrap();
    }
    let run = |script: &str, ws: &str| {
        let out = Command::new(keys.join(script))
            .env("FUX_DIR", &session.0)
            .env("FUX_WS", ws)
            .env("LC_ALL", "en_US.UTF-8")
            .run();
        String::from_utf8(out.stdout).unwrap()
    };
    // Wrapping and skipping invalid directory names (`bad space`, `colours`), in byte order (`Alpha` < `beta`).
    assert_eq!(run("C-b/w/l", "Alpha"), "workspace beta\n");
    assert_eq!(run("C-b/w/l", "beta"), "workspace Alpha\n");
    assert_eq!(run("C-b/w/h", "Alpha"), "workspace beta\n");
    assert_eq!(run("C-b/w/h", "beta"), "workspace Alpha\n");
    // New workspace folder has mode 0700 like `fux start` creates.
    let mut path = std::ffi::OsString::from(Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap());
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap());
    let out = Command::new(keys.join("C-b/w/n"))
        .env("FUX_DIR", &session.0)
        .env("FUX_KEYS", &keys)
        .env("FUX_WS", "Alpha")
        .env("PATH", path)
        .env("SHELL", "/bin/sh")
        .run();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let mode = fs::metadata(session.0.join("w1")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "w1 mode was {mode:o}");
}

#[test]
fn restore_skips_viewer_and_helper_panes() {
    let session = Session::new("restoreskip");
    let keys = Path::new(env!("CARGO_MANIFEST_DIR")).join("keys");
    let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display();
    for (name, cmd) in [
        ("v1", format!("{} view w", env!("CARGO_BIN_EXE_fux"))),
        ("v2", "fux view w".to_string()),
        ("ed", format!("{}/.shell {}/.edit --in /tmp/x 1", keys.display(), keys.display())),
        ("pk", format!("{}/.shell {}/.pick /tmp/x", keys.display(), keys.display())),
    ] {
        let dir = session.0.join("w").join(name);
        fs::create_dir_all(dir.join("main")).unwrap();
        fs::write(dir.join("status"), format!("holder 99999\npid 99998\ncommand {cmd}\n")).unwrap();
        fs::write(dir.join("state"), "exited 0\n").unwrap();
        fs::write(dir.join("main/screen"), "\n").unwrap();
    }
    let out = Command::new("sh")
        .arg(keys.join(".restore"))
        .env("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap()))
        .env("FUX_DIR", &session.0)
        .env_remove("FUX_KEYS")
        .run();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "", "should skip viewer and helper panes");
}

#[test]
fn bracketed_paste_does_not_leak_while_prefix_menu_is_open() {
    let (outer, inner) = (Session::new("pmouter"), Session::new("pminner"));
    let keys = inner.0.join(".keys");
    let pane = inner.start("main/one", &["sh", "-c", "printf 'READY\\n'; cat"]);
    until("pane ready", || read(&pane.join("shown/screen")).contains("READY"));
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let mut start = outer.fux();
    start.env("COLUMNS", "80").env("LINES", "12").args(["start", "view", "--"]).args(command);
    start.args([env!("CARGO_BIN_EXE_fux"), "view", "main"]);
    let view = PathBuf::from(String::from_utf8(start.run().stdout).unwrap().trim());
    let screen = || read(&view.join("shown/screen"));
    until_or("viewer shows READY", screen, || screen().contains("READY"));
    // Open prefix menu, paste text, close menu, then type a marker so we know the paste was processed.
    send(&view.join("in"), "\x02");
    until_or("menu open", screen, || screen().contains("C-b …"));
    send(&view.join("in"), "\x1b[200~LEAKED\x1b[201~");
    std::thread::sleep(Duration::from_millis(20));
    send(&view.join("in"), "\x1b");
    until_or("menu closed", screen, || !screen().contains("C-b …"));
    send(&view.join("in"), "AFTER\r");
    until_or(
        "AFTER arrived",
        || read(&pane.join("shown/screen")),
        || read(&pane.join("shown/screen")).contains("AFTER"),
    );
    assert!(
        !read(&pane.join("shown/screen")).contains("LEAKED"),
        "paste leaked through open menu:\n{}",
        read(&pane.join("shown/screen"))
    );
}

#[test]
fn single_unified_help_serves_humans_and_agents() {
    let bin = env!("CARGO_BIN_EXE_fux");
    let long = Command::new(bin).arg("--help").run();
    let short = Command::new(bin).arg("-h").run();
    let word = Command::new(bin).arg("help").run();
    assert!(long.status.success() && short.status.success() && word.status.success());
    assert_eq!(long.stdout, short.stdout);
    assert_eq!(long.stdout, word.stdout);
    let text = String::from_utf8(long.stdout).unwrap();
    assert!(text.contains("fux start") && text.contains("WHAT THERE IS") && !text.contains("fux help agents"));
    let old = Command::new(bin).args(["help", "agents"]).run();
    assert!(!old.status.success(), "`fux help agents` should be removed in favor of one unified help");
}

#[test]
fn picker_waits_for_finder_ready_so_first_enter_is_not_lost_and_checks_exit_status() {
    let (outer, inner) = (Session::new("pkouter"), Session::new("pkinner"));
    let keys = inner.0.join(".keys");
    let fakebin = inner.0.join(".fakebin");
    fs::create_dir_all(&fakebin).unwrap();
    // Simulate a TUI finder (like fzf) that takes ~80ms to start up, calls TCSAFLUSH on /dev/tty
    // (dropping any unread bytes typed before it was ready), draws a prompt, then reads one key.
    let fzf = fakebin.join("fzf");
    let picked = inner.0.join(".picked");
    fs::write(
        &fzf,
        format!(
            r#"#!/bin/sh
cat > "$FUX_PANE/items"
python3 - "$FUX_PANE/items" "{}" <<'PY'
import sys, time, termios, tty
time.sleep(0.08)
f = open("/dev/tty", "rb+", buffering=0)
old = termios.tcgetattr(f)
try:
    tty.setraw(f.fileno(), termios.TCSAFLUSH)
    f.write(b"FINDER_READY\r\n")
    b = f.read(1)
finally:
    termios.tcsetattr(f, termios.TCSADRAIN, old)
if b in (b"\r", b"\n"):
    open(sys.argv[2], "w").write("ok\n")
    sys.stdout.write(open(sys.argv[1]).readline())
else:
    sys.stdout.write("IGNORED\n")
    sys.exit(1)
PY
"#,
            picked.display()
        ),
    )
    .unwrap();
    assert!(Command::new("chmod").arg("+x").arg(&fzf).ended().success());
    let p1 = inner.start("main/one", &["sh", "-c", "printf 'PANE_ONE\\n'; sleep 600"]);
    until("p1 printed", || read(&p1.join("shown/screen")).contains("PANE_ONE"));
    let path = format!("{}:{}", fakebin.display(), std::env::var("PATH").unwrap());
    let command = [
        "env",
        &format!("FUX_DIR={}", inner.0.display()),
        &format!("FUX_KEYS={}", keys.display()),
        &format!("PATH={path}"),
    ];
    let mut start = outer.fux();
    start.env("COLUMNS", "80").env("LINES", "12").args(["start", "view", "--"]).args(command);
    start.args([env!("CARGO_BIN_EXE_fux"), "view", "main"]);
    let view = PathBuf::from(String::from_utf8(start.run().stdout).unwrap().trim());
    let screen = || read(&view.join("shown/screen"));
    until_or("viewer shows PANE_ONE", screen, || screen().contains("PANE_ONE"));
    // Press C-b f p and press Enter 20ms later (before the 80ms finder calls TCSAFLUSH).
    send(&view.join("in"), "\x02fp");
    std::thread::sleep(Duration::from_millis(20));
    send(&view.join("in"), "\r");
    // Picker pane must receive that first Enter, select `one`, and close cleanly!
    until_or("picker closed on first Enter", screen, || {
        picked.exists()
            && screen().contains("PANE_ONE")
            && fs::read_dir(inner.0.join("main")).unwrap().filter_map(Result::ok).count() <= 2
    });
    // Also verify a cancelled finder (non-zero exit with non-empty stdout) does not run the command.
    let marker = inner.0.join(".ran");
    let list = inner.0.join(".list");
    fs::write(&list, "one\tone\n").unwrap();
    let cancel_pane = inner.start(
        "main/cancel",
        &[
            "env",
            &format!("PATH={path}"),
            &format!("FUX_KEYS={}", keys.display()),
            keys.join(".pick").to_str().unwrap(),
            list.to_str().unwrap(),
            "sh",
            "-c",
            &format!("echo \"$1\" > {}", marker.display()),
            "sh",
        ],
    );
    until("cancel finder ready", || read(&cancel_pane.join("shown/screen")).contains("FINDER_READY"));
    send(&cancel_pane.join("in"), "q");
    assert!(inner.fux().arg("wait").arg(&cancel_pane).ended().success());
    assert!(!marker.exists(), "command must not run when finder exits non-zero");
}

#[test]
fn kitty_graphics_pane_files_and_viewer_passthrough() {
    // Program inside an inner pane queries Kitty graphics support, displays a 200x40 PNG
    // (20x2 cells at 10x20), waits for a keypress, then deletes it with a=d,d=A.
    let (outer, inner) = (Session::new("kgouter"), Session::new("kginner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    let rgba = [255u8, 64, 0, 255].repeat(200 * 40);
    let png_bytes = fux_vt::png::encode_rgba(200, 40, &rgba, 65536).unwrap();
    let mut b64 = String::new();
    fux_vt::kitty::encode_base64(&png_bytes, &mut b64);
    let b64_file = inner.0.join("img.b64");
    fs::write(&b64_file, &b64).unwrap();
    let script = format!(
        "stty raw -echo; \
         printf '\\033_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\\033\\\\'; \
         dd bs=1 count=12 2>/dev/null > '{}/reply'; \
         printf '\\033[H\\033_Ga=T,i=7,f=100,C=1,q=2;%s\\033\\\\' \"$(cat '{}')\"; \
         dd bs=1 count=1 >/dev/null 2>&1; \
         printf '\\033_Ga=d,d=A,q=2\\033\\\\'; \
         sleep 60",
        inner.0.display(),
        b64_file.display()
    );
    let pane = inner.start("pic", &["sh", "-c", &script]);
    until("pane received query OK and placed pic-1.png", || {
        fs::read(inner.0.join("reply")).ok().as_deref() == Some(b"\x1b_Gi=31;OK\x1b\\")
            && fact(&pane, "shown/pictures") == "pic-1.png 0 0 20 2 10 20"
            && pane.join("pic-1.png").is_file()
    });
    assert_eq!(fux_vt::png::dimensions(&fs::read(pane.join("pic-1.png")).unwrap()), Some((200, 40)));

    // Viewer running in a Kitty-capable outer terminal draws the PNG natively.
    let command = ["env", &format!("FUX_DIR={}", inner.0.display()), &format!("FUX_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &[env!("CARGO_BIN_EXE_fux"), "view"]].concat());
    until("viewer drew kitty png", || {
        fact(&view, "shown/pictures").starts_with("pic-") && fact(&view, "shown/pictures").contains(".png 0 0 20 2 ")
    });
    let pic_name = fact(&view, "shown/pictures").split_whitespace().next().unwrap().to_string();
    assert_eq!(fux_vt::png::dimensions(&fs::read(view.join(&pic_name)).unwrap()), Some((200, 40)));

    // Deleting the image in the inner pane removes pic-1.png and clears it from the viewer.
    send(&pane.join("in"), "x");
    until("kitty picture deleted in pane and viewer", || {
        fact(&pane, "shown/pictures").is_empty()
            && !pane.join("pic-1.png").exists()
            && fact(&view, "shown/pictures").is_empty()
    });
}
