//! A command against a server of another protocol runs with the server's
//! own binary, which speaks it (`client::follow`), and is refused, saying
//! why, when it cannot be.
//!
//! The test plays that server, in its own process: so the server's binary
//! is this test's, which, run as fux was (`argv[0]` fux's path), plays the
//! binary followed and says what it was given. That needs a `main` of its
//! own, not the test harness's.
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

fn main() -> ExitCode {
    if std::env::args_os().next().is_some_and(|arg0| arg0 == FUX) {
        return followed();
    }
    let tests: [(&str, Test); 6] = [
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
    let _ = std::io::stdin().read_to_string(&mut stdin);
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
/// 999, played in a socket of its own: it answers `connections` Hellos as
/// servers do, hanging up after each, and fails if they do not come within
/// `PATIENCE`.
fn against_another_protocol(
    name: &str,
    args: &[&str],
    stdin: &str,
    env: &[(&str, &str)],
    connections: usize,
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
                protocol: 999,
                version: "9.9.9".into(),
                role: Role::Command,
            });
            let exit = Greeting::Exit("the server speaks protocol 999");
            // In one write: the client may hang up once it has read the
            // Hello.
            let mut said = hello.encode().map_err(e)?;
            said.extend(exit.encode().map_err(e)?);
            stream.write_all(&said).map_err(e)?;
        }
        Ok(())
    });
    let mut child = Command::new(FUX)
        .args(args)
        .envs(env.iter().copied())
        .env("FUX_SOCKET", dir.join("fux.sock"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(e)?;
    let mut input = child.stdin.take().ok_or("no stdin")?;
    input.write_all(stdin.as_bytes()).map_err(e)?;
    drop(input);
    let out = child.wait_with_output().map_err(e)?;
    server.join().map_err(|_| "the server panicked")??;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(Ran {
        status: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
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
    follows("command", &["ls"], "typed\n", "[\"ls\"]\ntyped\n")?;
    let file = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let args = ["str", file, "read-file", "drop", "ls"];
    follows("file", &args, "", &format!("{args:?}\n"))
}

/// A program fux read from stdin is that binary's stdin.
fn a_program_read_from_stdin_follows() -> Outcome {
    follows("stdin", &["-"], "ls\n", "[\"-\"]\nls\n")
}

/// A program this fux cannot compile, a word of the server's vocabulary
/// perhaps, is the server's binary's to compile.
fn a_program_this_fux_cannot_compile_follows() -> Outcome {
    let word = "a-word-only-the-server-knows";
    follows("compile", &[word], "", &format!("[\"{word}\"]\n"))
}

/// A program that did something outside fux's process before it met the
/// server, on stderr, stdout or stdin, is refused, saying why and what to
/// do, rather than run twice.
fn a_program_that_acted_is_not_followed() -> Outcome {
    for (args, stdin, stdout, stderr) in [
        (
            &["str", "hi", "warn", "ls"][..],
            "",
            "",
            "hi\nfux: mismatch: ",
        ),
        (&["str", "hi", "print", "ls"], "", "hi\n", "fux: mismatch: "),
        (
            &["str", "/dev/stdin", "read-file", "drop", "ls"],
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
    for args in ["ls", "a-word-only-the-server-knows"] {
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

/// A program file that may not read the same again (a pipe) is refused as
/// a source before anything runs, so nothing is followed to run it empty.
fn a_program_file_that_is_a_pipe_is_not_followed() -> Outcome {
    let ran = against_another_protocol("pipe", &["--file", "/dev/stdin"], "ls\n", &[], 0)?;
    check(
        ran.status == Some(1)
            && ran.stdout.is_empty()
            && ran.stderr.starts_with("fux: read: ")
            && !ran.stderr.contains("running the server's own binary"),
        &ran,
    )
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
