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
    let expected = if cfg!(target_os = "linux") {
        "reading"
    } else {
        "settled"
    };
    assert_eq!(becomes(&server, &pane, &[expected])?, expected);
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
    if cfg!(target_os = "linux") {
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
    if !cfg!(target_os = "linux") {
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

/// A raw read that times only the gap between keys (VMIN 1, VTIME 10)
/// waits for the first key for good: `reading`, and a text it has not
/// printed cannot come (status 7). A program whose main thread reads its
/// terminal while another sleeps on a timer is not: what that thread
/// prints next is waited for.
#[test]
fn reads_are_told_from_programs_that_go_on_alone() -> Outcome {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    let server = Server::start("")?;
    let gap = "import sys, termios, tty\n\
               tty.setraw(0); m = termios.tcgetattr(0)\n\
               m[6][termios.VMIN] = 1; m[6][termios.VTIME] = 10\n\
               termios.tcsetattr(0, termios.TCSANOW, m)\n\
               sys.stdout.write('ready\\r\\n'); sys.stdout.flush(); sys.stdin.read(1)\n";
    let started = server.ok(&[
        "opts", "str", "python3", "program", "str", "-c", "arg", "str", gap, "arg", "run",
    ])?;
    let pane = started.split_whitespace().next().unwrap_or_default();
    assert!(started.starts_with(&format!("{pane} reading")), "{started}");
    let never = server.fux(&[
        "str",
        pane,
        "pane",
        "opts",
        "str",
        "NOPE",
        "wait-text",
        "5",
        "timeout",
        "wait",
    ])?;
    assert_eq!(never.status, 7, "{}{}", never.stdout, never.stderr);
    let thread = "import threading, time\n\
                  def later(): time.sleep(0.8); print('LATER', flush=True)\n\
                  threading.Thread(target=later).start(); input()\n";
    let later = server.fux(&[
        "opts",
        "str",
        "python3",
        "program",
        "str",
        "-c",
        "arg",
        "str",
        thread,
        "arg",
        "str",
        "LATER",
        "wait-text",
        "run",
    ])?;
    assert_eq!(later.status, 0, "{}{}", later.stdout, later.stderr);
    assert!(later.stdout.ends_with("\nLATER\n"), "{}", later.stdout);
    Ok(())
}
