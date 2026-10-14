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
        "keys",
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

/// A shell command that pauses, polling its terminal with a time limit, is
/// at work until it ends: `line` answers with its end and its output, not
/// with the quiet before them.
#[test]
fn a_shell_command_that_polls_is_answered_when_it_ends() -> Outcome {
    if !PROBED {
        return Ok(());
    }
    let server = Server::start("")?;
    let pane = server
        .ok(&[
            "opts", "str", "bash", "program", "str", "--norc", "arg", "str", "-i", "arg", "run",
        ])?
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    let pause = "python3 -c 'import select,sys; print(\"start\", flush=True); \
                 [select.select([sys.stdin], [], [], 0.1) for _ in range(25)]; \
                 print(\"end\")'; (exit 5)";
    let answer = server.ok(&["str", &pane, "pane", "str", pause, "line"])?;
    let first = answer.lines().next().unwrap_or_default();
    assert_eq!(first, format!("{pane} reading · done 5"), "{answer}");
    assert!(answer.contains("\nend\n"), "{answer}");
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
/// sleeps is asleep: busy. Nor, with what it may not be traced, is one
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
    ];
    for (code, state, status) in programs {
        let (pane, now) = started(&server, code)?;
        assert_eq!(now, state, "{code}");
        let never = server.fux(&[
            "str",
            &pane,
            "pane",
            "opts",
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
    assert!(state.ends_with(" busy"), "{}", busy.stdout);
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

/// A pane `waiting` is answered as it is: a capture settles at once, as an
/// action does; a bare `wait` waits for something new, and at its limit
/// answers the state with status 0, as only a busy pane fails (124).
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
    let capture = server.fux(&["str", &pane, "pane", "opts", "5", "timeout", "capture"])?;
    assert_eq!(capture.status, 0, "{}{}", capture.stdout, capture.stderr);
    assert!(
        capture
            .stdout
            .starts_with(&format!("{pane} waiting\nPOLLING")),
        "{}",
        capture.stdout
    );
    assert!(began.elapsed() < std::time::Duration::from_secs(3));
    let wait = server.fux(&["str", &pane, "pane", "opts", "1", "timeout", "wait"])?;
    assert_eq!(wait.status, 0, "{}{}", wait.stdout, wait.stderr);
    assert!(
        wait.stdout.starts_with(&format!("{pane} waiting")),
        "{}",
        wait.stdout
    );
    // With a wait's own limit, nothing new within 5s: answered as it is.
    let began = std::time::Instant::now();
    let plain = server.fux(&["str", &pane, "pane", "opts", "wait"])?;
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
    let pane = server
        .ok(&[
            "opts", "str", "bash", "program", "str", "--norc", "arg", "str", "-i", "arg", "run",
        ])?
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
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
    let screen = server.ok(&[&p[..], &["opts", "0", "timeout", "capture"]].concat())?;
    assert!(screen.contains("\nHOOKED\n"), "{screen}");
    Ok(())
}
