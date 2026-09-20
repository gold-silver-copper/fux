use crate::protocol::{Frame, Input};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, IsTerminal, Read, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use termina::{
    Event, PlatformTerminal, Terminal,
    event::{KeyCode, KeyEventKind, Modifiers, MouseButton, MouseEventKind},
};

const RESET: &[u8] = b"\x1b[?2026l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?7h\x1b[0m\x1b[?25h\x1b[?1049l";

pub fn rpc(endpoint: &str, method: &str, params: Option<Value>) -> Result<Value, String> {
    static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into()
    });
    let mut request = json!({"jsonrpc":"2.0","id":1,"method":method});
    if let Some(params) = params
        && let Some(object) = request.as_object_mut()
    {
        object.insert("params".into(), params);
    }
    let response: Value = AGENT
        .post(endpoint)
        .send_json(request)
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;
    result(response)
}
fn result(mut response: Value) -> Result<Value, String> {
    if let Some(error) = response.get("error") {
        return Err(error.to_string());
    }
    Ok(response
        .get_mut("result")
        .ok_or("missing RPC result")?
        .take())
}
fn trigger(endpoint: &str, event: &str, value: Value) -> Result<(), String> {
    rpc(
        endpoint,
        "world.trigger_event",
        Some(json!({"event":event,"value":value})),
    )
    .map(|_| ())
}
struct Screen(PlatformTerminal);
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = self.0.write_all(RESET);
        let _ = self.0.flush();
        let _ = self.0.enter_cooked_mode();
    }
}
enum Incoming {
    Input(Input),
    Paint,
    Resize,
    Escape(String),
    Stop,
    Error(String),
}

pub fn run(endpoint: &str, workspace: Option<&str>) -> Result<(), String> {
    let terminal = PlatformTerminal::new().map_err(|e| e.to_string())?;
    let size = terminal.get_dimensions().map_err(|e| e.to_string())?;
    let attached = rpc(
        endpoint,
        "fux.attach",
        Some(json!({"workspace":workspace,"rows":size.rows,"cols":size.cols})),
    )?;
    let viewer = attached
        .get("viewer")
        .and_then(Value::as_u64)
        .ok_or("attach did not return viewer")?;
    let mut screen = Screen(terminal);
    screen.0.set_panic_hook(|out| {
        let _ = out.write_all(RESET);
        let _ = out.flush();
    });
    screen.0.enter_raw_mode().map_err(|e| e.to_string())?;
    screen
        .0
        .write_all(b"\x1b[?1049h\x1b[?25l\x1b[?1003h\x1b[?1006h\x1b[?2004h")
        .map_err(|e| e.to_string())?;
    screen.0.flush().map_err(|e| e.to_string())?;
    // Termina still owns modes, dimensions and key/mouse decoding. This narrow
    // readiness adapter exposes bracketed-paste start before its buffered end.
    let input_file: std::fs::File = if std::io::stdin().is_terminal() {
        nix::unistd::dup(std::io::stdin())
            .map_err(|e| e.to_string())?
            .into()
    } else {
        std::fs::File::open("/dev/tty").map_err(|e| e.to_string())?
    };
    let (mut input_waker, input_stop) = UnixStream::pair().map_err(|e| e.to_string())?;
    let stopped = Arc::new(AtomicBool::new(false));
    let input_stopped = Arc::clone(&stopped);
    let (sender, receiver) = mpsc::sync_channel(64);
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
        signal_hook::consts::SIGWINCH,
    ])
    .map_err(|error| error.to_string())?;
    let signal_handle = signals.handle();
    let signal_sender = sender.clone();
    let signal_thread = thread::spawn(move || {
        for signal in signals.forever() {
            if signal == signal_hook::consts::SIGWINCH {
                if signal_sender.send(Incoming::Resize).is_err() {
                    break;
                }
            } else {
                let _ = signal_sender.send(Incoming::Stop);
                break;
            }
        }
    });
    let input_sender = sender.clone();
    let input_thread = thread::spawn(move || {
        let result = read_input(input_file, input_stop, &input_stopped, &input_sender);
        if let Err(error) = result {
            let _ = input_sender.send(Incoming::Error(error.to_string()));
        }
    });
    // Coalesce frames rather than making hot output queue stale paintings ahead
    // of keyboard input. The transport and terminal event buffers remain bounded.
    let latest = Arc::new(Mutex::new(None::<Frame>));
    let received = Arc::clone(&latest);
    let endpoint_owned = endpoint.to_owned();
    thread::spawn(move || {
        let stream = (|| -> Result<(), String> {
            let response=ureq::post(&endpoint_owned).send_json(json!({"jsonrpc":"2.0","id":2,"method":"fux.frame+watch","params":{"viewer":viewer}})).map_err(|e|e.to_string())?;
            let mut reader = BufReader::new(response.into_body().into_reader());
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                    return Err("server attachment closed".into());
                }
                if let Some(data) = line.strip_prefix("data:") {
                    let response: Value =
                        serde_json::from_str(data.trim()).map_err(|e| e.to_string())?;
                    let mut frame: Frame =
                        serde_json::from_value(result(response)?).map_err(|e| e.to_string())?;
                    // Clipboard writes are one-shot effects, not replaceable
                    // paintings. Preserve them when coalescing output frames.
                    while let Some(start) = frame.paint.find("\x1b]52;")
                        && let Some(end) =
                            frame.paint[start..].find('\x07').map(|end| start + end + 1)
                    {
                        let escape = frame.paint.drain(start..end).collect();
                        if sender.send(Incoming::Escape(escape)).is_err() {
                            return Ok(());
                        }
                    }
                    let detach = frame.detach;
                    let notify = received.lock().replace(frame).is_none();
                    if notify && sender.send(Incoming::Paint).is_err() {
                        return Ok(());
                    }
                    if detach {
                        return Ok(());
                    }
                }
            }
        })();
        if let Err(error) = stream {
            let _ = sender.send(Incoming::Error(error));
        }
    });
    let outcome = (|| -> Result<(), String> {
        loop {
            match receiver.recv().map_err(|e| e.to_string())? {
                Incoming::Input(input) => trigger(
                    endpoint,
                    "fux::control::UserInput",
                    json!({"viewer":viewer,"input":input}),
                )?,
                Incoming::Resize => {
                    let size = screen.0.get_dimensions().map_err(|e| e.to_string())?;
                    trigger(
                        endpoint,
                        "fux::control::UserInput",
                        json!({"viewer":viewer,"input":Input::Resize { rows:size.rows, cols:size.cols }}),
                    )?;
                }
                Incoming::Paint => {
                    let frame = latest.lock().take();
                    if let Some(frame) = frame {
                        if frame.detach {
                            return Ok(());
                        }
                        screen
                            .0
                            .write_all(frame.paint.as_bytes())
                            .and_then(|_| screen.0.flush())
                            .map_err(|e| e.to_string())?;
                    }
                }
                Incoming::Escape(escape) => screen
                    .0
                    .write_all(escape.as_bytes())
                    .and_then(|_| screen.0.flush())
                    .map_err(|e| e.to_string())?,
                Incoming::Stop => return Ok(()),
                Incoming::Error(error) => return Err(error),
            }
        }
    })();
    stopped.store(true, Ordering::Release);
    let _ = input_waker.write_all(&[1]);
    drop(receiver);
    signal_handle.close();
    let _ = signal_thread.join();
    let _ = input_thread.join();
    // Restore the user's terminal before a potentially slow final network call.
    drop(screen);
    let _ = trigger(
        endpoint,
        "fux::control::Control",
        json!({"viewer":viewer,"action":"detach","value":"","target":null}),
    );
    outcome
}
fn read_input(
    mut file: std::fs::File,
    stop: UnixStream,
    stopped: &AtomicBool,
    sender: &mpsc::SyncSender<Incoming>,
) -> std::io::Result<()> {
    use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
    let mut decoder = crate::paste::Decoder::default();
    let mut bytes = [0; 8192];
    while !stopped.load(Ordering::Acquire) {
        let mut fds = [
            PollFd::new(file.as_fd(), PollFlags::POLLIN),
            PollFd::new(stop.as_fd(), PollFlags::POLLIN),
        ];
        let timeout = if decoder.deadline_needed() {
            PollTimeout::from(35u16)
        } else {
            PollTimeout::NONE
        };
        let ready = match poll(&mut fds, timeout) {
            Ok(n) => n,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(error) => return Err(std::io::Error::from(error)),
        };
        if fds[1].revents().is_some_and(|flags| !flags.is_empty()) {
            break;
        }
        let mut closed = false;
        let mut emit = |input| {
            if sender.send(Incoming::Input(input)).is_err() {
                closed = true;
            }
        };
        if ready == 0 {
            decoder.timeout(&mut emit);
        } else {
            let n = file.read(&mut bytes)?;
            if n == 0 {
                let _ = sender.send(Incoming::Stop);
                break;
            }
            decoder.bytes(bytes.get(..n).unwrap_or_default(), &mut emit);
        }
        if closed {
            break;
        }
    }
    Ok(())
}

pub(crate) fn convert(event: Event) -> Option<Input> {
    match event {
        Event::WindowResized(size) => Some(Input::Resize {
            rows: size.rows,
            cols: size.cols,
        }),
        Event::Paste(text) => Some(Input::Paste { text }),
        Event::Key(event) if event.kind != KeyEventKind::Release => {
            let key = match event.code {
                KeyCode::Char(c) => c.to_string(),
                KeyCode::Enter => "enter".into(),
                KeyCode::Tab | KeyCode::BackTab => "tab".into(),
                KeyCode::Escape => "escape".into(),
                KeyCode::Backspace => "backspace".into(),
                KeyCode::Left => "left".into(),
                KeyCode::Right => "right".into(),
                KeyCode::Up => "up".into(),
                KeyCode::Down => "down".into(),
                KeyCode::Home => "home".into(),
                KeyCode::End => "end".into(),
                KeyCode::PageUp => "pageup".into(),
                KeyCode::PageDown => "pagedown".into(),
                KeyCode::Insert => "insert".into(),
                KeyCode::Delete => "delete".into(),
                KeyCode::Function(n) => format!("f{n}"),
                _ => return None,
            };
            Some(Input::Key {
                key,
                ctrl: event.modifiers.contains(Modifiers::CONTROL),
                alt: event.modifiers.contains(Modifiers::ALT),
                shift: event.modifiers.contains(Modifiers::SHIFT) || event.code == KeyCode::BackTab,
            })
        }
        Event::Mouse(event) => {
            let (action, button) = match event.kind {
                MouseEventKind::Down(button) => ("press", Some(button)),
                MouseEventKind::Up(button) => ("release", Some(button)),
                MouseEventKind::Drag(button) => ("move", Some(button)),
                MouseEventKind::Moved => ("move", None),
                MouseEventKind::ScrollUp => ("scrollup", None),
                MouseEventKind::ScrollDown => ("scrolldown", None),
                _ => return None,
            };
            Some(Input::Mouse {
                action: action.into(),
                button: match button {
                    Some(MouseButton::Left) => 0,
                    Some(MouseButton::Middle) => 1,
                    Some(MouseButton::Right) => 2,
                    None => 3,
                },
                x: event.column,
                y: event.row,
                ctrl: event.modifiers.contains(Modifiers::CONTROL),
                alt: event.modifiers.contains(Modifiers::ALT),
                shift: event.modifiers.contains(Modifiers::SHIFT),
            })
        }
        _ => None,
    }
}
