//! What becomes of a pane's processes: background jobs when the pane
//! closes, and a leader that is stopped rather than exited.
mod support;
use fuxix::process::Signal;
use std::time::Duration;
use support::*;

/// How long a pane's processes may outlive it: the hang-up grace, and
/// room for a loaded machine.
const GONE_WITHIN: Duration = Duration::from_secs(3);

/// Types a line whose program keeps running, in the foreground, once the
/// pane's shell shows a prompt.
fn start_line(server: &Server, pane: &str, line: &str) -> Outcome {
    eventually(&format!("a prompt in {pane}"), || {
        Ok(server.ok(&["str", pane, "pane", "capture"])?.contains('$'))
    })?;
    server.ok(&[
        "str",
        pane,
        "pane",
        "str",
        line,
        "text",
        "str",
        "Enter",
        "key",
        "send-keys",
    ])?;
    Ok(())
}

fn gone(pid: i32, what: &str) -> Outcome {
    let deadline = after(GONE_WITHIN);
    while alive(pid) {
        if std::time::Instant::now() > deadline {
            return Err(format!("{what} (pid {pid}) outlived its pane"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

/// dash starts a background job in a process group of its own, out of reach
/// of a signal to the shell's group, and does not hang it up when it exits
/// itself. However its pane closes, the job ends with it. A `nohup` job outlives its shell's own `exit`, as it would a closed
/// terminal; a pane fux closes takes its whole session, the `nohup` job too,
/// once the hang-up's grace is over.
#[test]
fn a_closed_panes_background_jobs_end_with_it_under_dash() -> Outcome {
    // dash's own way with jobs is what is tested; CI's machines all have it.
    let path = std::env::var_os("PATH").unwrap_or_default();
    let Some(dash) = std::env::split_paths(&path)
        .map(|dir| dir.join("dash"))
        .find(|dash| dash.is_file())
    else {
        eprintln!("skipped: dash is not installed");
        return Ok(());
    };
    let dash = fux::words::quote(&dash.to_string_lossy());
    let server = Server::start(&format!("str shell list str {dash} end set"))?;
    let mut reap = Reap::default();
    for how in ["pane kill", "exit", "workspace kill"] {
        let launch = if how == "workspace kill" {
            "opts 80 24 size str doomed name new-workspace"
        } else {
            "%1 horizontal split"
        };
        server.ok(&[&format!("str shell list str {dash} end set {launch}")])?;
        let (pane, _) = server.newest_pane()?;
        let job = server.dir.join(format!("job-{how}"));
        let kept = server.dir.join(format!("nohup-{how}"));
        server.type_line(
            &pane,
            &format!(
                "sleep 300 & echo $! > '{}'; nohup sleep 301 > /dev/null 2>&1 & echo $! > '{}'",
                job.display(),
                kept.display()
            ),
        )?;
        let (job, kept) = (pid_in(&job)?, pid_in(&kept)?);
        reap.0.extend([job, kept]);
        assert!(alive(job) && alive(kept));
        // `$!` is written once the shell forks; nohup ignores SIGHUP only
        // after that, before it runs sleep. Until then a hang-up ends it.
        eventually("the nohup job to run sleep", || {
            let out = std::process::Command::new("ps")
                .args(["-o", "comm=", "-p", &kept.to_string()])
                .output()
                .map_err(e)?;
            Ok(String::from_utf8_lossy(&out.stdout)
                .trim_end()
                .ends_with("sleep"))
        })?;
        match how {
            "pane kill" => {
                server.ok(&["str", &pane, "pane", "kill"])?;
            }
            // The shell exits and its pane closes: answered with status 4.
            "exit" => {
                let keys = ["str", "exit", "text", "str", "Enter", "key", "send-keys"];
                let out = server.fux(&[&["str", &pane, "pane"][..], &keys].concat())?;
                assert!(matches!(out.status, 0 | 4), "{}", out.stderr);
            }
            _ => {
                server.ok(&["str", "doomed", "workspace", "kill"])?;
            }
        }
        gone(job, &format!("a background job, after {how}"))?;
        if how == "exit" {
            assert!(alive(kept), "a nohup job ended, after {how}");
        } else {
            gone(kept, &format!("a nohup job, after {how}"))?;
        }
    }
    Ok(())
}

/// `signal TERM` ends what runs in the pane's foreground and nothing else: a
/// background job and the shell stay, as the README says.
#[test]
fn signal_ends_the_foreground_and_leaves_background_jobs() -> Outcome {
    let server = Server::start("")?;
    server.ok(&["str", "%1", "pane", "horizontal", "split"])?;
    let (pane, shell) = server.newest_pane()?;
    let job = server.dir.join("job");
    let front = server.dir.join("front");
    start_line(
        &server,
        &pane,
        &format!(
            "sleep 300 & echo $! > '{}'; sh -c 'echo $$ > \"$0\"; exec sleep 302' '{}'",
            job.display(),
            front.display()
        ),
    )?;
    let (job, front) = (pid_in(&job)?, pid_in(&front)?);
    let _reap = Reap(vec![job, front]);
    let answer = server.ok(&["str", &pane, "pane", "str", "TERM", "signal"])?;
    assert!(answer.starts_with(&format!("{pane}:")), "{answer}");
    gone(front, "the foreground program, after signal TERM")?;
    assert!(alive(job), "signal ended a background job");
    assert!(alive(shell), "signal ended the shell");
    assert!(server.panes()?.iter().any(|(id, _)| *id == pane));
    // Closing the pane ends the job.
    server.ok(&["str", &pane, "pane", "kill"])?;
    gone(job, "a background job, after pane kill")
}

/// A pane whose program is stopped is not one whose program exited: it
/// stays, and runs again on SIGCONT. Only an exit or a kill closes it, and
/// its viewers are told the status (macOS
/// reported the stop as an exit, status 145).
#[test]
fn a_stopped_pane_stays_until_its_program_ends() -> Outcome {
    let server = Server::start("")?;
    server.ok(&["str", "%1", "pane", "horizontal", "split"])?;
    let mut client = server.attach(20, 100)?;
    let (pane, leader) = server.newest_pane()?;
    // Settled: the new shell has a prompt.
    eventually("a prompt", || {
        Ok(server.ok(&["str", &pane, "pane", "capture"])?.contains('$'))
    })?;
    signal(leader, Signal::Stop);
    // Stopped, the kernel has sent the server SIGCHLD; two round trips
    // through its loop later, it has handled it.
    eventually("the shell stopped", || {
        let out = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &leader.to_string()])
            .output()
            .map_err(e)?;
        Ok(String::from_utf8_lossy(&out.stdout)
            .trim_start()
            .starts_with('T'))
    })?;
    server.ok(&["ls"])?;
    server.ok(&["ls"])?;
    assert!(alive(leader), "the stopped shell is gone");
    assert!(
        server.panes()?.iter().any(|(id, _)| *id == pane),
        "the stopped pane closed: {}",
        server.ok(&["ls"])?
    );
    client.pump()?;
    assert!(!client.bar().contains("exited"), "{}", client.bar());
    signal(leader, Signal::Cont);
    server.type_line(&pane, "echo alive-$((1+1))")?;
    eventually("the shell answers", || {
        Ok(server
            .ok(&["str", &pane, "pane", "capture"])?
            .contains("alive-2"))
    })?;
    // A kill closes it, with the signal's status: 128 + 9.
    signal(leader, Signal::Kill);
    eventually("the pane closed", || {
        Ok(!server.panes()?.iter().any(|(id, _)| *id == pane))
    })?;
    client.wait("the status", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains(pane.as_str()) && b.contains("exited with status 137"))
    })?;
    Ok(())
}

/// A program the shell stops with Ctrl-Z leaves the pane and its shell as
/// they were; the stopped job ends when the pane closes.
#[test]
fn a_job_stopped_in_a_pane_leaves_the_pane_running() -> Outcome {
    let server = Server::start("")?;
    server.ok(&["str", "%1", "pane", "horizontal", "split"])?;
    let (pane, _) = server.newest_pane()?;
    let front = server.dir.join("front");
    start_line(
        &server,
        &pane,
        &format!(
            "sh -c 'echo $$ > \"$0\"; exec sleep 300' '{}'",
            front.display()
        ),
    )?;
    let job = pid_in(&front)?;
    let _reap = Reap(vec![job]);
    server.ok(&["str", &pane, "pane", "str", "C-z", "press"])?;
    server.type_line(&pane, "echo still-$((2+2))")?;
    eventually("the shell answers", || {
        Ok(server
            .ok(&["str", &pane, "pane", "capture"])?
            .contains("still-4"))
    })?;
    assert!(alive(job), "the stopped job ended");
    assert!(server.panes()?.iter().any(|(id, _)| *id == pane));
    server.ok(&["str", &pane, "pane", "kill"])?;
    gone(job, "a stopped job, after pane kill")
}

/// A program a pane's program started and left (a double fork) becomes the
/// server's child, not init's (Linux): once it exits the server reaps it,
/// leaving no zombie.
#[cfg(target_os = "linux")]
#[test]
fn an_orphan_a_pane_leaves_is_read_and_reaped() -> Outcome {
    let server = Server::start("")?;
    let (fifo, pidfile) = (server.dir.join("fifo"), server.dir.join("orphan"));
    // The subshell starts cat and exits; the shell waits on a pipe, not
    // the terminal, so only cat can make the pane read.
    let code = "mkfifo \"$0\"; (cat </dev/tty & echo $! > \"$1\"); read x <> \"$0\"";
    let (fifo, pidfile) = (fifo.to_string_lossy(), pidfile.to_string_lossy());
    let answer = server.ok(&[
        "agent-opts",
        "80",
        "24",
        "size",
        "str",
        "sh",
        "program",
        "str",
        "-c",
        "arg",
        "str",
        code,
        "arg",
        "str",
        &fifo,
        "arg",
        "str",
        &pidfile,
        "arg",
        "run",
    ])?;
    let pane = answer
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    let orphan = pid_in(std::path::Path::new(&*pidfile))?;
    let _reap = Reap(vec![orphan]);
    // End of input: cat exits, and the server, its parent, reaps it.
    let gone = fuxix::process::Pid::from_raw(orphan).ok_or("a pid")?;
    eventually("the orphan adopted", || {
        let stat = std::fs::read_to_string(format!("/proc/{orphan}/stat")).map_err(e)?;
        let parent = stat
            .rsplit(')')
            .next()
            .and_then(|r| r.split_whitespace().nth(1));
        Ok(parent == server.pid().map(|p| p.to_string()).as_deref())
    })?;
    server.ok(&["str", &pane, "pane", "str", "C-d", "press"])?;
    eventually("the orphan reaped", || Ok(!fuxix::process::exists(gone)))
}
