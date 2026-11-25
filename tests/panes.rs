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
/// seconds `TEST_LIMIT` says. The helpers below end their waits at the one moment a test's first
/// session set (`deadline`): a command that has not ended by then is killed and named, and a FIFO
/// nobody reads is an error at once. (A test that waits another way is not held to it by anything
/// here: its watcher stops it, and the tests with it.)
fn limit() -> Duration {
    let seconds = std::env::var("TEST_LIMIT").ok().and_then(|seconds| seconds.parse().ok());
    Duration::from_secs(seconds.unwrap_or(120))
}
/// A test that is not over this long after its deadline is held where no deadline is asked (a call that
/// waits by itself): its watcher ends every session of the tests and the tests with them.
fn grace() -> Duration {
    limit() / 6
}
/// How long the end of a session may take, and the end of all of them where a watcher stops the tests:
/// what it asks of other programs (`ps`, `kill`) may not return either.
fn budget() -> Duration {
    (limit() / 12).max(Duration::from_secs(1))
}

/// The test on a thread: how many sessions it has, when it must be over, and the word to its
/// watcher that it is.
type Scenario = Option<(usize, Instant, Arc<AtomicBool>)>;
thread_local! {
    static SCENARIO: RefCell<Scenario> = const { RefCell::new(None) };
}
/// A pane a session started itself: its holder's number and its program's, as its `status` had them
/// when `fux start` had just said that it was up.
type Started = (u32, u32);
/// The sessions of the tests that run now, each with what it started: what a watcher ends.
static SESSIONS: Mutex<Vec<(PathBuf, Vec<Started>)>> = Mutex::new(Vec::new());
/// (Taken whatever became of a test that had it: a session that ends must not fail at its end, which
/// in a test that is failing would stop every test where it is, its panes left running.)
fn sessions() -> std::sync::MutexGuard<'static, Vec<(PathBuf, Vec<Started>)>> {
    locked(&SESSIONS)
}

/// When the test on this thread must be over (one with no session: `limit` from now).
fn deadline() -> Instant {
    SCENARIO
        .with(|scenario| scenario.borrow().as_ref().map(|scenario| scenario.1))
        .unwrap_or_else(|| Instant::now() + limit())
}

/// Ends a session. Each pane's holder leads a session of its own, and its program another: neither
/// goes with the test, nor at once with the directory (a holder looks every two seconds, and a program
/// may ignore the hangup). So of the panes the session `started` itself, the holder and the program
/// are killed, the program with its process group; then the directory goes. All of it is over by
/// the moment `by`: what was not done by then, or could not be, is what is given back.
///
/// Only those: no file of the session says whom to kill (a `status` there may be a test's own
/// making, and name anything). A pane that was started another way (by a key's script, by
/// `.restore`, by a `fux start` that a test runs as a command of its own and not through `start` or
/// `started`) ends as panes did before: its holder finds the folder gone within two seconds and
/// hangs up its program. None of the tests starts a program that ignores the hangup that way.
fn end(session: &Path, started: &[Started], by: Instant) -> Vec<String> {
    let mut wrong = Vec::new();
    let numbers: Vec<u32> = started.iter().flat_map(|(holder, program)| [*holder, *program]).collect();
    match processes(&numbers, by) {
        Err(why) => wrong.push(format!("{}: what it started was not asked after: {why}", session.display())),
        Ok(processes) => {
            // A number noted is not for ever the process it was. The holder is still ours if it is
            // this fux holding, and the program if it is that holder's child: both go. With its
            // holder gone (killed, and nobody left to hang the program up), the program is still
            // ours if it leads the process group it was started to lead: it goes.
            let of = |pid: u32| processes.iter().find(|process| process.pid == pid);
            let holds = |holder: u32| {
                of(holder).is_some_and(|holder| holder.line.contains(concat!(env!("CARGO_BIN_EXE_fux"), " hold ")))
            };
            let targets: Vec<String> = started
                .iter()
                .flat_map(|&(holder, program)| match (holds(holder), of(program)) {
                    (true, Some(process)) if process.parent == holder => {
                        vec![format!("-{program}"), holder.to_string()]
                    }
                    (false, Some(process)) if process.group == program => vec![format!("-{program}")],
                    _ => Vec::new(),
                })
                .collect();
            // (`kill` says no when one of them has gone meanwhile, which is nothing: what is said
            // here is a `kill` that could not be run, or did not end.)
            let mut kill = Command::new("kill");
            if let (false, Err(why)) =
                (targets.is_empty(), output_by(kill.args(["-KILL", "--"]).args(&targets), by, false))
            {
                wrong.push(format!("{}: {targets:?} were not killed: {why}", session.display()));
            }
        }
    }
    // (A holder may write a file while its folder is being removed: then once more, so that
    // no pane of a test outlives it.)
    while fs::remove_dir_all(session).is_err() && session.exists() {
        if Instant::now() >= by {
            wrong.push(format!("{}: not removed", session.display()));
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    wrong
}

/// A process as `ps` tells of it: its number, its parent's, its process group's, and the whole line
/// (which ends in its command line).
struct Process {
    pid: u32,
    parent: u32,
    group: u32,
    line: String,
}

/// What `ps` says of the processes of these numbers that there are, with their command lines in full
/// (whatever width the tests' own terminal is said to have: `ps` cuts its lines to that, and a line
/// cut short is another command's).
fn processes(pids: &[u32], by: Instant) -> Result<Vec<Process>, String> {
    if pids.is_empty() {
        return Ok(Vec::new());
    }
    let pids = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let mut ps = Command::new("ps");
    ps.args(["-ww", "-o", "pid=,ppid=,pgid=,args=", "-p", &pids]).env_remove("COLUMNS").stdin(Stdio::null());
    let out = output_by(&mut ps, by, false)?;
    // (It ends with 1, and says nothing, when none of them is there any more: that is an answer.)
    if !matches!((out.status.code(), out.stderr.is_empty()), (Some(0), _) | (Some(1), true)) {
        return Err(format!("ps ended with {:?}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
    }
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    let process = |line: &str| {
        let mut numbers = line.split_whitespace().map_while(|word| word.parse().ok());
        Some(Process { pid: numbers.next()?, parent: numbers.next()?, group: numbers.next()?, line: line.to_string() })
    };
    said.lines().map(|line| process(line).ok_or(format!("ps said `{line}`"))).collect()
}

/// A session directory of the test's own: the default one (`fux`) of a runtime directory that is the
/// test's, beside a home and a config directory that are. Dropped, it is ended: its panes with it.
struct Session(PathBuf);
impl Drop for Session {
    fn drop(&mut self) {
        let wrong = self.end();
        sessions().retain(|session| session.0 != self.root());
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
        // What a session's end could not do fails its test (one that is failing already says it and
        // no more: a second failure there would stop every test where it is).
        match (wrong.is_empty(), std::thread::panicking()) {
            (true, _) => {}
            (false, true) => eprintln!("{}", wrong.join("\n")),
            (false, false) => panic!("{}", wrong.join("\n")),
        }
    }
}

impl Session {
    fn new(name: &str) -> Session {
        // Short, as a deep path would not fit a Unix path limit on macOS. (And the folder's own path:
        // `fux start` prints that of a pane named by a path, whatever links lead to it.)
        let tmp = std::env::temp_dir();
        let dir = tmp.canonicalize().unwrap_or(tmp).join(format!("fux-{name}-{}", std::process::id()));
        // (A name is one test's: two that run at once in one directory end each other's panes.)
        let mine = {
            let mut sessions = sessions();
            let mine = sessions.iter().all(|session| session.0 != dir);
            sessions.push((dir.clone(), Vec::new()));
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
        fs::create_dir_all(dir.join("home")).unwrap();
        Session(dir.join("fux"))
    }
    /// The folder that is the test's: the session is in it, and all else that fux looks for by itself.
    fn root(&self) -> &Path {
        self.0.parent().unwrap()
    }
    /// A command that is told this session's surroundings and no others: where the default session
    /// is (`XDG_RUNTIME_DIR`), where the keys are (`XDG_CONFIG_HOME`), and a home.
    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command.env("XDG_RUNTIME_DIR", self.root()).env("XDG_CONFIG_HOME", self.root().join("config"));
        command.env("HOME", self.root().join("home"));
        command
    }
    fn fux(&self) -> Command {
        let mut command = self.command(env!("CARGO_BIN_EXE_fux"));
        command.env_remove("COLUMNS").env_remove("LINES");
        command
    }
    /// A key's script (its path in the keys' directory), to be run as a viewer runs it: in a folder of
    /// its own in the folder of a viewer of this session that is not there (`.view.0`), which tells it
    /// of the workspace, of the pane that has the keys if one is given, of the session, the keys and
    /// the viewer's FIFO, and of two sizes; with this `fux` first in `PATH`, and no size of a terminal.
    /// The folder goes when the script has ended, as a viewer removes it: what the script started
    /// or left running has it no more.
    fn key(&self, keys: &Path, script: &str, workspace: &str, pane: Option<&Path>) -> Command {
        let own = self.0.join(".view.0");
        let folder = (1..).map(|n| own.join(n.to_string())).find(|folder| !folder.exists()).unwrap();
        fs::create_dir_all(&folder).unwrap();
        fs::write(own.join("cwd"), format!("{}\n", self.root().join("home").display())).unwrap();
        let links = [("workspace", self.0.join(workspace)), ("session", self.0.clone()), ("keys", keys.to_path_buf())];
        let links = links.into_iter().chain([("ctl", own.join("ctl"))]).chain(pane.map(|pane| ("pane", pane.into())));
        for (name, to) in links {
            std::os::unix::fs::symlink(to, folder.join(name)).unwrap();
        }
        fs::write(folder.join("row"), "40 24\n").unwrap();
        fs::write(folder.join("col"), "80 12\n").unwrap();
        let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
        let mut command = self.command("sh");
        // (The folder is named to the shell that removes it: nothing is removed on a variable's word.)
        let runs = "folder=$0 script=$1; shift; \"$script\" \"$@\"; code=$?; rm -rf \"$folder\"; exit $code";
        command.args(["-c", runs]).arg(&folder).arg(keys.join(script));
        command.current_dir(&folder).env("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap()));
        command.env_remove("COLUMNS").env_remove("LINES");
        command
    }
    /// Ends the session now, as it is ended when it is let go: what could not be done is given back.
    fn end(&self) -> Vec<String> {
        let started = sessions()
            .iter_mut()
            .find(|session| session.0 == self.root())
            .map(|session| std::mem::take(&mut session.1));
        end(self.root(), &started.unwrap_or_default(), Instant::now() + budget())
    }
    /// Starts a pane by its name (`PANE`, in `main`, or `WORKSPACE/PANE`). Its program is told the
    /// pane's folder in a variable of the tests' own, `PANE`: what a starter has set is the program's.
    fn start(&self, name: &str, program: &[impl AsRef<std::ffi::OsStr>]) -> PathBuf {
        let pane = self.0.join(if name.contains('/') { name.to_string() } else { format!("main/{name}") });
        self.started(self.fux().env("PANE", pane).arg("start").arg(name).arg("--").args(program))
    }
    /// Runs a `fux start` (this one, with what a test has added to it) and notes whose the pane is:
    /// its holder and its program, as its `status` names them now that `fux start` has said the
    /// pane is up (one that has ended by now names two that are gone).
    fn started(&self, start: &mut Command) -> PathBuf {
        let out = start.run();
        assert!(out.status.success(), "start failed: {}", String::from_utf8_lossy(&out.stderr));
        let pane = PathBuf::from(String::from_utf8(out.stdout).unwrap().trim());
        let status = read(&pane.join("status"));
        let number = |name: &str| status.lines().find_map(|line| line.strip_prefix(name)?.parse::<u32>().ok());
        if let (Some(holder @ 2..), Some(program @ 2..)) = (number("holder "), number("pid ")) {
            sessions()
                .iter_mut()
                .filter(|session| session.0 == self.root())
                .for_each(|session| session.1.push((holder, program)));
        }
        pane
    }
}

/// Watches a test from a thread of its own: one that is not `over` at `last` waits where nothing
/// asks its deadline. It is named, the tests are shut, and they stop.
fn watch(test: String, last: Instant, over: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while !over.load(Ordering::Relaxed) {
            if Instant::now() >= last {
                eprintln!(
                    "the test of the session `{test}` was not over {:?} after its deadline: the tests stop",
                    grace()
                );
                shut().iter().for_each(|wrong| eprintln!("{wrong}"));
                std::process::exit(124);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });
}

/// Whether the tests are being shut: no command is started from then on.
static SHUT: AtomicBool = AtomicBool::new(false);

/// Shuts the tests: no command is started any more, what was started and is still waited for is
/// stopped, and every session is ended. What could not be done is given back. All of it is another
/// thread's work, and this one waits for it only so long: a command that does not die when it is
/// killed, a list another thread has in hand, a `ps` that does not return, hold nothing up here.
fn shut() -> Vec<String> {
    SHUT.store(true, Ordering::SeqCst);
    let (by, (done, ended)) = (Instant::now() + budget(), mpsc::channel());
    std::thread::spawn(move || {
        // First what the tests have started (those who wait for it find it ended), then the sessions.
        locked(&RUNNING).iter().for_each(|child| stop(child));
        let sessions = sessions().clone();
        done.send(sessions.iter().flat_map(|(session, started)| end(session, started, by)).collect::<Vec<_>>())
    });
    let late = |_| vec![format!("the tests were not shut in {:?}", budget())];
    ended.recv_timeout(budget() + Duration::from_secs(1)).unwrap_or_else(late)
}

/// What the tests have started and not yet seen end. A watcher that stops the tests stops these
/// first: a command that waits (a `fux start` whose holder never comes) is no pane's yet, and no
/// session's end finds it.
static RUNNING: Mutex<Vec<Arc<Mutex<Child>>>> = Mutex::new(Vec::new());
/// (Taken whatever became of a test that had it, as `sessions` is.)
fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A command a test started, which is known to the tests from that moment. It is waited for until
/// a moment and killed there; and one that is let go while it runs is killed too.
struct Run {
    child: Arc<Mutex<Child>>,
    what: String,
}

impl Run {
    /// How it ended, if it has.
    fn over(&self) -> Option<ExitStatus> {
        locked(&self.child).try_wait().unwrap_or_else(|e| panic!("{}: {e}", self.what))
    }
    /// Waits for its end until the moment `by`, where it is killed, and named.
    fn end_by(&self, by: Instant) -> Result<ExitStatus, String> {
        loop {
            if let Some(status) = self.over() {
                return Ok(status);
            }
            if Instant::now() >= by {
                stop(&self.child);
                return Err(format!("not ended by its deadline, and killed: {}", self.what));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    /// Waits for its end until the test's deadline: one that has not ended by then fails the test.
    fn end(&self) -> ExitStatus {
        self.end_by(deadline()).unwrap_or_else(|why| panic!("{why}"))
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        stop(&self.child);
        locked(&RUNNING).retain(|child| !Arc::ptr_eq(child, &self.child));
    }
}

/// Kills a command and waits for it (one that has ended is only waited for, which is nothing).
fn stop(child: &Mutex<Child>) {
    let mut child = locked(child);
    let _ = child.kill();
    let _ = child.wait();
}

/// What a test runs ends by the test's deadline, or is killed there and named.
trait Bounded {
    /// Starts the command, to be waited for as the test says (`Run`).
    fn begin(&mut self) -> Run;
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
    fn begin(&mut self) -> Run {
        launch(self, true).unwrap_or_else(|why| panic!("{why}"))
    }
    fn run_from(&mut self, from: Option<fs::File>) -> Output {
        let command = self.stdin(from.map_or(Stdio::null(), Stdio::from));
        output_by(command, deadline(), true).unwrap_or_else(|why| panic!("{why}"))
    }
    fn ended(&mut self) -> ExitStatus {
        self.stdin(Stdio::null()).begin().end()
    }
}

/// Starts a command, and puts it on the list as it is started: none is started and not on it.
/// One a test asks for (`asked`) is not started once the tests are being shut; that is looked at
/// with the list in hand, so whoever then stops what is on the list finds every command that was
/// let through. (What the end of a session asks of `ps` and `kill` is started then too.)
fn launch(command: &mut Command, asked: bool) -> Result<Run, String> {
    // No command of a test finds the session, the keys or the home of whoever runs the tests: one
    // that was not told a test's own (`Session::command`) is told of places where nothing can be made.
    for name in ["XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "HOME"] {
        if !command.get_envs().any(|(set, _)| set == name) {
            command.env(name, "/dev/null");
        }
    }
    let what = format!("{command:?}");
    let mut running = locked(&RUNNING);
    if asked && SHUT.load(Ordering::SeqCst) {
        return Err(format!("not started, as the tests are stopping: {what}"));
    }
    let child = Arc::new(Mutex::new(command.spawn().map_err(|e| format!("{what}: {e}"))?));
    running.push(child.clone());
    Ok(Run { child, what })
}

/// Runs a command to its end, or to the moment `by`, where it is killed: what it printed and how
/// it ended, or why neither is known.
fn output_by(command: &mut Command, by: Instant, asked: bool) -> Result<Output, String> {
    let run = launch(command.stdout(Stdio::piped()).stderr(Stdio::piped()), asked)?;
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
    let stdout = reader(locked(&run.child).stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    let stderr = reader(locked(&run.child).stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    let status = run.end_by(by)?;
    // (Something the command started may still hold its pipes: that too is waited for no longer.)
    // (A moment for it also where the command ended as `by` came: what it printed is there to read.)
    let all = |bytes: mpsc::Receiver<Vec<u8>>| {
        let left = by.saturating_duration_since(Instant::now()).max(Duration::from_millis(200));
        bytes.recv_timeout(left).map_err(|_| format!("{}: what it started still holds its output", run.what))
    };
    Ok(Output { status, stdout: all(stdout)?, stderr: all(stderr)? })
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
    assert_eq!(session.end(), [""; 0]);
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
    // (This test's deadline is over: the command has a moment of its own.)
    let made = Command::new("mkfifo").arg(&fifo).begin().end_by(Instant::now() + Duration::from_secs(10));
    assert!(made.is_ok_and(|made| made.success()));
    let refused = said(std::panic::catch_unwind(|| send(&fifo, "typed")));
    assert!(refused.contains("fifo") && read(&fifo).is_empty(), "{refused}");
    assert!(after.elapsed() < Duration::from_secs(3), "{:?}", after.elapsed());
}

#[test]
fn a_session_ends_a_program_whose_holder_has_died() {
    // A holder killed outright leaves its program with nobody to hang it up, and nothing holding
    // the pane's lock. The program is still the pane's: it is the one `status` names, it leads its
    // own process group, and it was born when that `status` was written. The session's end ends it.
    let session = Session::new("orphan");
    let deaf = ["sh", "-c", "trap '' HUP; echo r''eady; exec sh -c 'trap \"\" HUP; while :; do sleep 1; done'"];
    let pane = session.start("deaf", &deaf);
    until("the program ignores the hangup", || read(&pane.join("shown/screen")).contains("ready"));
    let status = read(&pane.join("status"));
    let numbers: Vec<&str> = status.lines().take(2).filter_map(|line| line.split(' ').nth(1)).collect();
    let alive = |pid: &str| Command::new("kill").args(["-0", pid]).stderr(Stdio::null()).ended().success();
    assert!(Command::new("kill").args(["-KILL", numbers[0]]).ended().success(), "{status}");
    until("the holder is gone", || !alive(numbers[0]));
    assert!(alive(numbers[1]), "the program went with its holder: the test means one that does not");
    assert_eq!(session.end(), [""; 0]);
    until("the program is gone", || !alive(numbers[1]));
}

#[test]
fn a_session_ends_no_process_that_a_file_in_it_names() {
    // A folder made by hand in a session, with a `status` that names a process which is no pane's
    // (a `sleep` of the test's own, leading a process group of its own, born as the file is
    // written). The session's end is not told by a file whom to end: the process lives on.
    use std::os::unix::process::CommandExt;
    let session = Session::new("named");
    let sleeps = Command::new("sleep").arg("600").process_group(0).begin();
    let pid = locked(&sleeps.child).id();
    fs::create_dir_all(session.0.join("main/made")).unwrap();
    fs::write(session.0.join("main/made/status"), format!("holder {pid}\npid {pid}\ncommand sleep 600\n")).unwrap();
    assert_eq!(session.end(), [""; 0]);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(sleeps.over(), None, "the session's end killed a process a file named");
}

/// The tests themselves, run anew for the tests named, with what `set` says of their surroundings.
fn tests(names: &[&str], set: &[(&str, &str)]) -> Output {
    let mut tests = Command::new(std::env::current_exe().unwrap());
    tests.arg("--exact").args(names).args(["--nocapture", "--test-threads", "4"]);
    tests.envs(set.iter().copied()).run()
}

#[test]
fn a_session_ends_its_panes_in_a_terminal_of_twenty_columns_too() {
    // What is asked of `ps` is asked in full, whatever the terminal the tests run in is said to be:
    // told of twenty columns, `ps` cut the holder's command line short, the holder was taken for
    // another's, and the program that ignores the hangup was left running.
    let out = tests(
        &["what_a_test_runs_and_writes_to_ends_at_the_tests_one_deadline"],
        &[("COLUMNS", "20"), ("TEST_LIMIT", "20")],
    );
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}

/// Where the tests that were run anew said their sessions are (`a_test_that_opens_a_fifo_nobody_writes`).
fn said_sessions(out: &Output) -> Vec<PathBuf> {
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    said.lines().filter_map(|line| line.strip_prefix("its session: ")).map(PathBuf::from).collect()
}

#[test]
fn a_test_held_where_nothing_asks_its_deadline_is_named_and_the_tests_stop() {
    // The tests themselves, run for the one test below with two seconds to its name: it waits for
    // good, so its watcher ends its session (the pane's program with it) and the run.
    let began = Instant::now();
    let out = tests(&["a_test_that_opens_a_fifo_nobody_writes"], &[("TEST_LIMIT", "2"), ("TEST_HELD", "1")]);
    let (said, sessions) = (String::from_utf8_lossy(&out.stderr).into_owned(), said_sessions(&out));
    assert_eq!(out.status.code(), Some(124), "{said}");
    assert!(said.contains("the test of the session `held` was not over"), "{said}");
    assert!(began.elapsed() < Duration::from_secs(30) && sessions.len() == 1 && !sessions[0].exists(), "{said}");
}

#[test]
fn the_tests_stop_also_when_what_ends_their_sessions_does_not_return() {
    // The same, where `ps` waits for good (it is a script that reads a FIFO nobody writes): ending
    // the sessions has a time of its own, the tests stop when it is over, and what was not done is said.
    let shim = Session::new("shim");
    fs::create_dir_all(&shim.0).unwrap();
    fs::write(shim.0.join("ps"), "#!/bin/sh\nexec cat \"$0.fifo\"\n").unwrap();
    assert!(Command::new("chmod").arg("+x").arg(shim.0.join("ps")).ended().success());
    assert!(Command::new("mkfifo").arg(shim.0.join("ps.fifo")).ended().success());
    let path = format!("{}:{}", shim.0.display(), std::env::var("PATH").unwrap());
    let began = Instant::now();
    let set = [("TEST_LIMIT", "2"), ("TEST_HELD", "1"), ("PATH", path.as_str())];
    let out = tests(&["a_test_that_opens_a_fifo_nobody_writes"], &set);
    let (said, sessions) = (String::from_utf8_lossy(&out.stderr).into_owned(), said_sessions(&out));
    assert_eq!(out.status.code(), Some(124), "{said}");
    assert!(began.elapsed() < Duration::from_secs(30) && said.contains("was not asked after"), "{said}");
    // (Its pane was not ended, as nobody could say whose its numbers are: it goes with its folder,
    // which a holder looks for every two seconds, and its program is one a hangup ends.)
    assert!(sessions.len() == 1 && !sessions[0].exists(), "{said}");
    let holder = said.lines().find_map(|line| line.strip_prefix("its holder: ")).unwrap().to_string();
    until("its holder has left", || {
        !Command::new("kill").args(["-0", &holder]).stderr(Stdio::null()).ended().success()
    });
}

#[test]
fn the_tests_stop_with_no_command_of_another_test_left_running() {
    // Beside the test that is held, another whose `fux start` waits (to say where its pane is, to a
    // FIFO that is full) and whose own deadline is far off when the watcher stops the tests: that
    // start is stopped with them. It is no pane of the session's own, so no session's end would have found it.
    let set = [("TEST_LIMIT", "2"), ("TEST_HELD", "1")];
    let out = tests(&["a_test_that_opens_a_fifo_nobody_writes", "a_test_whose_start_waits"], &set);
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(124), "{said}");
    let start = said.lines().find_map(|line| line.strip_prefix("its start: ")).unwrap_or_else(|| panic!("{said}"));
    let alive = Command::new("kill").args(["-0", start]).stderr(Stdio::null()).ended().success();
    assert!(!alive, "the start {start} of the other test outlived the tests");
    assert!(said_sessions(&out).iter().all(|session| !session.exists()), "{said}");
}

#[test]
fn a_test_whose_start_waits() {
    // (Where a test above tells it to: that one runs it, beside the one that is held.)
    if std::env::var_os("TEST_HELD").is_none() {
        return;
    }
    std::thread::sleep(Duration::from_millis(750));
    let session = Session::new("waits");
    eprintln!("its session: {}", session.0.display());
    // (Its answer goes to a FIFO that is full and that nobody empties: the start waits to say it.)
    fs::create_dir_all(&session.0).unwrap();
    let full = session.0.join("full");
    assert!(Command::new("mkfifo").arg(&full).ended().success());
    let (_reader, mut filler) = (fuxix::open(&full, false).unwrap(), fuxix::open(&full, true).unwrap());
    while filler.write(&[0; 4096]).is_ok() {}
    let answer = fs::OpenOptions::new().write(true).open(&full).unwrap();
    let start = session.fux().args(["start", "p", "--", "sleep", "600"]).stdout(answer).begin();
    eprintln!("its start: {}", locked(&start.child).id());
    start.end();
}

#[test]
fn a_command_begun_as_the_tests_stop_is_not_started() {
    // The tests run anew for the one test below, which shuts them itself, as a watcher does before
    // it stops them: what it had started is ended, and what it starts after that is refused.
    let out = tests(&["a_test_that_shuts_the_tests_itself"], &[("TEST_HELD", "1")]);
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_test_that_shuts_the_tests_itself() {
    // (Where the test above tells it to: that one runs it, alone.)
    if std::env::var_os("TEST_HELD").is_none() {
        return;
    }
    let before = Command::new("sleep").arg("600").begin();
    assert_eq!(shut(), [""; 0]);
    assert!(before.over().is_some(), "what the tests had started was not stopped");
    let said = |failed: std::thread::Result<()>| *failed.unwrap_err().downcast::<String>().unwrap();
    let after = said(std::panic::catch_unwind(|| drop(Command::new("sleep").arg("600").begin())));
    assert!(after.contains("\"sleep\" \"600\"") && after.contains("as the tests are stopping"), "{after}");
}

#[test]
fn a_test_that_opens_a_fifo_nobody_writes() {
    // (It waits for good, where a test above tells it to: that one runs it.)
    if std::env::var_os("TEST_HELD").is_none() {
        return;
    }
    let session = Session::new("held");
    eprintln!("its session: {}", session.0.display());
    let pane = session.start("p", &["sleep", "600"]);
    eprintln!(
        "its holder: {}",
        read(&pane.join("status")).lines().next().unwrap_or_default().trim_start_matches("holder ")
    );
    assert!(Command::new("mkfifo").arg(pane.join("fifo")).ended().success());
    // (To open a FIFO to read is to wait for its writer: nothing here has a deadline.)
    drop(fs::File::open(pane.join("fifo")));
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
    let pane = session.started(&mut start);
    let mut turns = Vec::new();
    until("the program has ended", || {
        // (Each `history.1` is looked at as it appears: its last line is whole.)
        // (Told apart by the number its first line begins with: two may be of one size.)
        let aside = fs::read(pane.join("history.1")).unwrap_or_default();
        let first = aside.get(..6).map(<[u8]>::to_vec);
        if aside.len() > 252 && turns.last().map(|(_, _, first)| first) != Some(&first) {
            turns.push((aside.len(), aside.ends_with(b"\n") && aside.len().is_multiple_of(252), first));
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
    let pane = session.started(&mut start);
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
    let pane = session.started(&mut start);
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
        w() { until [ -e \"$PANE/$1\" ]; do sleep 0.05; done; }; p one; w go1; p two; w go2; p three; sleep 600";
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
    let prints = "mkdir \"$PANE/history.styled.1\"; awk 'BEGIN { for (i = 0; i < 56000; i++) printf \"%079d\\n\", i }'; printf done; sleep 600";
    let pane = session.start("p", &["sh", "-c", prints]);
    until("the lines are printed", || read(&pane.join("shown/screen")).contains("done"));
    let (aside, history) = (read(&pane.join("history.1")), read(&pane.join("history")));
    let lines = aside.lines().count() + history.lines().count();
    assert!(aside.starts_with("0000") && lines == 56000 - 23, "{lines} lines, {} put aside", aside.len());
    let (styled, log) = (fs::metadata(pane.join("history.styled")).unwrap().len(), read(&pane.join("log")));
    assert!(styled < 4 << 20 && log.contains("history.styled was lost"), "{styled} {log}");
}

#[test]
fn a_pane_that_is_moved_is_held_where_it_went_and_its_name_is_anothers_to_start() {
    // A holder keeps the folder it was started in, whatever that is called by now (it works in it,
    // and never by its name). Moved, the pane lives on under its new name, and the old name is free
    // for another: each holder has its own folder, never two in one.
    // (What this does not cover: a folder moved between a start's taking its lock and the holder's
    // first screen. A pane is one run, so nothing can be put into a pane's folder before its holder
    // to make that holder wait there, as this test did while a start took a folder that was there.)
    let session = Session::new("twostarts");
    let (pane, old) = (session.0.join("main/p"), session.0.join("main/old"));
    assert_eq!(session.start("p", &["sh", "-c", "read x; echo mo''ved; exec sleep 600"]), pane);
    let held = read(&pane.join("status"));
    fs::rename(&pane, &old).unwrap();
    assert_eq!(session.start("p", &["sleep", "600"]), pane);
    let second = read(&pane.join("status"));
    send(&old.join("in"), "\n");
    until("the first holder writes where its folder went", || read(&old.join("shown/screen")).contains("moved"));
    assert_eq!((read(&old.join("status")), read(&pane.join("status"))), (held.clone(), second.clone()));
    assert!(held != second && !read(&pane.join("shown/screen")).contains("moved"));
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
    // (The other's arguments: what a shell would expand, one with a blank in it, and one that is empty.)
    let other = session.start("v/other", &["printf", "<%s>\\n", "*", "the-needle;$HOME", "two  words", ""]);
    until("both have printed", || {
        read(&old.join("history")).contains("needle") && read(&other.join("shown/screen")).contains("$HOME")
    });
    fs::write(session.0.join("ed"), "#!/bin/sh\nsed -n \"${1#+}p\" \"$2\" > \"$OUT\"\n").unwrap();
    Command::new("chmod").arg("+x").arg(session.0.join("ed")).ended();
    let out = session.0.join("out");
    // The search, in a pane as the key starts it: the words, then the first line of the list.
    // (By its key, as a viewer of the workspace `w` runs it; the editor is run as `.editor` says. The
    // key's folder is gone when the search asks: its pane, and the editor's after it, are started from
    // where the viewer was started, and a start from a folder that is gone would be refused.)
    editor(&keys, &format!("exec {} \"+$2\" \"$1\"", session.0.join("ed").display()));
    let mut search = session.key(&keys, "C-b/f/s", "w", None);
    search.env("OUT", &out).env("SHELL", "/bin/sh").env("PATH", format!("{}:{path}", session.0.join("none").display()));
    assert!(search.ended().success());
    let find = session.0.join("w/1");
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
        assert!(read(&find.join("shown/screen")).contains("v/other 2:<the-needle;$HOME>"));
        send(&find.join("in"), "1\r");
        until("the editor was opened at the line", || read(&out) == "<the-needle;$HOME>\n");
    }
    let _ = fs::remove_dir_all(&find);
    // A pane whose program has ended is started anew by `.restore`, from its `command` file: the
    // program and its arguments as they were given, each ended by a zero byte, none of them split,
    // expanded or lost. The folder of the run that is over goes, and what that run had printed is the
    // new pane's `.before`. The pane that runs is left alone.
    assert!(session.fux().arg("wait").arg(&other).ended().success());
    let (was, runs) = (read(&other.join("status")), read(&old.join("status")));
    assert!(was.ends_with("\ncommand printf <%s>\\n * the-needle;$HOME two  words \n"), "{was}");
    assert_eq!(fs::read(other.join("command")).unwrap(), b"printf\0<%s>\\n\0*\0the-needle;$HOME\0two  words\0\0");
    let printed = "<*>\n<the-needle;$HOME>\n<two  words>\n<>\n";
    // (The session is named from where the script is run, as a login may name it: it is the same
    // session after the script has gone to the pane's directory, here the home it is given.)
    let (home, named) = (session.0.join("home"), session.0.file_name().unwrap());
    fs::create_dir_all(&home).unwrap();
    let restore = || {
        let mut restore = session.command("sh");
        restore.arg(keys.join(".restore")).arg(named).env("PATH", &path).env("HOME", &home);
        restore.current_dir(session.0.parent().unwrap()).run()
    };
    let restored = restore();
    assert_eq!(String::from_utf8_lossy(&restored.stdout), "started v/other again\n", "{restored:?}");
    assert!(!home.join(named).exists(), "the pane was started in a session of that name under its own directory");
    until("it has run again", || {
        read(&other.join("status")) != was && read(&other.join("shown/screen")).starts_with(printed)
    });
    assert_eq!(read(&other.join(".before")).trim_end(), printed.trim_end());
    assert_eq!(read(&old.join("status")), runs);
    // Restored a second time, the pane has what both runs printed: the `.before` it had, then its own.
    assert!(session.fux().arg("wait").arg(&other).ended().success());
    assert_eq!(String::from_utf8_lossy(&restore().stdout), "started v/other again\n");
    assert_eq!(read(&other.join(".before")).matches("<two  words>").count(), 2);
    // Nothing of the script's is left in the session (a name with a dot first is nobody's pane).
    let left = fs::read_dir(&session.0).unwrap().flatten().map(|entry| entry.file_name().into_string().unwrap());
    assert_eq!(left.filter(|name| name.starts_with(".restore")).collect::<Vec<_>>(), [""; 0]);
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
    let mut script = session.command(concat!(env!("CARGO_MANIFEST_DIR"), "/keys/.text"));
    assert!(script.arg(&pane).arg(&text).ended().success());
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
/// How a test has its editor run: `.editor` in a keys' directory of its own, a line of sh that is
/// given the file as `$1` and the line as `$2`.
fn editor(keys: &Path, line: &str) {
    fs::write(keys.join(".editor"), format!("#!/bin/sh\n{line}\n")).unwrap();
    assert!(Command::new("chmod").arg("+x").arg(keys.join(".editor")).ended().success());
}

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
    send(&pane.join("in"), "exit 3\n");
    let waited = session.fux().arg("wait").arg(&pane).run();
    assert!(String::from_utf8_lossy(&waited.stdout).contains("state exited 3"));
    assert!(!pane.join("in").exists() && !pane.join("ctl").exists(), "the FIFOs go with the holder");
    assert!(!session.fux().args(["start", "layout"]).run().status.success(), "the layout file's name is taken");
    // A workspace's name that is none is refused: it is not taken to mean `main`.
    let odd = session.fux().args(["layout", "main/"]).run();
    assert!(!odd.status.success() && String::from_utf8_lossy(&odd.stderr).contains("main/"), "{odd:?}");
    // Nor is one that is no text.
    let none = <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(b"\xff");
    let odd = session.fux().arg("layout").arg(none).run();
    assert!(!odd.status.success() && String::from_utf8_lossy(&odd.stderr).contains("not text"), "{odd:?}");
}

#[test]
fn a_pane_is_one_run_and_a_name_that_is_there_is_refused_whatever_is_there() {
    let session = Session::new("onerun");
    let refused = |name: &str| {
        let out = session.fux().args(["start", name, "--", "sleep", "600"]).run();
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(!out.status.success() && out.stdout.is_empty() && said.contains("rm -r"), "{name}: {out:?}");
    };
    // A pane that runs is not started again; nor is it when its program has ended, and nothing of
    // that run is touched: a pane is one run, and its folder is the record of it.
    let pane = session.start("p", &["sh", "-c", "echo first; read x; exit 3"]);
    refused("p");
    send(&pane.join("in"), "\n");
    assert!(String::from_utf8_lossy(&session.fux().arg("wait").arg(&pane).run().stdout).contains("state exited 3"));
    let files = || ["status", "state", "generation", "main/screen", "log"].map(|name| stamp_of(&pane, name));
    let was = files();
    refused("p");
    assert_eq!(files(), was);
    assert!(fact(&pane, "state") == "exited 3" && read(&pane.join("main/screen")).starts_with("first\n"));
    // A pane that never came up stays as it is too, with the `log` that says why.
    assert!(!session.fux().args(["start", "none", "--", "/no/such/program"]).run().status.success());
    refused("none");
    assert!(read(&session.0.join("main/none/log")).contains("cannot start /no/such/program"));
    // A folder nobody started is a name that is there (making the folder is the claim to a name),
    // and a number that is there is not free.
    for made in ["made", "1"] {
        fs::create_dir(session.0.join("main").join(made)).unwrap();
    }
    refused("made");
    assert!(fs::read_dir(session.0.join("main/made")).unwrap().next().is_none(), "nothing was put into it");
    assert_eq!(session.start("", &["sleep", "600"]), session.0.join("main/2"));
    // Removed, the name is free, and its pane is a new one.
    fs::remove_dir_all(&pane).unwrap();
    assert_eq!(session.start("p", &["sleep", "600"]), pane);
    assert!(fact(&pane, "state") == "running" && fact(&pane, "generation") == "2");
}

#[test]
fn a_panes_run_is_written_once_after_its_fifos_and_before_its_status() {
    use std::os::unix::fs::MetadataExt;
    let session = Session::new("runfile");
    // 128 bits as 32 hex digits and a line end (`read -r run < run`), and every start draws its own.
    let pane = session.start("p", &["sh", "-c", "echo first; read x"]);
    let other = session.start("q", &["sleep", "600"]);
    let run = read(&pane.join("run"));
    let digits = run.strip_suffix('\n').unwrap_or_default();
    assert!(digits.len() == 32 && digits.bytes().all(|digit| b"0123456789abcdef".contains(&digit)), "{run:?}");
    assert_eq!(fux::run_of(&pane), u128::from_str_radix(digits, 16).ok());
    assert!(fux::run_of(&other).is_some() && fux::run_of(&other) != fux::run_of(&pane));
    // Written once: it is the file it was (one written again is another file) after a round of
    // writing, and after the pane's end.
    let file = fs::metadata(pane.join("run")).unwrap().ino();
    send(&pane.join("ctl"), "resize 40 12\n");
    until("a round was written", || fact(&pane, "size") == "40 12");
    send(&pane.join("in"), "\n");
    assert!(String::from_utf8_lossy(&session.fux().arg("wait").arg(&pane).run().stdout).contains("state exited 0"));
    assert!(read(&pane.join("run")) == run && fs::metadata(pane.join("run")).unwrap().ino() == file);
    // A holder as `fux start` starts it, in a folder where a folder that is not empty has a name
    // the holder needs: it cannot make what is to have that name, and leaves. With no FIFOs it has
    // written no `run` (a folder that has a `run` has had its FIFOs), and with no `run` no `status`.
    let hold = |blocked: &str| {
        let folder = session.0.join("main").join(format!("no-{blocked}"));
        fs::create_dir_all(folder.join(blocked).join("full")).unwrap();
        let mut hold = session.fux();
        let out = hold.args(["hold", ".", "true"]).current_dir(&folder).run_from(fuxix::open(&folder, false).ok());
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(!out.status.success() && said.contains("cannot make the FIFOs, the run or the status"), "{out:?}");
        folder
    };
    let folder = hold("ctl");
    assert!(!folder.join("run").exists() && !folder.join("status").exists());
    let folder = hold("run");
    assert!(folder.join("run").is_dir() && !folder.join("status").exists());
}

#[test]
fn a_pane_and_a_workspace_are_named_by_a_path_as_by_a_name() {
    // An argument that begins with `/` or `.` is a directory's path, and any other a name in the
    // default session. A pane named by a path is the last part of it, in the workspace that the
    // rest is, wherever that is: here in a folder that is no session of anyone's.
    let session = Session::new("paths");
    let other = session.root().join("elsewhere");
    let start = |pane: &Path, from: &Path| {
        fs::create_dir_all(from).unwrap();
        let mut start = session.fux();
        session.started(start.current_dir(from).arg("start").arg(pane).args(["--", "sleep", "600"]))
    };
    assert_eq!(start(&other.join("w/p"), session.root()), other.join("w/p"));
    assert_eq!(fact(&other.join("w/p"), "state"), "running");
    // What ends in `/` is the workspace, and the pane the first free number there. From where the
    // starter stands, with a `.` first: `./` is that folder itself.
    assert_eq!(start(&other.join("w/"), session.root()), other.join("w/1"));
    assert_eq!(start(Path::new("./w/q"), &other), other.join("w/q"));
    assert_eq!(start(Path::new("./"), &other.join("w")), other.join("w/2"));
    assert_eq!(start(Path::new("../v/r"), &other.join("w")), other.join("v/r"));
    // What is printed is the pane's folder by its own path, whatever led there: through a link to
    // the workspace (a key script's folder has such links, and goes when its script ends), the pane
    // is in the workspace and is said to be. And `..` is read as the system reads it, after the
    // link: the folder beside the workspace, not the one beside the link.
    let links = session.root().join("links");
    fs::create_dir_all(&links).unwrap();
    std::os::unix::fs::symlink(other.join("w"), links.join("workspace")).unwrap();
    assert_eq!(start(Path::new("./workspace/"), &links), other.join("w/3"));
    assert_eq!(start(&links.join("workspace/named"), session.root()), other.join("w/named"));
    assert_eq!(start(Path::new("./workspace/../u/s"), &links), other.join("u/s"));
    assert!(!links.join("u").exists() && !session.0.exists(), "a pane was started beside the link, or by a name");
    // The trailing `/` is looked at before anything is split: `.`, `..` and a name with a dot
    // first are no pane's name, with a path before them as without.
    for odd in [".", "..", ".hidden", "./w/.", "./w/..", "./w/.hidden"] {
        let out = session.fux().current_dir(&other).args(["start", odd, "--", "true"]).run();
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(!out.status.success() && said.contains("a name is letters"), "{odd}: {out:?}");
    }
    // A path that is no text is refused, to start in and to view: it is not read as another path
    // (with a sign in place of what is no text), and no folder is made for it.
    let none = <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(b"./\xff/p");
    for verb in ["start", "view"] {
        let out = session.fux().current_dir(&other).arg(verb).arg(none).run();
        assert!(!out.status.success() && !other.join("\u{fffd}").exists(), "{verb}: {out:?}");
    }
    // A name is a name still: in `main` of the default session, or in the workspace named.
    assert_eq!(start(Path::new("p"), &other), session.0.join("main/p"));
    assert_eq!(start(Path::new("w/p"), &other), session.0.join("w/p"));
    assert_eq!(start(Path::new("w/"), &other), session.0.join("w/1"));
    for odd in ["a/b/c", "w//p", "w/.p"] {
        assert!(!session.fux().args(["start", odd, "--", "true"]).run().status.success(), "{odd}");
    }

    // The viewer takes a workspace by its path too, and then the keys' directory: here in a pane, to
    // show the workspace started above, with keys that are not there yet (it writes the defaults).
    let keys = session.root().join("my keys");
    let viewer = [env!("CARGO_BIN_EXE_fux"), "view", links.join("workspace").to_str().unwrap(), keys.to_str().unwrap()]
        .map(String::from);
    let mut start = session.fux();
    start.env("COLUMNS", "60").env("LINES", "10").args(["start", "v/view", "--"]).args(&viewer);
    let view = session.started(&mut start);
    until("the viewer has written its keys and laid out the workspace", || {
        keys.join("C-b/v").exists() && read(&other.join("w/layout")).contains("named")
    });
    // It is the viewer of the session that workspace is in: its FIFO is there, and a workspace it is
    // told to show is one of that session.
    // (A viewer that is up: `cwd` is the last it writes into its folder, after its FIFO is open.)
    let viewers = |session: &Path| {
        let files = fs::read_dir(session).unwrap().flatten().map(|entry| entry.path());
        files.filter(|path| path.to_string_lossy().contains("/.view.") && path.join("cwd").exists()).count()
    };
    until("the viewer's FIFO is in the workspace's session", || viewers(&other) == 1);
    send(&view_fifo(&other), "workspace v\n");
    until("the other workspace of that session is shown", || read(&other.join("v/layout")).contains("* r focus=r"));
    send(&view_fifo(&other), "quit\n");
    assert!(session.fux().arg("wait").arg(&view).ended().success());
    // A name is a name to the viewer too: a workspace of the default session, whatever the folder of
    // that name is a link to. Here it leads to the workspace elsewhere, and the viewer is the default
    // session's all the same: only a path says another session.
    std::os::unix::fs::symlink(other.join("w"), session.0.join("alias")).unwrap();
    let mut start = session.fux();
    start.env("COLUMNS", "60").env("LINES", "10").args(["start", "v/named", "--", env!("CARGO_BIN_EXE_fux")]);
    let view = session.started(start.args(["view", "alias"]).arg(&keys));
    until("the viewer of a name has its folder", || viewers(&session.0) + viewers(&other) == 1);
    assert_eq!((viewers(&session.0), viewers(&other)), (1, 0), "a name's viewer is another session's");
    send(&view_fifo(&session.0), "quit\n");
    assert!(session.fux().arg("wait").arg(&view).ended().success());
    // With no terminal there is no viewer, and nothing is made for one: not the folders of a path
    // that are not there, nor a name's. More than two arguments are refused.
    let view = |odd: &[&str]| {
        let out = session.fux().current_dir(session.root()).arg("view").args(odd).run();
        assert!(!out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    assert!(view(&["./off/w"]).contains("needs a terminal") && view(&["new"]).contains("needs a terminal"));
    assert!(!session.root().join("off").exists() && !session.0.join("new").exists(), "a folder was made");
    assert!(view(&["main", "keys", "more"]).contains("view takes"));
    // A folder that is no workspace's name cannot be shown (asked where there is a terminal: in a pane).
    let asks = "cd \"$0\"; for odd in / './my keys'; do \"$1\" view \"$odd\"; done 2> said; echo done >> said";
    let mut start = session.fux();
    start.args(["start", "v/odd", "--", "sh", "-c", asks]).arg(session.root()).arg(env!("CARGO_BIN_EXE_fux"));
    session.started(&mut start);
    let said = || read(&session.root().join("said"));
    until("the two have answered", || said().ends_with("done\n"));
    assert_eq!(said().matches(": view takes a workspace").count(), 2, "{}", said());
}

#[test]
fn fux_has_no_variable_of_its_own_and_a_command_typed_in_a_pane_means_what_it_means_anywhere() {
    // A pane's program gets what its starter had, with the terminal's own four names and without the
    // starter's size. fux sets, reads and takes away no variable named for itself: the names an older
    // fux had are another program's now, passed on as they are, and nothing is read from them (here
    // they name another session, another workspace and another pane: the pane is started where its
    // argument says).
    let session = Session::new("names");
    let elsewhere = session.root().join("elsewhere");
    let mut start = session.fux();
    start.env("FUX_DIR", &elsewhere).env("FUX_WS", "w9").env("FUX_PANE", elsewhere.join("w9/p"));
    start.env("FUX_KEYS", elsewhere.join("keys")).env("FUX_SOCKET", "/another").env("COLUMNS", "90");
    // (The four an older holder took away from a program, and the one an older `.edit` read.)
    start.env("FUX_VIEW", "/a/viewer").env("FUX_ROW", "40x24").env("FUX_COL", "80x12").env("FUX_EDIT", "ed");
    let pane = session.started(start.args(["start", "w2/p", "--", "sh"]));
    assert_eq!(pane, session.0.join("w2/p"));
    // A command typed in a pane means what it means anywhere: a name is one of the default session,
    // and no workspace named is `main`, not the pane's own (the binary does not know its pane).
    let me = env!("CARGO_BIN_EXE_fux");
    let typed = format!(
        "env | grep -E '^(FUX_|TERM=|TERM_PROGRAM|COLORTERM|COLUMNS|LINES)' | LC_ALL=C sort > '{0}/env'; \
         '{me}' start q -- sleep 600 > '{0}/started'; '{me}' layout > '{0}/here'; '{me}' dir > '{0}/dir'\n",
        pane.display()
    );
    send(&pane.join("in"), typed);
    until("the commands typed in the pane have answered", || read(&pane.join("dir")).ends_with('\n'));
    assert_eq!(read(&pane.join("started")), format!("{}\n", session.0.join("main/q").display()));
    assert_eq!(read(&pane.join("here")), "* q focus=q\n");
    assert_eq!(read(&pane.join("dir")), format!("{}\n", session.0.display()));
    assert!(!elsewhere.exists(), "something was made where a variable said");
    let expected = format!(
        "COLORTERM=truecolor\nFUX_COL=80x12\nFUX_DIR={0}\nFUX_EDIT=ed\nFUX_KEYS={0}/keys\nFUX_PANE={0}/w9/p\nFUX_ROW=40x24\nFUX_SOCKET=/another\nFUX_VIEW=/a/viewer\nFUX_WS=w9\nTERM=xterm-256color\nTERM_PROGRAM=fux\nTERM_PROGRAM_VERSION={1}\n",
        elsewhere.display(),
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(read(&pane.join("env")), expected);
    assert_eq!(fact(&pane, "size"), "90 24");
    // The viewer reads none of them either. Told no workspace and no keys, it is the default session's,
    // shows `main` and writes the keys into the config directory: not what the three variables name.
    let mut start = session.fux();
    start.env("FUX_DIR", &elsewhere).env("FUX_WS", "w9").env("FUX_KEYS", elsewhere.join("keys"));
    let view = session.started(start.args(["start", "v/view", "--", me, "view"]));
    // (Its folder is the last of what it makes as it starts, and `cwd` the last in that: the
    // workspace's folder, the keys, its own with the FIFO.)
    until("the viewer has its folder in the default session", || {
        let files = fs::read_dir(&session.0).unwrap().flatten().map(|entry| entry.path());
        files.into_iter().any(|path| path.to_string_lossy().contains("/.view.") && path.join("cwd").exists())
    });
    assert!(session.root().join("config/fux/keys/C-b/v").exists(), "the keys are not in the config directory");
    assert!(!elsewhere.exists() && !session.0.join("w9").exists(), "the viewer went where a variable said");
    send(&view_fifo(&session.0), "quit\n");
    assert!(session.fux().arg("wait").arg(&view).ended().success());
    // A size that is set and is none is refused: the pane is not started at a size made up for it.
    let odd = session.fux().env("COLUMNS", "wide").args(["start", "w2/odd", "--", "sh"]).run();
    assert!(
        !odd.status.success() && String::from_utf8_lossy(&odd.stderr).contains("COLUMNS is not a number"),
        "{odd:?}"
    );
    // The default session is told by its whole path, also where the runtime directory is named from
    // where the asker stands.
    let (above, named) = (session.root().parent().unwrap(), session.root().file_name().unwrap());
    let out = session.fux().current_dir(above).env("XDG_RUNTIME_DIR", named).arg("dir").run();
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{}\n", session.0.display()));
    // And a command of a test that was told no session of a test's has none it can make anything in:
    // not the one of whoever runs the tests.
    let none = Command::new(me).arg("dir").run();
    assert_eq!(String::from_utf8_lossy(&none.stdout), "/dev/null/fux\n");
    assert!(!Command::new(me).args(["start", "x", "--", "true"]).run().status.success());
}

#[test]
fn kill_hangs_up_the_program_and_removing_the_directory_ends_the_pane() {
    let session = Session::new("end");
    let pane = session.start(
        "a",
        &["sh", "-c", "trap 'echo hup > \"$PANE/saw\"; exit 0' HUP; echo r''eady; while :; do sleep 1; done"],
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
    assert!(session.command("sh").args(["-c", &start]).ended().success());
    let interrupted = session.0.join("main/c");
    until("the program runs", || fact(&interrupted, "state") == "running");
    send(&interrupted.join("in"), "\x03");
    until("Ctrl-C ended it", || fact(&interrupted, "state") == "killed 2");

    let other = session.start("b", &["sh", "-c", "echo $$ > \"$PANE/pid\"; exec sleep 600"]);
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
    let writer = Command::new("sh").args(["-c", "head -c 100000 /dev/zero > in"]).current_dir(&pane).begin();
    let done = writer.end_by(Instant::now() + Duration::from_secs(5));
    assert!(
        done.as_ref().is_ok_and(|status| status.success()),
        "100,000 bytes to `in` after kill: {done:?} after 5 seconds"
    );
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
fn a_keys_script_is_told_by_a_folder_of_its_own_which_goes_with_it() {
    // The viewer has a folder of its own in the session, `.view.PID`: its FIFO (`ctl`), where it
    // was started (`cwd`), and for each key's script that runs a folder that is the script's working
    // directory, with links to what the script is about and two sizes. Here a viewer of 100 by 30
    // in a pane, started in a folder of its own, where a killed viewer of the same number had left
    // its folder (the shell that becomes the viewer makes it): that goes.
    use std::os::unix::fs::MetadataExt;
    let (outer, inner) = (Session::new("ownouter"), Session::new("owninner"));
    let (keys, project) = (inner.root().join("keys"), inner.root().join("project"));
    fs::create_dir_all(keys.join("C-b")).unwrap();
    fs::create_dir_all(&project).unwrap();
    let script = |name: &str, text: &str, mode: &str| {
        fs::write(keys.join(name), format!("#!/bin/sh\n# {name}\n{text}")).unwrap();
        assert!(Command::new("chmod").arg(mode).arg(keys.join(name)).ended().success());
    };
    // A key that writes down all it is told, and then stays until it is let go.
    let tells = r#"s=$(readlink session)
{ pwd -P; for l in pane workspace session keys ctl; do echo "$l $(readlink $l)"; done; echo "row $(cat row)"
  echo "col $(cat col)"; echo "size $COLUMNS $LINES"; ls | tr '\n' ' '; } > "$s/.told.new" 2>&1 && mv "$s/.told.new" "$s/.told.${PWD##*/}"
until [ -e "$s/.go" ]; do sleep 0.05; done
"#;
    script("C-b/t", tells, "755");
    script("C-b/n", "echo ran\n", "644");
    // (What is run for a workspace with no pane: it notes its folder, its number and what it has, and stays too.)
    let empty = r#"s=$(readlink session)
echo "${PWD##*/} $$ $(basename "$(readlink workspace)") $(ls | tr '\n' ' ')" >> "$s/.ran"
until [ -e "$s/.go" ]; do sleep 0.05; done
"#;
    script(".empty", empty, "755");
    let (a, _b) = (inner.start("a", &["cat"]), inner.start("b", &["cat"]));
    lay(&inner.0, "* row(a b) focus=a\n");
    let becomes = "mkdir -p \"$0/.view.$$/7\" && : > \"$0/.view.$$/7/stale\" && exec \"$@\"";
    let mut start = outer.fux();
    start.env("COLUMNS", "100").env("LINES", "30").current_dir(&project).args(["start", "view", "--", "sh", "-c"]);
    start.args([becomes, inner.0.to_str().unwrap(), env!("CARGO_BIN_EXE_fux"), "view"]);
    let view = outer.started(start.arg(inner.0.join("main")).arg(&keys));
    until("the panes are drawn", || read(&view.join("shown/screen")).contains('│'));
    let own = view_fifo(&inner.0).parent().unwrap().to_path_buf();
    let has = |folder: &Path| {
        let mut names: Vec<String> =
            fs::read_dir(folder).unwrap().flatten().map(|entry| entry.file_name().into_string().unwrap()).collect();
        names.sort();
        names.join(" ")
    };
    assert_eq!(has(&own), "ctl cwd", "the viewer's folder, with nothing a viewer before it left");
    assert_eq!(read(&own.join("cwd")), format!("{}\n", project.display()));
    assert_eq!(own.metadata().unwrap().mode() & 0o777, 0o700);

    // A key's script: its folder is made whole before it starts, and is where it runs.
    send(&view.join("in"), "\x02t");
    until("the first script has said what it was told", || inner.0.join(".told.1").exists());
    let told = |n: u32, pane: &str, sizes: &str| {
        let (folder, session, ctl) = (own.join(n.to_string()), inner.0.display(), own.join("ctl"));
        let links = format!(
            "pane {session}/main/{pane}\nworkspace {session}/main\nsession {session}\nkeys {}\n",
            keys.display()
        );
        format!(
            "{}\n{links}ctl {}\n{sizes}\nsize 100 30\ncol ctl keys pane row session workspace ",
            folder.display(),
            ctl.display()
        )
    };
    assert_eq!(read(&inner.0.join(".told.1")), told(1, "a", "row 33 30\ncol 50 14"));
    // The next key has a folder of its own, with the focus as it is at that key; the script that
    // still runs keeps its own as it was. (The keys waited two seconds for the first.)
    assert!(inner.fux().args(["layout", "main", "pane", "b"]).ended().success());
    send(&view.join("in"), "\x02t");
    until("the second script has said what it was told", || inner.0.join(".told.2").exists());
    assert_eq!(read(&inner.0.join(".told.2")), told(2, "b", "row 32 30\ncol 49 14"));
    assert_eq!((has(&own), fs::read_link(own.join("1/pane")).unwrap()), ("1 2 ctl cwd".to_string(), a.clone()));
    // A script's folder goes when the script has ended.
    fs::write(inner.0.join(".go"), "").unwrap();
    until("the folders of the two scripts are gone", || has(&own) == "ctl cwd");
    fs::remove_file(inner.0.join(".go")).unwrap();
    // A key whose file cannot be run says so, and leaves no folder.
    send(&view.join("in"), "\x02n");
    until("the viewer says that the key's file cannot be run", || {
        read(&view.join("shown/screen")).contains("Permission denied")
    });
    assert_eq!(has(&own), "ctl cwd");

    // 32 scripts at once and no more: here the one a workspace with no pane is first shown with,
    // 32 times (it has no `pane`: nothing has the keys there). One more is refused, and has no folder.
    for name in ["e1", "e2"] {
        fs::create_dir_all(inner.0.join(name)).unwrap();
    }
    let ran = || read(&inner.0.join(".ran")).lines().map(String::from).collect::<Vec<_>>();
    for n in 1..=32 {
        send(&own.join("ctl"), format!("workspace e{}\n", n % 2 + 1));
        until("the script of the empty workspace runs", || ran().len() == n);
    }
    let pids: Vec<String> = ran().iter().map(|line| line.split(' ').nth(1).unwrap().to_string()).collect();
    assert!(
        ran()[31].starts_with("35 ") && ran()[31].ends_with(" e1 col ctl keys row session workspace "),
        "{:?}",
        ran()
    );
    assert_eq!(fs::read_dir(&own).unwrap().count(), 34);
    // (The keys wait two seconds for each script, and the viewer says so in its corner when a wait is
    // over: all 32 are over before one more is asked for, so that what it says of that one stays.)
    std::thread::sleep(Duration::from_millis(2500));
    send(&own.join("ctl"), "workspace e2\n");
    until("the viewer says that one more was not started", || {
        read(&view.join("shown/screen")).contains("32 key scripts are still running")
    });
    assert_eq!((ran().len(), fs::read_dir(&own).unwrap().count()), (32, 34));
    // The viewer leaves: its folder goes, with the folders of the scripts that still run.
    send(&own.join("ctl"), "quit\n");
    assert!(outer.fux().arg("wait").arg(&view).ended().success());
    assert!(!own.exists(), "the viewer left its folder: {}", has(&own));
    // (The scripts are let go, and end: none of the tests' is left running.)
    fs::write(inner.0.join(".go"), "").unwrap();
    until("the scripts have ended", || {
        !Command::new("kill").arg("-0").args(&pids).stderr(Stdio::null()).ended().success()
    });
}

#[test]
fn a_path_with_a_line_end_in_it_is_whole_to_the_keys_scripts() {
    // The default keys through a viewer, where every folder's name has a line end in it, with a `-`
    // first, a blank and a `*`: the session's, the keys', the one the viewer is started in and the
    // one a pane says it is in. A path in a file is the whole file, and is read so, not its first line.
    let session = Session::new("lines");
    let odd = |name: &str| session.root().join(format!("-{name} *\nend"));
    let (shown, keys, project, said) = (odd("session"), odd("keys"), odd("project"), odd("said"));
    let home = session.root().join("home");
    for folder in [&project, &said] {
        fs::create_dir_all(folder).unwrap();
    }
    // (A shell that notes where it is started, as an editor does below, and then is a shell.)
    let notes = |name: &str| format!("pwd -P > \"$HOME/{name}.new\" && mv \"$HOME/{name}.new\" \"$HOME/{name}\"");
    fs::write(session.root().join("shell"), format!("#!/bin/sh\n{}\nexec /bin/sh\n", notes("shell"))).unwrap();
    assert!(Command::new("chmod").arg("+x").arg(session.root().join("shell")).ended().success());
    let stands = |name: &str, folder: &Path| {
        until_or(
            &format!("the {name} has said where it stands"),
            || format!(": {:?}", read(&home.join(name))),
            || read(&home.join(name)) == format!("{}\n", folder.display()),
        );
        fs::remove_file(home.join(name)).unwrap();
    };
    let mut start = session.fux();
    start.env("SHELL", session.root().join("shell")).env("HERE", keys.join(".here")).env("SHOWN", &shown);
    start.env("COLUMNS", "100").env("LINES", "30").current_dir(&project);
    let view = session.started(
        start.args(["start", "view", "--", env!("CARGO_BIN_EXE_fux"), "view"]).arg(shown.join("main")).arg(&keys),
    );
    // The first shell, which has no pane to follow, stands where the viewer was started (`../cwd`).
    stands("shell", &project);
    // A split stands where the focused pane's program says it is (`pane/cwd`: the holder writes no
    // path with a line end in it, a shell that writes the file itself may).
    let first = shown.join("main/1");
    fs::write(first.join("cwd"), format!("{}\n", said.display())).unwrap();
    send(&view.join("in"), "\x02v");
    stands("shell", &said);
    // The editor's pane and a new workspace's shell stand where the viewer was started.
    editor(&keys, &notes("editor"));
    send(&view.join("in"), "\x02c");
    stands("editor", &project);
    send(&view.join("in"), "\x02wn");
    stands("shell", &project);
    // A pane that is over is started again where its program last said it was.
    let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
    let path = format!("{bin}:{}", std::env::var("PATH").unwrap());
    let mut over = session.fux();
    over.current_dir(session.root());
    let over = session.started(over.arg("start").arg(shown.join("r/over")).args(["--", "sh", "-c", &notes("over")]));
    assert!(session.fux().arg("wait").arg(&over).ended().success());
    stands("over", session.root());
    fs::write(over.join("cwd"), format!("{}\n", said.display())).unwrap();
    let restored = session.command("sh").arg(keys.join(".restore")).arg(&shown).env("PATH", &path).run();
    assert_eq!(String::from_utf8_lossy(&restored.stdout), "started r/over again\n", "{restored:?}");
    stands("over", &said);
    // In that session `.here` finds the pane it is asked in, and a shell's pane closes when its shell
    // ends (`.shell` asks `.here` which folder to remove).
    let second = shown.join("main/2");
    send(&second.join("in"), "sh \"$HERE\" \"$SHOWN\" > \"$HOME/here\" 2>&1; echo \"ended $?\" >> \"$HOME/here\"\r");
    until("`.here` has answered", || read(&home.join("here")).contains("ended "));
    assert_eq!(read(&home.join("here")), format!("{}\nended 0\n", second.display()));
    send(&second.join("in"), "exit\r");
    until_or(
        "the pane of the shell that ended is gone",
        || format!(": {}", fact(&second, "state")),
        || !second.exists(),
    );
}

#[test]
fn a_viewer_whose_panes_do_not_change_does_no_work_and_draws_the_next_change() {
    // The viewer on a terminal of 500 by 500 cells: putting a frame of that size together is
    // work that shows in its processor time, which `ps` tells (in whole seconds, on Linux).
    let (outer, inner) = (Session::new("iouter"), Session::new("iinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    let one = inner.start("one", &["sh", "-c", "echo r''eady; exec cat"]);
    let mut start = outer.fux();
    start.env("COLUMNS", "500").env("LINES", "500").args(["start", "view", "--"]).args(viewer(&inner, &keys));
    let view = outer.started(&mut start);
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
    let mut start = outer.fux();
    start.env("SHELL", "/bin/sh").args(["start", "view", "--"]).args(viewer(&inner, &keys));
    let view = outer.started(&mut start);
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
    fs::write(keys.join(".empty"), "#!/bin/sh\nbasename \"$(readlink workspace)\" >> keys/ran\n").unwrap();
    fs::create_dir_all(inner.0.join("other")).unwrap();
    let ctl = view_fifo(&inner.0);
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
    let wide = |session: &Session, name: &str, program: &[String]| {
        let mut start = session.fux();
        start.env("COLUMNS", "300").env("LINES", "20").args(["start", name, "--"]).args(program);
        session.started(&mut start)
    };
    let fills = "printf '\\033[20;1H%s' \"$(head -c 300 /dev/zero | tr '\\0' x)\"; sleep 600";
    wide(&inner, "full", &["sh", "-c", fills].map(String::from));
    let view = wide(&outer, "view", &viewer(&inner, &keys));
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
    // (It names its pane through the link to the workspace, and is told the pane's own path.)
    let script = "#!/bin/sh\n# split\n# group: Panes\nread -r COLUMNS LINES < row && export COLUMNS LINES\np=$(fux start ./workspace/two -- sh -c 'stty size > \"$0/first\"; exec sh' \"$(readlink workspace)/two\") && [ \"$p\" = \"$(readlink workspace)/two\" ] && until [ -s \"$p/first\" ]; do sleep 0.01; done && fux layout ./workspace split row two\necho made $(readlink keys)\n";
    fs::write(keys.join("C-b/v"), script).unwrap();
    fs::write(keys.join("C-b/e"), "#!/bin/sh\nsleep 0.3\nprintf 'echo ' 1<>pane/in\n").unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/v")).arg(keys.join("C-b/e")).ended();
    let one = inner.start("one", &["sh"]);
    // The terminal the viewer runs in says what its colours are: here the outer session's file does.
    fs::create_dir_all(&outer.0).unwrap();
    fs::write(outer.0.join("colours"), "foreground #102030\nbackground #fdf6e3\ncursor #0a0b0c\n4 #268bd2\n").unwrap();
    let view = outer.start("view", &viewer(&inner, &keys));
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
    send(&inner.0.join("main/one/in"), "clear; printf '\\033[31mpl''ain\\033[m\\n'\r");
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
    send(&view_fifo(&inner.0), b"workspace other\nworkspace main\n");
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
    fs::write(keys.join("C-b/o"), "#!/bin/sh\n# other\nfux layout ./workspace focus next\n").unwrap();
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
            let out = inner.fux().args(["layout", "main"]).args(*edit).run();
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
        "printf '\\033[?1049h\\033[>4u\\033[?1049l'; {} view {}/main {}; stty raw -echo; printf '\\033[?1049h\\033[?u'; exec cat -vt",
        env!("CARGO_BIN_EXE_fux"),
        inner.0.display(),
        keys.display()
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
    send(&view_fifo(&inner.0), "quit\n");
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
    let view = outer.start("view", &viewer(&inner, &keys));
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
    let asks_then_stops = "stty raw -echo; printf '\\033[?1004h'; until [ -e \"$PANE/go\" ]; do sleep 0.05; done; printf '\\033[?1004l'; exec cat -vt";
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
    let pane = session.started(&mut start);
    until("the marks are there", || read(&pane.join("marks")).lines().count() == 10);
    let marks = read(&pane.join("marks"));
    let (first, last) =
        ("0 0 prompt\n0 2 command\n1 0 output\n14 0 end 0\n14 0 prompt\n", "17 0 end 1\n17 0 prompt\n17 2 command\n");
    assert!(marks.starts_with(first) && marks.ends_with(last), "{marks}");
    // One file says how the screen's rows are lines of the pane's text: the number of the line
    // its first row is of (the wrapped one, 13 lines in), then the rows that go on from the row
    // above (the first, from history). No other file says either.
    // (Waited for: `marks` is written before it in a round, and was read a moment ago.)
    until_or(
        "`main/lines` has the number of the first row's line, then the rows that go on",
        || read(&pane.join("main/lines")),
        || read(&pane.join("main/lines")) == "13\n0\n",
    );
    assert!(!pane.join("top").exists() && !pane.join("wraps").exists());
    // An editor for the test: it is called `vim`, so it is given a line number; it notes that
    // and copies the file it was to open. (The copy has its name only when it is whole, and the
    // note is written before it: a result that is there is all there, and of this key.)
    let out = session.0.join("out");
    let fake = |words: &str| {
        format!(
            "#!/bin/sh\nfor a; do f=$a; done\npwd -P > \"$OUT.in\"\necho \"{words}\" > \"$OUT.at\"\ncp \"$f\" \"$OUT.new\" && mv \"$OUT.new\" \"$OUT\"\n"
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
        let mut script = session.key(&keys, &format!("C-b/{name}"), "main", Some(&pane));
        assert!(script.env("EDITOR", session.0.join("vim")).env("OUT", &out).ended().success());
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
    // History in the editor opens at the text's end. The editor's pane was started from where the
    // viewer was started, not from the folder of the key's script, which is gone by now.
    let (text, line) = key("c");
    assert_eq!((text.lines().count(), text.lines().last()), (line, Some("$")));
    assert_eq!(read(&session.0.join("out.in")), format!("{}\n", session.root().join("home").display()));
    assert_eq!(fs::read_dir(session.0.join(".view.0")).unwrap().count(), 1, "a key's folder was left");
    // History in colour is the same text with its colours, in `less -R` at its end (a `less`
    // for the test: the same script).
    fs::write(session.0.join("less"), fake("$*")).unwrap();
    Command::new("chmod").arg("+x").arg(session.0.join("less")).ended();
    clear();
    let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
    let mut script = session.key(&keys, "C-b/M-c", "main", Some(&pane));
    script.env("PATH", format!("{}:{bin}:{}", session.0.display(), std::env::var("PATH").unwrap()));
    assert!(script.env("OUT", &out).ended().success());
    until("less was given the file", || bare(&read(&out)) == text);
    assert!(read(&out).contains("\n\x1b[0;32ma-long-line-\x1b[mthat-wraps-in-twelve\n$ second\n"), "{:?}", read(&out));
    let asked = read(&session.0.join("out.at"));
    assert!(asked.starts_with(&format!("-+F -R +G {}/.text.", session.0.display())), "{asked}");
    // The rows left out at the screen's end are those with no text: a colour alone is none.
    // And the cell a wide character left empty at a row's end is no text: the rows are joined without it.
    let blue = session.start("blue", &["sh", "-c", "printf 'x\\033[44m\\033[J%78s\u{754c}\\033[m' ''; sleep 600"]);
    until("the screen is blue", || read(&blue.join("shown/screen.styled")).matches("\x1b[0;44m").count() == 24);
    let mut text = session.command(keys.join(".text"));
    assert!(text.arg(&blue).arg(&out).arg("styled").run().status.success());
    assert!(
        read(&blue.join("shown/screen.styled")).starts_with(&format!("x\x1b[0;44m{}\u{200b} \x1b[m\n", " ".repeat(78)))
    );
    let joined = format!("x\x1b[0;44m{}\x1b[m\x1b[0;44m\u{754c} ", " ".repeat(78));
    assert!(read(&out).starts_with(&joined) && read(&out).lines().count() == 1, "{:?}", read(&out));
    // A command that prints nothing has no output to open, and the key says so; one whose
    // output does not end its line has that output, every character of it whole, without the
    // prompt that follows on the line. (And the editor is run as `.editor` in the keys' directory says.)
    let quiet = "m() { printf '\\033]133;%s\\007' \"$1\"; }; \
        m A; printf '$ '; m B; echo printf; m C; printf 'e\\314\\201\\347\\225\\214'; m 'D;0'; \
        m A; printf '$ '; m B; until [ -e \"$PANE/go\" ]; do sleep 0.05; done; echo true; m C; m 'D;0'; m A; printf '$ '; m B; sleep 600";
    let pane = session.start("quiet", &["sh", "-c", quiet]);
    until("the first command is done", || read(&pane.join("marks")).lines().count() == 6);
    let run_in = |name: &str, pane: &Path, edit: String| {
        editor(&keys, &edit);
        let mut script = session.key(&keys, &format!("C-b/{name}"), "main", Some(pane));
        String::from_utf8(script.env("OUT", &out).run().stdout).unwrap()
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
    // An editor still open when its pane is closed with the close key: the file it was given is a
    // dot-named one of the session, not in its pane's folder, and is removed when the pane's end
    // hangs the editor up.
    assert_eq!(run_in("c", &pane, "echo ed''iting; tail -f \"$1\"".into()), "");
    let editor = || {
        let panes = fs::read_dir(session.0.join("main")).unwrap().flatten();
        panes.map(|pane| pane.path()).find(|pane| read(&pane.join("shown/screen")).contains("editing"))
    };
    until("the editor has its file", || editor().is_some());
    let closing = editor().unwrap();
    assert!(!closing.join("text").exists(), "the text is in the editor's pane");
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
    // (`.shell` is told the session, where it finds its pane's folder when the shell has ended.)
    let here = session.0.to_str().unwrap();
    let mut start = session.fux();
    start.env("SHELL", session.0.join("sh127")).args(["start", "ran", "--", wrapper, here]);
    let ran = session.started(&mut start);
    let seen = || format!(": its screen is {:?}", read(&ran.join("shown/screen")).trim());
    until_or("the pane of a shell that ended with 127 is gone", seen, || !ran.exists());
    // A command that is not there could not be run: that is said, and stays until Enter.
    let none = session.start("none", &[wrapper, here, "/no/such/program"]);
    until("it says that it could not be run", || {
        read(&none.join("shown/screen")).contains("it could not be run: Enter")
    });
    send(&none.join("in"), "\r");
    until("Enter closed the pane", || !none.exists());
    // A pane that was given another name while its shell ran closes all the same: which folder is
    // its own is asked when the shell has ended, not noted when it began.
    let moved = session.start("moved", &[wrapper, here, "sh", "-c", "echo r''eady; read x"]);
    until("the shell runs", || read(&moved.join("shown/screen")).contains("ready"));
    let renamed = session.0.join("main/renamed");
    fs::rename(&moved, &renamed).unwrap();
    send(&renamed.join("in"), "\r");
    until("the pane that was renamed is gone", || !renamed.exists());
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
    // (The key's script is told the viewer's FIFO by a link in its folder, which is gone when the
    // choice is made: the picker's pane was given the FIFO's own path.)
    let view = session.0.join(".view.0/ctl");
    fs::create_dir_all(view.parent().unwrap()).unwrap();
    let choose = || {
        assert!(session.key(&keys, "C-b/w/g", "main", None).ended().success());
        let _ = fs::remove_dir_all(session.0.join(".view.0/1"));
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
        ("ed", format!("#!/bin/sh\nuntil [ -e \"{0}/go\" ]; do sleep 0.02; done\n", session.0.display())),
        (
            "fux",
            format!(
                "#!/bin/sh\nif [ \"$1 $3\" = 'layout pane' ]; then : > \"{0}/go\"; while [ -d \"{0}/main/$4\" ]; do sleep 0.02; done; fi\nexec {fux} \"$@\"\n",
                session.0.display()
            ),
        ),
    ];
    for (name, text) in scripts {
        fs::write(bin.join(name), text).unwrap();
        assert!(Command::new("chmod").arg("+x").arg(bin.join(name)).ended().success());
    }
    fs::write(session.0.join("file"), "text\n").unwrap();
    editor(&keys, "exec ed \"$1\"");
    let mut script = session.key(&keys, ".edit", "main", None);
    script.arg(session.0.join("main")).arg(session.0.join("file"));
    let out = script.env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())).run();
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
    fs::write(keys.join("C-b/o"), "#!/bin/sh\n# other\nfux layout ./workspace focus next\n").unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/o")).ended();
    fs::create_dir_all(&outer.0).unwrap();
    fs::write(outer.0.join("keyboard"), "31\n").unwrap();
    let raw = "stty raw -echo; exec cat -vt";
    let plain = inner.start("plain", &["sh", "-c", raw]);
    let view = outer.start("view", &viewer(&inner, &keys));
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
    send(&view_fifo(&inner.0), "quit\n");
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
        fs::write(&script, format!("#!/bin/sh\n# act\necho {menu} >> keys/ran\n")).unwrap();
        Command::new("chmod").arg("+x").arg(script).ended();
    }
    fs::create_dir_all(&outer.0).unwrap();
    fs::write(outer.0.join("keyboard"), "31\n").unwrap();
    fs::create_dir_all(&inner.0).unwrap();
    fs::write(inner.0.join("keyboard"), "31\n").unwrap();
    let asking = ["sh", "-c", "stty raw -echo; printf '\\033[>11u\\033[?u'; exec cat -vt"];
    let (first, second) = (inner.start("first", &asking), inner.start("second", &asking));
    let layout = |edit: &[&str]| assert!(inner.fux().args(["layout", "main"]).args(edit).ended().success());
    layout(&["pane", "first"]);
    let view = outer.start("view", &viewer(&inner, &keys));
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
    // The first pane is removed and another is started under its name, in a tab that is not
    // shown: what the terminal tells of the key after that is nothing to the new pane's program.
    // (Once more where its holder wrote a file as the folder was emptied.)
    until("the first pane's folder is removed", || fs::remove_dir_all(&first).is_ok() || !first.exists());
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
    let open = "printf '\\033]8;;http://open/\\033\\\\'; exec \"$@\"";
    let view = outer.start("view", &[&["sh", "-c", open, "sh"].map(String::from)[..], &viewer(&inner, &keys)].concat());
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
    let view = outer.start("view", &viewer(&inner, &keys));
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
    // A link under the picture's name is no picture, wherever it leads (here to one that is drawn
    // when it has the name itself): nothing of it is drawn.
    anew(format!("q\"1;1;600;60#1;2;0;0;100{}", ["!600~"; 10].join("-")));
    until("the picture is drawn again", || pixels(&drawn().1, [0, 0, 1000, 100]).is_some_and(all_blue));
    fs::rename(left.join("pic-1.sixel"), inner.0.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(inner.0.join("elsewhere"), left.join("pic-1.sixel")).unwrap();
    until("the link is not drawn", || fact(&view, "shown/pictures").is_empty());
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
    let view = outer.start("view", &viewer(&inner, &keys));
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
    let view = outer.start("view", &viewer(&inner, &keys));
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
    let view = outer.start("view", &viewer(&inner, &keys));
    until("the viewer asked its terminal for the mouse", || fact(&view, "modes").contains("mouse1000"));
    send(&view.join("in"), "\x1b[<0;50;3M");
    let layout = || read(&inner.0.join("main/layout"));
    until_or("the click moved the focus", layout, || layout().contains("focus=b"));
    until("the viewer asks for the mouse no more", || !fact(&view, "modes").contains("mouse1000"));
    assert!(inner.fux().args(["layout", "main", "pane", "a"]).ended().success());
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
    let asks = "printf '\\033[?1000h\\033[?1006h'; until [ -e \"$PANE/off\" ]; do sleep 0.05; done; \
                printf '\\033[?1000l'; until [ -e \"$PANE/on\" ]; do sleep 0.05; done; \
                printf '\\033[?1000h'; sleep 600";
    let a = inner.start("a", &["sh", "-c", asks]);
    inner.start("b", &["sh", "-c", "stty raw -echo; exec cat -v"]);
    until("the first asks for the mouse", || fact(&a, "modes") == "mousesgr mouse1000");
    lay(&inner.0, "* row(a b) focus=a\n");
    let view = outer.start("view", &viewer(&inner, &keys));
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
    let mut start = outer.fux();
    start.env("COLUMNS", "80").env("LINES", "1").args(["start", "view", "--"]).args(viewer(&inner, &keys));
    let view = outer.started(&mut start);
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
    let mut start = outer.fux();
    start.env("COLUMNS", "4").env("LINES", "3").args(["start", "view", "--"]).args(viewer(&inner, &keys));
    let view = outer.started(&mut start);
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
    let view = outer.start("view", &viewer(&inner, &keys));
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
    let view = outer.start("view", &viewer(&inner, &keys));
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
    let view = outer.start("view", &viewer(&inner, &keys));
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
    let view = outer.start("view", &viewer(&inner, &keys));
    let (screen, sent) = (|| read(&view.join("shown/screen")), || read(&view.join("clipboard")));
    // What the pane copies while it has the keys reaches the viewer's own terminal.
    until("the viewer has looked at the pane", || fact(&pane, "size") == "80 24");
    std::thread::sleep(Duration::from_millis(300));
    send(&pane.join("in"), "b25l\n");
    until("the first copy is passed on", || sent() == "one");
    // What it copies while the viewer shows a workspace with no pane is old when the viewer
    // comes back: it is not sent.
    let ctl = view_fifo(&inner.0);
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
    let start = session.fux().args(["start", "p", "--", "sleep", "600"]).begin();
    // (A few seconds, not the test's two minutes: a start that waits for the FIFO waits for good.)
    let status = start.end_by(Instant::now() + Duration::from_secs(4));
    assert!(status.as_ref().is_ok_and(|status| !status.success()), "{status:?}: the start was to say no at once");
}

#[test]
fn a_fifo_where_a_workspace_would_be_is_refused_at_once() {
    // A FIFO nobody reads or writes, under a workspace's name: it is no folder of panes, and
    // `fux layout` says so without waiting for its other end.
    let session = Session::new("fifoworkspace");
    fs::create_dir_all(&session.0).unwrap();
    assert!(Command::new("mkfifo").arg(session.0.join("main")).ended().success());
    let layout = session.fux().arg("layout").stdout(Stdio::null()).begin();
    let status = layout.end_by(Instant::now() + Duration::from_secs(4));
    assert!(status.as_ref().is_ok_and(|status| !status.success()), "{status:?}: the layout was to say no at once");
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
    let draws = "until [ -e \"$PANE/go\" ]; do sleep 0.05; done; i=0; while [ $i -lt 12 ]; do i=$((i + 1)); \
        printf '\\033[2J\\033[H\\033Pq\"1;1;10;6#0;2;100;0;0#0!10~\\033\\\\'; sleep 0.1; done; : > \"$PANE/drawn\"; sleep 600";
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
fn the_viewer_writes_to_a_fifo_under_a_panes_name_and_to_nothing_a_link_there_leads_to() {
    // A pane's folder that another than a holder wrote: under `in` and under `ctl` a link, each to a
    // file that is none of the pane's. The viewer shows the pane, asks it nothing and types nothing
    // into it: what a link leads to is not written, and a pane with no FIFO under `in` is not read.
    let (outer, inner) = (Session::new("wlouter"), Session::new("wlinner"));
    let (keys, pane) = (inner.0.join(".keys"), inner.0.join("main/linked"));
    fs::create_dir_all(&keys).unwrap();
    fs::create_dir_all(&pane).unwrap();
    let kept = [("in", inner.0.join("typed-kept")), ("ctl", inner.0.join("asked-kept"))];
    for (name, file) in &kept {
        fs::write(file, "not the pane's\n").unwrap();
        std::os::unix::fs::symlink(file, pane.join(name)).unwrap();
    }
    let view = outer.start("view", &viewer(&inner, &keys));
    let screen = || read(&view.join("shown/screen"));
    let written = || kept.iter().any(|(_, file)| read(file) != "not the pane's\n");
    until("the viewer shows the workspace", || fact(&view, "modes").contains("paste") || !screen().trim().is_empty());
    send(&view.join("in"), "typed\r");
    until("the viewer says what became of the keys", || written() || screen().contains("linked"));
    std::thread::sleep(Duration::from_millis(300));
    for (name, file) in &kept {
        assert_eq!(read(file), "not the pane's\n", "the viewer wrote through the link under `{name}`");
    }
    assert!(screen().contains("linked") && screen().contains("what you typed"), "{}", screen());
    // A file under the two names, and no link: it is not written either, and the viewer says what
    // is wrong with it.
    for (name, file) in &kept {
        fs::remove_file(pane.join(name)).unwrap();
        fs::copy(file, pane.join(name)).unwrap();
    }
    send(&view.join("in"), "typed again\r");
    until("the viewer says that it is no FIFO", || screen().contains("linked: its `in` is no FIFO"));
    assert!(["in", "ctl"].iter().all(|name| read(&pane.join(name)) == "not the pane's\n"), "a file was written");
}

#[test]
fn the_viewer_knows_a_pane_by_its_folder_and_asks_no_status_who_its_program_is() {
    // A pane's folder that no holder made, as the picture of a pane kept on another machine is: no
    // `status`, and so no program's number; FIFOs under `in` and `ctl`, which the test reads; a
    // `modes` that asks to be told of keys let go. The viewer asks it to take its tile's size, and
    // a key's release follows its press there.
    let (outer, inner) = (Session::new("idouter"), Session::new("idinner"));
    let (keys, pane) = (inner.0.join(".keys"), inner.0.join("main/made"));
    fs::create_dir_all(&keys).unwrap();
    fs::create_dir_all(&outer.0).unwrap();
    fs::write(outer.0.join("keyboard"), "31\n").unwrap();
    // (Made beside the workspace's panes under a name that is no pane's, and then given the name.)
    let make = |count: u8| {
        let new = inner.0.join(format!("main/.made.{count}"));
        fs::create_dir_all(new.join("main")).unwrap();
        std::os::unix::fs::symlink("main", new.join("shown")).unwrap();
        let screen = format!("made {count}\n");
        for (name, text) in [("modes", "keys11\n"), ("state", "running\n"), ("main/screen.styled", screen.as_str())] {
            fs::write(new.join(name), text).unwrap();
        }
        assert!(Command::new("mkfifo").arg(new.join("in")).arg(new.join("ctl")).ended().success());
        let fifos = (fuxix::open(new.join("in"), false).unwrap(), fuxix::open(new.join("ctl"), false).unwrap());
        let _ = fs::rename(&pane, inner.0.join(format!("main/.was.{count}")));
        fs::rename(&new, &pane).unwrap();
        (fifos.0, fifos.1, RefCell::new(String::new()))
    };
    // All that was written to a FIFO so far.
    let got = |mut fifo: &fs::File, all: &RefCell<String>| {
        let mut bytes = [0; 4096];
        while let Ok(n @ 1..) = fifo.read(&mut bytes) {
            all.borrow_mut().push_str(&String::from_utf8_lossy(&bytes[..n]));
        }
        all.borrow().clone()
    };
    let (typed, asked, all) = make(1);
    let view = outer.start("view", &viewer(&inner, &keys));
    until("the viewer asks its terminal for the keys the pane asks", || fact(&view, "modes").contains("keys11"));
    until("the pane is asked to take its tile's size", || got(&asked, &all).starts_with("resize 80 24 10 20\n"));
    until("its screen is drawn", || read(&view.join("shown/screen")).starts_with("made 1\n"));
    let key = |letter: u8, kind: u8| format!("\x1b[{letter};1:{kind}u");
    let all = RefCell::new(String::new());
    send(&view.join("in"), key(97, 1) + &key(97, 3));
    until("the key's release follows its press", || got(&typed, &all) == key(97, 1) + &key(97, 3));
    // Another folder under the pane's name is another pane: it is asked its size as the first was
    // (no number in a `status` tells the two apart: the folders do), and the release of a key that
    // went down in the folder before is nothing to it.
    send(&view.join("in"), key(98, 1));
    until("the next key is down", || got(&typed, &all).ends_with(&key(98, 1)));
    let (typed, asked, all) = make(2);
    until("the new folder is asked its size", || got(&asked, &all).starts_with("resize 80 24 10 20\n"));
    // And what is drawn is the new folder's own screen: what was read of the one before is not its
    // (neither has a `generation` to say that its files are others).
    until("the new folder's screen is drawn", || read(&view.join("shown/screen")).starts_with("made 2\n"));
    let all = RefCell::new(String::new());
    send(&view.join("in"), key(98, 3) + &key(99, 1) + &key(99, 3));
    until("a key after it is the new pane's", || got(&typed, &all).ends_with(&key(99, 3)));
    assert_eq!(got(&typed, &all), key(99, 1) + &key(99, 3));
}

#[test]
fn a_viewer_that_may_open_few_files_still_types_into_its_panes() {
    // Thirty panes side by side, and a viewer that may have 32 files open at a time (`ulimit -n`).
    // It holds a pane's folder open to know it again, where it can: not where that would leave it
    // none to open a pane's `in` with. What is typed reaches the pane, and a key's release too.
    let (outer, inner) = (Session::new("fewouter"), Session::new("fewinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    fs::create_dir_all(&outer.0).unwrap();
    fs::write(outer.0.join("keyboard"), "31\n").unwrap();
    fs::create_dir_all(&inner.0).unwrap();
    fs::write(inner.0.join("keyboard"), "31\n").unwrap();
    let first = inner.start("p1", &["sh", "-c", "stty raw -echo; printf '\\033[>11u'; exec cat -vt"]);
    let names: Vec<String> = (1..=30).map(|n| format!("p{n}")).collect();
    for name in &names[1..] {
        inner.start(name, &["sleep", "600"]);
    }
    lay(&inner.0, &format!("* row({}) focus=p1\n", names.join(" ")));
    let mut start = outer.fux();
    start.env("COLUMNS", "160").args(["start", "view", "--", "sh", "-c", "ulimit -n 32 && exec \"$@\"", "sh"]);
    start.args(viewer(&inner, &keys));
    let view = outer.started(&mut start);
    let last = inner.0.join("main/p30");
    until_or(
        "every pane has its tile's size",
        || read(&view.join("shown/screen")),
        || {
            fact(&last, "size").ends_with(" 24")
                && fact(&last, "size") != "80 24"
                && fact(&view, "modes").contains("keys11")
        },
    );
    send(&view.join("in"), "\x1b[97;1:1u\x1b[97;1:3uZ");
    until_or(
        "the key, its release and a letter are typed into the first pane",
        || format!("{}\n{}", read(&first.join("shown/screen")), read(&view.join("shown/screen"))),
        || read(&first.join("shown/screen")).replace('\n', "").contains("^[[97;1:1u^[[97;1:3uZ"),
    );
    assert!(!read(&view.join("shown/screen")).contains("Too many open files"));
}

#[test]
fn a_key_held_while_its_pane_is_renamed_is_held_and_let_go_in_that_pane() {
    // The folder is the pane, whatever it is called: a key that went down in it is still down in
    // it after `.rename`, and its repeat and its release go to the same program.
    let (outer, inner) = (Session::new("heldouter"), Session::new("heldinner"));
    let keys = inner.0.join(".keys");
    for session in [&outer, &inner] {
        fs::create_dir_all(&session.0).unwrap();
        fs::write(session.0.join("keyboard"), "31\n").unwrap();
    }
    fs::create_dir_all(&keys).unwrap();
    let pane = inner.start("a", &["sh", "-c", "stty raw -echo; printf '\\033[>11u'; exec cat -vt"]);
    let view = outer.start("view", &viewer(&inner, &keys));
    until("the viewer asked its terminal for the keys the pane asks", || fact(&view, "modes").contains("keys11"));
    let key = |letter: u8, kind: u8| format!("\x1b[{letter};1:{kind}u");
    send(&view.join("in"), key(97, 1));
    until("the key is down in the pane", || read(&pane.join("shown/screen")).contains("^[[97;1:1u"));
    let mut rename = inner.command(concat!(env!("CARGO_MANIFEST_DIR"), "/keys/.rename"));
    assert!(rename.arg(inner.0.join("main")).args(["a", "renamed"]).ended().success());
    let renamed = inner.0.join("main/renamed");
    send(&view.join("in"), key(97, 2) + &key(97, 3) + &key(98, 1) + &key(98, 3));
    let shown = || read(&renamed.join("shown/screen")).replace('\n', "");
    until("the next key has come and gone", || shown().ends_with("^[[98;1:1u^[[98;1:3u"));
    assert_eq!(shown(), "^[[97;1:1u^[[97;1:2u^[[97;1:3u^[[98;1:1u^[[98;1:3u");
    // A key's release is not lost on a name its pane had: it goes to the folder, under whatever name
    // that has by then. Here a key goes down, and the pane is moved by hand (`mv`: the layout knows
    // nothing of it, the pane leaves its tab, and the other pane has the keys). The key is let go
    // in the program it went down in, which the viewer has to find under its new name.
    // (The layout is edited under its lock: the viewer writes it too when a pane comes.)
    inner.start("other", &["sleep", "600"]);
    for edit in [&["split", "row", "other", "beside", "renamed"][..], &["pane", "renamed"]] {
        assert!(inner.fux().args(["layout", "main"]).args(edit).ended().success());
    }
    until("both panes are drawn", || read(&view.join("shown/screen")).contains('│'));
    send(&view.join("in"), key(99, 1));
    until("the key is down in the pane", || shown().ends_with("^[[99;1:1u"));
    let moved = inner.0.join("main/moved");
    fs::rename(&renamed, &moved).unwrap();
    until_or(
        "the key is let go in the pane that was moved",
        || read(&moved.join("shown/screen")),
        || read(&moved.join("shown/screen")).replace('\n', "").ends_with("^[[99;1:1u^[[99;1:3u"),
    );
}

#[test]
fn a_pane_whose_tab_is_shown_again_is_asked_its_size_once_more_and_no_more() {
    // The viewer knows a pane's folder while the pane is a tile. A pane whose tab was not shown for
    // a while may be another folder by then, for all the viewer knows: shown again, it is asked
    // once to take its tile's size, and then no more.
    let (outer, inner) = (Session::new("againouter"), Session::new("againinner"));
    let (keys, made) = (inner.0.join(".keys"), inner.0.join("main/a"));
    fs::create_dir_all(&keys).unwrap();
    fs::create_dir_all(made.join("main")).unwrap();
    std::os::unix::fs::symlink("main", made.join("shown")).unwrap();
    for (name, text) in [("state", "running\n"), ("main/screen.styled", "AYE\n")] {
        fs::write(made.join(name), text).unwrap();
    }
    assert!(Command::new("mkfifo").arg(made.join("in")).arg(made.join("ctl")).ended().success());
    let (_typed, mut asks) =
        (fuxix::open(made.join("in"), false).unwrap(), fuxix::open(made.join("ctl"), false).unwrap());
    inner.start("b", &["sh", "-c", "echo BEE; exec sleep 600"]);
    lay(&inner.0, "* a focus=a\n  b focus=b\n");
    let view = outer.start("view", &viewer(&inner, &keys));
    // How often the pane was asked so far.
    let all = RefCell::new(String::new());
    let mut asked = || {
        let mut bytes = [0; 4096];
        while let Ok(n @ 1..) = asks.read(&mut bytes) {
            all.borrow_mut().push_str(&String::from_utf8_lossy(&bytes[..n]));
        }
        all.borrow().matches("resize 80 24").count()
    };
    let shows = |word: &str| read(&view.join("shown/screen")).starts_with(word);
    until("the pane is drawn and asked its size", || shows("AYE") && asked() == 1);
    assert!(inner.fux().args(["layout", "main", "tab", "next"]).ended().success());
    until("the other tab is shown", || shows("BEE"));
    assert_eq!(asked(), 1);
    assert!(inner.fux().args(["layout", "main", "tab", "prev"]).ended().success());
    until("the pane is shown again, and asked again", || shows("AYE") && asked() == 2);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(asked(), 2, "{}", all.borrow());
}

#[test]
fn a_folder_given_the_number_of_the_one_before_it_is_another_pane_by_its_run() {
    // A pane is removed and its name started anew, and the new folder is given the number the old
    // one had (ext4 gives it; btrfs and APFS never do, so here the folder stays and what is in it
    // becomes another run's: to a viewer, which reads by the name, that is the same). Neither run
    // has a `generation` to say that its files are others. The `run` says it: the viewer draws
    // the screen of the run that has the name now, not what it read of the one before.
    let (outer, inner) = (Session::new("numberouter"), Session::new("numberinner"));
    let (keys, made) = (inner.0.join(".keys"), inner.0.join("main/a"));
    fs::create_dir_all(&keys).unwrap();
    fs::create_dir_all(made.join("main")).unwrap();
    std::os::unix::fs::symlink("main", made.join("shown")).unwrap();
    fs::write(made.join("state"), "running\n").unwrap();
    // (Each file whole at every moment, as a holder writes it.)
    let run = |number: u8, screen: &str| {
        for (name, text) in [("main/screen.styled", format!("{screen}\n")), ("run", format!("{number:032x}\n"))] {
            fs::write(made.join(".new"), text).unwrap();
            fs::rename(made.join(".new"), made.join(name)).unwrap();
        }
    };
    run(1, "AYE");
    inner.start("b", &["sh", "-c", "echo BEE; exec sleep 600"]);
    lay(&inner.0, "* a focus=a\n  b focus=b\n");
    let view = outer.start("view", &viewer(&inner, &keys));
    let shows = |word: &str| read(&view.join("shown/screen")).starts_with(word);
    until("the first run is drawn", || shows("AYE"));
    // While its tab is not shown (the sequence HANDOFF had as known, and left)…
    assert!(inner.fux().args(["layout", "main", "tab", "next"]).ended().success());
    until("the other tab is shown", || shows("BEE"));
    run(2, "SEE");
    assert!(inner.fux().args(["layout", "main", "tab", "prev"]).ended().success());
    until("the second run is drawn when its tab is shown again", || shows("SEE"));
    // …and while the viewer shows it.
    run(3, "DEE");
    until("the third run is drawn in its tile", || shows("DEE"));
}

#[test]
fn rename_gives_a_pane_another_name_and_keeps_its_place_in_the_layout() {
    use std::os::unix::fs::MetadataExt;
    let session = Session::new("rename");
    let main = session.0.join("main");
    let rename = |old: &str, new: &str| {
        session.command(concat!(env!("CARGO_MANIFEST_DIR"), "/keys/.rename")).arg(&main).args([old, new]).run()
    };
    let layout = || String::from_utf8(session.fux().arg("layout").run().stdout).unwrap();
    // Four panes in a split with shares: one named as the layout's own word, one a number.
    let build = session.start("build", &["sleep", "600"]);
    for name in ["log", "row", "2"] {
        session.start(name, &["sleep", "600"]);
    }
    lay(&session.0, "* row(build:50 col(log row:2) 2:50) focus=build\n");
    let (folder, status) = (fs::metadata(&build).unwrap().ino(), read(&build.join("status")));
    // The folder is moved and the layout names it anew, in its place, with its share and the keys:
    // the same folder, its program running on.
    let renamed = rename("build", "tests");
    assert!(renamed.status.success(), "{renamed:?}");
    assert_eq!(layout(), "* row(tests:50 col(log row:2) 2:50) focus=tests\n");
    assert!(!build.exists() && fs::metadata(main.join("tests")).unwrap().ino() == folder);
    assert!(read(&main.join("tests/status")) == status && fact(&main.join("tests"), "state") == "running");
    // A name is a name where a name stands: a share of the same digits is not one, nor is the
    // layout's own word of the same letters.
    assert!(rename("2", "50").status.success() && rename("50", "z").status.success());
    assert_eq!(layout(), "* row(tests:50 col(log row:2) z:50) focus=tests\n");
    assert!(rename("row", "r2").status.success());
    assert_eq!(layout(), "* row(tests:50 col(log r2:2) z:50) focus=tests\n");
    // Refused, and nothing changed: a name that is there, a pane that is not, and a name fux would
    // not make (a dot first, a line's end in it, a name a workspace has a file of).
    for (old, new) in [("log", "tests"), ("none", "x"), ("log", ".x"), ("log", "x\n"), ("log", "layout")] {
        let refused = rename(old, new);
        assert!(!refused.status.success() && !refused.stderr.is_empty(), "{old} {new:?}: {refused:?}");
    }
    assert_eq!(layout(), "* row(tests:50 col(log r2:2) z:50) focus=tests\n");
    let mut names: Vec<_> = fs::read_dir(&main).unwrap().flatten().map(|entry| entry.file_name()).collect();
    names.sort();
    assert_eq!(names, ["layout", "log", "r2", "tests", "z"]);
    // A layout that cannot be written once the folder is moved (a folder where it is put together):
    // the script says so and puts nothing back. The pane has its new name, and a tab of its own
    // when the layout is next settled.
    fs::create_dir(main.join(".layout.new")).unwrap();
    let unwritten = rename("log", "moved");
    let said = String::from_utf8_lossy(&unwritten.stderr).into_owned();
    assert!(unwritten.status.code() == Some(1) && said.contains("tab of its own"), "{unwritten:?}");
    assert!(main.join("moved/status").exists() && !main.join("log").exists());
    assert_eq!(read(&main.join("layout")), "* row(tests:50 col(log r2:2) z:50) focus=tests\n");
    fs::remove_dir(main.join(".layout.new")).unwrap();
    assert_eq!(layout(), "* row(tests:50 r2 z:50) focus=tests\n  moved focus=moved\n");
}

#[test]
fn the_windows_focus_is_not_told_to_a_program_that_asks_no_more() {
    // A program that asked to be told of the focus (mode 1004) and was told it has the keys. It
    // stops asking while the viewer is stopped, and the terminal says in that time that its window
    // lost the focus: the viewer, going on, reads that before it draws, and tells the program nothing.
    let (outer, inner) = (Session::new("focusout"), Session::new("focusin"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    let asks_then_stops = "stty raw -echo; printf '\\033[?1004h'; until [ -e \"$PANE/go\" ]; do sleep 0.05; done; printf '\\033[?1004l'; exec cat -vt";
    let one = inner.start("one", &["sh", "-c", asks_then_stops]);
    until("it asks", || fact(&one, "modes") == "focus");
    let view = outer.start("view", &viewer(&inner, &keys));
    until("the viewer asks its terminal for the focus", || fact(&view, "modes").contains("focus"));
    std::thread::sleep(Duration::from_millis(500));
    let pid = view_fifo(&inner.0).parent().unwrap().extension().unwrap().to_string_lossy().into_owned();
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

/// The command of a viewer of a session's `main`, with the keys of the directory named: for a pane
/// of another session, where this one is not the default one, and so is named by its path.
fn viewer(session: &Session, keys: &Path) -> Vec<String> {
    let main = session.0.join("main");
    vec![env!("CARGO_BIN_EXE_fux").into(), "view".into(), main.display().to_string(), keys.display().to_string()]
}

/// The FIFO of the one viewer of a session (`.view.PID/ctl`).
fn view_fifo(session: &Path) -> PathBuf {
    let files = fs::read_dir(session).unwrap().flatten().map(|entry| entry.path());
    let own = files.into_iter().find(|path| path.file_name().unwrap().to_string_lossy().starts_with(".view."));
    own.unwrap().join("ctl")
}

#[test]
fn a_name_that_is_a_number_of_no_line_is_chosen_without_a_word_of_complaint() {
    // With no finder, the list is numbered and a number or a name is asked for. `0` is no line's
    // number: it is the name of the second line here, and that is all that is said.
    let folder = Session::new("pickzero");
    let (bin, pane, list) = (folder.0.join("bin"), folder.0.join("pane"), folder.0.join("list"));
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&pane).unwrap();
    let links = "for tool in rm awk head; do ln -s \"$(command -v $tool)\" \"$0/$tool\"; done";
    assert!(Command::new("sh").arg("-c").arg(links).arg(&bin).ended().success());
    fs::write(&list, "one\tthe first\n0\tnought\n").unwrap();
    fs::write(folder.0.join("typed"), "0\n").unwrap();
    let mut pick = folder.command("/bin/sh");
    pick.arg(concat!(env!("CARGO_MANIFEST_DIR"), "/keys/.pick")).arg(&list).args(["echo", "chose"]);
    pick.env("PATH", &bin);
    let out = pick.run_from(Some(fs::File::open(folder.0.join("typed")).unwrap()));
    let (said, wrong) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(said.ends_with("chose 0\n") && wrong.is_empty(), "{said:?} {wrong:?}");
}

#[test]
fn a_shell_named_by_nothing_is_no_shell_named() {
    // `SHELL=` (set, and empty) is as if it were not set: `/bin/sh`.
    let session = Session::new("noshell");
    let out = session.fux().env("SHELL", "").args(["start", "e"]).run();
    assert!(out.status.success(), "start with SHELL empty: {}", String::from_utf8_lossy(&out.stderr));
    assert!(read(&session.0.join("main/e/status")).ends_with("command /bin/sh\n"));
}

#[test]
fn a_panes_status_names_its_holder_before_its_program_is_started() {
    // Who keeps a pane is in its `status` before the program's first instruction: the holder, which is
    // the program's parent. So a program that looks at once finds the pane it is in (`keys/.here`
    // looks so); its own number follows when it runs.
    let session = Session::new("statusfirst");
    let looks = "cat \"$PANE/status\" > \"$PANE/first\"; echo \"parent $PPID\" >> \"$PANE/first\"; exec sleep 600";
    let pane = session.start("p", &["sh", "-c", looks]);
    // (A line of its own: the `command` line it has copied by then says `parent` too.)
    until("the program has looked", || read(&pane.join("first")).contains("\nparent "));
    let (first, status) = (read(&pane.join("first")), read(&pane.join("status")));
    let lines: Vec<&str> = status.lines().collect();
    let holder = lines[0].strip_prefix("holder ").unwrap();
    assert!(lines.len() == 3 && lines[1].starts_with("pid ") && lines[2].starts_with("command sh -c cat "), "{status}");
    assert!(first.starts_with(&format!("holder {holder}\n")) && first.contains("\ncommand sh -c cat "), "{first}");
    assert!(first.ends_with(&format!("\nparent {holder}\n")), "{first}");
    // The order, without a race: a program that cannot be started was not tried before `status` said
    // who tries and what. That `status` stays, with no `pid` (no program ever ran here), beside the
    // `log` that says why; the FIFOs go, as nothing reads them.
    let none = session.fux().args(["start", "none", "--", "/no/such/program"]).run();
    assert!(String::from_utf8_lossy(&none.stderr).contains("cannot start /no/such/program"), "{none:?}");
    let (none, left) = (session.0.join("main/none"), read(&session.0.join("main/none/status")));
    let lines: Vec<&str> = left.lines().collect();
    assert!(lines.len() == 2 && lines[0].starts_with("holder ") && lines[1] == "command /no/such/program", "{left:?}");
    assert!(fs::symlink_metadata(none.join("in")).is_err() && fs::symlink_metadata(none.join("ctl")).is_err());
    // It has no `state` either: `fux wait` has no end to tell of, says so and fails.
    let waited = session.fux().arg("wait").arg(&none).run();
    let said = String::from_utf8_lossy(&waited.stderr).into_owned();
    assert!(waited.status.code() == Some(1) && waited.stdout.is_empty() && said.contains("/state: "), "{waited:?}");
}

#[test]
fn a_command_that_cannot_be_written_down_does_not_cost_the_pane_its_holder() {
    // The program is started, and then what it runs cannot be written: here a file may have 8 KiB
    // (`ulimit -f`, with the signal for it blocked, so that the write fails and nothing is killed)
    // and the arguments are 16. The holder goes on holding, and its `log` says what is missing: a
    // holder that left then left a program running that nothing kept.
    let session = Session::new("norecord");
    let blocks = "sigprocmask(SIG_BLOCK, POSIX::SigSet->new(SIGXFSZ)); exec @ARGV";
    let mut start = session.command("sh");
    start.args(["-c", "ulimit -f 16 && exec perl -MPOSIX -e \"$0\" \"$@\"", blocks, env!("CARGO_BIN_EXE_fux")]);
    start.env_remove("COLUMNS").env_remove("LINES");
    let pane =
        session.started(start.args(["start", "big", "--", "sh", "-c", "exec sleep 600", "sh", &"x".repeat(16 << 10)]));
    let (status, log) = (read(&pane.join("status")), read(&pane.join("log")));
    assert!(fact(&pane, "state") == "running" && status.lines().nth(1).is_some_and(|line| line.starts_with("pid ")));
    assert!(log.contains("command") && log.contains("File too large"), "{log}");
    assert!(!pane.join("command").exists() && pane.join("in").exists());
}

#[test]
fn here_says_which_pane_a_program_is_in_and_believes_only_a_folder_a_holder_keeps() {
    let session = Session::new("here");
    let here = concat!(env!("CARGO_MANIFEST_DIR"), "/keys/.here");
    let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
    let path = format!("{bin}:{}", std::env::var("PATH").unwrap());
    let (dir, out) = (session.0.display(), session.0.join(".out"));
    fs::create_dir_all(&out).unwrap();
    let said = |name: &str| read(&out.join(name));
    // In a pane's shell, in a shell that shell runs, and in a job of it in the background (the
    // session named, as it may be): the pane's folder.
    let pane = session.started(session.fux().env("PATH", &path).args(["start", "w/a", "--", "sh"]));
    let asks = format!(
        "'{here}' > '{0}/direct' 2>&1; sh -c '\"$0\" > \"$1/nested\" 2>&1' '{here}' '{0}'; '{here}' '{dir}' > '{0}/job' 2>&1 &\n",
        out.display()
    );
    send(&pane.join("in"), asks);
    until("the three have answered", || !said("job").is_empty() && !said("nested").is_empty());
    let folder = format!("{}\n", pane.display());
    assert_eq!((said("direct"), said("nested"), said("job")), (folder.clone(), folder.clone(), folder));
    // Outside a pane it fails, and says so. A `status` is anyone's to write: a folder that names
    // the asker (here its parent, this test) and that no holder keeps is not believed.
    let outside = || session.command(here).arg(&session.0).env("PATH", &path).run();
    let none = outside();
    assert!(none.status.code() == Some(1) && none.stdout.is_empty(), "{none:?}");
    assert!(String::from_utf8_lossy(&none.stderr).contains("not in a pane of "), "{none:?}");
    // With no session given it asks `fux dir`, and says so where there is no `fux` to ask.
    let asks = Command::new(here).env("PATH", "/nowhere").run();
    assert!(
        asks.status.code() == Some(1) && String::from_utf8_lossy(&asks.stderr).starts_with("here: no session"),
        "{asks:?}"
    );
    fs::create_dir_all(session.0.join("w/made")).unwrap();
    fs::write(session.0.join("w/made/status"), format!("holder 1\npid {}\ncommand sh\n", std::process::id())).unwrap();
    let made = outside();
    assert!(made.status.code() == Some(1) && made.stdout.is_empty(), "{made:?}");
    // Two folders that are kept and name one process are no answer: both are named, and it fails.
    let other = session.start("w/b", &["sleep", "600"]);
    fs::write(other.join("status"), read(&pane.join("status"))).unwrap();
    send(&pane.join("in"), format!("'{here}' > '{0}/twice' 2>&1; echo $? > '{0}/code'\n", out.display()));
    until("it has answered", || !said("code").is_empty());
    let twice = said("twice");
    assert!(said("code") == "1\n" && twice.contains("more than one pane names process"), "{twice}");
    assert!(twice.contains(&format!("{}\n", pane.display())) && twice.contains(&format!("{}\n", other.display())));
}

#[test]
fn a_program_that_writes_its_cwd_as_soon_as_it_runs_keeps_it() {
    // The program writes where it is (`pwd` into its pane's `cwd`, as the README says a shell may) as
    // its first act, before its holder's first screen or after: the file is the program's to write
    // too, and the holder removes none it did not write.
    let session = Session::new("owncwd");
    let says = "pwd > \"$PANE/cwd.mine\" && mv \"$PANE/cwd.mine\" \"$PANE/cwd\"; echo s''aid; exec sleep 600";
    let pane = session.start("p", &["sh", "-c", says]);
    let here = format!("{}\n", std::env::current_dir().unwrap().display());
    until("the program has said where it is", || read(&pane.join("shown/screen")).contains("said"));
    assert_eq!((fact(&pane, "state").as_str(), read(&pane.join("cwd"))), ("running", here));
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
    fs::write(keys.join("x"), "#!/bin/sh\necho \"$COLUMNS $LINES\" > session/told\n").unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("x")).ended();
    inner.start("one", &["sleep", "600"]);
    let views = |session: &Session, keys: &Path, first: &str| {
        let (dir, keys, fux) = (session.0.display(), keys.display(), env!("CARGO_BIN_EXE_fux"));
        format!("{first}; SHELL=/bin/sh exec '{fux}' view '{dir}/main' '{keys}'")
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
    let mut start = outer.fux();
    start.env("SHELL", "/bin/sh").args(["start", "view", "--"]).args(viewer(&inner, &keys));
    let view = outer.started(&mut start);
    until("the first workspace has its shell", || fact(&inner.0.join("main/1"), "state") == "running");
    // (The script by itself, as the viewer runs it: `fux` is the one beside the viewer.)
    let mut script = inner.key(&keys, "C-b/w/n", "main", Some(&inner.0.join("main/1")));
    let said = script.env("SHELL", "/bin/sh").run();
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
    // (`size` is written before the screen's `pictures` in the one round: both are waited for.)
    until("the screen is two rows and lists no picture", || {
        fact(&pane, "size") == "10 2" && fact(&pane, "shown/pictures").is_empty()
    });
    send(&pane.join("ctl"), "resize 80 24\n");
    until("the picture is listed again", || fact(&pane, "shown/pictures") == listed);
}

#[test]
fn a_restore_that_fails_leaves_the_run_before_whole_and_starts_the_others() {
    let session = Session::new("unrestored");
    let keys = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/keys"));
    let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
    // Two ended panes: one whose program is gone by the time it is started again, and one that
    // prints a word.
    fs::create_dir_all(&session.0).unwrap();
    let counts = session.0.join("counts");
    fs::write(&counts, "#!/bin/sh\nseq 1 60\n").unwrap();
    assert!(Command::new("chmod").arg("+x").arg(&counts).ended().success());
    let lost = session.start("w/lost", &[counts.to_str().unwrap()]);
    let told = session.start("w/told", &["echo", "told"]);
    for pane in [&lost, &told] {
        assert!(session.fux().arg("wait").arg(pane).ended().success());
    }
    assert!(read(&lost.join("history")).contains("\n17\n") && read(&told.join("shown/screen")).trim() == "told");
    fs::remove_file(&counts).unwrap();
    // (Run as a login runs it: by the script's path, with no session named: it is the default one.)
    // (A CDPATH that leads to the script's folder makes `cd` print where it went: the session is still one path.)
    let restore = || {
        let mut restore = session.command("sh");
        restore.arg("keys/.restore").env("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap()));
        restore.current_dir(env!("CARGO_MANIFEST_DIR")).env("CDPATH", env!("CARGO_MANIFEST_DIR"));
        restore.run()
    };
    let restored = restore();
    assert_eq!(String::from_utf8_lossy(&restored.stdout), "started w/told again\n");
    assert_eq!(restored.status.code(), Some(1), "a start failed: whoever ran it is told so");
    until_or(
        "the pane started again has printed its word, after the one of the run before",
        || read(&told.join("shown/screen")),
        || read(&told.join("shown/screen")).trim() == "told" && read(&told.join(".before")).trim() == "told",
    );
    // The start that failed put nothing back: the run before is whole where the script says, and
    // the name has the failed start's folder, with its `log` and no `command` (nothing ran there).
    let said = String::from_utf8_lossy(&restored.stderr).into_owned();
    let before = said.split("(the run before is in ").nth(1).and_then(|rest| rest.split(')').next()).map(PathBuf::from);
    let before = before.unwrap_or_else(|| panic!("{said}"));
    assert!(said.starts_with("w/lost: ") && said.contains("cannot start ") && before.starts_with(&session.0), "{said}");
    assert!(read(&before.join("history")).contains("\n17\n") && read(&before.join("state")) == "exited 0\n");
    assert!(read(&lost.join("log")).contains("cannot start ") && !lost.join("command").exists());
    // So it is not tried again, and nothing more is moved (the other has ended once more by now).
    assert!(session.fux().arg("wait").arg(&told).ended().success());
    let again = restore();
    assert_eq!(String::from_utf8_lossy(&again.stdout), "started w/told again\n");
    assert!(again.stderr.is_empty() && read(&before.join("history")).contains("\n17\n"), "{again:?}");
    assert!(again.status.success());
    // With no `fux` to start anything with (a login whose PATH has none yet), nothing is moved: before,
    // every pane that was over was moved aside, none was started, and the script ended well.
    let tools = session.0.join("tools");
    fs::create_dir_all(&tools).unwrap();
    let links =
        "for tool in sh perl mktemp mv rm rmdir sed cat dirname; do ln -s \"$(command -v $tool)\" \"$0/$tool\"; done";
    assert!(Command::new("sh").arg("-c").arg(links).arg(&tools).ended().success());
    assert!(session.fux().arg("wait").arg(&told).ended().success());
    let was = read(&told.join("status"));
    let mut bare = session.command("sh");
    bare.arg(keys.join(".restore")).arg(&session.0).env("PATH", &tools);
    let none = bare.run();
    assert!(none.status.code() == Some(1) && String::from_utf8_lossy(&none.stderr).contains("fux"), "{none:?}");
    assert_eq!(read(&told.join("status")), was);
    // Nor does it end well where it could move nothing: a session nothing can be made in (no place
    // to put the run that is over). The pane is as it was, and whoever ran the script is told.
    let mode = |mode: &str| assert!(Command::new("chmod").arg(mode).arg(&session.0).ended().success());
    mode("555");
    if fs::write(session.0.join("probe"), "").is_ok() {
        return mode("755"); // (Whoever may write anywhere, root, has no session it cannot move a pane in.)
    }
    let unmoved = restore();
    mode("755");
    assert!(unmoved.status.code() == Some(1) && !unmoved.stderr.is_empty() && unmoved.stdout.is_empty(), "{unmoved:?}");
    assert_eq!(read(&told.join("status")), was);
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
    let view = outer.start("view", &viewer(&inner, &keys));
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
fn a_paste_larger_than_a_pane_takes_at_once_is_typed_whole() {
    // A megabyte is pasted into a program that reads it more slowly than the viewer has it: the
    // pane's FIFO and its holder have room for a fraction of it at a time. All of it is typed, in
    // its order and once: what the FIFO does not take waits in the viewer for it.
    let (outer, inner) = (Session::new("p3outer"), Session::new("p3inner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    let reads = "stty raw -echo; while :; do dd bs=4096 count=8 2>/dev/null; sleep 0.02; done > \"$PANE/got\"";
    let pane = inner.start("a", &["sh", "-c", reads]);
    let view = outer.start("view", &viewer(&inner, &keys));
    until("the viewer has looked at the pane", || fact(&pane, "size") == "80 24");
    until("the terminal has said what it is like", || inner.0.join("keyboard").exists());
    let pasted: String = (0..1 << 17).map(|n| format!("{n:07}\n")).collect();
    send(&view.join("in"), format!("\x1b[200~{pasted}\x1b[201~"));
    let got = || fs::read(pane.join("got")).unwrap_or_default();
    until_or(
        "all of it arrives",
        || format!("{} of {} bytes", got().len(), pasted.len()),
        || got().len() >= pasted.len(),
    );
    assert!(got() == pasted.as_bytes(), "what arrived is not what was pasted");
}

#[test]
fn what_a_program_reads_none_of_for_two_seconds_is_dropped_and_the_keys_go_on() {
    // A program that reads nothing, and more is pasted than its pane has room for: the viewer waits
    // for it, the keys behind it, but not for good. After two seconds without a byte taken the rest
    // is dropped, the corner says so, and a key typed after the paste runs its script.
    let (outer, inner) = (Session::new("p4outer"), Session::new("p4inner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    fs::write(keys.join("C-t"), "#!/bin/sh\necho ran > session/ran\n").unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-t")).ended();
    let pane = inner.start("a", &["sh", "-c", "stty raw -echo; exec sleep 600"]);
    let view = outer.start("view", &viewer(&inner, &keys));
    until("the viewer has looked at the pane", || fact(&pane, "size") == "80 24");
    until("the terminal has said what it is like", || inner.0.join("keyboard").exists());
    let began = Instant::now();
    send(&view.join("in"), format!("\x1b[200~{}\x1b[201~", "x".repeat(1 << 20)));
    until("the corner says what was dropped", || read(&view.join("shown/screen")).contains("a reads nothing: "));
    assert!(began.elapsed() >= Duration::from_secs(2), "the viewer did not wait for the pane: {:?}", began.elapsed());
    send(&view.join("in"), "\x14");
    until("the key's script ran", || inner.0.join("ran").exists());
}

#[test]
fn a_key_script_is_told_the_size_the_terminal_has_when_the_key_is_typed() {
    // The terminal is made larger and a key typed at once, before the viewer has looked at its size
    // again (the holder reads `ctl` before `in`): the key's script is told the size the terminal has now.
    let (outer, inner) = (Session::new("szouter"), Session::new("szinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    let tells = "#!/bin/sh\necho \"$COLUMNS $LINES, $(cat row), $(cat col)\" > session/told\n";
    fs::write(keys.join("x"), tells).unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("x")).ended();
    let pane = inner.start("one", &["sleep", "600"]);
    let view = outer.start("view", &viewer(&inner, &keys));
    until("the viewer has looked at the pane", || fact(&pane, "size") == "80 24");
    // (At rest the viewer looks at its files, and at its terminal's size, every 50 ms.)
    std::thread::sleep(Duration::from_millis(3200));
    send(&view.join("ctl"), "resize 83 27\n");
    send(&view.join("in"), "x");
    until("the key's script ran", || read(&inner.0.join("told")).ends_with('\n'));
    assert_eq!(read(&inner.0.join("told")), "83 27, 41 27, 83 13\n");
}

#[test]
fn what_is_typed_before_a_workspace_with_no_pane_is_first_drawn_goes_to_its_shell() {
    // A letter that waits on the terminal when the viewer starts in a workspace with no pane (`dd`
    // took the byte before it): it is typed to the shell that workspace gets, not to nobody.
    let (outer, inner) = (Session::new("fzouter"), Session::new("fzinner"));
    let (dir, keys, fux) = (inner.0.display(), inner.0.join(".keys"), env!("CARGO_BIN_EXE_fux"));
    let views = format!(
        "stty raw -echo; dd bs=1 count=1 >/dev/null 2>&1; SHELL=/bin/sh exec '{fux}' view '{dir}/main' '{}'",
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
    let (outer, inner) = (Session::new("wtouter"), Session::new("wtinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b/w")).unwrap();
    fs::write(keys.join("C-b/w/l"), "#!/bin/sh\necho workspace other\nsleep 0.2\n").unwrap();
    let empty = "#!/bin/sh\nsleep 1\np=$(fux start ./workspace/1 -- cat) && fux layout ./workspace pane 1\n";
    fs::write(keys.join(".empty"), empty).unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/w/l")).arg(keys.join(".empty")).ended();
    inner.start("one", &["sh", "-c", "echo READY; exec cat"]);
    fs::create_dir_all(inner.0.join("other")).unwrap();
    let view = outer.start("view", &viewer(&inner, &keys));
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
    let key =
        "#!/bin/sh\necho workspace other\nsleep 1\nexec fux start \"$(readlink session)/other/1\" -- cat >/dev/null\n";
    fs::write(keys.join("C-b/w/l"), key).unwrap();
    fs::write(keys.join(".empty"), "#!/bin/sh\nexit 0\n").unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/w/l")).arg(keys.join(".empty")).ended();
    inner.start("one", &["sh", "-c", "echo READY; exec cat"]);
    fs::create_dir_all(inner.0.join("other")).unwrap();
    let view = outer.start("view", &viewer(&inner, &keys));
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
fn a_restore_that_cannot_copy_what_a_pane_printed_keeps_the_run_before_whole() {
    // `.restore` moves a pane that is over out of its workspace and starts its name anew; what the
    // old run printed is copied after that. Where it cannot be (a `screen` that cannot be read), the
    // old folder stays, whole, where the script says, and the new pane has no half of a copy.
    let session = Session::new("unreadable");
    let keys = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/keys"));
    let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
    let pane = session.start("w/kept", &["echo", "KEEP"]);
    assert!(session.fux().arg("wait").arg(&pane).ended().success());
    assert!(Command::new("chmod").arg("000").arg(pane.join("main/screen")).ended().success());
    if fs::read(pane.join("main/screen")).is_ok() {
        return; // (Whoever may read every file, root, has no file that cannot be copied.)
    }
    let mut restore = session.command("sh");
    restore.arg(keys.join(".restore")).env("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap()));
    let restored = restore.run();
    let (said, wrong) = (String::from_utf8_lossy(&restored.stdout), String::from_utf8_lossy(&restored.stderr));
    assert_eq!(said, "started w/kept again\n", "{wrong}");
    assert_eq!(restored.status.code(), Some(1), "a folder was left aside: whoever ran it is told so");
    let before = wrong.lines().find_map(|line| line.strip_prefix("w/kept: what it had printed is in "));
    let before = PathBuf::from(before.unwrap_or_else(|| panic!("{wrong}")));
    assert!(wrong.contains("screen"), "cat says which file it could not read: {wrong}");
    assert!(read(&before.join("status")).ends_with("command echo KEEP\n") && !pane.join(".before").exists());
    until("the new pane has printed", || read(&pane.join("main/screen")).starts_with("KEEP\n"));
}

#[test]
fn the_focus_goes_to_the_pane_that_is_beside_it_in_the_terminal_a_key_is_pressed_in() {
    // Of `row(col(a b:99) col(c d:9999))` in 80 columns and 24 rows, a and c have the first row
    // each: c is the pane to the right of a, and a key's script is told the terminal's size.
    let session = Session::new("diag");
    ["a", "b", "c", "d"].iter().for_each(|pane| fs::create_dir_all(session.0.join("main").join(pane)).unwrap());
    lay(&session.0, "* row(col(a b:99) col(c d:9999)) focus=a\n");
    let mut edit = session.fux();
    edit.env("COLUMNS", "80").env("LINES", "24").args(["layout", "main", "focus", "right"]);
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
    let mut script = session.key(&keys, "C-b/w/n", "main", None);
    script.env("PATH", format!("{}:{bin}:{}", shim.display(), std::env::var("PATH").unwrap()));
    let said = script.env("SHELL", "/bin/sh").run();
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
    // (An editor of the test's own, which `.edit` gives the file and no line: it is none it knows.)
    let ed = session.0.join("ed");
    fs::write(&ed, "#!/bin/sh\ncat \"$1\" > \"$OUT.new\" && mv \"$OUT.new\" \"$OUT\"\n").unwrap();
    assert!(Command::new("chmod").arg("+x").arg(&ed).ended().success());
    let mut script = session.key(Path::new(keys), "C-b/S-c", "main", Some(&pane));
    let said = script.env("OUT", &out).env("EDITOR", &ed).run();
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
    let view = outer.start("view", &viewer(&inner, Path::new(&keys)));
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
fn a_text_and_a_list_stay_in_the_session_and_go_when_their_script_ends_or_is_hung_up() {
    // The editor's text and the picker's list are dot-named files of the session, where their key
    // made them: no script writes a name into a pane's folder. Each is removed by the script that
    // has it when that ends.
    let session = Session::new("scratch");
    fs::create_dir_all(session.0.join("pane")).unwrap();
    let script = |name: &str| format!("{}/keys/{name}", env!("CARGO_MANIFEST_DIR"));
    let file = |name: &str, text: &str| {
        fs::write(session.0.join(name), text).unwrap();
        session.0.join(name)
    };
    let (text, list, typed) =
        (file(".text.case", "a transcript\n"), file(".pick.case", "a\tpane a\n"), file("typed", "a\n"));
    for (name, args) in [
        (".edit", vec!["--in", text.to_str().unwrap(), "1", "cat"]),
        (".pick", vec![list.to_str().unwrap(), "echo", "chose"]),
    ] {
        let mut helper = session.command("sh");
        helper.arg(script(name)).args(args);
        let out = helper.run_from(Some(fs::File::open(&typed).unwrap()));
        let said = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(said.ends_with(if name == ".edit" { "a transcript\n" } else { "chose a\n" }), "{name}: {out:?}");
    }
    assert!(!text.exists() && !list.exists(), "left behind: text {} list {}", text.exists(), list.exists());
    assert!(fs::read_dir(session.0.join("pane")).unwrap().next().is_none(), "something was put into the pane's folder");
    // Also when its pane is hung up under it, started as the keys start it (by `.shell`, which the
    // hangup ends first: that is what hangs up the rest): here the holder is killed outright, under
    // an editor that never ends by itself and under the list's question.
    let (text, list) = (file(".text.hup", "a transcript\n"), file(".pick.hup", "a\tpane a\n"));
    let (here, edit) = (session.0.to_str().unwrap(), ["sh", "-c", "echo ed''iting; exec sleep 600", "sh"]);
    let mut edits = session.fux();
    edits.args(["start", "w/edits", "--", "sh", &script(".shell"), here]);
    let edits = session.started(edits.args(["sh", &script(".edit"), "--in", text.to_str().unwrap(), "1"]).args(edit));
    // (No finder on the list's PATH: it asks, and waits for a line.)
    let bin = session.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let links = "for tool in sh ls rm awk; do ln -s \"$(command -v $tool)\" \"$0/$tool\"; done";
    assert!(Command::new("sh").arg("-c").arg(links).arg(&bin).ended().success());
    let mut picks = session.fux();
    picks.args(["start", "w/picks", "--", "env"]).arg(format!("PATH={}", bin.display()));
    let picks =
        session.started(picks.args(["sh", &script(".shell"), here, "sh", &script(".pick")]).arg(&list).arg("echo"));
    until("the editor and the list are open", || {
        read(&edits.join("shown/screen")).contains("editing") && read(&picks.join("shown/screen")).contains("which (")
    });
    assert!(text.exists() && list.exists());
    for pane in [&edits, &picks] {
        let holder = read(&pane.join("status")).lines().next().unwrap().trim_start_matches("holder ").to_string();
        assert!(Command::new("kill").args(["-KILL", &holder]).ended().success());
    }
    until("the text and the list are gone", || !text.exists() && !list.exists());
    // `.search` has two files of its own while it looks through the panes: the list it makes, and
    // each pane's text in turn. Here it never gets past one pane (a folder whose `history` is a FIFO
    // nobody writes), and is hung up there: both files go.
    let slow = session.0.join("w/slow");
    fs::create_dir_all(slow.join("main")).unwrap();
    fs::write(slow.join("main/lines"), "0\n\n").unwrap();
    assert!(Command::new("mkfifo").arg(slow.join("history")).ended().success());
    let mut search = session.fux();
    search.args(["start", "w/search", "--", "sh", &script(".shell"), here]);
    let search = session.started(search.args(["sh", &script(".search")]).arg(session.0.join("w")));
    until("it asks", || read(&search.join("shown/screen")).contains("search every pane for:"));
    send(&search.join("in"), "x\n");
    let scratch = || {
        let files = fs::read_dir(&session.0).unwrap().flatten().map(|file| file.file_name().into_string().unwrap());
        files.filter(|name| name.starts_with(".pick.") || name.starts_with(".text.")).count()
    };
    until("it has its list and a pane's text", || scratch() == 2);
    let holder = read(&search.join("status")).lines().next().unwrap().trim_start_matches("holder ").to_string();
    assert!(Command::new("kill").args(["-KILL", &holder]).ended().success());
    until("the list and the text are gone", || scratch() == 0);
}

#[test]
fn a_start_from_a_directory_that_is_gone_claims_no_name() {
    // What can refuse a start before a folder is made refuses it before: no name is taken for nothing.
    let session = Session::new("nowhere");
    let gone = session.0.join("gone");
    fs::create_dir_all(&gone).unwrap();
    let mut start = session.command("sh");
    start.args(["-c", r#"rmdir "$PWD" && exec "$0" start w/ -- true"#, env!("CARGO_BIN_EXE_fux")]);
    let out = start.current_dir(&gone).run();
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
fn the_marks_and_how_many_were_dropped_are_of_one_round_so_a_reader_numbers_each_mark() {
    // A program that marks as a shell does, four marks a command: command i (from 0) ends with the
    // code i % 100, and its `end` is mark number 4i + 4. Three hundred in one write when it is told
    // to go, and then without end.
    let command =
        r#"printf '\033]133;A\007$ \033]133;B\007run\n\033]133;C\007x\n\033]133;D;%s\007' $((i % 100)); i=$((i + 1))"#;
    let shell = format!(
        "i=0; read go; printf '%s' \"$(while [ $i -lt 300 ]; do {command}; done)\"; i=300; read go; while :; do {command}; done"
    );
    let session = Session::new("dropped");
    let pane = session.start("marks", &["sh", "-c", &shell]);
    // A reading: the two files, kept when `generation` is the same even number before and after.
    // Line n of `marks` (from 1) is mark number `marks.dropped` + n, if the two are of one round.
    let number = |name: &str| fact(&pane, name).parse::<u64>().ok();
    let reading = || {
        let (before, dropped, marks) = (number("generation"), number("marks.dropped"), read(&pane.join("marks")));
        let ends = (dropped? + 1..).zip(marks.lines()).filter_map(|(n, line)| Some((n, line.split_once(" end ")?.1)));
        let apart = ends.filter(|(n, code)| n % 4 != 0 || code.parse().ok() != Some((n / 4 - 1) % 100)).count();
        let kept = before.is_some_and(|n| n % 2 == 0) && number("generation") == before;
        kept.then_some((dropped?, marks.lines().count(), apart))
    };
    // No mark yet: none was dropped, and the file says so.
    assert_eq!((fact(&pane, "marks.dropped").as_str(), read(&pane.join("marks")).as_str()), ("0", ""));
    // Three hundred commands' marks since that reading: the file keeps the last thousand, and the
    // number says that two hundred more came and went.
    send(&pane.join("in"), "\r");
    until_or("the 1,200 marks are counted", || format!("{:?}", reading()), || reading() == Some((200, 1000, 0)));
    // Then marks without end: whenever the files are of one round, each `end` has its own number.
    send(&pane.join("in"), "\r");
    let (end, mut kept, mut apart, mut dropped) = (deadline(), 0, 0, 200);
    while Instant::now() < end && apart == 0 && (kept < 100 || dropped < 10_000) {
        if let Some((now, lines, wrong)) = reading() {
            assert!(now >= dropped && lines == 1000, "{now} dropped after {dropped}, {lines} marks");
            (kept, apart, dropped) = (kept + 1, wrong, now);
        }
    }
    assert!(
        kept >= 100 && apart == 0 && dropped >= 10_000,
        "{kept} reads kept, {apart} marks apart, {dropped} dropped"
    );
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
    let pane = session.started(&mut start);
    until("the program has ended", || fact(&pane, "state") == "exited 0");
    assert!(session.fux().arg("wait").arg(&pane).ended().success());
    // (The line was ended where history was put aside, and `log` says so each time.)
    assert!(
        pane.join("history.styled.1").exists() && read(&pane.join("log")).matches("was ended in history").count() == 2
    );
    // The pane's text as the keys make it, and the marks' places in it.
    let text = session.0.join("text");
    let mut script = session.command(concat!(env!("CARGO_MANIFEST_DIR"), "/keys/.text"));
    let add = script.arg(&pane).arg(&text).run().stdout;
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
    let prints = "read x; seq 1 200000; : > \"$PANE/printed\"; echo done; sleep 600";
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
    let mut start = outer.fux();
    start.env("COLUMNS", "80").env("LINES", "12").args(["start", "view", "--"]).args(viewer(&inner, &keys));
    let view = outer.started(&mut start);
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
    let project = session.0.join(".project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("history"), "precious\n").unwrap();
    fs::write(project.join("state"), "keep\n").unwrap();
    let out = session
        .command(env!("CARGO_BIN_EXE_fux"))
        .args(["hold", ".", "true"])
        .current_dir(&project)
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
        let out = session.key(&keys, script, ws, None).env("LC_ALL", "en_US.UTF-8").run();
        String::from_utf8(out.stdout).unwrap()
    };
    // Wrapping and skipping invalid directory names (`bad space`, `colours`), in byte order (`Alpha` < `beta`).
    assert_eq!(run("C-b/w/l", "Alpha"), "workspace beta\n");
    assert_eq!(run("C-b/w/l", "beta"), "workspace Alpha\n");
    assert_eq!(run("C-b/w/h", "Alpha"), "workspace beta\n");
    assert_eq!(run("C-b/w/h", "beta"), "workspace Alpha\n");
    // New workspace folder has mode 0700 like `fux start` creates.
    let out = session.key(&keys, "C-b/w/n", "Alpha", None).env("SHELL", "/bin/sh").run();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let mode = fs::metadata(session.0.join("w1")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "w1 mode was {mode:o}");
}

#[test]
fn two_restores_of_a_session_at_once_run_one_after_the_other() {
    // `.restore` looks whether a pane is over and then moves its folder away. Two runs at once did
    // both to one pane: the second moved away the folder the first had just started anew, alive.
    // A run has the session's folder locked from its first look to its last start, and another waits.
    let session = Session::new("tworestores");
    let keys = Path::new(env!("CARGO_MANIFEST_DIR")).join("keys");
    let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
    let restore = || {
        let mut restore = session.command("sh");
        restore.arg(keys.join(".restore")).arg(&session.0);
        restore.env("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap()));
        restore
    };
    let panes: Vec<PathBuf> = (1..=12).map(|n| session.start(&format!("c/{n}"), &["sleep", "600"])).collect();
    let holders = || panes.iter().map(|pane| fact(pane, "status").lines().next().unwrap()[7..].to_string()).collect();
    let alive = |pid: &String| Command::new("kill").args(["-0", pid]).stderr(Stdio::null()).ended().success();
    let kill = |holders: &Vec<String>| {
        assert!(Command::new("kill").arg("-KILL").args(holders).ended().success());
        until("the holders are gone", || !holders.iter().any(alive));
    };
    let first: Vec<String> = holders();
    kill(&first);
    // While the session's folder is locked by another (here by the test, as a run locks it), a run
    // waits: it has moved nothing and started nothing.
    let lock = fs::File::open(&session.0).unwrap();
    lock.lock().unwrap();
    let waits = restore().stdout(Stdio::null()).begin();
    std::thread::sleep(Duration::from_millis(400));
    assert!(waits.over().is_none() && holders() == first, "a run did not wait for the session's lock");
    drop(lock);
    assert!(waits.end().success());
    let second: Vec<String> = holders();
    assert!(second.iter().all(alive) && second.iter().all(|holder| !first.contains(holder)), "{second:?}");
    // Two runs at once: every pane is started once, by one of them, and nothing is left aside.
    kill(&second);
    let (one, other) = std::thread::scope(|runs| {
        let (one, other) = (runs.spawn(|| restore().run()), runs.spawn(|| restore().run()));
        (one.join().unwrap(), other.join().unwrap())
    });
    let said = format!("{}{}", String::from_utf8_lossy(&one.stdout), String::from_utf8_lossy(&other.stdout));
    let wrong = format!("{}{}", String::from_utf8_lossy(&one.stderr), String::from_utf8_lossy(&other.stderr));
    assert!(one.status.success() && other.status.success() && wrong.is_empty(), "{wrong}");
    assert_eq!(said.lines().filter(|line| line.starts_with("started c/")).count(), 12, "{said}");
    let third: Vec<String> = holders();
    assert!(third.iter().all(alive) && third.iter().all(|holder| !second.contains(holder)), "{third:?}");
    let left = fs::read_dir(&session.0).unwrap().flatten().map(|entry| entry.file_name().into_string().unwrap());
    assert_eq!(left.filter(|name| name.starts_with('.')).collect::<Vec<_>>(), [""; 0]);
}

#[test]
fn restore_skips_viewer_and_helper_panes() {
    let session = Session::new("restoreskip");
    let keys = Path::new(env!("CARGO_MANIFEST_DIR")).join("keys");
    let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display();
    for (name, cmd) in [
        ("v1", format!("{} view w", env!("CARGO_BIN_EXE_fux"))),
        ("v2", "fux view w".to_string()),
        ("v3", "fux-next view w".to_string()),
        ("h1", "/opt/fux-next hold /tmp sh".to_string()),
        ("ed", format!("{}/.shell {}/.edit --in /tmp/x 1", keys.display(), keys.display())),
        ("pk", format!("{}/.shell {}/.pick /tmp/x", keys.display(), keys.display())),
        ("mirror", "sleep 600".to_string()),
        ("old", "sleep 600".to_string()),
    ] {
        let dir = session.0.join("w").join(name);
        fs::create_dir_all(dir.join("main")).unwrap();
        fs::write(dir.join("status"), format!("holder 99999\npid 99998\ncommand {cmd}\n")).unwrap();
        fs::write(dir.join("command"), cmd.replace(' ', "\0") + "\0").unwrap();
        fs::write(dir.join("state"), "exited 0\n").unwrap();
        fs::write(dir.join("main/screen"), "\n").unwrap();
    }
    // Nor one whose `in` somebody reads, though no holder keeps its folder: the picture of a pane
    // that is kept elsewhere. And none that has no `command`: nothing says what to start.
    let reads = session.0.join("w/mirror/in");
    assert!(Command::new("mkfifo").arg(&reads).ended().success());
    let reader = fuxix::open(&reads, false).unwrap();
    fs::remove_file(session.0.join("w/old/command")).unwrap();
    let restore = || {
        let mut restore = session.command("sh");
        restore.arg(keys.join(".restore")).env("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap()));
        restore.run()
    };
    let out = restore();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "", "should skip viewer, helper and mirrored panes: {out:?}");
    assert!(out.status.success() && out.stderr.is_empty(), "a viewer under another name was not passed over: {out:?}");
    // (The same folder with nobody reading its `in` is a pane that is over, and is started.)
    drop(reader);
    assert_eq!(String::from_utf8_lossy(&restore().stdout), "started w/mirror again\n");
    // Whose pane it is, is its program's first argument as it was given, whole: a line's end in an
    // argument is a character of it. A program whose first argument only begins with the line `view`
    // is started anew, and a viewer whose own name has a line's end in it is passed over.
    let odd = session.0.join("fux\nnext");
    fs::write(&odd, "#!/bin/sh\nexit 0\n").unwrap();
    assert!(Command::new("chmod").arg("+x").arg(&odd).ended().success());
    let panes = [
        session.start("x/odd", &[odd.to_str().unwrap(), "view"]),
        session.start("x/prints", &["printf", "view\nno-viewer"]),
    ];
    for pane in &panes {
        assert!(session.fux().arg("wait").arg(pane).ended().success());
    }
    let out = restore();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "started x/prints again\n", "{out:?}");
    assert!(out.status.success() && out.stderr.is_empty(), "{out:?}");
}

#[test]
fn bracketed_paste_does_not_leak_while_prefix_menu_is_open() {
    let (outer, inner) = (Session::new("pmouter"), Session::new("pminner"));
    let keys = inner.0.join(".keys");
    let pane = inner.start("main/one", &["sh", "-c", "printf 'READY\\n'; cat"]);
    until("pane ready", || read(&pane.join("shown/screen")).contains("READY"));
    let mut start = outer.fux();
    start.env("COLUMNS", "80").env("LINES", "12").args(["start", "view", "--"]).args(viewer(&inner, &keys));
    let view = outer.started(&mut start);
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
cat > "{0}"
pwd -P > "{0}.in"
python3 - "{0}" "{1}" <<'PY'
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
            inner.0.join(".items").display(),
            picked.display()
        ),
    )
    .unwrap();
    assert!(Command::new("chmod").arg("+x").arg(&fzf).ended().success());
    let p1 = inner.start("main/one", &["sh", "-c", "printf 'PANE_ONE\\n'; sleep 600"]);
    until("p1 printed", || read(&p1.join("shown/screen")).contains("PANE_ONE"));
    let path = format!("{}:{}", fakebin.display(), std::env::var("PATH").unwrap());
    let mut start = outer.fux();
    start.env("COLUMNS", "80").env("LINES", "12").env("PATH", &path).args(["start", "view", "--"]);
    start.args(viewer(&inner, &keys));
    let view = outer.started(&mut start);
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
    // (The list's pane was started from where the viewer was started, not from the folder of the key's
    // script, which was gone before the finder had drawn.)
    let here = std::env::current_dir().unwrap().canonicalize().unwrap();
    assert_eq!(read(&inner.0.join(".items.in")), format!("{}\n", here.display()));
    // Also verify a cancelled finder (non-zero exit with non-empty stdout) does not run the command.
    let marker = inner.0.join(".ran");
    let list = inner.0.join(".list");
    fs::write(&list, "one\tone\n").unwrap();
    let cancel_pane = inner.start(
        "main/cancel",
        &[
            "env",
            &format!("PATH={path}"),
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
fn a_kitty_graphics_command_leaves_a_pane_as_it_was_and_its_program_is_told_only_what_the_terminal_is() {
    // A program asks whether Kitty graphics are drawn, the way programs do: the question, then
    // "what are you", and the first answer is the answer to both. It keeps the eleven bytes that
    // come first, sends an image to be shown where the cursor is, and prints a word.
    let session = Session::new("nokitty");
    fs::create_dir_all(&session.0).unwrap();
    let answer = session.0.join("answer");
    let asks = format!(
        "stty raw -echo; printf '\\033_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\\033\\\\\\033[c'; \
         dd bs=1 count=11 2>/dev/null > '{}'; \
         printf '\\033_Ga=T,i=7,f=24,s=1,v=1;AAAA\\033\\\\word'; sleep 600",
        answer.display()
    );
    let pane = session.start("asks", &["sh", "-c", &asks]);
    // It is told what the terminal is, and nothing about graphics before that.
    until("the program has its answer", || fs::read(&answer).is_ok_and(|answer| answer.len() == 11));
    assert_eq!(read(&answer), "\x1b[?62;4;22c");
    // The image is nothing to the pane: no picture listed, no file for one, no cell taken, the
    // cursor where the word ends, and not a line in the log.
    until("the word is on the screen", || fact(&pane, "shown/screen").contains("word"));
    let files = || fs::read_dir(&pane).unwrap().filter_map(|file| file.ok()?.file_name().into_string().ok());
    assert_eq!(
        (fact(&pane, "shown/screen").as_str(), fact(&pane, "cursor").as_str(), fact(&pane, "shown/pictures").as_str()),
        ("word", "0 4 shown 0", "")
    );
    assert_eq!((files().filter(|name| name.starts_with("pic-")).count(), read(&pane.join("log")).as_str()), (0, ""));
}

/// A terminal that is the test itself: the other end of the one a viewer is started on. It keeps
/// every byte the viewer writes, and says nothing but what the test types.
struct Terminal(fs::File, RefCell<Vec<u8>>);

impl Terminal {
    /// A terminal of 80 by 24 cells of 10 by 20 pixels, and a viewer of the session's `main` started on it.
    fn with_viewer(session: &Session, keys: &Path) -> (Terminal, Run) {
        let size = fuxix::Winsize { rows: 24, cols: 80, xpixel: 800, ypixel: 480 };
        let (terminal, viewers) = fuxix::pty(size).unwrap();
        let view = session.fux().args(["view", "main"]).arg(keys).stdin(viewers).stdout(Stdio::null()).begin();
        (Terminal(terminal, RefCell::default()), view)
    }
    /// All that the viewer has written so far, as text.
    fn seen(&self) -> String {
        let (mut bytes, mut kept) = ([0; 1 << 16], self.1.borrow_mut());
        while let Ok(n @ 1..) = (&self.0).read(&mut bytes) {
            kept.extend_from_slice(&bytes[..n]);
        }
        String::from_utf8_lossy(&kept).into_owned()
    }
}

#[test]
fn the_viewer_draws_a_sixel_file_where_its_terminal_draws_sixel_and_sends_no_other_picture_to_any() {
    // A pane whose program drew a sixel picture, printed a word under it and ended. Its list of
    // pictures is then written by hand, each line the file and its place and no more: first a PNG,
    // which is no picture to fux, then the sixel picture.
    let session = Session::new("onlysixel");
    let keys = session.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    let picture = "q\"1;1;20;12#1;2;100;0;0!20~-!20~";
    let pane = session.start("drew", &["sh", "-c", &format!("printf '\\033P%s\\033\\\\DREW' '{picture}'")]);
    assert!(session.fux().arg("wait").arg(&pane).ended().success());
    assert_eq!(fact(&pane, "main/pictures"), "pic-1.sixel 0 0 2 1 10 20");
    // (Ten pixels by twenty, blue: a file any program that reads PNG shows.)
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x0a\0\0\0\x14\x08\x02\0\0\0\x3b\x8c\x3b\x01\0\0\0\x14IDAT\
        \x78\xda\x63\x60\x60\xf8\x8f\x17\x8d\x4a\x8f\x4a\x53\x49\x1a\0\x87\x14\xc7\x39\x1a\x13\xdb\x92\0\0\0\0IEND\xae\x42\x60\x82";
    fs::write(pane.join("pic-2.png"), png).unwrap();
    fs::write(pane.join("main/pictures"), "pic-2.png 3 0\npic-1.sixel 0 0\n").unwrap();
    // The test is the viewer's terminal: one that says it draws sixel (the 4 in what it is), then
    // one that says it does not.
    for (what_it_is, drawn) in [("\x1b[?62;4;22c", vec![picture]), ("\x1b[?62;22c", vec![])] {
        let (terminal, view) = Terminal::with_viewer(&session, &keys);
        // What it is is the last thing a viewer asks. After the answer a key is typed, which the
        // pane cannot take: when the viewer says so, it has read the answer and drawn with it.
        until("the viewer has asked its terminal what it is", || terminal.seen().contains("\x1b[c"));
        (&terminal.0).write_all(format!("{what_it_is}x").as_bytes()).unwrap();
        until_or("the viewer has drawn the pane", || terminal.seen(), || terminal.seen().contains("drew has ended"));
        send(&view_fifo(&session.0), "quit\n");
        until("the viewer has given the terminal back", || terminal.seen().contains("\x1b[?1049l"));
        assert!(view.end().success());
        // Of all it wrote from its start to its end: no command of the Kitty graphics protocol
        // (no APC at all), and no picture but the pane's sixel, as the program printed it.
        let seen = terminal.seen();
        assert!(seen.contains("DREW"), "{seen:?}");
        let mut pictures: Vec<&str> = seen
            .split("\x1bP")
            .skip(1)
            .filter_map(|rest| rest.split_once("\x1b\\"))
            .map(|(picture, _)| picture)
            .collect();
        pictures.dedup();
        assert_eq!((seen.matches("\x1b_").count(), pictures), (0, drawn), "to a terminal that says {what_it_is:?}");
    }
}
