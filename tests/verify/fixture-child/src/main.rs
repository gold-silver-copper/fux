use serde::{Deserialize, Serialize};
use std::env;
use std::ffi::OsStr;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const MAX_FRAME_BYTES: usize = 16 * 1024;
const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
const MAX_COMMANDS: usize = 256;
const DEFAULT_DEADLINE_MS: u64 = 10_000;

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Write {
        chunks_hex: Vec<String>,
    },
    ReadExact {
        bytes: usize,
    },
    Query {
        bytes_hex: String,
        reply_bytes: usize,
        withhold: bool,
    },
    Size,
    Title {
        value: String,
    },
    Progress {
        state: u8,
    },
    Bell,
    Clipboard {
        base64: String,
    },
    Agent {
        payload: String,
    },
    Spawn {
        mode: DescendantMode,
        exit_status: Option<i32>,
    },
    WaitDescendant,
    FillStdout {
        bytes: usize,
        byte: u8,
    },
    RefuseStdin,
    Exit {
        status: i32,
    },
    Quit,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DescendantMode {
    Exit,
    IgnoreHup,
    HoldPty,
    WaitSignal,
}

#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Response {
    Ready {
        version: u16,
        pid: u32,
    },
    DescendantReady {
        pid: u32,
    },
    Wrote {
        bytes: usize,
        chunks: usize,
    },
    Input {
        bytes_hex: String,
    },
    QueryReply {
        bytes_hex: String,
    },
    QueryWithheld,
    Size {
        rows: u16,
        columns: u16,
    },
    Spawned {
        pid: u32,
        mode: &'static str,
    },
    DescendantExit {
        status: i32,
    },
    StdinRefused,
    Cleanup {
        descendants: usize,
    },
    Error {
        category: &'static str,
        message: String,
    },
}

fn main() -> ExitCode {
    if is_notification_invocation() {
        return match record_notification() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("fixture-child notifier: {error}");
                ExitCode::from(70)
            }
        };
    }
    if let Some(mode) = env::args()
        .nth(1)
        .and_then(|value| value.strip_prefix("--descendant=").map(str::to_owned))
    {
        return descendant(&mode);
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fixture-child: {error}");
            ExitCode::from(70)
        }
    }
}

fn is_notification_invocation() -> bool {
    env::args_os()
        .next()
        .and_then(|path| {
            std::path::PathBuf::from(path)
                .file_name()
                .map(OsStr::to_owned)
        })
        .is_some_and(|name| name == "terminal-notifier" || name == "notify-send")
}

fn record_notification() -> io::Result<()> {
    let path = env::var_os("FUX_FIXTURE_NOTIFICATION_LOG")
        .ok_or_else(|| invalid("FUX_FIXTURE_NOTIFICATION_LOG is required"))?;
    let arguments: Vec<_> = env::args_os()
        .skip(1)
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    let encoded = serde_json::to_vec(&arguments).map_err(invalid)?;
    if encoded.len() > MAX_FRAME_BYTES {
        return Err(invalid("notification arguments exceed fixture bound"));
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(&encoded)?;
    file.write_all(b"\n")
}

fn run() -> io::Result<()> {
    let mut terminal = rustix::termios::tcgetattr(io::stdin())?;
    terminal.make_raw();
    rustix::termios::tcsetattr(
        io::stdin(),
        rustix::termios::OptionalActions::Now,
        &terminal,
    )?;
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    let path = argument(&arguments, "--control=")
        .or_else(|| env::var_os("FUX_FIXTURE_CONTROL"))
        .ok_or_else(|| invalid("--control or FUX_FIXTURE_CONTROL is required"))?;
    let deadline_ms = argument(&arguments, "--deadline-ms=")
        .and_then(|value| value.into_string().ok())
        .or_else(|| env::var("FUX_FIXTURE_DEADLINE_MS").ok())
        .map(|value| value.parse::<u64>())
        .transpose()
        .map_err(invalid)?
        .unwrap_or(DEFAULT_DEADLINE_MS)
        .clamp(1, 60_000);
    let mut control = UnixStream::connect(&path)?;
    control.set_read_timeout(Some(Duration::from_millis(deadline_ms)))?;
    control.set_write_timeout(Some(Duration::from_millis(deadline_ms)))?;
    send(
        &mut control,
        &Response::Ready {
            version: 1,
            pid: std::process::id(),
        },
    )?;

    let mut reader = BufReader::new(control.try_clone()?);
    let mut line = String::new();
    let started = Instant::now();
    let deadline = Duration::from_millis(deadline_ms);
    let mut descendants = Descendants::default();
    for _ in 0..MAX_COMMANDS {
        let remaining = deadline
            .checked_sub(started.elapsed())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "fixture deadline expired"))?;
        reader.get_mut().set_read_timeout(Some(remaining))?;
        line.clear();
        let count = reader
            .by_ref()
            .take((MAX_FRAME_BYTES + 1) as u64)
            .read_line(&mut line)?;
        if count == 0 {
            break;
        }
        if count > MAX_FRAME_BYTES || !line.ends_with('\n') {
            send(
                &mut control,
                &Response::Error {
                    category: "frame_too_large",
                    message: "control frame exceeds bound or lacks newline".into(),
                },
            )?;
            return Err(invalid("invalid control frame"));
        }
        let request: Request = match decode_request(&line) {
            Ok(request) => request,
            Err(error) => {
                send(
                    &mut control,
                    &Response::Error {
                        category: "invalid_request",
                        message: error.to_string(),
                    },
                )?;
                return Err(invalid(error));
            }
        };
        if handle(
            request,
            &mut control,
            &mut descendants,
            started + deadline,
            &path,
        )? {
            break;
        }
    }
    descendants.cleanup();
    send(&mut control, &Response::Cleanup { descendants: 0 })
}

fn handle(
    request: Request,
    control: &mut UnixStream,
    descendants: &mut Descendants,
    deadline: Instant,
    control_path: &OsStr,
) -> io::Result<bool> {
    let mut stdout = io::stdout().lock();
    match request {
        Request::Write { chunks_hex } => {
            if chunks_hex.len() > 256 {
                return Err(invalid("too many chunks"));
            }
            let mut total = 0usize;
            for chunk in &chunks_hex {
                let bytes = decode_hex(chunk)?;
                total = total
                    .checked_add(bytes.len())
                    .ok_or_else(|| invalid("payload overflow"))?;
                bounded(total)?;
                stdout.write_all(&bytes)?;
                stdout.flush()?;
            }
            send(
                control,
                &Response::Wrote {
                    bytes: total,
                    chunks: chunks_hex.len(),
                },
            )?;
        }
        Request::ReadExact { bytes } => {
            bounded(bytes)?;
            let mut input = vec![0; bytes];
            read_exact_until(&mut io::stdin(), &mut input, deadline)?;
            send(
                control,
                &Response::Input {
                    bytes_hex: hex(&input),
                },
            )?;
        }
        Request::Query {
            bytes_hex,
            reply_bytes,
            withhold,
        } => {
            let query = decode_hex(&bytes_hex)?;
            bounded(reply_bytes)?;
            stdout.write_all(&query)?;
            stdout.flush()?;
            if withhold {
                send(control, &Response::QueryWithheld)?;
            } else {
                let mut reply = vec![0; reply_bytes];
                read_exact_until(&mut io::stdin(), &mut reply, deadline)?;
                send(
                    control,
                    &Response::QueryReply {
                        bytes_hex: hex(&reply),
                    },
                )?;
            }
        }
        Request::Size => {
            let size = rustix::termios::tcgetwinsize(io::stdin())?;
            send(
                control,
                &Response::Size {
                    rows: size.ws_row,
                    columns: size.ws_col,
                },
            )?;
        }
        Request::Title { value } => write_control(&mut stdout, 0, value.as_bytes())?,
        Request::Progress { state } if state <= 4 => {
            write_control(&mut stdout, 9, format!("4;{state}").as_bytes())?
        }
        Request::Progress { .. } => return Err(invalid("progress state must be 0..=4")),
        Request::Bell => {
            stdout.write_all(b"\x07")?;
            stdout.flush()?;
        }
        Request::Clipboard { base64 } => {
            write_control(&mut stdout, 52, format!("c;{base64}").as_bytes())?
        }
        Request::Agent { payload } => write_control(&mut stdout, 7877, payload.as_bytes())?,
        Request::Spawn { mode, exit_status } => {
            let mode_name = match mode {
                DescendantMode::Exit => "exit",
                DescendantMode::IgnoreHup => "ignore_hup",
                DescendantMode::HoldPty => "hold_pty",
                DescendantMode::WaitSignal => "wait_signal",
            };
            let mut child = Command::new(env::current_exe()?)
                .arg(format!("--descendant={mode_name}"))
                .env(
                    "FUX_FIXTURE_EXIT_STATUS",
                    exit_status.unwrap_or(0).to_string(),
                )
                .env("FUX_FIXTURE_DESCENDANT_CONTROL", control_path)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()?;
            let pid = child.id();
            if matches!(mode, DescendantMode::Exit) {
                let status = wait_child_until(&mut child, deadline)?;
                send(control, &Response::DescendantExit { status })?;
            } else {
                descendants.children.push(child);
                send(
                    control,
                    &Response::Spawned {
                        pid,
                        mode: mode_name,
                    },
                )?;
            }
        }
        Request::WaitDescendant => {
            let mut child = descendants
                .children
                .pop()
                .ok_or_else(|| invalid("no descendant"))?;
            let status = wait_child_until(&mut child, deadline)?;
            send(control, &Response::DescendantExit { status })?;
        }
        Request::FillStdout { bytes, byte } => {
            bounded(bytes)?;
            let chunk = vec![byte; bytes.min(16 * 1024)];
            let mut remaining = bytes;
            let original = rustix::fs::fcntl_getfl(io::stdout())?;
            rustix::fs::fcntl_setfl(io::stdout(), original | rustix::fs::OFlags::NONBLOCK)?;
            while remaining > 0 {
                let count = remaining.min(chunk.len());
                match stdout.write(&chunk[..count]) {
                    Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "stdout closed")),
                    Ok(written) => remaining -= written,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        wait_until(deadline)?
                    }
                    Err(error) => return Err(error),
                }
            }
            rustix::fs::fcntl_setfl(io::stdout(), original)?;
            stdout.flush()?;
            send(control, &Response::Wrote { bytes, chunks: 1 })?;
        }
        Request::RefuseStdin => send(control, &Response::StdinRefused)?,
        Request::Exit { status } => {
            descendants.cleanup();
            send(control, &Response::Cleanup { descendants: 0 })?;
            std::process::exit(status.clamp(0, 255));
        }
        Request::Quit => return Ok(true),
    }
    Ok(false)
}

fn descendant(mode: &str) -> ExitCode {
    let status = env::var("FUX_FIXTURE_EXIT_STATUS")
        .ok()
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(0);
    if announce_descendant().is_err() {
        return ExitCode::from(73);
    }
    match mode {
        "exit" => ExitCode::from(status),
        "hold_pty" => loop {
            std::thread::park();
        },
        "ignore_hup" => wait_for_signal(false, status),
        "wait_signal" => wait_for_signal(true, status),
        _ => ExitCode::from(64),
    }
}

fn announce_descendant() -> io::Result<()> {
    let path = env::var_os("FUX_FIXTURE_DESCENDANT_CONTROL")
        .ok_or_else(|| invalid("FUX_FIXTURE_DESCENDANT_CONTROL is required"))?;
    let mut control = UnixStream::connect(path)?;
    send(
        &mut control,
        &Response::DescendantReady {
            pid: std::process::id(),
        },
    )
}

fn wait_child_until(child: &mut Child, deadline: Instant) -> io::Result<i32> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code().unwrap_or(128));
        }
        if let Err(error) = wait_until(deadline) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    }
}

fn wait_for_signal(include_hup: bool, status: u8) -> ExitCode {
    use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    let ignored_hup = Arc::new(AtomicBool::new(false));
    if !include_hup && signal_hook::flag::register(SIGHUP, ignored_hup).is_err() {
        return ExitCode::from(71);
    }
    let watched = if include_hup {
        vec![SIGHUP, SIGINT, SIGTERM]
    } else {
        vec![SIGINT, SIGTERM]
    };
    let Ok(mut signals) = signal_hook::iterator::Signals::new(watched) else {
        return ExitCode::from(71);
    };
    if signals.forever().next().is_some() {
        ExitCode::from(status)
    } else {
        ExitCode::from(72)
    }
}

#[derive(Default)]
struct Descendants {
    children: Vec<Child>,
}

impl Descendants {
    fn cleanup(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.children.clear();
    }
}

impl Drop for Descendants {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn write_control(stdout: &mut impl Write, code: u16, payload: &[u8]) -> io::Result<()> {
    bounded(payload.len())?;
    write!(stdout, "\x1b]{code};")?;
    stdout.write_all(payload)?;
    stdout.write_all(b"\x1b\\")?;
    stdout.flush()
}

fn send(stream: &mut UnixStream, response: &Response) -> io::Result<()> {
    serde_json::to_writer(&mut *stream, response).map_err(invalid)?;
    stream.write_all(b"\n")?;
    stream.flush()
}

fn decode_request(line: &str) -> Result<Request, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(line)?;
    let command = value.get("command").and_then(serde_json::Value::as_str);
    let allowed: &[&str] = match command {
        Some("write") => &["command", "chunks_hex"],
        Some("read_exact") => &["command", "bytes"],
        Some("query") => &["command", "bytes_hex", "reply_bytes", "withhold"],
        Some("title") => &["command", "value"],
        Some("progress") => &["command", "state"],
        Some("clipboard") => &["command", "base64"],
        Some("agent") => &["command", "payload"],
        Some("spawn") => &["command", "mode", "exit_status"],
        Some("fill_stdout") => &["command", "bytes", "byte"],
        Some("exit") => &["command", "status"],
        Some("size" | "wait_descendant" | "bell" | "refuse_stdin" | "quit") => &["command"],
        _ => &[],
    };
    if let Some(object) = value.as_object()
        && object.keys().any(|key| !allowed.contains(&key.as_str()))
    {
        return Err(<serde_json::Error as serde::de::Error>::custom(
            "unknown control field",
        ));
    }
    serde_json::from_value(value)
}

fn bounded(bytes: usize) -> io::Result<()> {
    if bytes > MAX_PAYLOAD_BYTES {
        Err(invalid("payload exceeds bound"))
    } else {
        Ok(())
    }
}

fn read_exact_until(
    reader: &mut impl Read,
    output: &mut [u8],
    deadline: Instant,
) -> io::Result<()> {
    let original = rustix::fs::fcntl_getfl(io::stdin())?;
    rustix::fs::fcntl_setfl(io::stdin(), original | rustix::fs::OFlags::NONBLOCK)?;
    let result = (|| {
        let mut offset = 0;
        while offset < output.len() {
            match reader.read(&mut output[offset..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "PTY input closed",
                    ));
                }
                Ok(read) => offset += read,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => wait_until(deadline)?,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    })();
    let restore = rustix::fs::fcntl_setfl(io::stdin(), original).map_err(io::Error::from);
    result.and(restore)
}

fn wait_until(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "fixture deadline expired",
        ));
    }
    std::thread::sleep(Duration::from_millis(1));
    Ok(())
}

fn decode_hex(value: &str) -> io::Result<Vec<u8>> {
    if value.len() > MAX_PAYLOAD_BYTES * 2 || !value.len().is_multiple_of(2) {
        return Err(invalid("invalid hex length"));
    }
    value
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).map_err(invalid)?;
            u8::from_str_radix(text, 16).map_err(invalid)
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(value, "{byte:02x}");
    }
    value
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn argument(arguments: &[std::ffi::OsString], prefix: &str) -> Option<std::ffi::OsString> {
    arguments.iter().find_map(|argument| {
        let value = argument.to_str()?;
        value.strip_prefix(prefix).map(std::ffi::OsString::from)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_protocol_rejects_unknown_fields_and_commands() {
        assert!(decode_request(r#"{"command":"bell","extra":true}"#).is_err());
        assert!(decode_request(r#"{"command":"surprise"}"#).is_err());
    }

    #[test]
    fn exact_byte_codec_is_bounded_and_round_trips() {
        let bytes = b"\0\x1b]7877;state=idle\x1b\\\xff";
        assert_eq!(decode_hex(&hex(bytes)).expect("valid hex"), bytes);
        assert!(decode_hex("0").is_err());
        assert!(decode_hex(&"00".repeat(MAX_PAYLOAD_BYTES + 1)).is_err());
    }

    #[test]
    fn command_line_values_take_their_named_prefix() {
        let arguments = [
            "--control=/private/socket".into(),
            "--deadline-ms=20".into(),
        ];
        assert_eq!(
            argument(&arguments, "--control="),
            Some("/private/socket".into())
        );
        assert_eq!(argument(&arguments, "--missing="), None);
    }
}
