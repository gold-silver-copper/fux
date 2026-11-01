//! The `fuxth` runner, run as a program: over pipes, and on a terminal.
use fuxix::poll::{Events, PollFd, poll};
use fuxix::terminal::Size;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

type Outcome = Result<(), String>;

const FUXTH: &str = env!("CARGO_BIN_EXE_fuxth");

fn e(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// A directory of the test's own, empty.
fn dir(name: &str) -> Result<PathBuf, String> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(e)?;
    Ok(dir)
}

/// `fuxth ARGS…` in `dir`, `stdin` piped in: what it said, and its status.
fn run(dir: &PathBuf, args: &[&str], stdin: &str) -> Result<Output, String> {
    let mut child = Command::new(FUXTH)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(e)?;
    let mut input = child.stdin.take().ok_or("no stdin")?;
    input.write_all(stdin.as_bytes()).map_err(e)?;
    drop(input);
    child.wait_with_output().map_err(e)
}

/// `wait` from now.
fn after(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait).unwrap_or(now)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_batch_program_over_a_pipe_prints_only_its_output_and_fails_nonzero() -> Outcome {
    let dir = dir("batch")?;
    let program = "define square dup mul end\nstr hello print 7 square str 'two words'\n";
    std::fs::write(dir.join("square.fux"), program).map_err(e)?;
    // A file, a file on stdin and a pipe are the same program.
    let piped = std::process::Command::new("sh")
        .args(["-c", &format!("cat square.fux | {FUXTH}")])
        .current_dir(&dir)
        .output()
        .map_err(e)?;
    for ran in [
        run(&dir, &["square.fux"], "")?,
        run(&dir, &[], program)?,
        piped,
    ] {
        assert_eq!(text(&ran.stdout), "hello\n49\ntwo words\n");
        assert_eq!(text(&ran.stderr), "");
        assert!(ran.status.success(), "{:?}", ran.status);
    }
    // Byte for byte: a backslash at the very end is itself, not a line
    // joined to one stdin never had.
    std::fs::write(dir.join("tail.fux"), "str tail\\").map_err(e)?;
    for ran in [run(&dir, &["tail.fux"], "")?, run(&dir, &[], "str tail\\")?] {
        assert_eq!(text(&ran.stdout), "tail\\\n");
    }
    // What is left prints as fux prints it: a list an item a line, a list
    // in it as JSON, nothing for an empty list or string, and a blank line
    // for an empty string in a list.
    let ran = run(
        &dir,
        &[],
        "list 1 2 end list end list list 3 str x end str '' end str '' 5",
    )?;
    assert_eq!(text(&ran.stdout), "1\n2\n[3,\"x\"]\n\n5\n");
    // What it printed stays; why it failed and what took effect go to
    // stderr, and nothing is left printed.
    let failing =
        "str before print 1 2\nstr made.txt create-file throw close-file throw\n1 0 div\n";
    let ran = run(&dir, &[], failing)?;
    assert_eq!(text(&ran.stdout), "before\n");
    assert_eq!(
        text(&ran.stderr),
        "fuxth: div_by_zero: stdin:3: div: division by zero\n\
         fuxth: already took effect: stdin:2: create-file\n\
         fuxth: already took effect: stdin:2: close-file\n"
    );
    assert_eq!(ran.status.code(), Some(1));
    Ok(())
}

#[test]
fn a_key_driven_program_over_a_pipe_saves_a_file() -> Outcome {
    let dir = dir("keys")?;
    // Keys until Ctrl-S, an Up arrow dropping the last character, written
    // to a temporary file, which is renamed over the file named; the
    // file's mode kept.
    let program = r#"
        define typed
          str '' begin
            read-key dup 19 eq if drop true else
              dup key-up eq if drop dup len 1 sub 0 swap slice else char cat then false
            then
          until
        end
        args first let path
        typed let text
        path file-mode throw let mode
        str .saved create-file throw let file
        text file write-line throw  file close-file throw
        mode str .saved set-file-mode throw
        str .saved path rename-file throw
    "#;
    std::fs::write(dir.join("keys.fux"), program).map_err(e)?;
    std::fs::write(dir.join("notes.txt"), "old\n").map_err(e)?;
    let mode = |mode| {
        std::fs::set_permissions(
            dir.join("notes.txt"),
            std::os::unix::fs::PermissionsExt::from_mode(mode),
        )
    };
    mode(0o640).map_err(e)?;
    let ran = run(&dir, &["keys.fux", "notes.txt"], "naïvex\x1b[A\x13ignored")?;
    assert_eq!(text(&ran.stderr), "");
    assert!(ran.status.success(), "{:?}", ran.status);
    assert_eq!(
        std::fs::read_to_string(dir.join("notes.txt")).map_err(e)?,
        "naïve\n"
    );
    let meta = std::fs::metadata(dir.join("notes.txt")).map_err(e)?;
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o777,
        0o640
    );
    // Keys running out is a failure, and nothing is saved.
    let ran = run(&dir, &["keys.fux", "notes.txt"], "abc")?;
    assert_eq!(
        text(&ran.stderr),
        format!(
            "fuxth: end_of_input: {}:4: read-key: stdin ended\n",
            dir.join("keys.fux").canonicalize().map_err(e)?.display()
        )
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("notes.txt")).map_err(e)?,
        "naïve\n"
    );
    // Keys and lines take from one queue: what a key read read ahead is
    // still there for a line.
    std::fs::write(dir.join("line.fux"), "read-key drop 99 accept print").map_err(e)?;
    let ran = run(&dir, &["line.fux"], "xhello\nrest")?;
    assert_eq!(text(&ran.stdout), "hello\n");
    // A bracketed paste of any length arrives whole, a key a character.
    let counting = "0 let n str end_of_input try begin read-key drop n 1 add to n false until catch drop end n";
    std::fs::write(dir.join("count.fux"), counting).map_err(e)?;
    let pasted: String = std::iter::repeat_n('a', 70_000).collect();
    let paste = format!("\x1b[200~{pasted}\x1b[201~");
    let ran = run(&dir, &["count.fux"], &paste)?;
    assert_eq!(text(&ran.stdout), "70000\n");
    Ok(())
}

#[test]
fn ctrl_c_is_a_failure_try_catches_and_uncaught_exits_130() -> Outcome {
    let dir = dir("interrupt")?;
    // `-28 throw` is the interrupt; a loop of words that change files
    // stops on Ctrl-C as a loop of arithmetic does.
    let program = "str interrupt try -28 throw catch drop str numbered print end \
        str item create-file throw close-file throw \
        str interrupt try begin 384 str item set-file-mode throw false until catch drop end \
        str caught print begin false until";
    std::fs::write(dir.join("loop.fux"), program).map_err(e)?;
    let child = Command::new(FUXTH)
        .arg("loop.fux")
        .current_dir(&dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(e)?;
    let pid = fuxix::process::Pid::of(&child).ok_or("no pid")?;
    for _ in 0..2 {
        std::thread::sleep(Duration::from_millis(300));
        fuxix::process::kill(pid, fuxix::process::Signal::Int).map_err(e)?;
    }
    let ran = child.wait_with_output().map_err(e)?;
    assert_eq!(text(&ran.stdout), "numbered\ncaught\n");
    assert!(
        text(&ran.stderr).starts_with("fuxth: interrupt: "),
        "{}",
        text(&ran.stderr)
    );
    assert_eq!(ran.status.code(), Some(130));
    Ok(())
}

#[test]
fn ctrl_c_reaches_a_wait_on_a_fifo() -> Outcome {
    let dir = dir("fifo")?;
    let made = Command::new("mkfifo")
        .args(["source", "data"])
        .current_dir(&dir)
        .status();
    assert!(made.map_err(e)?.success());
    std::fs::write(dir.join("read.fux"), "str data read-file").map_err(e)?;
    // A writer that holds `data` open and writes nothing.
    let mut writer = Command::new("sh")
        .args(["-c", "exec sleep 30 > data"])
        .current_dir(&dir)
        .spawn()
        .map_err(e)?;
    // A source with no writer at all, and a file read that waits on one.
    for args in [["source"], ["read.fux"]] {
        let child = Command::new(FUXTH)
            .args(args)
            .current_dir(&dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(e)?;
        let pid = fuxix::process::Pid::of(&child).ok_or("no pid")?;
        std::thread::sleep(Duration::from_millis(300));
        fuxix::process::kill(pid, fuxix::process::Signal::Int).map_err(e)?;
        let ran = child.wait_with_output().map_err(e)?;
        assert!(
            text(&ran.stderr).starts_with("fuxth: interrupt: "),
            "{args:?}: {}",
            text(&ran.stderr)
        );
        assert_eq!(ran.status.code(), Some(130), "{args:?}");
    }
    let _ = writer.kill();
    let _ = writer.wait();
    Ok(())
}

/// A program on a terminal of its own: what it writes is read, and keys
/// typed into it.
struct Terminal {
    master: fuxix::pty::Master,
    child: std::process::Child,
    seen: String,
}

impl Terminal {
    fn start(dir: &PathBuf, args: &[&str]) -> Result<Self, String> {
        let size = Size::new(24, 80).ok_or("size")?;
        let (master, slave) = fuxix::pty::open(size).map_err(e)?;
        let argv: Vec<&str> = std::iter::once(FUXTH).chain(args.iter().copied()).collect();
        let (child, _) = slave
            .spawn(&argv, |command| {
                command.current_dir(dir);
            })
            .map_err(e)?;
        Ok(Self {
            master,
            child,
            seen: String::new(),
        })
    }

    fn type_in(&self, keys: &str) -> Outcome {
        fuxix::io::write(&self.master, keys.as_bytes())
            .map(drop)
            .map_err(e)
    }

    /// Reads until what it wrote, since the last wait, ends with `end`.
    fn wait_for(&mut self, end: &str) -> Result<String, String> {
        let deadline = after(Duration::from_secs(10));
        while !self.seen.ends_with(end) {
            let left = deadline.saturating_duration_since(Instant::now());
            let mut fds = [PollFd::new(&self.master, Events::IN)];
            if left.is_zero() || poll(&mut fds, Some(left)).map_err(e)? == 0 {
                return Err(format!("never ended with {end:?}: {:?}", self.seen));
            }
            let mut buffer = [0; 4096];
            let n = fuxix::io::read(&self.master, &mut buffer).map_err(e)?;
            self.seen
                .push_str(&text(buffer.get(..n).unwrap_or_default()));
        }
        Ok(std::mem::take(&mut self.seen))
    }
}

impl Terminal {
    /// Waits until the program has the terminal in key mode: what is typed
    /// before would be echoed and edited by the terminal.
    fn wait_until_keys(&self) -> Outcome {
        let deadline = after(Duration::from_secs(10));
        while fuxix::terminal::attributes(&self.master)
            .map_err(e)?
            .modes()
            .canonical
        {
            if Instant::now() > deadline {
                return Err("the terminal never went into key mode".to_owned());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn an_ending_signal_puts_the_terminal_back() -> Outcome {
    let dir = dir("ending")?;
    std::fs::write(dir.join("keys.fux"), "begin read-key drop false until").map_err(e)?;
    let mut keys = Terminal::start(&dir, &["keys.fux"])?;
    keys.wait_until_keys()?;
    let pid = fuxix::process::Pid::of(&keys.child).ok_or("no pid")?;
    fuxix::process::kill(pid, fuxix::process::Signal::Term).map_err(e)?;
    let status = keys.child.wait().map_err(e)?;
    assert_eq!(status.code(), Some(128 + 15));
    let modes = fuxix::terminal::attributes(&keys.master)
        .map_err(e)?
        .modes();
    assert!(modes.canonical && modes.echo, "{modes:?}");
    Ok(())
}

#[test]
fn the_repl_on_a_terminal_prints_the_stack_and_keeps_the_dictionary_through_errors() -> Outcome {
    let dir = dir("repl")?;
    let mut repl = Terminal::start(&dir, &[])?;
    assert_eq!(repl.wait_for("ok\r\n")?, "ok\r\n");
    // The terminal echoes each line; the stack follows, the deepest first.
    for (line, said) in [
        ("1 2", "1 2 ok"),
        ("define square dup", "1 2 compiled"),
        ("mul end", "1 2 ok"),
        ("square", "1 4 ok"),
        ("str 'two", ""),
        ("words'", "1 4 two\r\nwords ok"),
        (
            "1 0 div",
            "fuxth: div_by_zero: evaluate:1: div: division by zero\r\nok",
        ),
        ("3 square", "9 ok"),
    ] {
        repl.type_in(&format!("{line}\n"))?;
        let expected = match said {
            "" => format!("{line}\r\n"),
            said => format!("{line}\r\n{said}\r\n"),
        };
        assert_eq!(repl.wait_for(&expected)?, expected);
    }
    // Ctrl-C stops a runaway, and the REPL goes on.
    repl.type_in("begin false until\n")?;
    repl.wait_for("begin false until\r\n")?;
    repl.type_in("\x03")?;
    let stopped = repl.wait_for("ok\r\n")?;
    assert!(stopped.contains("fuxth: interrupt: "), "{stopped:?}");
    // A key word reads keys as they are typed, the terminal kept as it
    // was outside it; Ctrl-D ends the REPL.
    repl.type_in("read-key print\n")?;
    repl.wait_for("read-key print\r\n")?;
    repl.wait_until_keys()?;
    repl.type_in("\x1b[A")?;
    assert_eq!(repl.wait_for("ok\r\n")?, "1114112\r\nok\r\n");
    repl.type_in("\x04")?;
    let status = repl.child.wait().map_err(e)?;
    assert!(status.success(), "{status:?}");
    Ok(())
}
