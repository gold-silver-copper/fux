//! The real client in a console, as a person meets it: this binary hosts a
//! pseudo console (it plays the terminal), `cmd.exe` runs in it, and runs
//! this binary again as the shell, which notes the console's modes and
//! code page, runs real `fux`, and notes them again; `fux`'s pane runs it
//! once more, printing each byte it gets. Each test has a deadline and a
//! folder of its own. Its own `main`, as a pane must write nothing but what
//! it was given, and one poller per process wants one test at a time.
#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    windows::main()
}

#[cfg(windows)]
mod windows {
    use fuxix::poll::{Events, PollFd};
    use fuxix::process::{Dispositions, Job, Reach, Signal};
    use fuxix::pty::{self, BREAK, Master};
    use fuxix::terminal::{Raw, Size};
    use std::fmt::Write as _;
    use std::io::{Read as _, Write as _};
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode, Stdio};
    use std::time::{Duration, Instant};

    type Outcome = Result<(), String>;

    const FUX: &str = env!("CARGO_BIN_EXE_fux");
    /// What this binary is, run again.
    const ROLE: &str = "FUX_CONSOLE_ROLE";
    /// The test's folder, in every role.
    const DIR: &str = "FUX_CONSOLE_DIR";
    /// The words the shell runs `fux` with.
    const WORDS: &str = "FUX_CONSOLE_WORDS";
    const DEADLINE: Duration = Duration::from_secs(45);
    /// What ends a pane's program.
    const QUIT: &[u8] = b"Q";
    /// The package of the pinned ConPTY, as `tests/windows.rs` puts it.
    const PACKAGE: &str = "microsoft.windows.console.conpty.1.25.260930003.nupkg";

    macro_rules! say {
        ($($arg:tt)*) => {{
            let _ = writeln!(std::io::stderr(), $($arg)*);
        }};
    }

    fn text(error: impl std::fmt::Display) -> String {
        error.to_string()
    }

    pub fn main() -> ExitCode {
        match std::env::var(ROLE).as_deref() {
            Ok("shell") => shell(),
            Ok("pane") => pane(),
            Ok("built-in") => {
                // A folder with no ConPTY: the terminal is Windows' own.
                let _ = pty::load(
                    Path::new(env!("CARGO_TARGET_TMPDIR"))
                        .join("no-conpty")
                        .as_path(),
                );
                let tests: [Test; 1] = [("keys_reach_the_pane_through_the_built_in", keys)];
                report(&tests)
            }
            _ => driver(),
        }
    }

    type Test = (&'static str, fn() -> Outcome);

    fn report(tests: &[Test]) -> ExitCode {
        let mut failed = 0u8;
        for (name, test) in tests {
            let start = Instant::now();
            let outcome = test();
            let took = start.elapsed().as_secs_f32();
            match &outcome {
                Ok(()) => say!("test {name} ... ok ({took:.1} s)"),
                Err(why) => say!("test {name} ... FAILED ({took:.1} s): {why}"),
            }
            failed = failed.saturating_add(u8::from(outcome.is_err()));
        }
        match failed {
            0 => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        }
    }

    fn driver() -> ExitCode {
        let run = std::process::id();
        let conpty = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("console-conpty-{run}"));
        if let Err(why) = package_in(&conpty).and_then(|()| {
            pty::fetch(&conpty).map_err(text)?;
            pty::load(&conpty).map_err(text)
        }) {
            say!("the fetched ConPTY: {why}");
            return ExitCode::FAILURE;
        }
        let tests: [Test; 7] = [
            ("attach_paints_types_and_detaches", attach),
            ("keys_reach_the_pane_byte_for_byte", keys),
            ("a_new_size_reaches_the_server", resize),
            ("every_way_out_puts_the_console_back", ways_out),
            ("a_keys_program_asks_and_stops_off_the_poller", worker),
            ("only_a_live_server_counts_as_running", finding),
            ("pipes_end_with_their_readers", pipes),
        ];
        let ours = report(&tests);
        let exe = std::env::current_exe().map_err(text);
        let built_in = exe.and_then(|exe| {
            Command::new(exe)
                .env(ROLE, "built-in")
                .status()
                .map_err(text)
        });
        match (ours, built_in) {
            (ExitCode::SUCCESS, Ok(status)) if status.success() => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        }
    }

    /// CI's package, where `fetch` finds one by hand, so no test downloads.
    fn package_in(conpty: &Path) -> Outcome {
        if let Some(package) = std::env::var_os("FUXIX_CONPTY_PACKAGE")
            && !conpty.join(PACKAGE).exists()
        {
            std::fs::create_dir_all(conpty).map_err(text)?;
            std::fs::copy(package, conpty.join(PACKAGE)).map_err(text)?;
        }
        Ok(())
    }

    /// One test's folder, its fux folder, and everything the roles write.
    struct Place {
        dir: PathBuf,
    }

    impl Place {
        fn new(name: &str) -> Result<Place, String> {
            // Of this run alone: another user's run can't be cleared away,
            // and its logs would read as this one's.
            let run = std::process::id();
            let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("console-{name}-{run}"));
            std::fs::create_dir_all(dir.join("appdata").join("fux")).map_err(text)?;
            let folder = fux_server::socket::folder().map_err(text)?;
            let home = dir.join(folder);
            fuxix::file::private_dir(&home).map_err(text)?;
            package_in(&home.join("conpty"))?;
            Ok(Place { dir })
        }

        fn socket(&self) -> Result<PathBuf, String> {
            Ok(self
                .dir
                .join(fux_server::socket::folder().map_err(text)?)
                .join("server.sock"))
        }

        /// The variables every role and `fux` run with.
        fn environment(&self, command: &mut Command) -> Outcome {
            let exe = std::env::current_exe().map_err(text)?;
            command
                .env(DIR, &self.dir)
                .env("LOCALAPPDATA", &self.dir)
                .env("APPDATA", self.dir.join("appdata"))
                .env("FUX_SOCKET", self.socket()?)
                // What a pane runs: this binary.
                .env("COMSPEC", exe)
                .env_remove("XDG_CONFIG_HOME")
                .env_remove("FUX_PANE")
                .env_remove("FUX_SERVER");
            Ok(())
        }

        /// `fux` with `words`, not in the console, with its output.
        fn fux(&self, words: &[&str]) -> Result<(i32, String, String), String> {
            let mut command = Command::new(FUX);
            self.environment(&mut command)?;
            let out = command
                .env(ROLE, "pane")
                .args(words)
                .stdin(Stdio::null())
                .output()
                .map_err(text)?;
            Ok((
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ))
        }

        fn lines(&self, role: &str) -> Vec<String> {
            let path = self.dir.join(format!("{role}.log"));
            let read = std::fs::read_to_string(path).unwrap_or_default();
            read.lines().map(str::to_owned).collect()
        }

        fn config(&self, program: &str) -> Outcome {
            let path = self.dir.join("appdata").join("fux").join("config.grift");
            std::fs::write(path, program).map_err(text)
        }
    }

    /// `line`, appended to `role`'s log in the test's folder.
    fn note(role: &str, line: &str) {
        let Some(dir) = std::env::var_os(DIR) else {
            return;
        };
        let path = Path::new(&dir).join(format!("{role}.log"));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path);
        if let Ok(mut file) = file {
            let _ = writeln!(file, "{line}");
        }
    }

    /// `cmd.exe` in a console this process hosts, running the shell role,
    /// which runs `fux` with `words`.
    struct Console {
        master: Master,
        job: Option<Job>,
        seen: Vec<u8>,
        deadline: Instant,
    }

    impl Console {
        fn start(place: &Place, words: &str, rows: u16, cols: u16) -> Result<Console, String> {
            let size = Size::new(rows, cols).ok_or("a size")?;
            let (master, slave) = pty::open(size).map_err(text)?;
            let exe = std::env::current_exe().map_err(text)?;
            let mut failed = None;
            let line = [
                "cmd.exe".as_ref(),
                "/d".as_ref(),
                "/c".as_ref(),
                exe.as_os_str(),
            ];
            let job = slave
                .spawn(
                    &line,
                    |command| {
                        failed = place.environment(command).err();
                        command.env(ROLE, "shell").env(WORDS, words);
                    },
                    Dispositions::Default,
                )
                .map_err(|e| format!("spawn: {e}"))?;
            if let Some(why) = failed {
                return Err(why);
            }
            Ok(Console {
                master,
                job: Some(job),
                seen: Vec::new(),
                deadline: Instant::now().checked_add(DEADLINE).ok_or("a deadline")?,
            })
        }

        fn write(&self, bytes: &[u8]) -> Outcome {
            let mut rest = bytes;
            while !rest.is_empty() {
                match fuxix::io::write(&self.master, rest) {
                    Ok(n) => rest = rest.get(n..).unwrap_or_default(),
                    Err(fuxix::Errno::AGAIN) => {
                        let mut fds = [PollFd::new(&self.master, Events::OUT)];
                        fuxix::poll::poll(&mut fds, Some(Duration::from_millis(50)))
                            .map_err(text)?;
                    }
                    Err(e) => return Err(text(e)),
                }
            }
            Ok(())
        }

        /// Reads what the console paints till `done`, or the deadline.
        fn until(&mut self, what: &str, done: impl Fn(&Console) -> bool) -> Outcome {
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                if done(self) {
                    return Ok(());
                }
                if Instant::now() > self.deadline {
                    let tail = self.seen.len().saturating_sub(1500);
                    let tail = String::from_utf8_lossy(self.seen.get(tail..).unwrap_or_default());
                    return Err(format!("no {what} in time; the console last said {tail:?}"));
                }
                let mut fds = [PollFd::new(&self.master, Events::IN)];
                fuxix::poll::poll(&mut fds, Some(Duration::from_millis(50))).map_err(text)?;
                match fuxix::io::read(&self.master, &mut buffer) {
                    Ok(0) => return Err(format!("the console ended before {what}")),
                    Ok(n) => self
                        .seen
                        .extend_from_slice(buffer.get(..n).unwrap_or_default()),
                    Err(fuxix::Errno::AGAIN) => {}
                    Err(e) => return Err(text(e)),
                }
            }
        }

        fn shows(&self, wanted: &str) -> bool {
            contains(&self.seen, wanted.as_bytes())
        }
    }

    impl Drop for Console {
        fn drop(&mut self) {
            if let Some(job) = self.job.take() {
                let _ = job.kill(Reach::Group);
            }
        }
    }

    /// Stops the test's server, however the test went.
    struct Stops<'a>(&'a Place);

    impl Drop for Stops<'_> {
        /// A client whose console just went may still be attached, which
        /// `kill-server` refuses.
        fn drop(&mut self) {
            for _ in 0..50 {
                if self
                    .0
                    .fux(&["(kill-server)"])
                    .is_ok_and(|(status, ..)| status == 0)
                    || self
                        .0
                        .fux(&["(status)"])
                        .is_ok_and(|(status, ..)| status != 0)
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    fn pane_ready(place: &Place, console: &mut Console) -> Outcome {
        console.until("a pane", |_| {
            place.lines("pane").iter().any(|l| l == "ready")
        })
    }

    /// Whether the pane's log shows it got `bytes`, in one or several reads.
    fn pane_got(place: &Place, bytes: &[u8]) -> bool {
        let mut got = Vec::new();
        for line in place.lines("pane") {
            if let Some(hex) = line.strip_prefix("got ") {
                got.extend(
                    hex.split(' ')
                        .filter_map(|b| u8::from_str_radix(b, 16).ok()),
                );
            }
        }
        contains(&got, bytes)
    }

    /// How many bytes the pane has got.
    fn pane_count(place: &Place) -> usize {
        let got = place.lines("pane");
        let bytes = got.iter().filter_map(|line| line.strip_prefix("got "));
        bytes.map(|hex| hex.split(' ').count()).sum()
    }

    fn contains(bytes: &[u8], wanted: &[u8]) -> bool {
        (0..bytes.len()).any(|at| bytes.get(at..).is_some_and(|rest| rest.starts_with(wanted)))
    }

    /// The shell's modes before and after `fux`: the same.
    fn modes_back(place: &Place, how: &str) -> Outcome {
        let lines = place.lines("shell");
        let of = |prefix: &str| {
            lines
                .iter()
                .find_map(|l| l.strip_prefix(prefix).map(str::to_owned))
        };
        let (before, after) = (of("before "), of("after "));
        say!("{how}: before {before:?} after {after:?}");
        match (before, after) {
            (Some(before), Some(after)) if before == after => Ok(()),
            (before, after) => Err(format!("{how}: modes {before:?} became {after:?}")),
        }
    }

    fn attach() -> Outcome {
        let place = Place::new("attach")?;
        let _stops = Stops(&place);
        let mut console = Console::start(&place, "", 30, 100)?;
        pane_ready(&place, &mut console)?;
        console.write(b"hello")?;
        console.until("the typed text in the pane", |_| pane_got(&place, b"hello"))?;
        console.until("the pane's echo painted", |c| c.shows("hello"))?;
        console.write(b"\x02d")?;
        console.until("the detach", |c| c.shows("[detached"))?;
        console.until("the shell's end", |_| {
            place.lines("shell").iter().any(|l| l.starts_with("after "))
        })?;
        modes_back(&place, "detach")
    }

    /// Keys as a terminal sends them, each reaching the pane exact.
    fn keys() -> Outcome {
        let place = Place::new("keys")?;
        let _stops = Stops(&place);
        let mut console = Console::start(&place, "", 30, 100)?;
        pane_ready(&place, &mut console)?;
        let sent: [(&str, &[u8]); 8] = [
            ("e acute", "é".as_bytes()),
            ("emoji", "😀".as_bytes()),
            ("up", b"\x1b[A"),
            ("ctrl-c", b"\x03"),
            ("ctrl-z", b"\x1a"),
            ("ctrl-space", b"\x00"),
            ("paste", b"\x1b[200~pasted text\x1b[201~"),
            ("after", b"x"),
        ];
        for (name, bytes) in sent {
            console.write(bytes)?;
            // A paste reaches the pane as its text, bracketed by fux.
            let want: &[u8] = if name == "paste" {
                b"pasted text"
            } else {
                bytes
            };
            console.until(name, |_| pane_got(&place, want))?;
            say!("key {name}: reached the pane");
        }
        console.until("the emoji painted back", |c| c.shows("😀"))?;
        // More than a read holds, whole: its tail waits for no next key.
        let paste: String = std::iter::repeat_n("0123456789abcdef", 4096).collect();
        console.write(format!("\x1b[200~{paste}\x1b[201~").as_bytes())?;
        console.until("a paste's last byte", |_| pane_count(&place) > paste.len())?;
        // A pane that asked for focus reports, as a terminal gives them.
        console.write(b"\x1b[O\x1b[I")?;
        console.until("the focus report", |_| pane_got(&place, b"\x1b[I"))
    }

    fn resize() -> Outcome {
        let place = Place::new("resize")?;
        let _stops = Stops(&place);
        let mut console = Console::start(&place, "", 30, 100)?;
        pane_ready(&place, &mut console)?;
        let size = Size::new(40, 120).ok_or("a size")?;
        let asked = Instant::now();
        fuxix::terminal::set_window_size(&console.master, size, None).map_err(text)?;
        console.until("the new size in the pane", |_| {
            place.lines("pane").iter().any(|l| l.ends_with(" 120"))
        })?;
        say!("resized in {} ms", asked.elapsed().as_millis());
        Ok(())
    }

    fn ways_out() -> Outcome {
        for (name, how) in [("last-pane", 0), ("break", 1)] {
            let place = Place::new(&format!("out-{name}"))?;
            let _stops = Stops(&place);
            let mut console = Console::start(&place, "", 30, 100)?;
            pane_ready(&place, &mut console)?;
            match how {
                // Its last pane ended, the server stops and says so.
                0 => console.write(QUIT)?,
                _ => console.write(BREAK)?,
            }
            console.until("the shell's end", |_| {
                place.lines("shell").iter().any(|l| l.starts_with("after "))
            })?;
            modes_back(&place, name)?;
        }
        Ok(())
    }

    fn worker() -> Outcome {
        let place = Place::new("worker")?;
        let _stops = Stops(&place);
        place.config(
            "(bind-key \"C-b j\" (code! (lambda! () (wait (timeout (focused (me)) 60000)) #inert)))
             (bind-key \"C-b k\" (code! (lambda! () (send-keys (text (focused (me)) \"next-ok\")) #inert)))",
        )?;
        let mut console = Console::start(&place, "", 30, 100)?;
        pane_ready(&place, &mut console)?;
        // A key's program blocked on its request, on the worker, while the
        // main thread polls; the prefix then Esc stops it.
        console.write(b"\x02j")?;
        std::thread::sleep(Duration::from_millis(300));
        console.write(b"\x02\x1b")?;
        std::thread::sleep(Duration::from_millis(300));
        console.write(b"\x02k")?;
        console.until("the next program's keys in the pane", |_| {
            pane_got(&place, b"next-ok")
        })
    }

    /// No server, a run that crashed, and a squatter with every instance
    /// busy: none runs, none is joined.
    fn finding() -> Outcome {
        let place = Place::new("finding")?;
        let (status, stdout, stderr) = place.fux(&["(status)"])?;
        say!("no server: {status} {stdout:?} {stderr:?}");
        if status == 0 {
            return Err("a server was found where none runs".to_owned());
        }
        let socket = place.socket()?;
        let name = fux_server::socket::pipe_name(&socket).map_err(text)?;
        // Its only instance taken by a client of its own: busy.
        let mut squatter = fuxix::socket::Listener::bind(&name).map_err(text)?;
        let _taken = fuxix::socket::connect(&name).map_err(text)?;
        let _connection = accepted(&mut squatter)?;
        std::fs::write(
            socket.with_file_name("server.sock.server"),
            "4 133000000000000000\n",
        )
        .map_err(text)?;
        let (status, stdout, stderr) = place.fux(&["(status)"])?;
        say!("a squatter: {status} {stdout:?} {stderr:?}");
        if status == 0 {
            return Err("a squatter counted as a server".to_owned());
        }
        drop((_connection, _taken, squatter));
        // A record left by a server that crashed, and no pipe.
        let (status, stdout, stderr) = place.fux(&["(status)"])?;
        say!("a crashed record: {status} {stdout:?} {stderr:?}");
        if status == 0 || !stderr.contains("left") {
            return Err(format!("a crashed record read as {stderr:?}"));
        }
        let (status, _, stderr) = place.fux(&["(start)"])?;
        let _stops = Stops(&place);
        match status {
            0 => Ok(()),
            _ => Err(format!("`fux start` over a crashed record: {stderr}")),
        }
    }

    fn accepted(
        listener: &mut fuxix::socket::Listener,
    ) -> Result<fuxix::socket::Connection, String> {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(5))
            .ok_or("a deadline")?;
        loop {
            match listener.accept() {
                Ok(connection) => return Ok(connection),
                Err(fuxix::Errno::AGAIN) if Instant::now() < deadline => {
                    let mut fds = [PollFd::new(&*listener, Events::IN)];
                    fuxix::poll::poll(&mut fds, Some(Duration::from_millis(50))).map_err(text)?;
                }
                Err(e) => return Err(text(e)),
            }
        }
    }

    /// `fux start` hands its server nothing of its stdout, so a reader of
    /// it ends; and a reader that goes ends a waiting `events`, on a
    /// write-only pipe, with no event to come.
    fn pipes() -> Outcome {
        let place = Place::new("pipes")?;
        let _stops = Stops(&place);
        let mut start = Command::new(FUX);
        place.environment(&mut start)?;
        let mut child = start
            .env(ROLE, "pane")
            .arg("(start)")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(text)?;
        let stdout = child.stdout.take().ok_or("its stdout")?;
        let (said, heard) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut all = String::new();
            let _ = std::io::BufReader::new(stdout).read_to_string(&mut all);
            let _ = said.send(all);
        });
        heard
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| "`fux start`'s reader never saw its stdout end".to_owned())?;
        let first = child.wait_with_output().map_err(text)?;
        let said = String::from_utf8_lossy(&first.stderr);
        if said.matches("fetching").count() != 1 {
            return Err(format!("a first start said {said:?}"));
        }
        place.fux(&["(kill-server)"])?;
        let (_, _, said) = place.fux(&["(start)"])?;
        if said.contains("fetching") {
            return Err(format!("a second start said {said:?}"));
        }
        let mut events = Command::new(FUX);
        place.environment(&mut events)?;
        let mut child = events
            .env(ROLE, "pane")
            .args(["all", "str", "", "events"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(text)?;
        std::thread::sleep(Duration::from_millis(500));
        drop(child.stdout.take());
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .ok_or("a deadline")?;
        loop {
            if child.try_wait().map_err(text)?.is_some() {
                return Ok(());
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                return Err("`events` outlived its reader by 10 s".to_owned());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The console's modes and output code page, as words.
    fn state() -> String {
        format!("{:x?}", fuxix::terminal::console_state())
    }

    /// Stands for a person's shell: `fux`, between two looks at the console.
    fn shell() -> ExitCode {
        // A shell lives through the Ctrl-Break that detaches what it runs.
        let _lives = fuxix::signal::Pipe::new(&[Signal::Quit, Signal::Int]);
        note("shell", &format!("before {}", state()));
        let words = std::env::var(WORDS).unwrap_or_default();
        let status = Command::new(FUX)
            .env(ROLE, "pane")
            .args(words.split_whitespace())
            .status();
        note("shell", &format!("fux {status:?}"));
        note("shell", &format!("after {}", state()));
        ExitCode::SUCCESS
    }

    /// A pane's program: each read's bytes, in hex, in its log, written back
    /// when they are text; each size it is told.
    fn pane() -> ExitCode {
        fuxix::terminal::open_stdio();
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let raw = Raw::enter(std::io::stdin());
        if let Err(why) = &raw {
            note("pane", &format!("raw: {why}"));
            return ExitCode::FAILURE;
        }
        let Ok(winch) = fuxix::signal::Pipe::new(&[Signal::Winch]) else {
            return ExitCode::FAILURE;
        };
        let _ = fuxix::io::write(&stdout, b"\x1b[?1004h");
        note("pane", "ready");
        let mut buffer = [0u8; 4096];
        loop {
            let mut fds = [
                PollFd::new(&stdin, Events::IN),
                PollFd::new(winch.reader(), Events::IN),
            ];
            if fuxix::poll::poll(&mut fds, None).is_err() {
                return ExitCode::FAILURE;
            }
            while winch.next().is_some() {
                if let Some((size, _)) = fuxix::terminal::stdin_size() {
                    note("pane", &format!("size {} {}", size.rows, size.cols));
                }
            }
            match fuxix::io::read(&stdin, &mut buffer) {
                Ok(0) => return ExitCode::SUCCESS,
                Ok(n) => {
                    let got = buffer.get(..n).unwrap_or_default();
                    if got == QUIT {
                        return ExitCode::SUCCESS;
                    }
                    let mut hex = String::new();
                    for byte in got {
                        let _ = write!(hex, "{byte:02x} ");
                    }
                    note("pane", &format!("got {}", hex.trim_end()));
                    if std::str::from_utf8(got).is_ok_and(|t| t.chars().all(|c| !c.is_control())) {
                        let _ = fuxix::io::write(&stdout, got);
                    }
                }
                Err(fuxix::Errno::AGAIN) => {}
                Err(_) => return ExitCode::FAILURE,
            }
        }
    }
}
