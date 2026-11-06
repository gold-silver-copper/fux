//! Pseudo consoles through the fetched ConPTY (WINDOWS.md, PR 6): spike
//! S1's byte-exactness and S4's probes, each one property with a deadline.
//!
//! Its own `main` (no libtest), so that the probe, which is this binary
//! run again with `PROBE` set, writes nothing but its own bytes, and so
//! that `load` is called once for all of them. The package comes from
//! `FUXIX_CONPTY_PACKAGE` (CI fetches and hash-checks it in a step of its
//! own, so no test touches the network) put in the folder as a user would
//! put one by hand; unset, `fetch` downloads it.
#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    windows::main()
}

#[cfg(windows)]
mod windows {
    use fuxix::io::read;
    use fuxix::poll::{Events, Poll, PollFd};
    use fuxix::process::{Dispositions, Job, KILLED, Scan, Status};
    use fuxix::pty::{self, BREAK, Master};
    use fuxix::terminal::{self, Size};
    use std::fmt::Write as _;
    use std::io::Write as _;
    use std::os::windows::process::CommandExt;
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    type Outcome = Result<(), String>;
    type Test = (&'static str, fn() -> Outcome);

    /// A line of the log, on stderr (the probe's output is its stdout).
    macro_rules! say {
        ($($arg:tt)*) => {{
            let _ = writeln!(std::io::stderr(), $($arg)*);
        }};
    }

    const PROBE: &str = "FUXIX_CONPTY_PROBE";
    /// A program Ctrl-C, Ctrl-Break or a console's close ended, with no
    /// handler of its own: `STATUS_CONTROL_C_EXIT`.
    const CONTROL_C_EXIT: i32 = 0xC000_013A_u32.cast_signed();
    /// What the fetched ConPTY sends before and after the program's own
    /// bytes (spike S1, 30 of 30 runs on both images).
    const PREFIX: &[u8] = b"\x1b[1t\x1b[c\x1b[?1004h\x1b[?9001h";
    const SUFFIX: &[u8] = b"\x1b[?1004l\x1b[?9001l";

    pub fn main() -> ExitCode {
        if let Some(probe) = std::env::var_os(PROBE) {
            return run_probe(&probe.to_string_lossy());
        }
        let dir = folder();
        let mut failed = 0u8;
        let mut report = |name: &str, outcome: Outcome| {
            match &outcome {
                Ok(()) => say!("test {name} ... ok"),
                Err(why) => say!("test {name} ... FAILED: {why}"),
            }
            failed = failed.saturating_add(u8::from(outcome.is_err()));
        };
        report("setup: the package fetches and loads", fetched(&dir));
        let attached: Test = (
            "kill_of_the_attached_ends_what_the_shell_runs_and_the_shell_lives",
            shell_lives,
        );
        let tests: [Test; 5] = [
            attached,
            (
                "bytes_arrive_exact_through_the_fetched_conpty",
                bytes_arrive_exact,
            ),
            (
                "ctrl_c_and_break_end_a_program_without_a_handler",
                keys_end_it,
            ),
            (
                "hang_up_ends_the_console_and_kill_ends_the_rest",
                hang_up_then_kill,
            ),
            (
                "kill_of_the_group_ends_the_leader_alone_with_fuxs_code",
                kill_leader_alone,
            ),
        ];
        for (name, test) in tests {
            report(name, test());
        }
        match failed {
            0 => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        }
    }

    /// The ConPTY's folder.
    fn folder() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("conpty")
    }

    fn text(error: impl std::fmt::Display) -> String {
        error.to_string()
    }

    fn after(duration: Duration) -> Result<Instant, String> {
        Instant::now()
            .checked_add(duration)
            .ok_or_else(|| "a deadline".to_owned())
    }

    /// Bytes as text, escapes shown.
    fn shown(bytes: &[u8]) -> String {
        let mut out = String::new();
        for byte in bytes {
            match byte {
                0x1b => out.push_str("^["),
                b'\r' => out.push_str("\\r"),
                b'\n' => out.push_str("\\n"),
                0x20..=0x7e => out.push(char::from(*byte)),
                _ => {
                    let _ = write!(out, "\\x{byte:02x}");
                }
            }
        }
        out
    }

    /// The package put in `dir` as a user would by hand, fetched from
    /// there, and loaded: the pinned ConPTY, not the built-in.
    fn fetched(dir: &std::path::Path) -> Outcome {
        std::fs::create_dir_all(dir).map_err(text)?;
        if let Some(package) = std::env::var_os("FUXIX_CONPTY_PACKAGE") {
            let name = "microsoft.windows.console.conpty.1.25.260930003.nupkg";
            std::fs::copy(package, dir.join(name)).map_err(text)?;
        }
        let started = Instant::now();
        pty::fetch(dir).map_err(|e| format!("fetch: {e}"))?;
        pty::load(dir).map_err(|e| format!("load chose the built-in: {e}"))?;
        say!("fetched and loaded in {:?}", started.elapsed());
        Ok(())
    }

    /// A pane: the pseudo console, and this binary run on it as `probe`.
    struct Pane {
        master: Master,
        job: Job,
        output: Vec<u8>,
    }

    impl Pane {
        fn start(probe: &str) -> Result<Pane, String> {
            let exe = std::env::current_exe().map_err(text)?;
            Pane::run(&[exe.into_os_string()], probe)
        }

        /// `argv` on a new pane, with `PROBE` set to `probe`.
        fn run(argv: &[std::ffi::OsString], probe: &str) -> Result<Pane, String> {
            let size = Size::new(24, 80).ok_or("a size")?;
            let (master, slave) = pty::open(size).map_err(text)?;
            let job = slave
                .spawn(
                    argv,
                    |command| {
                        command.env(PROBE, probe);
                    },
                    Dispositions::Default,
                )
                .map_err(|e| format!("spawn: {e}"))?;
            Ok(Pane {
                master,
                job,
                output: Vec::new(),
            })
        }

        /// Reads until `done` says so of all read, or the output ends
        /// (`true`), or `deadline` passes (an error).
        fn read_until(
            &mut self,
            deadline: Instant,
            done: impl Fn(&[u8]) -> bool,
        ) -> Result<bool, String> {
            let mut poll = Poll::default();
            let mut buffer = vec![0u8; 65536];
            loop {
                if done(&self.output) {
                    return Ok(false);
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(format!("deadline, after reading «{}»", shown(&self.output)));
                }
                let mut list = poll.list();
                let entry = list.push(PollFd::new(&self.master, Events::IN));
                list.poll(Some(left.min(Duration::from_millis(100))))
                    .map_err(text)?;
                let ready = list.events(entry);
                drop(list);
                if ready.is_empty() {
                    continue;
                }
                loop {
                    match read(&self.master, &mut buffer) {
                        Ok(0) => return Ok(true),
                        Ok(n) => self
                            .output
                            .extend_from_slice(buffer.get(..n).unwrap_or_default()),
                        Err(fuxix::Errno::AGAIN) => break,
                        Err(errno) => return Err(format!("read: {errno}")),
                    }
                }
            }
        }

        fn read_to_end(&mut self, deadline: Instant) -> Outcome {
            self.read_until(deadline, |_| false).map(drop)
        }

        /// Waits for its leader's end, and how.
        fn ended(&self, deadline: Instant) -> Result<Status, String> {
            loop {
                if let Some(status) = self.job.ended().map_err(text)? {
                    return Ok(status);
                }
                if Instant::now() > deadline {
                    return Err("the leader did not end".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn write(&self, bytes: &[u8]) -> Outcome {
            let written = fuxix::io::write_what_fits(&self.master, bytes).map_err(text)?;
            match written == bytes.len() {
                true => Ok(()),
                false => Err(format!("wrote {written} of {} bytes", bytes.len())),
            }
        }
    }

    /// Ends what is left of a pane, whatever the test found.
    impl Drop for Pane {
        fn drop(&mut self) {
            let _ = self.job.kill(fuxix::process::Reach::Session);
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        (0..haystack.len()).any(|at| {
            haystack
                .get(at..)
                .is_some_and(|rest| rest.starts_with(needle))
        })
    }

    /// S1: a program's bytes (sixel, synchronized output, links, titles,
    /// SGR, the alternate screen, scroll regions) arrive exactly, between
    /// only the ConPTY's own fixed sequences, then the end; its exit code
    /// after.
    fn bytes_arrive_exact() -> Outcome {
        let deadline = after(Duration::from_secs(20))?;
        let mut pane = Pane::start("script")?;
        pane.read_to_end(deadline)?;
        let status = pane.ended(deadline)?;
        let want: Vec<u8> = [PREFIX, &script(), SUFFIX].concat();
        if pane.output != want {
            let same = (pane.output.iter().zip(&want))
                .take_while(|(a, b)| a == b)
                .count();
            return Err(format!(
                "{} bytes, not {}; the same for {same}, then «{}», not «{}»",
                pane.output.len(),
                want.len(),
                shown(pane.output.get(same..).unwrap_or_default()),
                shown(want.get(same..).unwrap_or_default()),
            ));
        }
        if status != Status::Exited(42) {
            return Err(format!("{status:?}, not Exited(42)"));
        }
        say!(
            "{} bytes exact: {} of the program's between ^[[1t^[[c^[[?1004h^[[?9001h and ^[[?1004l^[[?9001l; {status:?}",
            pane.output.len(),
            want.len()
                .saturating_sub(PREFIX.len())
                .saturating_sub(SUFFIX.len()),
        );
        Ok(())
    }

    /// S4: Ctrl-C (0x03) and Ctrl-Break written to the master end a
    /// program with no handler, as a console's keys do, though this
    /// process ignores Ctrl-C, as a pane process started detached in a
    /// new process group does: `Dispositions::Default` clears it.
    fn keys_end_it() -> Outcome {
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
        for (key, name) in [(&b"\x03"[..], "Ctrl-C"), (BREAK, "Ctrl-Break")] {
            let deadline = after(Duration::from_secs(10))?;
            // SAFETY: with no handler, this only sets "ignore Ctrl-C",
            // which the program would inherit.
            if unsafe { SetConsoleCtrlHandler(None, 1) } == 0 {
                return Err(format!(
                    "ignoring Ctrl-C: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let mut pane = Pane::start("sleep")?;
            pane.read_until(deadline, |out| contains(out, b"ready"))?;
            let sent = Instant::now();
            pane.write(key)?;
            let status = pane.ended(deadline)?;
            if status != Status::Exited(CONTROL_C_EXIT) {
                return Err(format!("{name}: {status:?}, not 0xC000013A"));
            }
            say!("{name} ended it in {:?}: {status:?}", sent.elapsed());
        }
        Ok(())
    }

    /// The hang-up closes the console: its program ends as a closed
    /// console's do and the output ends, while a process it started off
    /// the console lives on in its job, until `kill(Session)` ends it.
    fn hang_up_then_kill() -> Outcome {
        let deadline = after(Duration::from_secs(15))?;
        let mut pane = Pane::start("family")?;
        pane.read_until(deadline, |out| contains(out, b"ready"))?;
        let hung = Instant::now();
        pane.master.hang_up().map_err(text)?;
        pane.read_to_end(deadline)?;
        let status = pane.ended(deadline)?;
        if status != Status::Exited(CONTROL_C_EXIT) {
            return Err(format!("hung up, the leader: {status:?}, not 0xC000013A"));
        }
        let members = pane.job.members();
        if members.pids.len() != 1 || members.scan != Scan::Complete {
            return Err(format!("after the hang-up, members {members:?}"));
        }
        say!(
            "hung up: output ended and the leader exited {status:?} in {:?}; its detached child lives: {members:?}",
            hung.elapsed()
        );
        let sent = pane.job.kill(fuxix::process::Reach::Session);
        if !sent.failed.is_empty() {
            return Err(format!("kill(Session): {sent:?}"));
        }
        loop {
            let members = pane.job.members();
            if members.pids.is_empty() && members.scan == Scan::Complete {
                break;
            }
            if Instant::now() > deadline {
                return Err(format!("after kill(Session), members {members:?}"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        say!("kill(Session) left no member");
        Ok(())
    }

    /// `kill(Group)` ends the leader alone, with fux's own exit code, so
    /// that fux can tell its kill from the program's end; the leader's
    /// child lives on, and a kill of an ended leader says it is gone.
    fn kill_leader_alone() -> Outcome {
        let deadline = after(Duration::from_secs(15))?;
        let mut pane = Pane::start("family")?;
        pane.read_until(deadline, |out| contains(out, b"ready"))?;
        // While it runs, the console takes a new size and tells it back.
        let size = Size::new(30, 100).ok_or("a size")?;
        terminal::set_window_size(&pane.master, size, None).map_err(text)?;
        let told = terminal::window_size(&pane.master).map(|(size, _)| size);
        if told != Some(size) {
            return Err(format!("window_size {told:?} after setting {size:?}"));
        }
        let sent = pane.job.kill(fuxix::process::Reach::Group);
        if !sent.failed.is_empty() {
            return Err(format!("kill(Group): {sent:?}"));
        }
        let status = pane.ended(deadline)?;
        if status != Status::Exited(KILLED) {
            return Err(format!("{status:?}, not Exited(KILLED)"));
        }
        let members = pane.job.members();
        if members.pids.len() != 1 {
            return Err(format!("after kill(Group), members {members:?}"));
        }
        let again = pane.job.kill(fuxix::process::Reach::Group);
        if again.failed.first().map(|(_, errno)| *errno) != Some(fuxix::Errno::SRCH) {
            return Err(format!("a second kill(Group): {again:?}"));
        }
        say!("kill(Group): {status:?} (KILLED), the child lives: {members:?}");
        Ok(())
    }

    /// S5: `kill(Attached)` from a detached process (as fux's pane
    /// process is) ends what a shell runs on its console, but neither the
    /// shell, which still answers, nor what left the console. Run in this
    /// binary again, detached, as the probe `attached-host`.
    fn shell_lives() -> Outcome {
        const DETACHED_PROCESS: u32 = 0x8;
        let deadline = after(Duration::from_secs(25))?;
        let exe = std::env::current_exe().map_err(text)?;
        let mut host = std::process::Command::new(exe)
            .env(PROBE, "attached-host")
            .creation_flags(DETACHED_PROCESS)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(text)?;
        let status = loop {
            if let Some(status) = host.try_wait().map_err(text)? {
                break status;
            }
            if Instant::now() > deadline {
                let _ = host.kill();
                let _ = host.wait();
                return Err("the detached host did not finish".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut log = String::new();
        if let Some(mut stderr) = host.stderr.take() {
            use std::io::Read as _;
            let _ = stderr.read_to_string(&mut log);
        }
        say!("{}", log.trim_end());
        match status.success() {
            true => Ok(()),
            false => Err(format!("the detached host: {status}")),
        }
    }

    /// The detached host of [`shell_lives`]: its own `load`, then `cmd`
    /// with a `start /b` program on its console, which starts one off it.
    fn attached_host() -> Outcome {
        let deadline = after(Duration::from_secs(20))?;
        pty::load(&folder()).map_err(|e| format!("load: {e}"))?;
        let system = std::env::var_os("SystemRoot").ok_or("no SystemRoot")?;
        let cmd = std::path::Path::new(&system)
            .join("System32")
            .join("cmd.exe");
        let argv = [cmd.into_os_string(), "/d".into(), "/k".into()];
        let mut shell = Pane::run(&argv, "family")?;
        shell.read_until(deadline, |out| contains(out, b">"))?;
        let exe = std::env::current_exe().map_err(text)?;
        shell.write(format!("start /b \"\" \"{}\"\r", exe.display()).as_bytes())?;
        // Its child is started, so no kill lands in the middle of that.
        shell.read_until(deadline, |out| contains(out, b"ready"))?;
        let on_console = loop {
            let attached = shell.job.attached();
            let members = shell.job.members();
            if attached.pids.len() == 1 && members.pids.len() == 2 {
                break attached;
            }
            if Instant::now() > deadline {
                return Err(format!("attached {attached:?}, members {members:?}"));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let sent = shell.job.kill(fuxix::process::Reach::Attached);
        if !sent.failed.is_empty() || sent.scan != Scan::Complete {
            return Err(format!("kill(Attached): {sent:?}"));
        }
        loop {
            let attached = shell.job.attached();
            if attached.pids.is_empty() && attached.scan == Scan::Complete {
                break;
            }
            if Instant::now() > deadline {
                return Err(format!("after kill(Attached), attached {attached:?}"));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // The job's list catches up with a process's end a moment after the
        // console's does (master f31d365fe's CI saw both still listed):
        // what it settles to, by the deadline, is what is checked.
        let members = loop {
            let members = shell.job.members();
            let settled = members.pids.len() == 1 && members.pids != on_console.pids;
            if settled || Instant::now() > deadline {
                break members;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if members.pids.len() != 1 || members.pids == on_console.pids {
            return Err(format!("after kill(Attached), members {members:?}"));
        }
        if shell.job.ended().map_err(text)?.is_some() {
            return Err("the shell ended".into());
        }
        shell.write(b"echo fux-%OS%\r")?;
        shell.read_until(deadline, |out| contains(out, b"fux-Windows_NT"))?;
        say!(
            "kill(Attached) ended {:?} (start /b); the shell answers, and its detached child lives: {members:?}",
            on_console.pids
        );
        Ok(())
    }

    /// S1's full section script, as its probe wrote it.
    fn script() -> Vec<u8> {
        const END: &[u8] = b"S1-END\r\n";
        let mut b: Vec<u8> = Vec::new();
        let mut section = |bytes: &[u8]| {
            b.extend_from_slice(bytes);
            b.extend_from_slice(END);
        };
        section(
            "plain ASCII text\r\nwide: \u{6f22}\u{5b57}\u{304b}\u{306a} end\r\nemoji: \u{1f44d}\u{1f3fd} \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467} \u{1f1ef}\u{1f1f5} end\r\ncombining: e\u{301} a\u{308} end\r\n"
                .as_bytes(),
        );
        section(b"\x1b[1mbold\x1b[0m \x1b[38;5;196mc256\x1b[0m \x1b[38;2;255;128;0mrgbfg\x1b[48;2;0;64;128mrgbbg\x1b[0m\r\n\x1b[4:3mcurly\x1b[4:0m \x1b[4:2mdouble\x1b[24m \x1b[58:2::255:0:0m\x1b[4mulcolor\x1b[0m \x1b[3;9mitalstrike\x1b[0m\r\n");
        section(b"\x1b]8;id=s1;https://example.com/s1\x1b\\link\x1b]8;;\x1b\\ after\r\n");
        section(b"\x1b]133;A\x07$ \x1b]133;B\x07echo hi\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;7\x07\x1b]133;A\x07$ \x1b]133;B\x07sleep\r\n\x1b]133;C\x07running\r\n");
        section(b"\x1b]0;s1 icon and title\x07\x1b]2;s1 title two\x1b\\titled\r\n");
        section(b"\x1b[?2026hframe one\r\n\x1b[?2026l\x1b[?2026hframe two left open\r\n");
        section(b"\x1b[?2048hinband\r\n");
        section(b"sixel:\x1bPq\"1;1;40;24#0;2;100;0;0#1;2;0;0;100#0!40~-#1!40~-#0!40~-#1!40~\x1b\\\r\nafter sixel\r\n");
        section(b"\x1b[>1u\x1b[>9u\x1b[<ukitty\r\n");
        let mut region = b"\x1b[2J\x1b[Htop fixed\r\n\x1b[2;5r\x1b[2;1H".to_vec();
        for i in 1..=8 {
            region.extend_from_slice(format!("region line {i}\r\n").as_bytes());
        }
        region.extend_from_slice(b"\x1b[r\x1b[7;1Hbelow region\r\n");
        section(&region);
        let mut wrap = Vec::new();
        for i in 0..20 {
            wrap.extend_from_slice(format!("{i:02}abcdefgh").as_bytes());
        }
        wrap.extend_from_slice(b"\r\n");
        section(&wrap);
        section(b"main screen line\r\n\x1b[?1049h\x1b[Halt screen text\r\n");
        b
    }

    /// The probe, on its pseudo console.
    fn run_probe(probe: &str) -> ExitCode {
        match probe {
            "script" => {
                virtual_terminal();
                let mut out = std::io::stdout().lock();
                match out.write_all(&script()).and_then(|()| out.flush()) {
                    Ok(()) => ExitCode::from(42),
                    Err(_) => ExitCode::from(3),
                }
            }
            "sleep" => {
                ready();
                std::thread::sleep(Duration::from_secs(20));
                ExitCode::SUCCESS
            }
            "family" => {
                const DETACHED_PROCESS: u32 = 0x8;
                let child = std::env::current_exe().and_then(|exe| {
                    std::process::Command::new(exe)
                        .env(PROBE, "alone")
                        .creation_flags(DETACHED_PROCESS)
                        .spawn()
                });
                if child.is_err() {
                    return ExitCode::from(4);
                }
                ready();
                std::thread::sleep(Duration::from_secs(20));
                ExitCode::SUCCESS
            }
            "attached-host" => match attached_host() {
                Ok(()) => ExitCode::SUCCESS,
                Err(why) => {
                    say!("attached-host: {why}");
                    ExitCode::FAILURE
                }
            },
            "alone" => {
                std::thread::sleep(Duration::from_secs(20));
                ExitCode::SUCCESS
            }
            _ => ExitCode::from(2),
        }
    }

    fn ready() {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(b"ready\r\n").and_then(|()| out.flush());
    }

    /// The console's output as a VT program sets it: sequences passed on,
    /// no newline turned into a carriage return too.
    fn virtual_terminal() {
        use windows_sys::Win32::System::Console::{
            DISABLE_NEWLINE_AUTO_RETURN, ENABLE_PROCESSED_OUTPUT,
            ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE,
            SetConsoleMode,
        };
        // SAFETY: it takes a constant and touches no memory.
        let out = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        let mut mode = 0u32;
        // SAFETY: `mode` is writable; a handle that is no console fails.
        if unsafe { GetConsoleMode(out, &mut mode) } != 0 {
            let mode = mode
                | ENABLE_PROCESSED_OUTPUT
                | ENABLE_VIRTUAL_TERMINAL_PROCESSING
                | DISABLE_NEWLINE_AUTO_RETURN;
            // SAFETY: as above; it touches no memory.
            unsafe { SetConsoleMode(out, mode) };
        }
    }
}
