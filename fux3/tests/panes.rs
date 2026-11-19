//! Real panes, driven only through their files, as any client would.
#![cfg(any(target_os = "linux", target_os = "macos"))]
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

type Result<T = ()> = std::result::Result<T, String>;

/// A session directory of the test's own, its holders killed at the end.
struct Session {
    root: PathBuf,
}

impl Session {
    fn new(name: &str) -> Result<Session> {
        let base = fuxix::file::scratch().map_err(|e| e.to_string())?;
        let root = base.join(format!("fux3-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        Ok(Session { root })
    }

    fn fux3(&self, args: &[&str]) -> Result<String> {
        let out = Command::new(env!("CARGO_BIN_EXE_fux3"))
            .args(args)
            .env("FUX3_DIR", &self.root)
            .output()
            .map_err(|e| e.to_string())?;
        match out.status.success() {
            true => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
            false => Err(String::from_utf8_lossy(&out.stderr).into_owned()),
        }
    }

    fn start(&self, name: &str, program: &[&str]) -> Result {
        let mut args = vec!["start", name, "--size", "80x24", "--"];
        args.extend_from_slice(program);
        self.fux3(&args).map(drop)
    }

    fn pane(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn read(&self, name: &str, file: &str) -> String {
        std::fs::read_to_string(self.pane(name).join(file)).unwrap_or_default()
    }

    fn holder(&self, name: &str) -> Option<i32> {
        let status = self.read(name, "status");
        let line = status.lines().find_map(|l| l.strip_prefix("holder "))?;
        line.parse().ok()
    }

    /// Writes `bytes` to a pane's FIFO, as `printf > file` does.
    fn write(&self, name: &str, fifo: &str, bytes: &[u8]) -> Result {
        let mut file = std::fs::File::options()
            .write(true)
            .open(self.pane(name).join(fifo))
            .map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        for name in self.fux3(&["ls"]).unwrap_or_default().lines() {
            let _ = self.write(name, "ctl", b"kill\n");
        }
        let start = Instant::now();
        while self.fux3(&["ls"]).is_ok_and(|l| !l.is_empty())
            && start.elapsed() < Duration::from_secs(5)
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Waits up to ten seconds for `test`.
fn until(what: &str, mut test: impl FnMut() -> bool) -> Result {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if test() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(format!("no {what} within ten seconds"))
}

fn screen_has(s: &Session, name: &str, text: &str) -> bool {
    s.read(name, "screen")
        .lines()
        .skip(1)
        .any(|l| l.contains(text))
}

/// Two writers, one after the other, both reach the program; then the
/// holder is killed outright. A FIFO with no reader would block a writer's
/// `open` for ever, so it is opened without waiting, which says no one
/// reads; any fux3 command then clears the leftovers, and a writer is told
/// at once that the pane is gone.
#[test]
fn two_writers_then_a_holder_killed_outright() -> Result {
    let s = Session::new("fifo")?;
    s.start("p", &["sh"])?;
    s.write("p", "in", b"echo one-$((1+1))\n")?;
    s.write("p", "in", b"echo two-$((2+2))\n")?;
    until("both lines", || {
        screen_has(&s, "p", "one-2") && screen_has(&s, "p", "two-4")
    })?;
    let holder = s.holder("p").ok_or("no holder pid")?;
    let killed = Command::new("kill")
        .args(["-9", &holder.to_string()])
        .status()
        .map_err(|e| e.to_string())?;
    assert!(killed.success());
    let in_fifo = s.pane("p").join("in");
    until("the holder gone", || {
        std::fs::File::options()
            .write(true)
            .custom_flags(fuxix::file::NONBLOCK)
            .open(&in_fifo)
            .is_err_and(|e| e.raw_os_error() == Some(6))
    })?;
    assert_eq!(s.fux3(&["ls"])?, "");
    assert!(!s.pane("p").exists(), "the dead pane's leftovers stayed");
    let late = std::fs::File::options().write(true).open(&in_fifo);
    assert!(late.is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound));
    Ok(())
}

/// A send is all in the program's input or none of it: typing on `in` at
/// the same time never lands inside it.
#[test]
fn a_send_is_never_split_by_typing() -> Result {
    let s = Session::new("send")?;
    let out = s.root.join("received");
    let script = format!("stty raw -echo; cat > {}", out.display());
    s.start("p", &["sh", "-c", &script])?;
    until("raw mode", || {
        s.read("p", "status").contains("state running")
    })?;
    std::thread::sleep(Duration::from_millis(300));
    let sent = s.root.join("sent");
    std::fs::write(&sent, std::iter::repeat_n('S', 400_000).collect::<String>())
        .map_err(|e| e.to_string())?;
    let typist = {
        let fifo = s.pane("p").join("in");
        std::thread::spawn(move || {
            for _ in 0..200 {
                if let Ok(mut f) = std::fs::File::options().write(true).open(&fifo) {
                    let _ = f.write_all(b"t");
                }
            }
        })
    };
    s.write("p", "ctl", format!("send {}\n", sent.display()).as_bytes())?;
    typist.join().map_err(|_| "the typist failed")?;
    until("everything received", || {
        std::fs::metadata(&out).is_ok_and(|m| m.len() >= 400_200)
    })?;
    let got = std::fs::read_to_string(&out).map_err(|e| e.to_string())?;
    let first = got.find('S').ok_or("no send")?;
    let last = got.rfind('S').ok_or("no send")?;
    assert_eq!(
        last.saturating_sub(first).saturating_add(1),
        400_000,
        "typing landed inside the send"
    );
    // What `status` says waits drains with it, with nothing else printed.
    until("queued 0", || {
        s.read("p", "status").contains("\nqueued 0\n")
    })
}

/// A program asking where its cursor is gets its answer and carries on,
/// with nobody watching but `cat`.
#[test]
fn a_query_is_answered_with_nobody_watching() -> Result {
    let s = Session::new("query")?;
    let script = "stty -echo -icanon min 1; printf '\\033[6n'; \
                  r=$(dd bs=1 count=6 2>/dev/null); echo \"got${r#?}\"; sleep 100";
    s.start("p", &["sh", "-c", script])?;
    until("the answer", || screen_has(&s, "p", "got[1;1R"))
}

/// The directory and every entry in it are the user's alone, and a kill
/// removes the directory with everything the program started.
#[test]
fn files_are_private_and_kill_leaves_nothing() -> Result {
    let s = Session::new("private")?;
    let pids = s.root.join("pids");
    let script = format!("sleep 1000 & echo $! > {}; exec sleep 1000", pids.display());
    s.start("p", &["sh", "-c", &script])?;
    until("the screen", || s.pane("p").join("screen").exists())?;
    let mode =
        |path: &Path| std::fs::symlink_metadata(path).map_or(0, |m| m.permissions().mode() & 0o777);
    assert_eq!(mode(&s.pane("p")), 0o700);
    for entry in std::fs::read_dir(s.pane("p")).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let want = if path.is_dir() { 0o700 } else { 0o600 };
        assert_eq!(mode(&path), want, "{}", path.display());
    }
    until("the child's pid", || {
        std::fs::metadata(&pids).is_ok_and(|m| m.len() > 0)
    })?;
    let child = std::fs::read_to_string(&pids).map_err(|e| e.to_string())?;
    s.write("p", "ctl", b"kill\n")?;
    until("the directory gone", || !s.pane("p").exists())?;
    let alive = Command::new("kill")
        .args(["-0", child.trim()])
        .status()
        .map_err(|e| e.to_string())?;
    assert!(
        !alive.success(),
        "a process the program started outlived kill"
    );
    Ok(())
}

/// A sixel picture is written once to `images/`, named by the styled
/// screen, readable with `cat`; once the program erases it, its file goes.
#[test]
fn a_picture_comes_and_goes_as_a_file() -> Result {
    let s = Session::new("picture")?;
    s.start("p", &["sh"])?;
    // Twelve pixel rows of red, six columns wide: two sixel bands.
    let draw = "printf '\\033Pq#0;2;100;0;0#0!6~-#0!6~\\033\\\\'; echo\n";
    s.write("p", "in", draw.as_bytes())?;
    until("a picture line", || {
        s.read("p", "screen.styled").contains("\npicture 1.sixel")
    })?;
    let picture = s.pane("p").join("images/1.sixel");
    let bytes = std::fs::read(&picture).map_err(|e| e.to_string())?;
    assert!(bytes.starts_with(b"\x1bP") && bytes.ends_with(b"\x1b\\"));
    s.write("p", "in", b"clear\n")?;
    until("the picture's file gone", || !picture.exists())?;
    // Drawn again, then scrolled wholly into history: no longer shown.
    s.write("p", "in", draw.as_bytes())?;
    let second = s.pane("p").join("images/2.sixel");
    until("the second picture", || second.exists())?;
    s.write("p", "in", b"seq 1 40\n")?;
    until("the scrolled picture's file gone", || !second.exists())?;
    assert!(!s.read("p", "screen.styled").contains("\npicture "));
    Ok(())
}

/// Once the program has ended, its last screen stays: what a child it
/// left behind prints later changes nothing, and typing is dropped with a
/// line in `errors`.
#[test]
fn an_ended_programs_screen_stays() -> Result {
    let s = Session::new("ended")?;
    s.start(
        "p",
        &[
            "sh",
            "-c",
            "(trap \"\" HUP; sleep 0.5; echo late) & echo first; exit 4",
        ],
    )?;
    until("the exit", || {
        s.read("p", "status").contains("\nstate exited 4\n")
    })?;
    assert!(screen_has(&s, "p", "first"));
    std::thread::sleep(Duration::from_millis(1000));
    assert!(
        !screen_has(&s, "p", "late"),
        "the screen changed after the exit"
    );
    s.write("p", "in", b"typed\n")?;
    until("the dropped bytes", || {
        s.read("p", "errors").contains("in: 6 bytes dropped")
    })
}
