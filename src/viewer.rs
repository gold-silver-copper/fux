use crate::protocol::{Frame, Input};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, IsTerminal, Read, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use termina::{
    Event, PlatformTerminal, Terminal,
    event::{KeyCode, KeyEventKind, MouseButton, MouseEventKind},
};

const RESET: &[u8] = b"\x1b[?2026l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?7h\x1b[0m\x1b[?25h\x1b[?1049l";

pub fn rpc(socket: &Path, method: &str, params: Option<Value>) -> Result<Value, String> {
    // One pooled agent per socket, reused for every call of this process.
    static AGENT: OnceLock<(PathBuf, ureq::Agent)> = OnceLock::new();
    let (path, agent) = AGENT.get_or_init(|| {
        (
            socket.to_owned(),
            crate::unix_http::agent(socket, Some(Duration::from_secs(10))),
        )
    });
    let fresh;
    let agent = if path == socket {
        agent
    } else {
        fresh = crate::unix_http::agent(socket, Some(Duration::from_secs(10)));
        &fresh
    };
    let mut request = json!({"jsonrpc":"2.0","id":1,"method":method});
    if let Some(params) = params
        && let Some(object) = request.as_object_mut()
    {
        object.insert("params".into(), params);
    }
    let response: Value = agent
        .post(crate::unix_http::URL)
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
fn trigger(socket: &Path, event: &str, value: Value) -> Result<(), String> {
    rpc(
        socket,
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

pub fn run(socket: &Path, workspace: Option<&str>) -> Result<(), String> {
    let terminal = PlatformTerminal::new().map_err(|e| e.to_string())?;
    let size = terminal.get_dimensions().map_err(|e| e.to_string())?;
    let attached = rpc(
        socket,
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
    // The watch has no deadline: it lasts as long as the attachment.
    let watch = crate::unix_http::agent(socket, None);
    thread::spawn(move || {
        let stream = (|| -> Result<(), String> {
            let response = watch
                .post(crate::unix_http::URL)
                .send_json(json!({"jsonrpc":"2.0","id":2,"method":"fux.frame+watch","params":{"viewer":viewer}}))
                .map_err(|e| e.to_string())?;
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
                    socket,
                    "fux::control::UserInput",
                    json!({"viewer":viewer,"input":input}),
                )?,
                Incoming::Resize => {
                    let size = screen.0.get_dimensions().map_err(|e| e.to_string())?;
                    trigger(
                        socket,
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
        socket,
        "fux::control::Control",
        json!({"viewer":viewer,"command":{"kind":"detach"}}),
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
    use crate::protocol::{Direction, Key, Modifiers, MouseAction};
    let modifiers = |m: termina::event::Modifiers, shifted: bool| Modifiers {
        ctrl: m.contains(termina::event::Modifiers::CONTROL),
        alt: m.contains(termina::event::Modifiers::ALT),
        shift: m.contains(termina::event::Modifiers::SHIFT) || shifted,
    };
    match event {
        Event::WindowResized(size) => Some(Input::Resize {
            rows: size.rows,
            cols: size.cols,
        }),
        Event::Paste(text) => Some(Input::Paste { text }),
        Event::Key(event) if event.kind != KeyEventKind::Release => {
            let key = match event.code {
                KeyCode::Char(c) => Key::Char(c),
                KeyCode::Enter => Key::Enter,
                KeyCode::Tab | KeyCode::BackTab => Key::Tab,
                KeyCode::Escape => Key::Escape,
                KeyCode::Backspace => Key::Backspace,
                KeyCode::Left => Key::Arrow(Direction::Left),
                KeyCode::Right => Key::Arrow(Direction::Right),
                KeyCode::Up => Key::Arrow(Direction::Up),
                KeyCode::Down => Key::Arrow(Direction::Down),
                KeyCode::Home => Key::Home,
                KeyCode::End => Key::End,
                KeyCode::PageUp => Key::PageUp,
                KeyCode::PageDown => Key::PageDown,
                KeyCode::Insert => Key::Insert,
                KeyCode::Delete => Key::Delete,
                KeyCode::Function(n @ 1..=12) => Key::F(n),
                _ => return None,
            };
            Some(Input::Key {
                key,
                modifiers: modifiers(event.modifiers, event.code == KeyCode::BackTab),
            })
        }
        Event::Mouse(event) => {
            let (action, button) = match event.kind {
                MouseEventKind::Down(button) => (MouseAction::Press, Some(button)),
                MouseEventKind::Up(button) => (MouseAction::Release, Some(button)),
                MouseEventKind::Drag(button) => (MouseAction::Move, Some(button)),
                MouseEventKind::Moved => (MouseAction::Move, None),
                MouseEventKind::ScrollUp => (MouseAction::ScrollUp, None),
                MouseEventKind::ScrollDown => (MouseAction::ScrollDown, None),
                _ => return None,
            };
            Some(Input::Mouse {
                action,
                button: match button {
                    Some(MouseButton::Left) => crate::protocol::MouseButton::Left,
                    Some(MouseButton::Middle) => crate::protocol::MouseButton::Middle,
                    Some(MouseButton::Right) => crate::protocol::MouseButton::Right,
                    _ => crate::protocol::MouseButton::None,
                },
                x: event.column,
                y: event.row,
                modifiers: modifiers(event.modifiers, false),
            })
        }
        _ => None,
    }
}
