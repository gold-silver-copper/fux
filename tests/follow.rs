//! A command against a server of another protocol runs with the server's
//! own binary, which speaks it (`client::follow`), and is refused, saying
//! why, when it cannot be.
//!
//! The test plays that server, in its own process: so the server's binary
//! is this test's, which, run as fux was (`argv[0]` fux's path), plays the
//! binary followed and says what it was given. That needs a `main` of its
//! own, not the test harness's.
// It scripts `/bin/sh` over a Unix socket (WINDOWS.md, "What PR 8 found").
#[cfg(unix)]
mod unix {
    use std::io::{Read, Write};
    use std::process::{Command, ExitCode, Stdio};
    use std::time::{Duration, Instant};

    const FUX: &str = env!("CARGO_BIN_EXE_fux");
    /// The status the binary followed exits with, which fux's must be.
    const FOLLOWED: u8 = 7;

    type Outcome = Result<(), String>;
    type Test = fn() -> Outcome;

    fn e(error: impl std::fmt::Display) -> String {
        error.to_string()
    }

    pub fn main() -> ExitCode {
        if std::env::args_os().next().is_some_and(|arg0| arg0 == FUX) {
            return followed();
        }
        let tests: [(&str, Test); 7] = [
            ("a_command_follows_the_server", a_command_follows_the_server),
            (
                "a_program_read_from_stdin_follows",
                a_program_read_from_stdin_follows,
            ),
            (
                "a_program_this_fux_cannot_compile_follows",
                a_program_this_fux_cannot_compile_follows,
            ),
            (
                "a_program_that_acted_is_not_followed",
                a_program_that_acted_is_not_followed,
            ),
            ("a_command_follows_once", a_command_follows_once),
            (
                "a_server_that_reads_no_grift_is_not_followed",
                a_server_that_reads_no_grift_is_not_followed,
            ),
            (
                "a_program_file_that_is_a_pipe_is_not_followed",
                a_program_file_that_is_a_pipe_is_not_followed,
            ),
        ];
        let failed = (tests.iter())
            .filter(|(name, test)| {
                let outcome = test();
                let said = match &outcome {
                    Ok(()) => "ok".to_owned(),
                    Err(why) => format!("FAILED: {why}"),
                };
                let _ = writeln!(std::io::stderr(), "test {name} ... {said}");
                outcome.is_err()
            })
            .count();
        ExitCode::from(u8::from(failed > 0))
    }

    /// The binary followed: says its arguments and its stdin on stdout, and
    /// exits with `FOLLOWED`; or, with `AGAIN` set, runs fux again as it was
    /// run, in this same process, as a binary that follows would.
    fn followed() -> ExitCode {
        use std::os::unix::process::CommandExt;
        if std::env::var_os(AGAIN).is_some() {
            let mut args = std::env::args_os();
            let mut fux = Command::new(FUX);
            let error = fux.arg0(args.next().unwrap_or_default()).args(args).exec();
            let _ = writeln!(std::io::stderr(), "running fux again: {error}");
            return ExitCode::FAILURE;
        }
        let args: Vec<_> = std::env::args().skip(1).collect();
        let mut stdin = String::new();
        // A terminal's has no end to read to.
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            let _ = std::io::stdin().read_to_string(&mut stdin);
        }
        let _ = write!(std::io::stdout(), "{args:?}\n{stdin}");
        ExitCode::from(FOLLOWED)
    }

    /// How long the server waits for each connection.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// Set for the binary followed to run fux again (`followed`).
    const AGAIN: &str = "FOLLOW_TEST_AGAIN";

    /// What a command said and its status.
    struct Ran {
        status: Option<i32>,
        stdout: String,
        stderr: String,
    }

    /// Runs fux with `args`, `stdin` and `env` against a server of protocol
    /// 999.
    fn against_another_protocol(
        name: &str,
        args: &[&str],
        stdin: &str,
        env: &[(&str, &str)],
        connections: usize,
    ) -> Result<Ran, String> {
        against_protocol(999, name, args, stdin, env, connections)
    }

    /// Runs fux with `args`, `stdin` and `env` against a server of `protocol`.
    fn against_protocol(
        protocol: u32,
        name: &str,
        args: &[&str],
        stdin: &str,
        env: &[(&str, &str)],
        connections: usize,
    ) -> Result<Ran, String> {
        serving(protocol, name, connections, |socket| {
            let mut child = Command::new(FUX)
                .args(args)
                .envs(env.iter().copied())
                .env("FUX_SOCKET", socket)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(e)?;
            let mut input = child.stdin.take().ok_or("no stdin")?;
            input.write_all(stdin.as_bytes()).map_err(e)?;
            drop(input);
            let out = child.wait_with_output().map_err(e)?;
            Ok(Ran {
                status: out.status.code(),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            })
        })
    }

    /// A plain `fux`, on a terminal, against a server of `protocol`.
    fn attach_to_protocol(protocol: u32) -> Result<Ran, String> {
        // It looks for a server first, then connects.
        serving(protocol, "attach", 2, |socket| {
            let size = fuxix::terminal::Size::from((
                std::num::NonZeroU16::MIN.saturating_add(23),
                std::num::NonZeroU16::MIN.saturating_add(79),
            ));
            let (master, slave) = fuxix::pty::open(size).map_err(e)?;
            let setup = |command: &mut Command| {
                command.env("FUX_SOCKET", socket).env("TERM", "xterm");
            };
            let dispositions = fuxix::process::Dispositions::Default;
            let (mut child, _) = slave.spawn(&[FUX], setup, dispositions).map_err(e)?;
            // Its terminal is read as it runs: macOS holds a process at its
            // exit until what it wrote there has gone.
            let mut said = Vec::new();
            let mut buffer = [0; 4096];
            let began = Instant::now();
            let status = loop {
                if let Ok(n) = fuxix::io::read(&master, &mut buffer) {
                    said.extend_from_slice(buffer.get(..n).unwrap_or_default());
                }
                if let Some(status) = child.try_wait().map_err(e)? {
                    break status;
                }
                if began.elapsed() > PATIENCE {
                    let _ = child.kill();
                    return Err(format!("still running: {}", String::from_utf8_lossy(&said)));
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            drop(master);
            Ok(Ran {
                status: status.code(),
                stdout: String::from_utf8_lossy(&said).into_owned(),
                stderr: String::new(),
            })
        })
    }

    /// What `run` came to given the socket of a server of `protocol`, played
    /// in a directory of its own: it answers `connections` Hellos as servers
    /// do, hanging up after each, and fails if they do not come within
    /// `PATIENCE`.
    fn serving(
        protocol: u32,
        name: &str,
        connections: usize,
        run: impl FnOnce(&std::path::Path) -> Result<Ran, String>,
    ) -> Result<Ran, String> {
        use fux_wire::protocol::{Frame, Greeting, Hello, Role};
        use std::os::unix::fs::PermissionsExt;
        let dir = (fuxix::file::scratch().map_err(e)?)
            .join(format!("fux-follow-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(e)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
        let listener = std::os::unix::net::UnixListener::bind(dir.join("fux.sock")).map_err(e)?;
        listener.set_nonblocking(true).map_err(e)?;
        let deadline = Instant::now().checked_add(PATIENCE).ok_or("no deadline")?;
        let server = std::thread::spawn(move || -> Outcome {
            for _ in 0..connections {
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(accepted) => break accepted,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => return Err(format!("waiting for fux to connect: {error}")),
                    }
                };
                stream.set_nonblocking(false).map_err(e)?;
                let hello = Greeting::Hello(Hello {
                    protocol,
                    version: "9.9.9".into(),
                    role: Role::Command,
                });
                let exit = Greeting::Exit("the server speaks protocol 999");
                // In one write: the client may hang up once it has read the
                // Hello, or at once, only looking for a server.
                let mut said = hello.encode();
                said.extend(exit.encode());
                let _ = stream.write_all(&said);
            }
            Ok(())
        });
        let ran = run(&dir.join("fux.sock"))?;
        server.join().map_err(|_| "the server panicked")??;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(ran)
    }

    /// The line fux says as it follows the server, whose binary is this one.
    fn following() -> Result<String, String> {
        let exe = std::env::current_exe().map_err(e)?;
        Ok(format!(
            "fux: the server runs fux 9.9.9 (protocol 999) and this client fux {} (protocol {}): running the server's own binary, {}\n",
            env!("CARGO_PKG_VERSION"),
            fux_wire::protocol::PROTOCOL,
            exe.display()
        ))
    }

    /// Runs fux with `args` and `stdin` against a server of another protocol:
    /// it follows, and the binary followed gets them, saying `stdout`; or,
    /// where the system does not say a socket's peer process (macOS), the
    /// server's binary cannot be found, and fux says so.
    fn follows(name: &str, args: &[&str], stdin: &str, stdout: &str) -> Outcome {
        let ran = against_another_protocol(name, args, stdin, &[], 1)?;
        if cfg!(target_os = "macos") {
            return check(
                ran.status == Some(1)
                    && ran.stderr.starts_with("fux: mismatch: ")
                    && ran.stderr.contains("fux 9.9.9 (protocol 999)")
                    && ran
                        .stderr
                        .contains("the system does not say which process the server is"),
                &ran,
            );
        }
        check(ran.status == Some(FOLLOWED.into()), &ran)?;
        check(ran.stdout == stdout, &ran)?;
        check(ran.stderr == following()?, &ran)
    }

    /// A command runs with the server's binary: its arguments and stdin, and
    /// that binary's output and exit status, pass through, and fux says so in
    /// one line. Reading a regular file first is no reason not to: it reads
    /// the same again.
    fn a_command_follows_the_server() -> Outcome {
        follows(
            "command",
            &["(list-session)"],
            "typed\n",
            "[\"(list-session)\"]\ntyped\n",
        )?;
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let program = format!("(read-file {}) (list-session)", fux::script::quoted(file));
        let args = [program.as_str()];
        follows("file", &args, "", &format!("{args:?}\n"))
    }

    /// A program fux read from stdin is that binary's stdin.
    fn a_program_read_from_stdin_follows() -> Outcome {
        follows(
            "stdin",
            &["-"],
            "(list-session)\n",
            "[\"-\"]\n(list-session)\n",
        )
    }

    /// A program this fux cannot compile, a word of the server's vocabulary
    /// perhaps, is the server's binary's to compile.
    fn a_program_this_fux_cannot_compile_follows() -> Outcome {
        let word = "(a-word-only-the-server-knows)";
        follows("compile", &[word], "", &format!("{:?}\n", [word]))
    }

    /// A program that did something outside fux's process before it met the
    /// server, on stderr, stdout or stdin, is refused, saying why and what to
    /// do, rather than run twice.
    fn a_program_that_acted_is_not_followed() -> Outcome {
        for (args, stdin, stdout, stderr) in [
            (
                &["(warn \"hi\") (list-session)"][..],
                "",
                "",
                "hi\nfux: mismatch: ",
            ),
            (
                &["(print \"hi\") (list-session)"],
                "",
                "hi\n",
                "fux: mismatch: ",
            ),
            (
                &["(read-file \"/dev/stdin\") (list-session)"],
                "payload",
                "",
                "fux: mismatch: ",
            ),
        ] {
            let ran = against_another_protocol("acted", args, stdin, &[], 1)?;
            check(ran.status == Some(1) && ran.stdout == stdout, &ran)?;
            check(
                ran.stderr.starts_with(stderr)
                    && ran
                        .stderr
                        .contains("the server runs fux 9.9.9 (protocol 999)")
                    && ran.stderr.contains("`kill` on its process")
                    && ran.stderr.contains("running it again would do twice"),
                &ran,
            )?;
        }
        Ok(())
    }

    /// A command follows once: the binary followed, running fux again, meets
    /// the same server and is refused, with `mismatch`, rather than follow it
    /// for ever; so is a program this fux cannot compile.
    fn a_command_follows_once() -> Outcome {
        if cfg!(target_os = "macos") {
            return Ok(());
        }
        for args in ["(list-session)", "(a-word-only-the-server-knows)"] {
            let ran = against_another_protocol("once", &[args], "", &[(AGAIN, "1")], 2)?;
            check(ran.status == Some(1), &ran)?;
            let rest = ran.stderr.strip_prefix(&following()?).unwrap_or_default();
            check(
                rest.starts_with("fux: mismatch: ") && rest.contains("follows once"),
                &ran,
            )?;
        }
        Ok(())
    }

    /// A server older than grift reads none: its binary is not handed the
    /// program, whose failure there would say nothing of why. The refusal
    /// says to stop that server, and `(status)` still tells of it.
    fn a_server_that_reads_no_grift_is_not_followed() -> Outcome {
        let ran = against_protocol(76, "fuxth", &["(list-session)"], "", &[], 1)?;
        check(
            ran.status == Some(1)
                && ran.stderr.starts_with("fux: mismatch: ")
                && ran.stderr.contains("stop it")
                && !ran.stderr.contains("running the server's own binary"),
            &ran,
        )?;
        let status = against_protocol(76, "status", &["(status)"], "", &[], 1)?;
        check(
            status.status == Some(0)
                && status.stdout.contains("(server-protocol 76)")
                && status.stdout.contains("(compatible #false)"),
            &status,
        )?;
        // A plain attach hands that binary no grift, so it still goes there:
        // the one way to reach a server whose file was replaced.
        let attach = attach_to_protocol(76)?;
        let follows = if cfg!(target_os = "macos") {
            1
        } else {
            FOLLOWED.into()
        };
        check(
            attach.status == Some(follows) && !attach.stdout.contains("stop it"),
            &attach,
        )?;
        // What this fux can't read is told on one line of the refusal.
        let unread = against_protocol(76, "unread", &["(nosuch)"], "", &[], 1)?;
        check(
            unread.stderr.contains("nosuch") && !unread.stderr.contains("\\n"),
            &unread,
        )
    }

    /// A program file that may not read the same again (a pipe) is read as
    /// any source is, and never followed: the server's binary would run it
    /// again from a pipe already read.
    fn a_program_file_that_is_a_pipe_is_not_followed() -> Outcome {
        let ran = against_another_protocol(
            "pipe",
            &["--file", "/dev/stdin"],
            "(list-session)\n",
            &[],
            0,
        )?;
        check(
            ran.status == Some(1)
                && ran.stdout.is_empty()
                && ran.stderr.starts_with("fux: ")
                && !ran.stderr.contains("running the server's own binary"),
            &ran,
        )?;
        // Nor one that reads a pipe with `require` or `include`: the server
        // that answers in another protocol is told of, not followed.
        for word in ["require", "include"] {
            let program = format!("({word}! \"/dev/stdin\")");
            let ran =
                against_another_protocol("pipe-read", &[&program], "(list-session)\n", &[], 1)?;
            check(
                ran.status == Some(1)
                    && ran.stdout.is_empty()
                    && ran.stderr.starts_with("fux: mismatch: ")
                    && !ran.stderr.contains("running the server's own binary"),
                &ran,
            )?;
        }
        Ok(())
    }

    fn check(holds: bool, ran: &Ran) -> Outcome {
        match holds {
            true => Ok(()),
            false => Err(format!(
                "status {:?}, stdout {:?}, stderr {:?}",
                ran.status, ran.stdout, ran.stderr
            )),
        }
    }
}

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix::main()
}

#[cfg(windows)]
fn main() {}
