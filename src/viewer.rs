use crate::protocol::{Frame, Input};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
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
    if let Some(params) = params {
        request["params"] = params;
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
    let reader = screen.0.event_reader();
    let input_waker = reader.waker();
    let stopped = Arc::new(AtomicBool::new(false));
    let input_stopped = Arc::clone(&stopped);
    let (sender, receiver) = mpsc::sync_channel(64);
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ])
    .map_err(|error| error.to_string())?;
    let signal_handle = signals.handle();
    let signal_sender = sender.clone();
    let signal_thread = thread::spawn(move || {
        if signals.forever().next().is_some() {
            let _ = signal_sender.send(Incoming::Stop);
        }
    });
    let input_sender = sender.clone();
    let input_thread = thread::spawn(move || {
        while !input_stopped.load(Ordering::Acquire) {
            match reader.poll(None, |_| true).and_then(|ready| {
                if ready {
                    reader.read(|_| true).map(Some)
                } else {
                    Ok(None)
                }
            }) {
                Ok(Some(event)) => {
                    if let Some(input) = convert(event)
                        && input_sender.send(Incoming::Input(input)).is_err()
                    {
                        break;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = input_sender.send(Incoming::Error(error.to_string()));
                    break;
                }
            }
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
    let _ = input_waker.wake();
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
fn convert(event: Event) -> Option<Input> {
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
