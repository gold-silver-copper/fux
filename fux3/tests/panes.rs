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

    /// A pane's directory: `WORKSPACE/NAME`, or `NAME` in `main`.
    fn pane(&self, name: &str) -> PathBuf {
        match name.contains('/') {
            true => self.root.join(name),
            false => self.root.join("main").join(name),
        }
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
    let script = format!("stty raw -echo; echo raw-ready; cat > {}", out.display());
    s.start("p", &["sh", "-c", &script])?;
    // Only once the terminal is raw: a line-mode terminal holds a line of
    // at most 1024 bytes on macOS.
    until("raw mode", || screen_has(&s, "p", "raw-ready"))?;
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
    let styled = s.read("p", "screen.styled");
    assert!(!styled.contains("\npicture "), "{styled}");
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
        s.read("p", "errors").contains("in: 6 bytes dropped in all")
    })
}

/// A pane started from inside another pane is part of what that pane's
/// program started. On Linux the outer holder adopts its holder, so killing
/// the outer pane ends it too; macOS has no subreaper, so there it left the
/// outer pane's session for good, outlives the kill, and is ended through
/// its own `ctl`.
#[test]
fn a_pane_started_inside_a_pane_is_part_of_it() -> Result {
    let s = Session::new("nested")?;
    s.start("outer", &["sh"])?;
    let start = format!("{} start inner -- sh\n", env!("CARGO_BIN_EXE_fux3"));
    s.write("outer", "in", start.as_bytes())?;
    until("the inner pane", || s.pane("inner").join("screen").exists())?;
    s.write("outer", "ctl", b"kill\n")?;
    until("the outer pane gone", || !s.pane("outer").exists())?;
    #[cfg(target_os = "linux")]
    until("the inner pane gone with it", || {
        s.fux3(&["ls"]).is_ok_and(|live| live.is_empty())
    })?;
    #[cfg(target_os = "macos")]
    {
        assert_eq!(s.fux3(&["ls"])?, "main/inner\n");
        s.write("inner", "ctl", b"kill\n")?;
        until("the inner pane gone", || !s.pane("inner").exists())?;
    }
    Ok(())
}

/// Once `fux3 start` has returned, the pane's files are there to read.
#[test]
fn start_returns_once_the_files_are_there() -> Result {
    let s = Session::new("ready")?;
    for n in 0..20 {
        let name = format!("p{n}");
        s.start(&name, &["sh"])?;
        for file in ["screen", "screen.styled", "status", "errors", "history"] {
            assert!(
                s.pane(&name).join(file).exists(),
                "{name}/{file} missing after start"
            );
        }
        s.write(&name, "ctl", b"kill\n")?;
    }
    Ok(())
}

/// Workspaces are directories: the same pane name lives in two of them,
/// `ls` lists both, and a dead pane's leftovers are cleared in any of them
/// while an empty workspace stays.
#[test]
fn workspaces_are_directories() -> Result {
    let s = Session::new("spaces")?;
    s.start("rig/build", &["sh"])?;
    s.start("harmony/build", &["sh"])?;
    s.start("harmony/gone", &["sh"])?;
    std::fs::create_dir(s.root.join("empty")).map_err(|e| e.to_string())?;
    std::fs::set_permissions(s.root.join("empty"), std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    s.write("rig/build", "in", b"echo in-rig\n")?;
    until("the rig pane", || screen_has(&s, "rig/build", "in-rig"))?;
    assert!(!screen_has(&s, "harmony/build", "in-rig"));
    let holder = s.holder("harmony/gone").ok_or("no holder pid")?;
    Command::new("kill")
        .args(["-9", &holder.to_string()])
        .status()
        .map_err(|e| e.to_string())?;
    until("the dead pane cleared", || {
        s.fux3(&["ls"])
            .is_ok_and(|l| l == "harmony/build\nrig/build\n")
    })?;
    assert!(!s.pane("harmony/gone").exists());
    assert!(
        s.root.join("empty").is_dir(),
        "an empty workspace was removed"
    );
    Ok(())
}

/// `history` at its cap becomes `history.1` and starts anew, so a reader
/// following it (as `tail -F` does) sees every line once, and the two files
/// together hold the newest lines, in order, under the cap.
#[test]
fn history_rotates_without_repeating_a_line() -> Result {
    let s = Session::new("rotate")?;
    let mut args = vec!["start", "p", "--size", "80x24", "--history", "20000", "--"];
    args.extend_from_slice(&["sh"]);
    s.fux3(&args)?;
    s.write("p", "in", b"stty -echo; seq 1 20000; echo done-$((1+1))\n")?;
    until("the end of output", || screen_has(&s, "p", "done-2"))?;
    let both = || format!("{}{}", s.read("p", "history.1"), s.read("p", "history"));
    until("history settled", || both().contains("\n19970\n"))?;
    let older = s.read("p", "history.1");
    let newer = s.read("p", "history");
    assert!(!older.is_empty(), "no rotation happened");
    assert!(older.len().saturating_add(newer.len()) <= 20000);
    let lines: Vec<u64> = older
        .lines()
        .chain(newer.lines())
        .filter_map(|l| l.parse().ok())
        .collect();
    let next = lines.iter().skip(1);
    assert!(
        lines
            .iter()
            .zip(next)
            .all(|(a, b)| a.saturating_add(1) == *b)
    );
    assert!(
        lines.last().is_some_and(|n| *n >= 19970),
        "{:?}",
        lines.last()
    );
    Ok(())
}

/// `fux3 wait` on a pane that is killed while it waits says the pane is
/// gone, at once, rather than waiting out its time.
#[test]
fn wait_ends_when_its_pane_goes() -> Result {
    let s = Session::new("waitgone")?;
    s.start("p", &["sh"])?;
    until("the prompt", || screen_has(&s, "p", "$"))?;
    let waiter = Command::new(env!("CARGO_BIN_EXE_fux3"))
        .args(["wait", "p", "--ms", "20000"])
        .env("FUX3_DIR", &s.root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(200));
    let start = Instant::now();
    // A screen change would end the wait too: the kill comes first.
    s.write("p", "ctl", b"kill\n")?;
    let out = waiter.wait_with_output().map_err(|e| e.to_string())?;
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "wait outlived its pane"
    );
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(said.contains("gone") || out.status.success(), "{said}");
    Ok(())
}

/// Clearing dead panes removes only what a holder made: an ordinary
/// directory that happens to be named like a pane, in a session directory
/// pointed at a place with other files, stays with all it holds.
#[test]
fn clearing_dead_panes_never_removes_other_directories() -> Result {
    let s = Session::new("keep")?;
    let notes = s.root.join("main").join("notes");
    std::fs::create_dir_all(&notes).map_err(|e| e.to_string())?;
    std::fs::set_permissions(s.root.join("main"), std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    std::fs::write(notes.join("todo.txt"), "keep me").map_err(|e| e.to_string())?;
    s.start("other/p", &["sh"])?;
    assert_eq!(s.fux3(&["ls"])?, "other/p\n");
    assert!(
        notes.join("todo.txt").exists(),
        "an ordinary directory was removed"
    );
    Ok(())
}

/// Narrowing a pane reflows its history; the lines already in `history`
/// are not appended again.
#[test]
fn a_resize_appends_no_line_twice() -> Result {
    let s = Session::new("reflow")?;
    s.fux3(&["start", "p", "--size", "16x3", "--", "sh"])?;
    s.write(
        "p",
        "in",
        b"stty -echo; PS1=; printf 'aaaabbbbccccdddd\\neeeeffffgggghhhh\\n1\\n2\\n3\\n4\\n'\n",
    )?;
    until("both lines in history", || {
        s.read("p", "history").contains("eeeeffffgggghhhh")
    })?;
    s.write("p", "ctl", b"resize 4 3\n")?;
    until("the resize", || {
        s.read("p", "status").contains("\nsize 4 3\n")
    })?;
    s.write("p", "in", b"printf 'x\\ny\\nz\\nw\\n'\n")?;
    until("later output in history", || {
        s.read("p", "history").lines().any(|l| l == "x")
    })?;
    let history = s.read("p", "history");
    let lines = history.lines().filter(|l| *l == "eeeeffffgggghhhh").count();
    assert_eq!(lines, 1, "{history:?}");
    assert!(
        !history.contains("eeee\n"),
        "a reflowed row came back: {history:?}"
    );
    Ok(())
}
