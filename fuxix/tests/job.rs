//! A pane's process tree as fuxix sees it: this process's [`tree`], and a
//! [`Job`]'s members. Their own binary, so no other test's processes come
//! and go meanwhile.
#![cfg(unix)]
use fuxix::process::{Dispositions, Pid, Reach, Scan, Signal, reap_orphans, tree};
use std::collections::HashMap;
use std::io::BufRead;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

type Outcome = Result<(), Box<dyn std::error::Error>>;

fn pid(text: &str) -> Result<Pid, Box<dyn std::error::Error>> {
    Ok(Pid::from_raw(text.trim().parse()?).ok_or("a pid")?)
}

/// Every process's parent, as `ps` lists them.
fn parents() -> Result<HashMap<Pid, Pid>, Box<dyn std::error::Error>> {
    let out = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid="])
        .output()?;
    let listed = String::from_utf8_lossy(&out.stdout).into_owned();
    let pairs = listed.lines().filter_map(|line| {
        let (child, parent) = line.trim().split_once(char::is_whitespace)?;
        Some((pid(child).ok()?, pid(parent).ok()?))
    });
    Ok(pairs.collect())
}

/// `tree()` lists this process's descendants, a grandchild too, and no
/// other process: on macOS not every process the system has. Each one it
/// lists that `ps` saw just before descends from this process there.
#[test]
fn a_tree_is_this_processs_descendants_and_no_other_process() -> Outcome {
    let me = pid(&std::process::id().to_string())?;
    let mut child = Command::new("sh")
        .args(["-c", "sleep 30 & echo $!; wait"])
        .stdout(Stdio::piped())
        .spawn()?;
    let mut line = String::new();
    let out = child.stdout.take().ok_or("its stdout")?;
    std::io::BufReader::new(out).read_line(&mut line)?;
    let grandchild = pid(&line)?;
    let parents = parents()?;
    let found = tree();
    let _ = fuxix::process::kill(grandchild, Signal::Kill);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(found.scan, Scan::Complete);
    let child = Pid::of(&child).ok_or("a pid")?;
    assert!(found.pids.contains(&child) && found.pids.contains(&grandchild));
    for pid in found.pids.iter().filter(|pid| parents.contains_key(pid)) {
        let mut up = *pid;
        for _ in 0..64 {
            match parents.get(&up) {
                Some(parent) if *parent != me => up = *parent,
                _ => break,
            }
        }
        assert_eq!(parents.get(&up), Some(&me), "{pid} is not a descendant");
    }
    Ok(())
}

/// A job's members are the other processes in its session wherever they
/// are, a background job and an orphan (on macOS, launchd's child), not
/// its leader; once they are killed and gone, none are left.
#[test]
fn a_jobs_members_are_the_rest_of_its_session_till_they_are_gone() -> Outcome {
    fuxix::process::adopt_orphans()?;
    let file = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("fuxix-job-{}", std::process::id()));
    let path = file.to_string_lossy();
    let script = format!(
        "set -m; sleep 31 & echo $! > '{path}'; (sleep 32 & echo $! >> '{path}'); exec sleep 30"
    );
    let size = fuxix::terminal::Size::new(24, 80).ok_or("a size")?;
    let (_master, slave) = fuxix::pty::open(size)?;
    let (_child, job) = slave.spawn(&["sh", "-c", &script], |_| {}, Dispositions::Default)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let left = loop {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let pids: Vec<Pid> = text.lines().filter_map(|l| pid(l).ok()).collect();
        if pids.len() == 2 || Instant::now() > deadline {
            break pids;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let _ = std::fs::remove_file(&file);
    let members = job.members();
    assert_eq!(left.len(), 2, "the job and the orphan started");
    assert_eq!(members.scan, Scan::Complete);
    assert!(
        left.iter().all(|pid| members.pids.contains(pid)),
        "{left:?} in {members:?}"
    );
    assert!(!members.pids.contains(&job.leader()));
    let sent = job.signal(Signal::Kill, Reach::Session);
    assert_eq!(sent.scan, Scan::Complete);
    let mut members = job.members();
    while !members.pids.is_empty() && Instant::now() < deadline {
        // On Linux the orphans are this process's to reap.
        let _ = reap_orphans(|pid| pid == job.leader());
        std::thread::sleep(Duration::from_millis(10));
        members = job.members();
    }
    assert_eq!((members.pids, members.scan), (Vec::new(), Scan::Complete));
    let mut job = job;
    while let Err(running) = job.reap() {
        if Instant::now() > deadline {
            return Err("the leader was never reaped".into());
        }
        job = running;
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
