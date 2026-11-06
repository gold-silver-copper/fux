//! A console's modes come back from what ends a process unasked: a panic,
//! and the console closing with no pipe asking for it. This binary hosts a
//! pseudo console and runs itself in it, as the program that enters `Raw`
//! and as the shell that looks at the console after it. Its own `main`, as a
//! program in a console must print nothing of a harness's.
#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    windows::main()
}

#[cfg(windows)]
mod windows {
    use fuxix::poll::{Events, PollFd};
    use fuxix::process::{Dispositions, Reach};
    use fuxix::pty;
    use fuxix::terminal::{self, Raw, Size};
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_sys::core::BOOL;

    type Outcome = Result<(), String>;

    const ROLE: &str = "FUXIX_CONSOLE_ROLE";
    const LOG: &str = "FUXIX_CONSOLE_LOG";
    const DEADLINE: Duration = Duration::from_secs(45);

    macro_rules! say {
        ($($arg:tt)*) => {{
            let _ = writeln!(std::io::stderr(), $($arg)*);
        }};
    }

    fn text(error: impl std::fmt::Display) -> String {
        error.to_string()
    }

    fn note(line: &str) {
        if let Some(path) = std::env::var_os(LOG)
            && let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    fn state() -> String {
        format!("{:x?}", terminal::console_state())
    }

    pub fn main() -> ExitCode {
        match std::env::var(ROLE).as_deref() {
            Ok("shell") => shell(),
            Ok("panics") => panics(),
            Ok("closes") => closes(),
            _ => tests(),
        }
    }

    /// The pinned ConPTY, as `tests/conpty.rs` gets it.
    fn load() -> Outcome {
        const PACKAGE: &str = "microsoft.windows.console.conpty.1.25.260930003.nupkg";
        let run = std::process::id();
        let folder = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("conpty-{run}"));
        std::fs::create_dir_all(&folder).map_err(text)?;
        if let Some(package) = std::env::var_os("FUXIX_CONPTY_PACKAGE") {
            std::fs::copy(package, folder.join(PACKAGE)).map_err(text)?;
        }
        pty::fetch(&folder).map_err(text)?;
        pty::load(&folder).map_err(text)
    }

    fn tests() -> ExitCode {
        if let Err(why) = load() {
            say!("the ConPTY: {why}");
            return ExitCode::FAILURE;
        }
        let name = "what_ends_a_process_unasked_puts_the_console_back";
        match panic().and_then(|()| close()) {
            Ok(()) => {
                say!("test {name} ... ok");
                ExitCode::SUCCESS
            }
            Err(why) => {
                say!("test {name} ... FAILED: {why}");
                ExitCode::FAILURE
            }
        }
    }

    fn log(name: &str) -> Result<PathBuf, String> {
        // Of this run alone: another user's can't be cleared away, and
        // would read as this one's.
        let run = std::process::id();
        let path = format!("console-{name}-{run}.log");
        Ok(Path::new(env!("CARGO_TARGET_TMPDIR")).join(path))
    }

    fn lines(path: &Path) -> Vec<String> {
        let read = std::fs::read_to_string(path).unwrap_or_default();
        read.lines().map(str::to_owned).collect()
    }

    /// This binary in a console of its own, as `role`, till `done` says of
    /// what it noted. What the console paints is read, so it never stalls.
    fn hosted(
        role: &str,
        path: &Path,
        then: impl FnOnce(&pty::Master, &fuxix::process::Job),
        done: impl Fn(&[String]) -> bool,
    ) -> Outcome {
        let size = Size::new(24, 80).ok_or("a size")?;
        let (master, slave) = pty::open(size).map_err(text)?;
        let exe = std::env::current_exe().map_err(text)?;
        let job = slave
            .spawn(
                &[exe.as_os_str()],
                |command| {
                    command.env(ROLE, role).env(LOG, path);
                },
                Dispositions::Default,
            )
            .map_err(text)?;
        let deadline = Instant::now().checked_add(DEADLINE).ok_or("a deadline")?;
        let mut then = Some(then);
        let mut buffer = [0u8; 4096];
        loop {
            let noted = lines(path);
            if noted.iter().any(|l| l == "ready")
                && let Some(act) = then.take()
            {
                act(&master, &job);
            }
            if done(&noted) {
                let _ = job.kill(Reach::Group);
                return Ok(());
            }
            if Instant::now() > deadline {
                let _ = job.kill(Reach::Group);
                return Err(format!("no end in time; noted {noted:?}"));
            }
            let mut fds = [PollFd::new(&master, Events::IN)];
            fuxix::poll::poll(&mut fds, Some(Duration::from_millis(50))).map_err(text)?;
            match fuxix::io::read(&master, &mut buffer) {
                Ok(0) | Err(fuxix::Errno::AGAIN) => {}
                Ok(_) => {}
                Err(e) if e.is_peer_gone() => {}
                Err(e) => return Err(text(e)),
            }
        }
    }

    /// Whether `before` and `after` were noted, and are the same.
    fn same(noted: &[String]) -> Outcome {
        let of = |prefix: &str| {
            noted
                .iter()
                .find_map(|l| l.strip_prefix(prefix).map(str::to_owned))
        };
        match (of("before "), of("after ")) {
            (Some(before), Some(after)) if before == after => {
                say!("before and after {before}");
                Ok(())
            }
            (before, after) => Err(format!("{before:?} became {after:?}")),
        }
    }

    fn panic() -> Outcome {
        let path = log("panic")?;
        hosted(
            "shell",
            &path,
            |_, _| {},
            |noted| noted.iter().any(|l| l.starts_with("after ")),
        )?;
        same(&lines(&path))
    }

    /// The close handler's own look, from a handler of the test's that runs
    /// after fuxix's, as the console still is.
    fn close() -> Outcome {
        let path = log("close")?;
        hosted(
            "closes",
            &path,
            |master, _| {
                let _ = master.hang_up();
            },
            |noted| noted.iter().any(|l| l.starts_with("after ")),
        )?;
        same(&lines(&path))
    }

    /// Stands for a shell: the program that panics, between two looks.
    fn shell() -> ExitCode {
        note(&format!("before {}", state()));
        let run = std::env::current_exe().and_then(|exe| {
            Command::new(exe)
                .env(ROLE, "panics")
                .stderr(std::process::Stdio::null())
                .status()
        });
        note(&format!("ran {run:?}"));
        note(&format!("after {}", state()));
        ExitCode::SUCCESS
    }

    #[expect(clippy::panic, reason = "the panic is what is tested")]
    fn panics() -> ExitCode {
        terminal::open_stdio();
        match Raw::enter(std::io::stdin()) {
            Ok(_raw) => panic!("the test's panic, while raw"),
            Err(why) => {
                note(&format!("raw: {why}"));
                ExitCode::FAILURE
            }
        }
    }

    /// What runs on close after fuxix's handler, which was set later:
    /// handlers run last set first.
    unsafe extern "system" fn after_fuxix(_: u32) -> BOOL {
        note(&format!("after {}", state()));
        0
    }

    fn closes() -> ExitCode {
        terminal::open_stdio();
        // SAFETY: the handler lives as long as the process.
        if unsafe { SetConsoleCtrlHandler(Some(after_fuxix), 1) } == 0 {
            return ExitCode::FAILURE;
        }
        note(&format!("before {}", state()));
        let raw = Raw::enter(std::io::stdin());
        if raw.is_err() {
            return ExitCode::FAILURE;
        }
        note("ready");
        std::thread::sleep(DEADLINE);
        ExitCode::SUCCESS
    }
}
