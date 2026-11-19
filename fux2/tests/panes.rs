//! What a pane promises, against the real daemon. Each test has a session
//! of its own, and ends what it started.
use fux2::client::{ANSWER_WAIT, Client};
use fux2::conn::Conn;
use fux2::dir::Session;
use fux2::wire::{Answer, Code, Failure, MAX_ANSWER, Part, REVISION, Request, Token};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

type Tested = Result<(), Box<dyn std::error::Error>>;

fn soon(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait).unwrap_or(now)
}

struct Lab {
    session: Session,
}

fn program(script_unix: &str, script_windows: &str) -> Vec<String> {
    let words: &[&str] = if cfg!(windows) {
        &["cmd.exe", "/d", "/c", script_windows]
    } else {
        &["sh", "-c", script_unix]
    };
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn idle() -> Vec<String> {
    program("sleep 300", "ping -n 300 127.0.0.1 > NUL")
}

impl Lab {
    fn new() -> Result<Lab, Box<dyn std::error::Error>> {
        let root: PathBuf =
            fuxix::file::scratch()?.join(format!("f2t-{}", fux2::wire::PaneId::random()));
        fuxix::file::private_dir(&root)?;
        Ok(Lab {
            session: Session { root },
        })
    }

    fn fux2(&self, args: &[&str]) -> Result<Output, std::io::Error> {
        Command::new(env!("CARGO_BIN_EXE_fux2"))
            .args(args)
            .env("FUX2_DIR", &self.session.root)
            .output()
    }

    fn start(&self, name: &str, argv: &[String]) -> Result<Token, Box<dyn std::error::Error>> {
        let mut args = vec!["start", "--name", name, "--"];
        args.extend(argv.iter().map(String::as_str));
        let out = self.fux2(&args)?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "start {name}: {stderr}");
        let token = text.split_whitespace().nth(1).unwrap_or_default();
        Ok(token.parse::<Token>()?)
    }

    fn client(&self, name: &str) -> Result<Client, Failure> {
        Client::connect(&self.session, name)
    }
}

impl Drop for Lab {
    /// Ends every pane it finds, by its daemon's pid if `kill` fails, and
    /// removes the session only once none is left, saying so if one is.
    fn drop(&mut self) {
        for name in self.session.names().unwrap_or_default() {
            let pid = self.client(&name).map(|client| client.hello.pid);
            let killed = self
                .fux2(&["kill", &name])
                .is_ok_and(|out| out.status.success());
            if !killed && let Ok(pid) = pid {
                kill_process(pid);
            }
        }
        let left = self.session.names().unwrap_or_default();
        let reachable: Vec<_> = left
            .iter()
            .filter(|name| self.client(name).is_ok())
            .collect();
        if reachable.is_empty() {
            let _ = std::fs::remove_dir_all(&self.session.root);
        } else {
            let mut err = std::io::stderr();
            let root = self.session.root.display();
            let _ = writeln!(err, "panes {reachable:?} outlived the test; {root} is kept");
        }
    }
}

fn kill_process(pid: u32) {
    let pid = pid.to_string();
    let _ = if cfg!(windows) {
        Command::new("taskkill").args(["/F", "/PID", &pid]).output()
    } else {
        Command::new("kill").args(["-9", &pid]).output()
    };
}

/// A wait wakes for one change followed by silence, whether the change
/// came before the wait arrived or after: nothing later can mask a lost one.
#[test]
fn a_wait_wakes_for_a_single_change() -> Tested {
    let lab = Lab::new()?;
    let quiet = program("exec cat", "findstr x");
    lab.start("quiet", &quiet)?;
    let mut waiter = lab.client("quiet")?;
    let mut typist = lab.client("quiet")?;
    // Echo settles; then each change is one key and then nothing.
    std::thread::sleep(Duration::from_millis(300));
    for round in 0..40 {
        let Answer::Screen { token, .. } =
            waiter.ask(&Request::Screen { since: None }, ANSWER_WAIT)?
        else {
            return Err("not a screen".into());
        };
        let key = Request::Send {
            parts: vec![Part::Bytes(b"x".to_vec())],
            expected: None,
        };
        let before = round % 2 == 0;
        if before {
            typist.ask(&key, ANSWER_WAIT)?;
            std::thread::sleep(Duration::from_millis(30));
        }
        let wait = Request::Wait {
            after: token,
            ms: 5_000,
        };
        waiter.send_only(&wait)?;
        if !before {
            std::thread::sleep(Duration::from_millis(30));
            typist.ask(&key, ANSWER_WAIT)?;
        }
        let asked = Instant::now();
        let Answer::Waited { token: woken, .. } = waiter.receive(ANSWER_WAIT)? else {
            return Err("not a wait's answer".into());
        };
        assert!(woken.version > token.version, "round {round}");
        assert!(
            asked.elapsed() < Duration::from_secs(4),
            "round {round} slept"
        );
        // The echo is all the change there is: let it end before the next.
        std::thread::sleep(Duration::from_millis(60));
    }
    Ok(())
}

/// A client that stops reading costs the others nothing.
#[test]
fn a_stalled_reader_holds_up_nobody() -> Tested {
    let lab = Lab::new()?;
    let noisy = program(
        "yes 0123456789012345678901234567890123456789012345678901234567890123",
        "for /L %i in (1,0,2) do @echo 0123456789012345678901234567890123456789",
    );
    lab.start("noisy", &noisy)?;
    // Never read from: its answers pile up until the pane gives up on it.
    let (stream, _) = fux2::sys::connect(&lab.session.pane_dir("noisy"))?;
    let mut stalled = Conn::new(stream, MAX_ANSWER);
    stalled.queue(&Request::Hello { revision: REVISION }.encode())?;
    for _ in 0..64 {
        stalled.queue(&Request::Screen { since: None }.encode())?;
    }
    let until = soon(Duration::from_secs(2));
    let mut other = lab.client("noisy")?;
    while Instant::now() < until {
        let _ = stalled.flush();
        let asked = Instant::now();
        other.ask(&Request::Screen { since: None }, ANSWER_WAIT)?;
        assert!(
            asked.elapsed() < Duration::from_secs(2),
            "a read took too long"
        );
    }

    Ok(())
}

/// Every slot taken, a read and a kill still get through.
#[test]
fn control_requests_are_served_with_every_slot_busy() -> Tested {
    let lab = Lab::new()?;
    let token = lab.start("busy", &idle())?;
    let mut held = Vec::new();
    for _ in 0..16 {
        let mut client = lab.client("busy")?;
        let wait = Request::Wait {
            after: token,
            ms: 30_000,
        };
        // Those past the limit on waits are refused, and stay connected.
        let _ = client.call(&wait, Duration::from_millis(50));
        held.push(client);
    }
    let mut reader = lab.client("busy")?;
    let asked = Instant::now();
    reader.ask(&Request::Screen { since: None }, ANSWER_WAIT)?;
    assert!(asked.elapsed() < Duration::from_secs(2));
    let killed = lab.fux2(&["kill", "busy"])?;
    assert!(killed.status.success(), "{killed:?}");
    Ok(())
}

/// A name that passed to a new pane is never reached through its old
/// token, however it passed: by kill, by a crash, or in a race to start.
#[test]
fn an_old_token_never_reaches_the_replacement() -> Tested {
    let lab = Lab::new()?;
    let old = lab.start("job", &idle())?;
    let pid = lab.client("job")?.hello.pid.to_string();
    let killed = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/F", "/PID", &pid])
            .output()?
    } else {
        Command::new("kill").args(["-9", &pid]).output()?
    };
    assert!(killed.status.success(), "{killed:?}");
    let until = soon(Duration::from_secs(5));
    while lab.client("job").is_ok() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    // The crashed pane's claim is free, so its leftovers are cleared; of
    // several starting at once, exactly one gets the name.
    // All started before any is waited for: they really race.
    let started: Vec<_> = (0..6)
        .map(|_| {
            Command::new(env!("CARGO_BIN_EXE_fux2"))
                .args(["start", "--name", "job", "--"])
                .args(idle())
                .env("FUX2_DIR", &lab.session.root)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
        })
        .collect::<Result<_, _>>()?;
    let racers: Vec<Output> = started
        .into_iter()
        .map(std::process::Child::wait_with_output)
        .collect::<Result<_, _>>()?;
    let winners = racers.iter().filter(|out| out.status.success()).count();
    assert_eq!(winners, 1, "{racers:?}");
    let mut client = lab.client("job")?;
    assert_ne!(client.hello.id, old.pane);
    let guarded = Request::Send {
        parts: vec![Part::Bytes(b"x".to_vec())],
        expected: Some(old),
    };
    let code = client.ask(&guarded, ANSWER_WAIT).err().map(|f| f.code);
    assert_eq!(code, Some(Code::Replaced));
    let by_id = format!("job@{}", old.pane);
    let out = lab.fux2(&["send", &by_id, "x"])?;
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    Ok(())
}

/// Kill reaches what the program left behind, after the program has ended
/// and its status has been read.
#[cfg(unix)]
#[test]
fn kill_ends_what_an_ended_program_left_behind() -> Tested {
    let lab = Lab::new()?;
    // Ignored before the child exists, so the hang-up the terminal's end
    // sends cannot reach it first: it stays behind, as a daemon would.
    let script = "trap '' HUP; sleep 300 & echo left=$!; exit 0";
    lab.start("orphan", &program(script, ""))?;
    let until = soon(Duration::from_secs(10));
    let pid = loop {
        let out = lab.fux2(&["read", "orphan"])?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let listed = lab.fux2(&["ls", "--long"])?;
        let ended = String::from_utf8_lossy(&listed.stdout).contains("exited");
        if let Some(pid) = text.split("left=").nth(1).filter(|_| ended) {
            break pid.split_whitespace().next().unwrap_or_default().to_owned();
        }
        assert!(Instant::now() < until, "it never ended: {text}");
        std::thread::sleep(Duration::from_millis(20));
    };
    let alive = |pid: &str| Command::new("kill").args(["-0", pid]).output();
    assert!(alive(&pid)?.status.success(), "it should still run");
    let killed = lab.fux2(&["kill", "orphan"])?;
    assert!(killed.status.success(), "{killed:?}");
    assert!(!alive(&pid)?.status.success(), "{pid} outlived the kill");
    Ok(())
}

/// What a program printed and scrolled away is in the text read, in order,
/// each line once, a line the terminal wrapped back in one piece.
#[test]
fn history_text_holds_every_line_once_in_order() -> Tested {
    let lab = Lab::new()?;
    let long: String = std::iter::repeat_n('w', 200).collect();
    let script_unix =
        format!("i=1; while [ $i -le 500 ]; do echo line-$i; i=$((i+1)); done; echo {long}");
    let windows = format!("(for /L %i in (1,1,500) do @echo line-%i) & echo {long}");
    lab.start("many", &program(&script_unix, &windows))?;
    let until = soon(Duration::from_secs(30));
    loop {
        let listed = lab.fux2(&["ls", "--long"])?;
        if String::from_utf8_lossy(&listed.stdout).contains("exited") {
            break;
        }
        assert!(Instant::now() < until, "the program never ended");
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = lab.fux2(&["read", "many", "--history-text"])?;
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let numbered: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| l.starts_with("line-"))
        .collect();
    let wanted: Vec<String> = (1..=500).map(|n| format!("line-{n}")).collect();
    assert_eq!(numbered, wanted);
    assert!(
        lines.contains(&long.as_str()),
        "the wrapped line came apart"
    );
    Ok(())
}

/// Kill by label takes exactly the panes that carry it.
#[test]
fn kill_by_label_takes_only_its_group() -> Tested {
    let lab = Lab::new()?;
    for (name, group) in [("a", "x"), ("b", "x"), ("c", "y")] {
        let label = format!("group={group}");
        let mut args = vec!["start", "--name", name, "--label", &label, "--"];
        let argv = idle();
        args.extend(argv.iter().map(String::as_str));
        let out = lab.fux2(&args)?;
        assert!(out.status.success(), "{out:?}");
    }
    let out = lab.fux2(&["kill", "--label", "group=x"])?;
    assert!(out.status.success(), "{out:?}");
    let listed = lab.fux2(&["ls"])?;
    assert_eq!(String::from_utf8_lossy(&listed.stdout).trim(), "c");
    Ok(())
}

/// True while `pid` is a process that has not ended.
#[cfg(target_os = "linux")]
fn process_alive(pid: &str) -> Result<bool, std::io::Error> {
    Ok(Command::new("kill")
        .args(["-0", pid])
        .output()?
        .status
        .success())
}

/// The number the program printed after `left=`, once it has.
#[cfg(target_os = "linux")]
fn printed_pid(lab: &Lab, pane: &str) -> Result<String, Box<dyn std::error::Error>> {
    let until = soon(Duration::from_secs(10));
    loop {
        let out = lab.fux2(&["read", pane])?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        if let Some(rest) = text.split("left=").nth(1)
            && let Some(pid) = rest.split_whitespace().next()
        {
            return Ok(pid.to_owned());
        }
        assert!(Instant::now() < until, "no pid on the screen: {text}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A1: a process that left the session is still the pane's to end, on the
/// system that can find it.
#[cfg(target_os = "linux")]
#[test]
fn kill_reaches_a_process_that_left_the_session() -> Tested {
    let lab = Lab::new()?;
    let script = "setsid sleep 300 & echo left=$!; exec sleep 300";
    lab.start("escaper", &program(script, ""))?;
    let pid = printed_pid(&lab, "escaper")?;
    assert!(process_alive(&pid)?, "it should be running");
    let killed = lab.fux2(&["kill", "escaper"])?;
    assert!(killed.status.success(), "{killed:?}");
    assert!(
        !process_alive(&pid)?,
        "{pid} outlived a kill that said all was gone"
    );
    Ok(())
}

/// A4: what a finished program leaves behind is still reaped as it ends.
#[cfg(target_os = "linux")]
#[test]
fn an_ended_pane_leaves_no_zombies() -> Tested {
    let lab = Lab::new()?;
    let script = "trap '' HUP; (i=0; while [ $i -lt 8 ]; do (sleep 0 &); i=$((i+1)); sleep 0.05; done) & exit 0";
    lab.start("litter", &program(script, ""))?;
    let pid = lab.client("litter")?.hello.pid;
    std::thread::sleep(Duration::from_millis(1200));
    let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))?;
    let zombies = children
        .split_whitespace()
        .filter(|child| {
            std::fs::read_to_string(format!("/proc/{child}/stat")).is_ok_and(|stat| {
                stat.rsplit(')')
                    .next()
                    .is_some_and(|r| r.trim_start().starts_with('Z'))
            })
        })
        .count();
    // Once the last thing it started is gone, the program is reaped too.
    assert_eq!(zombies, 0, "zombies among {children}");
    Ok(())
}

/// A3: a frame's time limit does not restart with each byte, so connections
/// dribbling requests cannot keep a kill out.
#[test]
fn slow_requests_cannot_lock_out_a_kill() -> Tested {
    let lab = Lab::new()?;
    let token = lab.start("slow", &idle())?;
    let mut parked = Vec::new();
    for _ in 0..12 {
        let mut client = lab.client("slow")?;
        client.send_only(&Request::Wait {
            after: token,
            ms: 30_000,
        })?;
        parked.push(client);
    }
    let dir = lab.session.pane_dir("slow");
    let mut dribblers = Vec::new();
    for _ in 0..4 {
        let (stream, _) = fux2::sys::connect(&dir)?;
        // A header promising 1000 bytes that come one at a time.
        fuxix::io::write(&stream, &1000u32.to_be_bytes())?;
        dribblers.push(stream);
    }
    let mut killer = Command::new(env!("CARGO_BIN_EXE_fux2"))
        .args(["kill", "slow"])
        .env("FUX2_DIR", &lab.session.root)
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let until = soon(Duration::from_secs(12));
    let status = loop {
        if let Some(status) = killer.try_wait()? {
            break status;
        }
        if Instant::now() >= until {
            let _ = killer.kill();
            let _ = killer.wait();
            return Err("the kill never got through".into());
        }
        for stream in &dribblers {
            let _ = fuxix::io::write(stream, b"x");
            let mut fds = [fuxix::poll::PollFd::new(stream, fuxix::poll::Events::OUT)];
            let _ = fuxix::poll::poll(&mut fds, Some(Duration::from_millis(1)));
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(status.success(), "{status:?}");
    Ok(())
}

/// B1: a call that gave up leaves its connection finished: the next call
/// cannot be handed the old answer.
#[test]
fn a_timed_out_client_does_not_read_an_old_answer() -> Tested {
    let lab = Lab::new()?;
    // A program that prints nothing, so that the wait below cannot be
    // answered at once.
    lab.start("late", &program("exec sleep 300", "pause >NUL"))?;
    std::thread::sleep(Duration::from_millis(500));
    let mut client = lab.client("late")?;
    let Answer::Screen { token, .. } = client.ask(&Request::Screen { since: None }, ANSWER_WAIT)?
    else {
        return Err("not a screen".into());
    };
    let wait = Request::Wait {
        after: token,
        ms: 100,
    };
    let gave_up = client.call(&wait, Duration::from_millis(5));
    assert_eq!(gave_up.err().map(|f| f.code), Some(Code::Indeterminate));
    std::thread::sleep(Duration::from_millis(200));
    let next = client.call(&Request::Screen { since: None }, ANSWER_WAIT);
    assert_eq!(next.err().map(|f| f.code), Some(Code::Indeterminate));
    Ok(())
}

/// B5: a token from an earlier life is refused even with nothing to wait for.
#[test]
fn since_checks_its_token_without_a_wait() -> Tested {
    let lab = Lab::new()?;
    let old = lab.start("again", &idle())?.to_string();
    assert!(lab.fux2(&["kill", "again"])?.status.success());
    lab.start("again", &idle())?;
    let out = lab.fux2(&["read", "again", "--since", &old, "--text"])?;
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    Ok(())
}

/// B7: past a limit is the exit code the README gives for it.
#[test]
fn too_much_history_is_a_limit() -> Tested {
    let lab = Lab::new()?;
    let out = lab.fux2(&["start", "--name", "x", "--history", "16777217", "--", "sh"])?;
    assert_eq!(out.status.code(), Some(6), "{out:?}");
    Ok(())
}

/// E1: `page` from inside an ordinary pane leaves that pane alone; only a
/// pane started to show the pager asks to go.
#[test]
fn page_removes_only_a_pane_made_for_it() -> Tested {
    let lab = Lab::new()?;
    lab.start("shown", &idle())?;
    lab.start("caller", &idle())?;
    // It succeeds, as a pager quitting normally does.
    let cat = if cfg!(windows) { "cmd /d /c findstr . & exit 0" } else { "cat" };
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_fux2"))
            .args(["page", "shown"])
            .args(extra)
            .env("FUX2_DIR", &lab.session.root)
            .env("FUX2_PANE", "caller")
            .env("PAGER", cat)
            .output()
    };
    assert!(run(&[])?.status.success());
    assert!(
        lab.client("caller").is_ok(),
        "page killed the pane it ran in"
    );
    let id = lab.client("caller")?.hello.id.to_string();
    let close = |pager: &str| {
        Command::new(env!("CARGO_BIN_EXE_fux2"))
            .args(["page", "shown", "--close-pane"])
            .env("FUX2_DIR", &lab.session.root)
            .env("FUX2_PANE", "caller")
            .env("FUX2_PANE_ID", &id)
            .env("PAGER", pager)
            .stdin(std::process::Stdio::null())
            .output()
    };
    // G9: a pager that fails is said, not hidden, and the pane still goes.
    let failed = close(if cfg!(windows) {
        "cmd /c exit 1"
    } else {
        "false"
    })?;
    assert!(!failed.status.success(), "{failed:?}");
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("pager"),
        "{failed:?}"
    );
    assert!(lab.client("caller").is_err(), "--close-pane left its pane");
    Ok(())
}

/// C1, C3 and C4: what is in the directory decides nothing by its name alone.
#[cfg(unix)]
mod directory {
    use super::*;

    /// A daemon whose directory was moved aside leaves its replacement be.
    #[test]
    fn a_moved_pane_does_not_remove_its_replacement() -> Tested {
        let lab = Lab::new()?;
        lab.start("job", &idle())?;
        let mut old = lab.client("job")?;
        let dir = lab.session.pane_dir("job");
        let aside = lab.session.root.join("aside");
        std::fs::rename(&dir, &aside)?;
        lab.start("job", &idle())?;
        let _ = old.call(&Request::Kill { expected: None }, ANSWER_WAIT);
        assert!(lab.client("job").is_ok(), "the replacement lost its socket");
        Ok(())
    }

    /// A link at a pane's name does not lead to another pane.
    #[test]
    fn a_link_is_not_a_pane() -> Tested {
        let lab = Lab::new()?;
        lab.start("real", &idle())?;
        let link = lab.session.pane_dir("fake");
        std::os::unix::fs::symlink(lab.session.pane_dir("real"), &link)?;
        let out = lab.fux2(&["send", "fake", "x"])?;
        assert!(!out.status.success(), "{out:?}");
        let dangling = lab.session.pane_dir("gone");
        std::os::unix::fs::symlink(lab.session.root.join("nowhere"), &dangling)?;
        lab.start("other", &idle())?;
        std::fs::remove_file(&link)?;
        std::fs::remove_file(&dangling)?;
        Ok(())
    }

    /// A dead pane's directory that cannot be cleared says why, by path.
    #[test]
    fn a_stuck_leftover_is_reported() -> Tested {
        let lab = Lab::new()?;
        let dir = lab.session.pane_dir("stuck");
        fuxix::file::private_dir(&dir)?;
        std::fs::write(dir.join("extra"), "x")?;
        let out = lab.fux2(&["start", "--name", "stuck", "--", "sh"])?;
        let told = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(!out.status.success());
        assert!(
            told.contains("p-stuck") && !told.contains("exists:"),
            "{told}"
        );
        std::fs::remove_file(dir.join("extra"))?;
        Ok(())
    }
}

/// G1 and G2: a wait for a line counts only what it saw appear, however many
/// times the screen was redrawn meanwhile, and a pane that never stops
/// printing ends it as a timeout, not as busy.
#[cfg(unix)]
#[test]
fn a_wait_ignores_old_lines_through_many_redraws() -> Tested {
    let lab = Lab::new()?;
    lab.start("bar", &program("exec sh", ""))?;
    let t = lab.fux2(&["send", "bar", "echo done\\n"])?;
    let t = String::from_utf8_lossy(&t.stdout).trim().to_owned();
    let out = lab.fux2(&[
        "read",
        "bar",
        "--since",
        &t,
        "--wait",
        "5000",
        "--until-line",
        "done",
    ])?;
    assert!(out.status.success(), "{out:?}");
    let redraws = "i=0; while [ $i -lt 400 ]; do printf '\\r%d' $i; i=$((i+1)); done; sleep 1; echo; echo finished\\n";
    let t = lab.fux2(&["send", "bar", redraws])?;
    let t = String::from_utf8_lossy(&t.stdout).trim().to_owned();
    let out = lab.fux2(&[
        "read",
        "bar",
        "--since",
        &t,
        "--wait",
        "3000",
        "--until-line",
        "done",
        "--text",
    ])?;
    assert_eq!(out.status.code(), Some(5), "{out:?}");
    lab.start("flood", &program("yes flood", ""))?;
    let t = lab.client("flood")?.hello.id;
    let token = format!("{t}@1");
    let out = lab.fux2(&[
        "read",
        "flood",
        "--since",
        &token,
        "--wait",
        "1500",
        "--until-line",
        "never",
    ])?;
    assert_eq!(out.status.code(), Some(5), "{out:?}");
    Ok(())
}
