//! A pane's state: whether its program waits for its terminal (`reading`),
//! for something else (`waiting`), is at work (`busy`), or ended. fux reads
//! it from what the program's threads are blocked in, where the system says
//! (Linux), and from a quiet screen elsewhere.
mod support;
use support::*;

fn state(server: &Server, pane: &str) -> Result<String, String> {
    Ok(server
        .ok(&["str", pane, "pane", "str", "state", "get"])?
        .trim()
        .to_owned())
}

/// The state, once it is one of `want`, within a few seconds.
fn becomes(server: &Server, pane: &str, want: &[&str]) -> Result<String, String> {
    let until = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(10))
        .unwrap_or_else(std::time::Instant::now);
    loop {
        let now = state(server, pane)?;
        if want.contains(&now.as_str()) || std::time::Instant::now() > until {
            return Ok(now);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn a_program_at_its_prompt_reads_and_one_asleep_is_busy() -> Outcome {
    let server = Server::start("")?;
    let prompt = "import time; time.sleep(0.5); input('name> '); time.sleep(30)";
    let pane = server
        .ok(&[
            "opts", "str", "python3", "program", "str", "-c", "arg", "str", prompt, "arg", "0",
            "timeout", "run",
        ])?
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    assert_eq!(becomes(&server, &pane, &[READING])?, READING);
    server.ok(&[
        "str",
        &pane,
        "pane",
        "str",
        "bob",
        "text",
        "str",
        "Enter",
        "key",
        "0",
        "timeout",
        "send-keys",
    ])?;
    if PROBED {
        assert_eq!(becomes(&server, &pane, &["busy"])?, "busy");
    }
    let done = server
        .ok(&["opts", "str", "true", "program", "0", "timeout", "run"])?
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    assert_eq!(becomes(&server, &done, &["exited 0"])?, "exited 0");
    Ok(())
}

/// An interactive bash fux marks, in a pane of its own.
fn bash(server: &Server) -> Result<String, String> {
    Ok(server
        .ok(&[
            "opts", "str", "bash", "program", "str", "--norc", "arg", "str", "-i", "arg", "run",
        ])?
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned())
}

/// A shell command that polls its terminal with a time limit is at work
/// until it ends, as a pause that watches for keys is: `line` answers with
/// its end. One that takes pastes as it polls (bracketed paste on: a line
/// editor reading, as Python's REPL) waits for keys once quiet: `line`
/// answers `waiting · command running` in a second or so, and the line
/// typed next is what it reads.
#[test]
fn a_shell_command_that_polls_its_terminal_waits_for_keys_while_editing() -> Outcome {
    if !PROBED {
        return Ok(());
    }
    let server = Server::start("")?;
    let pane = bash(&server)?;
    let p = ["str", pane.as_str(), "pane"];
    let pause = "python3 -c 'import select,sys; print(\"start\", flush=True); \
                 [select.select([sys.stdin], [], [], 0.1) for _ in range(25)]; \
                 print(\"end\")'; (exit 5)";
    let answer = server.ok(&[&p[..], &["str", pause, "line"]].concat())?;
    let first = answer.lines().next().unwrap_or_default();
    assert_eq!(first, format!("{pane} reading · done 5"), "{answer}");
    assert!(answer.contains("\nend\n"), "{answer}");
    let poll = "python3 -c 'import select,sys; print(\"\\x1b[?2004hask>\", flush=True)\n\
                while not select.select([sys.stdin], [], [], 0.1)[0]: pass\n\
                print(\"\\x1b[?2004lgot\", input())'; (exit 5)";
    let began = std::time::Instant::now();
    let answer = server.ok(&[&p[..], &["str", poll, "line"]].concat())?;
    assert!(began.elapsed() < std::time::Duration::from_secs(10));
    let first = answer.lines().next().unwrap_or_default();
    assert_eq!(
        first,
        format!("{pane} waiting · command running"),
        "{answer}"
    );
    assert!(answer.contains("\nask>\n"), "{answer}");
    let answer = server.ok(&[&p[..], &["str", "bob", "line"]].concat())?;
    assert!(answer.contains("got bob\n"), "{answer}");
    assert!(
        answer.starts_with(&format!("{pane} reading · done 5")),
        "{answer}"
    );
    Ok(())
}

/// A shell command blocked on a pipe (its writer, out of the terminal's
/// group, slow) is at work until it ends: `line` answers with its end and
/// its output, not with the quiet before them.
#[test]
fn a_shell_command_blocked_on_a_pipe_is_answered_when_it_ends() -> Outcome {
    if !PROBED {
        return Ok(());
    }
    let server = Server::start("")?;
    let pane = bash(&server)?;
    let slow = "python3 -c 'import os,time; r,w=os.pipe(); print(\"start\", flush=True)\n\
                if os.fork() == 0: os.setsid(); time.sleep(2.5); os.write(w, b\"end\\n\"); os._exit(0)\n\
                os.close(w); print(os.read(r, 9).decode(), end=\"\")'; (exit 5)";
    let answer = server.ok(&["str", &pane, "pane", "str", slow, "line"])?;
    let first = answer.lines().next().unwrap_or_default();
    assert_eq!(first, format!("{pane} reading · done 5"), "{answer}");
    assert!(answer.contains("\nend\n"), "{answer}");
    Ok(())
}

/// Python's own REPL (pyrepl, 3.13 on) polls its terminal with a time
/// limit: typed as a shell command, it answers once it shows its prompt,
/// and takes a line. Left out where there is no `python3`.
#[test]
fn a_python_repl_typed_at_a_shell_answers_at_its_prompt() -> Outcome {
    let found = std::process::Command::new("python3")
        .arg("-c")
        .arg("")
        .status()
        .is_ok_and(|s| s.success());
    if !PROBED || !found {
        return Ok(());
    }
    let server = Server::start("")?;
    let pane = bash(&server)?;
    let p = ["str", pane.as_str(), "pane"];
    let began = std::time::Instant::now();
    let answer = server.ok(&[&p[..], &["str", "python3 -i", "line"]].concat())?;
    assert!(
        began.elapsed() < std::time::Duration::from_secs(10),
        "{answer}"
    );
    assert!(answer.contains(">>>"), "{answer}");
    assert!(answer.contains("command running"), "{answer}");
    let answer = server.ok(&[&p[..], &["str", "6*7", "line"]].concat())?;
    assert!(answer.contains("\n42\n"), "{answer}");
    let answer = server.ok(&[&p[..], &["str", "exit()", "line"]].concat())?;
    assert!(answer.contains("done 0"), "{answer}");
    Ok(())
}

/// The pane `run` started from `code` (Python), and its state then.
fn started(server: &Server, code: &str) -> Result<(String, String), String> {
    let answer = server.ok(&[
        "opts", "str", "python3", "program", "str", "-c", "arg", "str", code, "arg", "run",
    ])?;
    let mut head = answer.lines().next().unwrap_or_default().split_whitespace();
    let pane = head.next().unwrap_or_default().to_owned();
    Ok((pane, head.next().unwrap_or_default().to_owned()))
}

/// What a program waits on decides whether it reads: a select, poll or
/// epoll that watches its terminal with no time limit is `reading`, and a
/// text it has not printed cannot come (status 7): so does a raw read whose
/// timer runs only once a key came (VMIN 1, VTIME 100), no key sent since
/// it started, and one
/// whose child ended unreaped (a zombie), one whose main thread waits for
/// another that reads, and one whose grandchild reads. One that reads or
/// watches a pipe alone, or watches its terminal for no input, may go on
/// alone: `waiting`, never 7. Nor is a program whose main thread reads its
/// terminal while another thread, or a child, sleeps on a timer: what that
/// one prints next is waited for. A main thread waiting for another that
/// sleeps is asleep: busy. One asleep on a timer while another thread
/// polls the terminal (a frame loop and its input thread) polls: `waiting`. Nor, with what it may not be traced, is one
/// still printing.
#[test]
fn reads_are_told_from_programs_that_go_on_alone() -> Outcome {
    if !PROBED {
        return Ok(());
    }
    let server = Server::start("")?;
    let programs = [
        (
            "import select, sys\nprint('ready', flush=True)\nselect.select([sys.stdin], [], [])\n",
            "reading",
            7,
        ),
        (
            "import select, sys\ne = select.epoll(); e.register(0, select.EPOLLIN)\n\
             print('ready', flush=True); e.poll()\n",
            "reading",
            7,
        ),
        (
            "import os, select\nr, w = os.pipe(); p = select.poll(); p.register(r)\n\
             print('ready', flush=True); p.poll()\n",
            "waiting",
            124,
        ),
        (
            "import sys, termios, tty\n\
             tty.setraw(0); m = termios.tcgetattr(0)\n\
             m[6][termios.VMIN] = 1; m[6][termios.VTIME] = 100\n\
             termios.tcsetattr(0, termios.TCSANOW, m)\n\
             sys.stdout.write('ready\\r\\n'); sys.stdout.flush(); sys.stdin.read(5)\n",
            "reading",
            7,
        ),
        (
            "import os\nif os.fork() == 0: os._exit(0)\nprint('ready', flush=True); input()\n",
            "reading",
            7,
        ),
        (
            "import select\ne = select.epoll(); e.register(0, select.EPOLLPRI)\n\
             print('ready', flush=True); e.poll()\n",
            "waiting",
            124,
        ),
        (
            "import os\nr, w = os.pipe()\nprint('ready', flush=True); os.read(r, 1)\n",
            "waiting",
            124,
        ),
        (
            "import threading\nt = threading.Thread(target=input)\n\
             print('ready', flush=True); t.start(); t.join()\n",
            "reading",
            7,
        ),
        (
            "import os, threading\nr, w = os.pipe()\n\
             t = threading.Thread(target=os.read, args=(r, 1))\n\
             print('ready', flush=True); t.start(); t.join()\n",
            "waiting",
            124,
        ),
        (
            "import subprocess\nprint('ready', flush=True)\n\
             subprocess.run(['sh', '-c', 'cat; :'])\n",
            "reading",
            7,
        ),
        // An input thread polling an epoll set that holds the terminal,
        // beside a frame loop asleep on a timer (rig's TUI, a game).
        (
            "import select, threading, time\n\
             e = select.epoll(); e.register(0, select.EPOLLIN)\n\
             def keys():\n    while True: e.poll(0.1)\n\
             threading.Thread(target=keys, daemon=True).start()\n\
             print('ready', flush=True)\nwhile True: time.sleep(0.05)\n",
            "waiting",
            124,
        ),
    ];
    for (code, state, status) in programs {
        let (pane, now) = started(&server, code)?;
        assert_eq!(now, state, "{code}");
        let never = server.fux(&[
            "str",
            &pane,
            "pane",
            "str",
            "NOPE",
            "wait-text",
            "2",
            "timeout",
            "wait",
        ])?;
        assert_eq!(
            never.status, status,
            "{code}: {}{}",
            never.stdout, never.stderr
        );
    }
    let thread = "import threading, time\n\
                  def later(): time.sleep(0.8); print('LATER', flush=True)\n\
                  threading.Thread(target=later).start(); input()\n";
    let child = "import subprocess\n\
                 subprocess.Popen(['sh', '-c', 'sleep 0.8; echo LATER']); input()\n";
    let asleep = "import threading, time\nt = threading.Thread(target=time.sleep, args=(30,))\n\
                  print('ready', flush=True); t.start(); t.join()\n";
    let busy = server.fux(&[
        "opts", "str", "python3", "program", "str", "-c", "arg", "str", asleep, "arg", "2",
        "timeout", "run",
    ])?;
    assert_eq!(busy.status, 124, "{}{}", busy.stdout, busy.stderr);
    let state = busy.stdout.lines().next().unwrap_or_default();
    assert!(state.contains(" busy"), "{}", busy.stdout);
    for code in [thread, child] {
        let later = server.fux(&[
            "opts",
            "str",
            "python3",
            "program",
            "str",
            "-c",
            "arg",
            "str",
            code,
            "arg",
            "str",
            "LATER",
            "wait-text",
            "run",
        ])?;
        assert_eq!(later.status, 0, "{code}: {}{}", later.stdout, later.stderr);
        assert!(later.stdout.ends_with("\nLATER\n"), "{}", later.stdout);
    }
    // A program that may not be traced (non-dumpable) hides what it is
    // blocked in: its quiet screen is taken as settled, not busy, and not
    // before it is quiet.
    let hidden = "import ctypes, time\nctypes.CDLL(None).prctl(4, 0, 0, 0, 0)\n\
                  for i in range(15): print(i, flush=True); time.sleep(0.05)\n\
                  print('ready', flush=True); input()\n";
    let answer = server.ok(&[
        "opts", "str", "python3", "program", "str", "-c", "arg", "str", hidden, "arg", "run",
    ])?;
    let state = answer.lines().next().unwrap_or_default();
    assert!(state.ends_with(" settled"), "{answer}");
    assert!(answer.ends_with("\nready\n"), "{answer}");
    Ok(())
}

/// A raw read whose timer runs once a key came (VMIN 3, VTIME 30) returned
/// with the keys typed: the read the program is in next has taken none,
/// so it waits for keys with no limit, `reading`, not on the last one's
/// timer.
#[test]
fn a_timed_read_that_returned_is_not_timed_after() -> Outcome {
    if !PROBED {
        return Ok(());
    }
    let server = Server::start("")?;
    let code = "import os, termios, tty\n\
                tty.setraw(0); m = termios.tcgetattr(0)\n\
                m[6][termios.VMIN] = 3; m[6][termios.VTIME] = 30\n\
                termios.tcsetattr(0, termios.TCSANOW, m)\n\
                os.write(1, b'ready\\r\\n'); got = os.read(0, 3)\n\
                os.write(1, b'got ' + got + b'\\r\\n'); os.read(0, 3)\n";
    let (pane, state) = started(&server, code)?;
    assert_eq!(state, "reading");
    let keys = ["str", &pane, "pane", "str", "abc", "text", "send-keys"];
    let typed = server.ok(&keys)?;
    assert_eq!(typed, format!("{pane} reading\ngot abc\n"));
    Ok(())
}

/// A pane `waiting` is answered as it is: a capture settles at once, as an
/// action does; a bare `wait` waits for something new, and at its limit
/// answers the state with status 0, as only a busy pane fails (124). Not
/// a full-screen program, it is said to be at work maybe, `wait-exit`
/// waiting for its end.
#[test]
fn a_waiting_pane_is_answered_as_it_is() -> Outcome {
    if !PROBED {
        return Ok(());
    }
    let server = Server::start("")?;
    let poll = "import select, sys\nprint('POLLING', flush=True)\n\
                select.select([sys.stdin], [], [], 30)\n";
    let (pane, state) = started(&server, poll)?;
    assert_eq!(state, "waiting");
    let began = std::time::Instant::now();
    let capture = server.fux(&["str", &pane, "pane", "5", "timeout", "capture"])?;
    assert_eq!(capture.status, 0, "{}{}", capture.stdout, capture.stderr);
    // Not a full-screen program: it may be at work, to end alone, which
    // `wait-exit` waits for.
    let note =
        format!("{pane} waiting · may be at work: `{pane} wait-exit wait` waits for its end\n");
    assert!(
        capture.stdout.starts_with(&format!("{note}POLLING")),
        "{}",
        capture.stdout
    );
    assert!(began.elapsed() < std::time::Duration::from_secs(3));
    let wait = server.fux(&["str", &pane, "pane", "1", "timeout", "wait"])?;
    assert_eq!(wait.status, 0, "{}{}", wait.stdout, wait.stderr);
    assert!(
        wait.stdout.starts_with(&format!("{pane} waiting")),
        "{}",
        wait.stdout
    );
    // With a wait's own limit, nothing new within 5s: answered as it is.
    let began = std::time::Instant::now();
    let plain = server.fux(&["str", &pane, "pane", "wait"])?;
    assert_eq!(plain.status, 0, "{}{}", plain.stdout, plain.stderr);
    assert!(plain.stdout.starts_with(&format!("{pane} waiting")));
    assert!(began.elapsed() < std::time::Duration::from_secs(15));
    Ok(())
}

/// A shell back at its prompt, its command ended, reads: a job it left in
/// its own group (job control off; fish's start-up work) is not the
/// agent's command.
#[test]
fn a_shell_at_its_prompt_reads_whatever_it_left_running() -> Outcome {
    if !PROBED {
        return Ok(());
    }
    let server = Server::start("")?;
    let pane = bash(&server)?;
    let p = ["str", pane.as_str(), "pane"];
    server.ok(&[&p[..], &["str", "set +m; (sleep 30; :) & disown", "line"]].concat())?;
    let answer = server.ok(&[&p[..], &["str", "echo hi", "line"]].concat())?;
    assert!(
        answer.starts_with(&format!("{pane} reading · done 0\n")),
        "{answer}"
    );
    // A prompt's own hook, run after the command ended, is not left behind:
    // the shell reads again only once it is done.
    let hook = "PROMPT_COMMAND+=$'\\n''sleep 1; echo HOOKED'";
    server.ok(&[&p[..], &["str", hook, "line"]].concat())?;
    server.ok(&[&p[..], &["str", "clear", "line"]].concat())?;
    let screen = server.ok(&[&p[..], &["0", "timeout", "capture"]].concat())?;
    assert!(screen.contains("\nHOOKED\n"), "{screen}");
    Ok(())
}

/// Keys for a shell at its prompt go in at its pace, a job it left running
/// in its group notwithstanding: each piece as soon as it reads again, not
/// held for `PACE` (30 ms) as for a program at work.
#[test]
fn keys_for_a_shell_that_left_a_job_are_not_held() -> Outcome {
    if !PROBED {
        return Ok(());
    }
    let server = Server::start("")?;
    let pane = bash(&server)?;
    let p = ["str", pane.as_str(), "pane"];
    server.ok(&[&p[..], &["str", "set +m; (sleep 30; :) & disown", "line"]].concat())?;
    // 100 pieces, typed and erased in turn: held, they take 3 s at least.
    let mut keys = vec!["str", pane.as_str(), "pane"];
    for _ in 0..50 {
        keys.extend(["str", "x", "text", "str", "BSpace", "key"]);
    }
    keys.push("send-keys");
    let began = std::time::Instant::now();
    server.ok(&keys)?;
    let took = began.elapsed();
    assert!(took < std::time::Duration::from_secs(3), "{took:?}");
    Ok(())
}

/// A program that drops its terminal leaves it no foreground group: it is
/// judged by what it does itself, here waiting on a timer, not busy.
#[test]
fn a_program_that_drops_its_terminal_is_judged_by_itself() -> Outcome {
    if !PROBED {
        return Ok(());
    }
    let server = Server::start("")?;
    let code = "import fcntl, select, signal, termios\n\
                signal.signal(signal.SIGHUP, signal.SIG_IGN)\n\
                fcntl.ioctl(0, termios.TIOCNOTTY)\n\
                print('DETACHED', flush=True)\nselect.select([], [], [], 30)\n";
    let (_, state) = started(&server, code)?;
    assert_eq!(state, "waiting");
    Ok(())
}

/// A full-screen program that redraws on its own (a clock, `top`: raw, on
/// the alternate screen, polling its terminal between frames) is never
/// quiet: once it drew on its own a few times, an action on it answers
/// once its keys are read and the frame drawn after them is done, and says
/// it redraws on its own, not at its time limit. Its frames come every
/// 100 ms: twice the gap a frame drawn alone follows (`SETTLE`), and a third
/// of the quiet that, where the system says only idle, is taken as settled
/// (`GUESS`, macOS), which a frame every 200 ms on a busy machine reached.
#[test]
fn a_full_screen_program_that_redraws_on_its_own_answers_its_keys() -> Outcome {
    let server = Server::start("")?;
    let clock = "import os, select, sys, tty\n\
                 tty.setraw(0); os.write(1, b'\\x1b[?1049h'); n = 0; got = b'-'\n\
                 while got != b'q':\n\
                 \x20   n += 1\n\
                 \x20   os.write(1, b'\\x1b[H\\x1b[2Jtick %d\\r\\ngot %s' % (n, got))\n\
                 \x20   if select.select([0], [], [], 0.1)[0]: got = os.read(0, 1)\n\
                 os.write(1, b'\\x1b[?1049l')\n";
    let began = std::time::Instant::now();
    let answer = server.ok(&[
        "opts", "str", "python3", "program", "str", "-c", "arg", "str", clock, "arg", "run",
    ])?;
    assert!(began.elapsed() < PATIENCE, "{answer}");
    let pane = answer
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    let first = answer.lines().next().unwrap_or_default();
    assert_eq!(
        first,
        format!("{pane} waiting · redraws on its own"),
        "{answer}"
    );
    let p = ["str", pane.as_str(), "pane"];
    let began = std::time::Instant::now();
    let answer = server.ok(&[&p[..], &["str", "x", "type"]].concat())?;
    assert!(
        began.elapsed() < std::time::Duration::from_secs(3),
        "{answer}"
    );
    assert!(
        answer.starts_with(&format!("{pane} waiting · redraws on its own\n")),
        "{answer}"
    );
    assert!(answer.contains("\ngot x"), "{answer}");
    let answer = server.ok(&[&p[..], &["str", "q", "type"]].concat())?;
    assert!(answer.starts_with(&format!("{pane} exited 0")), "{answer}");
    Ok(())
}

/// A full-screen program `run` started that marks a command's output
/// (`133;C`) and never its end, then reads its terminal, is waited for no
/// longer than an unmarked one: no action's input began that command, so
/// `run` and a bare `wait` after it answer once it is quiet, polling its
/// terminal (`less`), not at their limit (decision 29). Where fux is not
/// told what it is blocked in (macOS), the quiet pane is `settled`.
#[test]
fn a_command_a_run_program_marks_itself_is_not_waited_for() -> Outcome {
    let server = Server::start("")?;
    let code = "import os, select, tty\n\
                tty.setraw(0); os.write(1, b'\\x1b[?1049h\\x1b]133;C\\x07marked')\n\
                select.select([0], [], [], 60)\n";
    let began = std::time::Instant::now();
    let answer = server.ok(&[
        "opts", "str", "python3", "program", "str", "-c", "arg", "str", code, "arg", "run",
    ])?;
    assert!(began.elapsed() < PATIENCE, "{answer}");
    let pane = answer
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    let quiet = format!("{pane} {}", if PROBED { "waiting" } else { "settled" });
    let first = answer.lines().next().unwrap_or_default();
    assert!(first.starts_with(&quiet), "{answer}");
    let began = std::time::Instant::now();
    let answer = server.ok(&["str", &pane, "pane", "wait"])?;
    assert!(began.elapsed() < PATIENCE, "{answer}");
    assert!(answer.starts_with(&quiet), "{answer}");
    Ok(())
}

/// `wait-done` waits as long as the shell command running in the pane
/// runs, past the wait's limit (here `wait-seconds` 2): it ends at what
/// else it waits for, or at the command's end, `· done STATUS`; a text
/// that had not come by then fails it, status 5. With no command
/// running, a wait is as it was.
#[test]
fn wait_done_waits_as_long_as_the_command_runs() -> Outcome {
    if !PROBED {
        return Ok(());
    }
    let server = Server::start("str wait-seconds 2 set")?;
    let pane = bash(&server)?;
    let p = ["str", pane.as_str(), "pane"];
    let start = |command: &str| {
        let line = [&p[..], &["0", "timeout", "str", command, "line"]].concat();
        server.ok(&line)
    };
    let waited = |opts: &[&str]| {
        let began = std::time::Instant::now();
        let wait = [&p[..], &[], opts, &["wait"]].concat();
        server.fux(&wait).map(|out| (out, began.elapsed()))
    };
    let long = std::time::Duration::from_secs(3);
    start("sleep 4; echo READY; sleep 2; (exit 4)")?;
    let (ready, took) = waited(&["str", "READY", "wait-text", "wait-done"])?;
    assert_eq!(ready.status, 0, "{}{}", ready.stdout, ready.stderr);
    assert!(ready.stdout.contains("met \"READY\""), "{}", ready.stdout);
    assert!(took > long, "{took:?}");
    let (never, _) = waited(&["str", "NEVER", "wait-text", "wait-done"])?;
    assert_eq!(never.status, 5, "{}{}", never.stdout, never.stderr);
    let first = never.stdout.lines().next().unwrap_or_default();
    assert_eq!(
        first,
        format!("{pane} reading · done 4"),
        "{}",
        never.stdout
    );
    assert!(never.stderr.contains("ended"), "{}", never.stderr);
    start("sleep 4; (exit 2)")?;
    let (done, took) = waited(&["wait-done"])?;
    assert_eq!(done.status, 0, "{}{}", done.stdout, done.stderr);
    assert!(
        done.stdout.starts_with(&format!("{pane} reading · done 2")),
        "{}",
        done.stdout
    );
    assert!(took > long, "{took:?}");
    // At a limit given, the hint says how to wait for the command.
    start("sleep 30")?;
    let (limited, _) = waited(&["str", "NEVER", "wait-text", "1", "timeout"])?;
    assert_eq!(limited.status, 124, "{}{}", limited.stdout, limited.stderr);
    assert!(limited.stderr.contains("wait-done"), "{}", limited.stderr);
    server.ok(&[&p[..], &["str", "C-c", "press"]].concat())?;
    // No command runs, nor ended since the last input: the shell reads,
    // and the text cannot come (7).
    server.ok(&[&p[..], &["str", "x", "type"]].concat())?;
    let (none, took) = waited(&["str", "NEVER", "wait-text", "wait-done"])?;
    assert_eq!(none.status, 7, "{}{}", none.stdout, none.stderr);
    assert!(took < long, "{took:?}");
    Ok(())
}

/// A remote client blocked reading its terminal alone, in line mode (ssh
/// asking about a host key, or for a password with echo off), asks at a
/// prompt of its own, here: `reading`, its question answered by keys, not
/// a command at work on another machine. One watching its terminal beside
/// its socket is the client (`agents.rs`'s
/// `a_remote_client_is_not_taken_for_its_remote_program`). Linux says
/// which; macOS does not.
#[test]
fn a_remote_client_asking_at_its_prompt_reads() -> Outcome {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    let server = Server::start("")?;
    for (name, asks) in [
        (
            "host-key",
            "printf 'continue connecting (yes/no)? '; read answer",
        ),
        ("password", "stty -echo; printf 'password: '; read answer"),
    ] {
        let dir = server.dir.join(name);
        std::fs::create_dir_all(&dir).map_err(e)?;
        let ssh = dir.join("ssh");
        std::fs::write(&ssh, format!("#!/bin/sh\n{asks}\necho \"got $answer\"\n")).map_err(e)?;
        std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .map_err(e)?;
        let ssh = ssh.to_string_lossy().into_owned();
        let answer = server.ok(&["opts", "str", &ssh, "program", "10", "timeout", "run"])?;
        let pane = answer
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned();
        assert_eq!(
            becomes(&server, &pane, &[READING])?,
            READING,
            "{name}: {answer}"
        );
        assert!(!answer.contains("another machine"), "{name}: {answer}");
    }
    Ok(())
}
