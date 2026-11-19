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

/// `exit` appears once the program has ended, holding `status`'s state
/// line, and only after the last screen is there: a reader that sees it
/// finds the program's last words on `screen`.
#[test]
fn exit_appears_after_the_last_screen() -> Result {
    let s = Session::new("exit")?;
    s.start("p", &["sh", "-c", "sleep 0.3; echo last-words; exit 7"])?;
    assert!(!s.pane("p").join("exit").exists());
    until("exit", || s.pane("p").join("exit").exists())?;
    assert_eq!(s.read("p", "exit"), "state exited 7\n");
    assert!(screen_has(&s, "p", "last-words"));
    Ok(())
}

/// A start writes the pane's recipe, an `sh` script outside the pane's
/// directory; after a kill, `fux3 start NAME` with no program runs it: the
/// same program, in the same directory, with the variables named at start.
#[test]
fn a_recipe_starts_the_same_program_again() -> Result {
    let s = Session::new("recipe")?;
    let run = s.root.join("ran");
    let script = format!("echo \"$PWD $FUX3_T\" >> {}; exec sleep 100", run.display());
    let out = Command::new(env!("CARGO_BIN_EXE_fux3"))
        .args([
            "start",
            "p",
            "--env",
            "FUX3_T=it's",
            "--",
            "sh",
            "-c",
            &script,
        ])
        .env("FUX3_DIR", &s.root)
        .current_dir(&s.root)
        .output()
        .map_err(|e| e.to_string())?;
    assert!(out.status.success(), "{out:?}");
    let recipe = s.root.join("main/.recipes/p");
    let saved = std::fs::read_to_string(&recipe).map_err(|e| e.to_string())?;
    assert!(
        !saved.contains("HOME="),
        "the recipe holds an inherited variable"
    );
    // The variable's value is no argument of the holder's: arguments are
    // public, a program's environment is not.
    #[cfg(target_os = "linux")]
    {
        let holder = s.holder("p").ok_or("no holder pid")?;
        let args = std::fs::read(format!("/proc/{holder}/cmdline")).map_err(|e| e.to_string())?;
        assert!(
            !String::from_utf8_lossy(&args).contains("it's"),
            "--env is in the holder's arguments"
        );
    }
    until("the first run", || {
        std::fs::read_to_string(&run).is_ok_and(|t| t.lines().count() == 1)
    })?;
    assert!(std::fs::read_to_string(&recipe).is_ok_and(|t| t.starts_with("#!/bin/sh\n")));
    s.write("p", "ctl", b"kill\n")?;
    until("the pane gone", || !s.pane("p").exists())?;
    assert!(recipe.exists(), "a kill removed the recipe");
    s.fux3(&["start", "p"])?;
    until("the second run", || {
        std::fs::read_to_string(&run).is_ok_and(|t| t.lines().count() == 2)
    })?;
    let text = std::fs::read_to_string(&run).map_err(|e| e.to_string())?;
    let mut lines = text.lines();
    assert_eq!(lines.next(), lines.next(), "the second run differs");
    assert!(text.ends_with(" it's\n"), "{text:?}");
    // A start that fails leaves the recipe of the last good one; a
    // variable sh keeps for itself is refused, as its recipe could not run.
    s.write("p", "ctl", b"kill\n")?;
    until("the pane gone", || !s.pane("p").exists())?;
    let good = std::fs::read(&recipe).map_err(|e| e.to_string())?;
    assert!(s.start("p", &["/no/such/program"]).is_err());
    let refused = s.fux3(&["start", "p", "--env", "PPID=1", "--", "sh"]);
    assert!(refused.is_err_and(|e| e.contains("PPID")));
    assert_eq!(
        std::fs::read(&recipe).ok(),
        Some(good),
        "a failed start's recipe"
    );
    // A program whose name starts with `-` runs again, with the size and
    // labels its first start had.
    let dash = s.root.join("-odd");
    std::fs::write(&dash, "#!/bin/sh\necho ran-$((40+2)); exec sleep 100\n")
        .map_err(|e| e.to_string())?;
    std::fs::set_permissions(&dash, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    let out = Command::new(env!("CARGO_BIN_EXE_fux3"))
        .args([
            "start", "d", "--size", "50x7", "--label", "role=db", "--", "-odd",
        ])
        .env("FUX3_DIR", &s.root)
        .env(
            "PATH",
            format!(
                "{}:{}",
                s.root.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .current_dir(&s.root)
        .output()
        .map_err(|e| e.to_string())?;
    assert!(out.status.success(), "{out:?}");
    s.write("d", "ctl", b"kill\n")?;
    until("the pane gone", || !s.pane("d").exists())?;
    let again = Command::new(env!("CARGO_BIN_EXE_fux3"))
        .args(["start", "d"])
        .env("FUX3_DIR", &s.root)
        .env(
            "PATH",
            format!(
                "{}:{}",
                s.root.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .output()
        .map_err(|e| e.to_string())?;
    assert!(again.status.success(), "{again:?}");
    until("the second run", || screen_has(&s, "d", "ran-42"))?;
    let status = s.read("d", "status");
    assert!(
        status.contains("\nsize 50 7\n") && status.contains("\nlabel role db\n"),
        "{status}"
    );
    Ok(())
}

/// `fux3 put` writes to a live pane, and refuses at once, saying why, a
/// pane that is not there, one whose holder is dead, and more than 512 bytes.
#[test]
fn put_writes_or_refuses_but_never_waits() -> Result {
    let s = Session::new("put")?;
    s.start("p", &["sh"])?;
    s.fux3(&["put", "p", "echo put-$((3+4))\n"])?;
    until("the put line", || screen_has(&s, "p", "put-7"))?;
    let refused = |args: &[&str]| -> Result<String> {
        let start = Instant::now();
        let said = s.fux3(args).err().ok_or("it was not refused")?;
        assert!(start.elapsed() < Duration::from_secs(3), "put waited");
        Ok(said)
    };
    assert!(refused(&["put", "nobody", "x"])?.contains("no pane"));
    let long: String = std::iter::repeat_n('x', 600).collect();
    assert!(refused(&["put", "p", &long])?.contains("512"));
    // A pane whose `in` nobody reads, its lock held as a live holder's
    // is: a blocking open would wait for ever.
    let q = s.pane("q");
    std::fs::create_dir(&q).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&q, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    for fifo in ["in", "ctl"] {
        fuxix::file::private_fifo(&q.join(fifo)).map_err(|e| e.to_string())?;
    }
    // What makes a directory a pane's: its marker.
    std::fs::write(q.join("id"), "0000000000000000\n").map_err(|e| e.to_string())?;
    let lock = std::fs::File::open(&q).map_err(|e| e.to_string())?;
    lock.try_lock().map_err(|e| e.to_string())?;
    let said = refused(&["put", "q", "x"])?;
    assert!(said.contains("dead"), "{said}");
    drop(lock);
    std::fs::remove_dir_all(&q).map_err(|e| e.to_string())?;
    // A pane whose program has ended takes nothing: put says so, rather
    // than report success for bytes the holder drops.
    s.start("over", &["sh", "-c", "exit 3"])?;
    until("the end", || s.pane("over").join("exit").exists())?;
    let said = refused(&["put", "over", "x"])?;
    assert!(said.contains("ended (exited 3)"), "{said}");
    Ok(())
}

/// A live pane renamed and moved to another workspace with `mv` is the
/// same pane: the same id, every file kept current where it is now, every
/// `ctl` command done, and its kill frees the name it has now.
#[test]
fn a_moved_pane_is_the_same_pane() -> Result {
    let s = Session::new("mv")?;
    s.fux3(&[
        "start",
        "build",
        "--size",
        "80x24",
        "--history",
        "8192",
        "--",
        "sh",
    ])?;
    let id = |name: &str| {
        let status = s.read(name, "status");
        status
            .lines()
            .find_map(|l| l.strip_prefix("id ").map(str::to_owned))
    };
    let first = id("build").ok_or("no id")?;
    assert_eq!(s.read("build", "id").trim(), first);
    let stop = s.root.join("stop");
    let ticker = format!(
        "while [ ! -e {} ]; do seq 1 40; sleep 0.01; done &\n",
        stop.display()
    );
    s.write("build", "in", ticker.as_bytes())?;
    // A workspace is a directory: `mkdir` makes one, whatever its mode.
    let rig = s.root.join("rig");
    std::fs::create_dir(&rig).map_err(|e| e.to_string())?;
    let renamed = s.pane("build").with_file_name("made");
    std::fs::rename(s.pane("build"), &renamed).map_err(|e| e.to_string())?;
    std::fs::rename(&renamed, rig.join("server")).map_err(|e| e.to_string())?;
    let p = "rig/server";
    assert_eq!(s.fux3(&["ls"])?, "rig/server\n");
    assert!(!s.pane("build").exists(), "the old name came back");
    // history and its rotation go on where the pane is now; then in,
    // screen, screen.styled and status.
    until("history where the pane is now", || {
        s.read(p, "history").contains("\n40\n") && s.pane(p).join("history.1").exists()
    })?;
    std::fs::write(&stop, "").map_err(|e| e.to_string())?;
    s.write(p, "in", b"clear; echo moved-$((20+3))\n")?;
    until("the typed line on both screens", || {
        screen_has(&s, p, "moved-23") && s.read(p, "screen.styled").contains("moved-23")
    })?;
    assert_eq!(id(p), Some(first.clone()), "another pane");
    // resize, cell, signal, send, paste, and a refusal in errors.
    s.write(p, "ctl", b"resize 60 10\ncell 8 16\nbogus\n")?;
    until("the resize, the cell and the refusal", || {
        let status = s.read(p, "status");
        status.contains("\nsize 60 10\n")
            && status.contains("\ncell 8 16\n")
            && s.read(p, "errors").contains("3 bogus: ")
    })?;
    let file = s.root.join("sent");
    std::fs::write(&file, "echo sent-$((30+4))\n").map_err(|e| e.to_string())?;
    s.write(
        p,
        "ctl",
        format!("send {0}\npaste {0}\n", file.display()).as_bytes(),
    )?;
    // The send runs; the paste is there as text (a shell with bracketed
    // paste waits for Enter).
    until("the send and the paste", || {
        let screen = s.read(p, "screen");
        screen.contains("\nsent-34\n") && screen.matches("echo sent-").count() >= 2
    })?;
    s.write(p, "in", b"exec sleep 1000\n")?;
    until("sleep in front", || screen_has(&s, p, "exec sleep 1000"))?;
    std::thread::sleep(Duration::from_millis(100));
    s.write(p, "ctl", b"signal INT\n")?;
    until("exit, after the signal", || {
        s.read(p, "exit").starts_with("state ")
            && s.read(p, "status").contains("\nstate signalled INT\n")
    })?;
    // wait follows it too; then the kill takes the name it has now.
    s.fux3(&["wait", p, "--ms", "0"])
        .err()
        .ok_or("wait saw a change in no time")?;
    s.write(p, "ctl", b"kill\n")?;
    until("the pane gone", || !s.pane(p).exists())?;
    assert_eq!(
        std::fs::read_dir(&rig).map_err(|e| e.to_string())?.count(),
        0
    );
    assert_eq!(s.fux3(&["ls"])?, "");
    Ok(())
}

/// A start that names the workspace's directory is refused once that
/// workspace was removed and made again: it is another workspace.
#[test]
fn a_start_goes_to_the_workspace_it_was_given() -> Result {
    use std::os::unix::fs::MetadataExt;
    let s = Session::new("wsid")?;
    s.start("w/first", &["sh"])?;
    let space = s.root.join("w");
    let meta = std::fs::metadata(&space).map_err(|e| e.to_string())?;
    let id = format!("{}:{}", meta.dev(), meta.ino());
    s.fux3(&["start", "w/second", "--workspace-id", &id, "--", "sh"])?;
    for pane in ["w/first", "w/second"] {
        s.write(pane, "ctl", b"kill\n")?;
        until("the pane gone", || !s.pane(pane).exists())?;
    }
    // Kept open, so the new directory cannot get the old one's number.
    let _old = std::fs::File::open(&space).map_err(|e| e.to_string())?;
    std::fs::remove_dir_all(&space).map_err(|e| e.to_string())?;
    std::fs::create_dir(&space).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&space, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    let said = s.fux3(&["start", "w/third", "--workspace-id", &id, "--", "sh"]);
    assert!(said.is_err_and(|e| e.contains("workspace w was removed")));
    assert_eq!(s.fux3(&["ls"])?, "");
    Ok(())
}

/// A pane has its name only while it is whole, and only what a holder
/// made is ever cleared: what a holder killed outright leaves at any step
/// (a directory half made, one named, one taken out of its name) goes
/// with the next command, and anything else in or beside it stays.
#[test]
fn a_name_is_given_whole_and_only_a_panes_files_are_cleared() -> Result {
    use std::os::unix::fs::MetadataExt;
    let s = Session::new("mark")?;
    // Whenever the name is there, the pane is whole.
    let name = s.root.join("main/p");
    let done = std::sync::atomic::AtomicBool::new(false);
    let torn = std::thread::scope(|scope| {
        let watcher = scope.spawn(|| {
            let mut torn = Vec::new();
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                let Ok(dir) = std::fs::File::open(&name) else {
                    continue;
                };
                use fuxix::file::{NONBLOCK, at};
                let whole = ["id", "in", "ctl", "screen", "screen.styled", "status"];
                let there = |f: &&str| at::open(&dir, f, at::READ | NONBLOCK).is_ok();
                let missing = whole.iter().find(|f| !there(f));
                // Only if it had the name all the while: one taken out of
                // its name is being taken apart, and may lack anything.
                let same = |a: &std::fs::Metadata, b: &std::fs::Metadata| a.ino() == b.ino();
                let named = matches!((dir.metadata(), std::fs::metadata(&name)), (Ok(a), Ok(b)) if same(&a, &b));
                if let (Some(missing), true) = (missing, named) {
                    torn.push((*missing).to_owned());
                }
            }
            torn
        });
        let mut outcome = Ok(());
        for _ in 0..15 {
            outcome = (s.start("p", &["sh"]))
                .and_then(|()| s.write("p", "ctl", b"kill\n"))
                .and_then(|()| until("the name free", || !name.exists()));
            if outcome.is_err() {
                break;
            }
        }
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        outcome.map(|()| watcher.join().unwrap_or_default())
    })?;
    assert!(torn.is_empty(), "the name was there without {torn:?}");
    // What a holder killed outright leaves, at each step.
    let main = s.root.join("main");
    let dead = |dir: &str, files: &[&str]| -> Result {
        let dir = main.join(dir);
        std::fs::create_dir(&dir).map_err(|e| e.to_string())?;
        for file in files {
            match *file {
                "in" | "ctl" => fuxix::file::private_fifo(&dir.join(file)),
                "images" => std::fs::create_dir(dir.join(file))
                    .and_then(|()| std::fs::write(dir.join("images/1.sixel"), "x")),
                _ => std::fs::write(dir.join(file), "x\n"),
            }
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    };
    let made = [
        "id",
        "in",
        "ctl",
        "images",
        "history",
        "history.1",
        "screen",
        ".screen.new",
    ];
    dead(".new-aaaa", &[])?;
    dead(".new-bbbb", &["id", "in"])?;
    dead(".gone-cccc", &made)?;
    dead("named", &made)?;
    // Not a pane's: no marker; and a pane's holding what no holder made.
    dead("notes", &["in", "ctl", "todo"])?;
    dead(".new-dddd", &["todo"])?;
    dead("mixed", &["id", "in", "screen", "todo"])?;
    std::fs::create_dir(main.join("mixed/inner")).map_err(|e| e.to_string())?;
    std::fs::write(main.join("mixed/inner/id"), "x\n").map_err(|e| e.to_string())?;
    assert_eq!(s.fux3(&["ls"])?, "");
    let mut left: Vec<String> = std::fs::read_dir(&main)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != ".recipes")
        .collect();
    left.sort();
    assert_eq!(left, [".new-dddd", "mixed", "notes"]);
    assert!(main.join("notes/todo").exists() && main.join("notes/in").exists());
    // Its own files went, the rest and the marker stay, nothing inside a
    // directory in it was touched; a start says why the name is taken.
    assert!(main.join("mixed/todo").exists() && main.join("mixed/inner/id").exists());
    assert!(main.join("mixed/id").exists() && !main.join("mixed/in").exists());
    let said = s
        .start("mixed", &["sh"])
        .err()
        .ok_or("it took a name in use")?;
    assert!(said.contains("left alone"), "{said}");
    assert!(main.join("mixed/todo").exists());
    Ok(())
}

/// History is records: a line, or a marked piece of a line too long to be
/// one. Each file holds whole records, so rotation never cuts one, and
/// taking the marks out gives the line back.
#[test]
fn history_is_whole_records() -> Result {
    let s = Session::new("records")?;
    s.fux3(&[
        "start",
        "p",
        "--size",
        "80x4",
        "--history",
        "8192",
        "--",
        "sh",
    ])?;
    // One line of 45,000 bytes, printed at once: longer than a whole file
    // (4,096), and more than both files hold.
    let line = "awk 'BEGIN { for (i = 0; i < 5000; i++) printf \"%08d,\", i; print \"\" }'";
    s.write(
        "p",
        "in",
        format!("stty -echo; {line}; seq 1 5; echo done-$((1+1))\n").as_bytes(),
    )?;
    until("the end of output", || screen_has(&s, "p", "done-2"))?;
    let read = |file: &str| s.read("p", file);
    until("the line's end in history", || {
        read("history").contains("00004999,")
    })?;
    let (older, newer) = (read("history.1"), read("history"));
    for file in [&older, &newer] {
        assert!(file.len() <= 4096, "a file of {} bytes", file.len());
        assert!(file.is_empty() || file.ends_with('\n'), "a record was cut");
        assert!(file.lines().all(|l| l.len() < 4096));
    }
    // Both files are in use: what is kept is the cap's worth, not half.
    let (old, new) = (older.len(), newer.len());
    assert!(old > 2048 && new > 9, "{old} and {new} bytes kept");
    let joined = format!("{older}{newer}").replace(" [fux3: continued]\n", "");
    let whole = joined
        .lines()
        .find(|l| l.ends_with("00004999,"))
        .ok_or("no end")?;
    // The newest part of the line, in order, nothing missing or doubled.
    let numbers: Vec<u32> = whole.split(',').filter_map(|n| n.parse().ok()).collect();
    assert!(numbers.len() > 100, "{} numbers kept", numbers.len());
    let next = numbers.iter().skip(1);
    assert!(
        numbers
            .iter()
            .zip(next)
            .all(|(a, b)| a.saturating_add(1) == *b)
    );
    Ok(())
}

/// A kill ends the panes started inside its pane the way their own kill
/// does, so their directories go with it and no later command is needed;
/// and a holder told to end (SIGTERM) does the same. After it, writing
/// `kill` to every `ctl` left cannot wait on a dead pane's FIFO.
#[test]
fn a_kill_takes_the_panes_inside_apart_too() -> Result {
    let s = Session::new("inner")?;
    s.start("outer", &["sh"])?;
    s.start("beside", &["sh"])?;
    let fux3 = env!("CARGO_BIN_EXE_fux3");
    let inner = format!("for n in 1 2 3; do {fux3} start in$n -- sh; done\n");
    s.write("outer", "in", inner.as_bytes())?;
    until("the inner panes", || s.pane("in3").join("screen").exists())?;
    let holder = s.holder("outer").ok_or("no holder pid")?;
    let ended = Command::new("kill").arg(holder.to_string()).status();
    assert!(ended.is_ok_and(|s| s.success()));
    let names = || -> Vec<String> {
        let entries = std::fs::read_dir(s.root.join("main")).into_iter().flatten();
        let mut names: Vec<_> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != ".recipes")
            .collect();
        names.sort();
        names
    };
    // With no fux3 command run: the directories went by themselves.
    #[cfg(target_os = "linux")]
    until("only the pane beside left", || names() == ["beside"])?;
    // macOS has no subreaper: the inner panes left the outer one for good.
    #[cfg(target_os = "macos")]
    until("the outer pane gone", || {
        names() == ["beside", "in1", "in2", "in3"]
    })?;
    let (said, hears) = std::sync::mpsc::channel();
    let ctls: Vec<_> = names().iter().map(|n| s.pane(n).join("ctl")).collect();
    std::thread::spawn(move || {
        for ctl in ctls {
            let _ = std::fs::File::options()
                .write(true)
                .open(ctl)
                .map(|mut f| f.write_all(b"kill\n"));
        }
        let _ = said.send(());
    });
    let done = hears.recv_timeout(Duration::from_secs(5));
    assert!(done.is_ok(), "a kill written to every ctl waited");
    until("every pane gone", || names().is_empty())?;
    Ok(())
}

/// `exit` appears only beside screens whose pictures are all on disk, and
/// a publish that failed is in `status`.
#[test]
fn exit_waits_for_every_picture_the_screen_names() -> Result {
    let s = Session::new("late")?;
    let draw = "printf '\\033Pq#0;2;100;0;0#0!6~-#0!6~\\033\\\\'; echo";
    s.start("p", &["sh", "-c", &format!("read go; {draw}")])?;
    let images = s.pane("p").join("images");
    let mode = |mode| std::fs::set_permissions(&images, std::fs::Permissions::from_mode(mode));
    mode(0o500).map_err(|e| e.to_string())?;
    s.write("p", "in", b"\n")?;
    until("the failed publish in status", || {
        s.read("p", "status").contains("\npublish failed: ")
    })?;
    // Past the next try, a second later: still no picture, so no exit.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(!s.pane("p").join("exit").exists(), "exit, and no picture");
    mode(0o700).map_err(|e| e.to_string())?;
    until("exit", || s.pane("p").join("exit").exists())?;
    let styled = s.read("p", "screen.styled");
    let named = styled
        .lines()
        .find_map(|l| l.strip_prefix("picture ")?.split(' ').next());
    let named = named.ok_or("the last screen names no picture")?;
    assert!(
        images.join(named).exists(),
        "{named} is named and not there"
    );
    Ok(())
}
