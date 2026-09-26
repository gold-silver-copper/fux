//! What becomes of a pane's processes: background jobs when the pane
//! closes, and a leader that is stopped rather than exited.
mod support;
use fuxix::process::Signal;
use std::time::Duration;
use support::*;

/// How long a pane's processes may outlive it: the hang-up grace, and
/// room for a loaded machine.
const GONE_WITHIN: Duration = Duration::from_secs(3);

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
/// itself. However its pane closes, the job ends with it; a `nohup` job
/// survives, as it would a closed terminal (bevy-final finding 013).
#[test]
fn a_closed_panes_background_jobs_end_with_it_under_dash() -> Outcome {
    let server = Server::start("set shell /bin/dash")?;
    let mut reap = Reap::default();
    for how in ["kill-pane", "exit", "kill-workspace"] {
        if how == "kill-workspace" {
            server.ok(&["new-workspace", "-n", "doomed"])?;
        } else {
            server.ok(&["split", "-h", "-t", "%1"])?;
        }
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
        match how {
            "kill-pane" => {
                server.ok(&["kill-pane", "-t", &pane])?;
            }
            "exit" => server.type_line(&pane, "exit")?,
            _ => {
                server.ok(&["kill-workspace", "-t", "doomed"])?;
            }
        }
        gone(job, &format!("a background job, after {how}"))?;
        assert!(alive(kept), "a nohup job ended, after {how}");
    }
    Ok(())
}

/// `terminate` ends what runs in the pane's foreground and nothing else: a
/// background job and the shell stay, as the README says.
#[test]
fn terminate_ends_the_foreground_and_leaves_background_jobs() -> Outcome {
    let server = Server::start("set shell /bin/dash")?;
    server.ok(&["split", "-h", "-t", "%1"])?;
    let (pane, shell) = server.newest_pane()?;
    let job = server.dir.join("job");
    let front = server.dir.join("front");
    server.type_line(
        &pane,
        &format!(
            "sleep 300 & echo $! > '{}'; sh -c 'echo $$ > \"$0\"; exec sleep 302' '{}'",
            job.display(),
            front.display()
        ),
    )?;
    let (job, front) = (pid_in(&job)?, pid_in(&front)?);
    let _reap = Reap(vec![job, front]);
    eventually("the foreground program running", || {
        Ok(server.fux(&["terminate", "-t", &pane])?.status == 0)
    })?;
    gone(front, "the foreground program, after terminate")?;
    assert!(alive(job), "terminate ended a background job");
    assert!(alive(shell), "terminate ended the shell");
    assert!(server.panes()?.iter().any(|(id, _)| *id == pane));
    // Closing the pane ends the job.
    server.ok(&["kill-pane", "-t", &pane])?;
    gone(job, "a background job, after kill-pane")
}

/// A pane whose program is stopped is not one whose program exited: it
/// stays, and runs again on SIGCONT. Only an exit or a kill closes it, and
/// its viewers are told the status (bevy-final finding 020, where macOS
/// reported the stop as an exit, status 145).
#[test]
fn a_stopped_pane_stays_until_its_program_ends() -> Outcome {
    let server = Server::start("")?;
    server.ok(&["split", "-h", "-t", "%1"])?;
    let mut client = server.attach(20, 100)?;
    let (pane, leader) = server.newest_pane()?;
    // Settled: the new shell has a prompt.
    eventually("a prompt", || {
        Ok(server.ok(&["capture-pane", "-t", &pane])?.contains('$'))
    })?;
    signal(leader, Signal::Stop);
    std::thread::sleep(Duration::from_secs(1));
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
            .ok(&["capture-pane", "-t", &pane])?
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
    server.ok(&["split", "-h", "-t", "%1"])?;
    let (pane, _) = server.newest_pane()?;
    let front = server.dir.join("front");
    server.type_line(
        &pane,
        &format!(
            "sh -c 'echo $$ > \"$0\"; exec sleep 300' '{}'",
            front.display()
        ),
    )?;
    let job = pid_in(&front)?;
    let _reap = Reap(vec![job]);
    server.ok(&["send-keys", "-t", &pane, "C-z"])?;
    server.type_line(&pane, "echo still-$((2+2))")?;
    eventually("the shell answers", || {
        Ok(server
            .ok(&["capture-pane", "-t", &pane])?
            .contains("still-4"))
    })?;
    assert!(alive(job), "the stopped job ended");
    assert!(server.panes()?.iter().any(|(id, _)| *id == pane));
    server.ok(&["kill-pane", "-t", &pane])?;
    gone(job, "a stopped job, after kill-pane")
}
