//! What a pane promises, against the real daemon. Each test has a session
//! of its own, and ends what it started.
use fux2::client::{ANSWER_WAIT, Client};
use fux2::conn::Conn;
use fux2::dir::Session;
use fux2::pane::MAX_INPUT;
use fux2::wire::{Answer, Code, Failure, MAX_ANSWER, Part, REVISION, Request, Token};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
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
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let number = NEXT.fetch_add(1, Ordering::SeqCst);
        let root: PathBuf =
            fuxix::file::scratch()?.join(format!("f2t-{}-{number}", std::process::id()));
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
    fn drop(&mut self) {
        for name in self.session.names().unwrap_or_default() {
            let _ = self.fux2(&["kill", &name]);
        }
        let _ = std::fs::remove_dir_all(&self.session.root);
    }
}

fn send_bytes(len: usize) -> Request {
    Request::Send {
        parts: vec![Part::Bytes(vec![b'x'; len])],
        expected: None,
    }
}

/// Output racing a wait's registration never leaves the wait asleep.
#[test]
fn a_wait_always_wakes_for_output_that_races_it() -> Tested {
    let lab = Lab::new()?;
    let noisy = program(
        "while :; do echo x; done",
        "for /L %i in (1,0,2) do @echo x",
    );
    lab.start("noisy", &noisy)?;
    let mut client = lab.client("noisy")?;
    for _ in 0..200 {
        let answer = client.ask(&Request::Screen { since: None }, ANSWER_WAIT)?;
        let Answer::Screen { token, .. } = answer else {
            return Err("not a screen".into());
        };
        let wait = Request::Wait {
            after: token,
            ms: 10_000,
        };
        let Answer::Waited { token: woken, .. } = client.ask(&wait, ANSWER_WAIT)? else {
            return Err("not a wait's answer".into());
        };
        assert!(woken.version > token.version);
    }
    Ok(())
}

/// A client that stops reading costs others nothing, and a send that does
/// not fit whole adds nothing at all.
#[test]
fn a_stalled_reader_holds_up_nobody_and_a_refused_send_adds_nothing() -> Tested {
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

    lab.start("idle", &idle())?;
    let mut client = lab.client("idle")?;
    let chunk = MAX_INPUT / 3;
    let waiting = |failure: &Failure| -> Option<usize> {
        let tail = failure.message.split("would make ").nth(1)?;
        tail.split(' ').next()?.parse().ok()
    };
    let mut seen = Vec::new();
    for _ in 0..200 {
        match client.ask(&send_bytes(chunk), ANSWER_WAIT) {
            Ok(_) => seen.clear(),
            Err(failure) => {
                assert_eq!(failure.code, Code::Busy, "{failure}");
                seen.push(waiting(&failure).ok_or("no count in the refusal")?);
                if seen.len() == 3 {
                    break;
                }
            }
        }
    }
    assert_eq!(seen.len(), 3, "the queue never filled");
    let (first, last) = (seen.first().copied(), seen.last().copied());
    assert!(last <= first, "refused sends grew the queue: {seen:?}");
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
    let racers: Vec<_> = (0..6)
        .map(|_| {
            let mut command = Command::new(env!("CARGO_BIN_EXE_fux2"));
            command
                .args(["start", "--name", "job", "--"])
                .args(idle())
                .env("FUX2_DIR", &lab.session.root);
            command.output()
        })
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
    let script = "sleep 300 & echo left=$!; exit 0";
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
