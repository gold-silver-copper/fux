//! What becomes of a pane's processes: background jobs when the pane
//! closes, and a leader that is stopped rather than exited.
mod support;
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
