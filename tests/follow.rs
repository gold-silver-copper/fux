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
    let tests: [(&str, Test); 3] = [
        ("a_command_follows_the_server", a_command_follows_the_server),
        (
            "a_program_read_from_stdin_follows",
            a_program_read_from_stdin_follows,
        ),
        (
            "a_program_that_acted_is_not_followed",
            a_program_that_acted_is_not_followed,
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
/// exits with `FOLLOWED`.
fn followed() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    let _ = write!(std::io::stdout(), "{args:?}\n{stdin}");
    ExitCode::from(FOLLOWED)
}

/// What a command said and its status.
struct Ran {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs fux with `args` and `stdin` against a server of protocol 999,
/// played in a socket of its own: it answers one Hello as servers do, and
/// hangs up.
fn against_another_protocol(name: &str, args: &[&str], stdin: &str) -> Result<Ran, String> {
    use fux_wire::protocol::{Frame, Greeting, Hello, Role};
    use std::os::unix::fs::PermissionsExt;
    let dir = (fuxix::file::scratch().map_err(e)?)
        .join(format!("fux-follow-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(e)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
    let listener = std::os::unix::net::UnixListener::bind(dir.join("fux.sock")).map_err(e)?;
    let server = std::thread::spawn(move || -> Outcome {
        let (mut stream, _) = listener.accept().map_err(e)?;
        let hello = Greeting::Hello(Hello {
            protocol: 999,
            version: "9.9.9".into(),
            role: Role::Command,
        });
        let exit = Greeting::Exit("the server speaks protocol 999");
        // In one write: the client may hang up once it has read the Hello.
        let mut said = hello.encode().map_err(e)?;
        said.extend(exit.encode().map_err(e)?);
        stream.write_all(&said).map_err(e)
    });
    let mut child = Command::new(FUX)
        .args(args)
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
        "fux: the server speaks protocol 999 (fux 9.9.9) and this client {} (fux {}): running the server's own binary, {}\n",
        fux_wire::protocol::PROTOCOL,
        env!("CARGO_PKG_VERSION"),
        exe.display()
    ))
}

/// Where the system does not say a socket's peer process (macOS), the
/// server's binary cannot be found, and fux says so.
fn unfollowed(ran: &Ran) -> Outcome {
    check(
        ran.status == Some(1)
            && ran.stderr.contains("protocol 999 (fux 9.9.9)")
            && ran
                .stderr
                .contains("the system does not say which process the server is"),
        ran,
    )
}

/// A command runs with the server's binary: its arguments and stdin, and
/// that binary's output and exit status, pass through, and fux says so in
/// one line.
fn a_command_follows_the_server() -> Outcome {
    let ran = against_another_protocol("command", &["ls"], "typed\n")?;
    if cfg!(target_os = "macos") {
        return unfollowed(&ran);
    }
    check(ran.status == Some(FOLLOWED.into()), &ran)?;
    check(ran.stdout == "[\"ls\"]\ntyped\n", &ran)?;
    check(ran.stderr == following()?, &ran)
}

/// A program fux read from stdin is that binary's stdin.
fn a_program_read_from_stdin_follows() -> Outcome {
    let ran = against_another_protocol("stdin", &["-"], "ls\n")?;
    if cfg!(target_os = "macos") {
        return unfollowed(&ran);
    }
    check(ran.status == Some(FOLLOWED.into()), &ran)?;
    check(ran.stdout == "[\"-\"]\nls\n", &ran)?;
    check(ran.stderr == following()?, &ran)
}

/// A program that did something outside fux's process before it met the
/// server is refused, saying why and what to do, rather than run twice.
fn a_program_that_acted_is_not_followed() -> Outcome {
    let ran = against_another_protocol("acted", &["str", "hi", "warn", "ls"], "")?;
    check(ran.status == Some(1) && ran.stdout.is_empty(), &ran)?;
    check(
        ran.stderr.starts_with("hi\nfux: mismatch: ")
            && ran
                .stderr
                .contains("the server speaks protocol 999 (fux 9.9.9)")
            && ran.stderr.contains("fux force-kill-server")
            && ran.stderr.contains("running it again would do twice"),
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
