//! fuxix-ptop against real jobs: the batch view shows a job's tree with its
//! orphans, and in the interactive view `K` ends the selected group.

use std::io::{Read, Write};
use std::process::Command;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_fuxix-ptop");

#[test]
fn the_batch_view_shows_the_job_its_children_and_its_orphans() -> Result<(), String> {
    // The subshell's sleep is orphaned once the subshell exits, and adopted.
    let out = Command::new(BIN)
        .args([
            "-b",
            "-n",
            "2",
            "-d",
            "0.3",
            "--",
            "sh",
            "-c",
            "(sleep 30 &); sleep 31; true",
        ])
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let last = text.rsplit("job ").next().ok_or("no snapshot")?;
    let rows: Vec<Vec<&str>> = last
        .lines()
        .skip(2)
        .map(|l| l.split_whitespace().collect())
        .collect();
    let sh = rows
        .iter()
        .find(|r| r.last() == Some(&"sh"))
        .ok_or(format!("no sh in\n{last}"))?;
    let sleeps: Vec<_> = rows.iter().filter(|r| r.last() == Some(&"sleep")).collect();
    assert_eq!(sleeps.len(), 2, "{last}");
    // The leader leads its session and group; both sleeps are in them.
    assert_eq!(
        (sh.first(), sh.get(2), sh.get(3)),
        (sh.first(), sh.first(), sh.first())
    );
    for s in sleeps {
        assert_eq!((s.get(2), s.get(3)), (sh.first(), sh.first()), "{last}");
    }
    Ok(())
}

#[test]
fn k_in_the_interactive_view_terms_the_selected_group() -> Result<(), String> {
    let size = fuxix::terminal::Size::new(30, 120).ok_or("size")?;
    let (mut master, slave) = fuxix::pty::open(size).map_err(|e| e.to_string())?;
    let (mut child, pid) = slave
        .spawn(
            &[
                BIN,
                "-d",
                "0.2",
                "--",
                "sh",
                "-c",
                "sleep 40 & sleep 41; true",
            ],
            |_| {},
            fuxix::process::Dispositions::Default,
        )
        .map_err(|e| e.to_string())?;
    let mut seen = Vec::new();
    let read_until =
        |master: &mut fuxix::pty::Master, want: &str, seen: &mut Vec<u8>| -> Result<(), String> {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                let mut buf = [0u8; 4096];
                match master.read(&mut buf) {
                    Ok(n) => seen.extend_from_slice(buf.get(..n).unwrap_or_default()),
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
                if String::from_utf8_lossy(seen).contains(want) {
                    return Ok(());
                }
            }
            Err(format!("no {want:?} in {}", String::from_utf8_lossy(seen)))
        };
    read_until(&mut master, "3 processes", &mut seen)?;
    // The first row in tree order is the leader: its group is the job.
    master.write_all(b"K").map_err(|e| e.to_string())?;
    read_until(&mut master, "SIGTERM sent to group", &mut seen)?;
    read_until(&mut master, "killed by SIGTERM | 0 processes", &mut seen)?;
    master.write_all(b"q").map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            assert!(status.success(), "{status}");
            return Ok(());
        }
        if Instant::now() > deadline {
            let _ = fuxix::process::kill(pid, fuxix::process::Signal::Kill);
            return Err("fuxix-ptop did not quit".to_owned());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
