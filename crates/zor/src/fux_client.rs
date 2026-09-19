//! zor as a BRP client of fux over loopback HTTP (prompt 4.2): fux's `brp.json` for port and
//! token (`FUX_BRP` or `Paths::fux_descriptor`), typed `fux/*` wrappers over fux's own param
//! and result shapes, an `events+watch` consumer on `IoTaskPool` that streams
//! `fux/events+watch` with a cursor into `Inbound::FuxEvent`, resuming after gaps and
//! reconnecting with backoff, and the service-ownership helpers every destructive call goes
//! through (`ensure_pane_identity`: instance/workspace/pane/pid before acting;
//! service-ownership-contract.md:22, TASKS.md:71-73). Transport reconnection never authorises
//! replay: a reconnect only re-reads the descriptor and resumes the cursor.

use core::time::Duration;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use async_channel::Sender;
use async_io::{Async, Timer};
use bevy_ecs::error::BevyError;
use bevy_tasks::{IoTaskPool, Task};
use bevy_tasks::futures_lite::future;
use fux::remote::client::{
    self, ClientError, Descriptor, encode_request, parse_response, read_descriptor, unwrap_reply,
};
use fux::remote::input_methods::{InputReceipt, PaneFinal};
use fux::remote::methods::{
    Capture, PaneCreated, PaneEntry, PaneNewParams, ServerInfo, WorkspaceCreated, WorkspaceList,
};
use fux::remote::scene_methods::SceneApplied;
use fux::remote::surface_methods::{SurfaceClosed, SurfaceOpened, SurfaceUpdated};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::model::{Effect, Inbound, PaneHandle};
use crate::runner::Adapter;

pub const EVENTS_METHOD: &str = "fux/events+watch";
/// Reconnect backoff: doubles from the first to the last value.
pub const BACKOFF_MIN: Duration = Duration::from_millis(250);
pub const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// Bound on a streamed frame before the parser gives up on the connection.
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Locates fux's descriptor: `FUX_BRP` when set, else `<fux runtime>/<server>.brp.json`.
pub fn descriptor_path(paths: &crate::paths::Paths, server: &str) -> PathBuf {
    paths.fux_descriptor(server)
}

// ---------------------------------------------------------------------------------------------
// Typed request/reply client
// ---------------------------------------------------------------------------------------------

/// A synchronous client over one descriptor (re-read on demand).
#[derive(Clone, Debug)]
pub struct FuxClient {
    pub brp: PathBuf,
    pub descriptor: Descriptor,
}

impl FuxClient {
    pub fn open(brp: &Path) -> Result<Self, ClientError> {
        Ok(Self {
            brp: brp.to_path_buf(),
            descriptor: read_descriptor(brp)?,
        })
    }

    /// Re-reads the descriptor (fux restarted: new port, token, instance).
    pub fn refresh(&mut self) -> Result<(), ClientError> {
        self.descriptor = read_descriptor(&self.brp)?;
        Ok(())
    }

    pub fn instance(&self) -> &str {
        &self.descriptor.instance
    }

    pub fn call<R: DeserializeOwned>(&self, method: &str, params: Value) -> Result<R, ClientError> {
        let reply = client::call_with(&self.descriptor, method, params)?;
        serde_json::from_value(reply).map_err(|e| ClientError::Malformed(e.to_string()))
    }

    pub fn server_info(&self) -> Result<ServerInfo, ClientError> {
        self.call("fux/server.info", json!({}))
    }

    pub fn workspace_list(&self) -> Result<WorkspaceList, ClientError> {
        self.call("fux/workspace.list", json!({}))
    }

    pub fn workspace_new(
        &self,
        name: &str,
        template: Option<Value>,
    ) -> Result<WorkspaceCreated, ClientError> {
        self.call(
            "fux/workspace.new",
            json!({ "name": name, "template": template }),
        )
    }

    pub fn workspace_kill(&self, name: &str) -> Result<(), ClientError> {
        self.call::<Value>("fux/workspace.kill", json!({ "name": name }))
            .map(|_| ())
    }

    pub fn pane_new(&self, params: &PaneNewParams) -> Result<PaneCreated, ClientError> {
        let params =
            serde_json::to_value(params).map_err(|e| ClientError::Malformed(e.to_string()))?;
        self.call("fux/pane.new", params)
    }

    /// Destructive: the caller validated the pane's identity first (`ensure_pane_identity`).
    pub fn pane_close(&self, pane: u64) -> Result<(), ClientError> {
        self.call::<Value>("fux/pane.close", json!({ "pane": pane }))
            .map(|_| ())
    }

    /// Destructive: the caller validated the pane's identity first.
    pub fn pane_send_keys(&self, pane: u64, keys: &str) -> Result<usize, ClientError> {
        let written: Value = self.call(
            "fux/pane.send_keys",
            json!({ "pane": pane, "keys": keys, "notation": "escapes" }),
        )?;
        Ok(written
            .get("bytes")
            .and_then(Value::as_u64)
            .and_then(|b| usize::try_from(b).ok())
            .unwrap_or(0))
    }

    pub fn pane_capture(&self, pane: u64, scrollback: usize) -> Result<Capture, ClientError> {
        self.call(
            "fux/pane.capture",
            json!({ "pane": pane, "scrollback": scrollback }),
        )
    }

    pub fn pane_final(&self, pane: u64) -> Result<PaneFinal, ClientError> {
        self.call("fux/pane.final", json!({ "pane": pane }))
    }

    pub fn input_reserve(&self, pane: u64, retain_ms: u64) -> Result<InputReceipt, ClientError> {
        self.call(
            "fux/input.reserve",
            json!({ "pane": pane, "retain_ms": retain_ms }),
        )
    }

    /// Destructive: the caller validated the pane's identity first.
    pub fn input_submit(&self, operation: u64, keys: &str) -> Result<InputReceipt, ClientError> {
        self.call(
            "fux/input.submit",
            json!({ "operation": operation, "keys": keys }),
        )
    }

    pub fn input_status(&self, operation: u64) -> Result<InputReceipt, ClientError> {
        self.call("fux/input.status", json!({ "operation": operation }))
    }

    pub fn scene_restore(
        &self,
        workspace: &str,
        name: &str,
        expected: Option<Value>,
    ) -> Result<SceneApplied, ClientError> {
        self.call(
            "fux/scene.restore",
            json!({ "workspace": workspace, "name": name, "expected": expected }),
        )
    }

    pub fn surface_open(
        &self,
        workspace: &str,
        node: u64,
        provider: &str,
        generation: u64,
    ) -> Result<SurfaceOpened, ClientError> {
        self.call(
            "fux/surface.open",
            json!({ "workspace": workspace, "node": node, "provider": provider, "generation": generation }),
        )
    }

    pub fn surface_update(
        &self,
        surface: u64,
        expected_provider: &str,
        revision: u64,
        full: bool,
        delta: &str,
    ) -> Result<SurfaceUpdated, ClientError> {
        self.call(
            "fux/surface.update",
            json!({ "surface": surface, "expected_provider": expected_provider, "revision": revision, "full": full, "delta": delta }),
        )
    }

    pub fn surface_close(&self, surface: u64) -> Result<SurfaceClosed, ClientError> {
        self.call("fux/surface.close", json!({ "surface": surface }))
    }

    /// The listed pane in `workspace`, if fux has it.
    pub fn find_pane(&self, workspace: &str, pane: u64) -> Result<Option<PaneEntry>, ClientError> {
        let list = self.workspace_list()?;
        Ok(list
            .workspaces
            .into_iter()
            .filter(|w| w.name == workspace)
            .flat_map(|w| w.roots)
            .flat_map(|r| r.panes)
            .find(|p| p.id == pane))
    }

    /// The service-ownership rule (service-ownership-contract.md:19-24): before any destructive
    /// operation the fux instance nonce, the workspace route, the pane id and the originally
    /// observed pid must all match the retained handle. A mismatch is a refusal, never a
    /// retarget.
    pub fn ensure_pane_identity(&self, handle: &PaneHandle) -> Result<PaneEntry, IdentityError> {
        ensure_instance(self.instance(), handle)?;
        let entry = self
            .find_pane(&handle.workspace, handle.pane)
            .map_err(IdentityError::Transport)?
            .ok_or(IdentityError::PaneMissing {
                workspace: handle.workspace.clone(),
                pane: handle.pane,
            })?;
        ensure_pid(&entry, handle)?;
        Ok(entry)
    }
}

/// Why a destructive operation was refused.
#[derive(Debug)]
pub enum IdentityError {
    /// fux is another incarnation than the one the handle was recorded against.
    Instance {
        expected: String,
        actual: String,
    },
    PaneMissing {
        workspace: String,
        pane: u64,
    },
    /// The pane exists but its root process is not the one observed at launch.
    Pid {
        expected: Option<u32>,
        actual: Option<u32>,
    },
    Transport(ClientError),
}

impl core::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Instance { expected, actual } => {
                write!(f, "fux instance {actual} is not the recorded {expected}")
            }
            Self::PaneMissing { workspace, pane } => {
                write!(f, "pane {pane} is not in workspace {workspace}")
            }
            Self::Pid { expected, actual } => {
                write!(f, "pane pid {actual:?} is not the recorded {expected:?}")
            }
            Self::Transport(e) => write!(f, "fux unreachable: {e}"),
        }
    }
}

impl core::error::Error for IdentityError {}

pub fn ensure_instance(actual: &str, handle: &PaneHandle) -> Result<(), IdentityError> {
    if actual == handle.instance {
        Ok(())
    } else {
        Err(IdentityError::Instance {
            expected: handle.instance.clone(),
            actual: actual.to_owned(),
        })
    }
}

/// A recorded pid must match the listed one; a handle without an observed pid (TASKS.md:135)
/// accepts a pane whose pid is still unknown, never a different one.
pub fn ensure_pid(entry: &PaneEntry, handle: &PaneHandle) -> Result<(), IdentityError> {
    match (handle.pid, entry.pid) {
        (Some(expected), Some(actual)) if expected == actual => Ok(()),
        (None, _) => Ok(()),
        (expected, actual) => Err(IdentityError::Pid { expected, actual }),
    }
}

// ---------------------------------------------------------------------------------------------
// Effect adapter: `Effect::FuxCall` → `Inbound::FuxReply`
// ---------------------------------------------------------------------------------------------

/// Routes `Effect::FuxCall` to fux on `IoTaskPool` and answers with `Inbound::FuxReply`.
pub struct FuxAdapter {
    brp: PathBuf,
    inbound: Sender<Inbound>,
    calls: Vec<Task<()>>,
}

impl FuxAdapter {
    pub fn new(brp: PathBuf, inbound: Sender<Inbound>) -> Self {
        Self { brp, inbound, calls: Vec::new() }
    }
}

impl Adapter for FuxAdapter {
    fn handles(&self, effect: &Effect) -> bool {
        matches!(effect, Effect::FuxCall { .. })
    }

    fn pending(&self) -> bool {
        self.calls.iter().any(|call| !call.is_finished())
    }

    fn apply(&mut self, effect: Effect) -> Result<(), BevyError> {
        let Effect::FuxCall {
            call,
            method,
            params,
        } = effect
        else {
            return Err(BevyError::from("fux adapter: not a FuxCall"));
        };
        self.calls.retain(|call| !call.is_finished());
        if self.calls.len() >= 256 {
            self.inbound.try_send(Inbound::FuxReply {
                call, result: Err("fux call capacity reached before dispatch".into()),
            }).map_err(|error| BevyError::from(error.to_string()))?;
            return Ok(());
        }
        let brp = self.brp.clone();
        let inbound = self.inbound.clone();
        self.calls.push(IoTaskPool::get()
            .spawn(async move {
                let result = match read_descriptor(&brp) {
                    Ok(descriptor) => call_async(&descriptor, &method, params)
                        .await
                        .map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                };
                let _ = inbound.send(Inbound::FuxReply { call, result }).await;
            }));
        Ok(())
    }
}

/// One JSON-RPC call over `async_io` with the descriptor's envelope injected.
pub async fn call_async(
    descriptor: &Descriptor,
    method: &str,
    params: Value,
) -> Result<Value, ClientError> {
    future::race(
        call_inner(descriptor, method, params),
        async {
            Timer::after(client::TIMEOUT).await;
            Err(ClientError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "BRP call deadline elapsed; reconcile mutation outcome before retrying",
            )))
        },
    )
    .await
}

async fn call_inner(
    descriptor: &Descriptor,
    method: &str,
    params: Value,
) -> Result<Value, ClientError> {
    let Value::Object(mut fields) = params else {
        return Err(ClientError::Params);
    };
    if let Some(expected) = fields.remove("_expected_instance") {
        if expected.as_str() != Some(descriptor.instance.as_str()) {
            return Err(ClientError::Malformed(
                "instance mismatch: fux descriptor was replaced before dispatch".into(),
            ));
        }
    }
    if fields.get("instance").is_some_and(|expected| {
        expected.as_str() != Some(descriptor.instance.as_str())
    }) {
        return Err(ClientError::Malformed(
            "instance mismatch: refusing to replace caller authority with a new descriptor".into(),
        ));
    }
    fields.insert("token".into(), Value::String(descriptor.token.clone()));
    fields.insert(
        "instance".into(),
        Value::String(descriptor.instance.clone()),
    );
    let body = encode_request(method, Value::Object(fields))?;
    let stream = connect(descriptor).await?;
    let header = format!(
        "POST / HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        descriptor.http.host,
        descriptor.http.port,
        body.len()
    );
    write_all(&stream, header.as_bytes()).await?;
    write_all(&stream, &body).await?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = read_some(&stream, &mut buf).await?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(buf.get(..n).unwrap_or_default());
        if raw.len() > client::MAX_REPLY_BYTES {
            return Err(ClientError::Malformed("reply too large".into()));
        }
    }
    unwrap_reply(parse_response(&raw)?)
}

async fn connect(descriptor: &Descriptor) -> Result<Async<TcpStream>, ClientError> {
    use std::net::ToSocketAddrs;
    let address = (descriptor.http.host.as_str(), descriptor.http.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::other("unresolvable host"))?;
    let connect = Async::<TcpStream>::connect(address);
    let timeout = async {
        Timer::after(client::TIMEOUT).await;
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "connect timed out",
        ))
    };
    Ok(future::or(connect, timeout).await?)
}

async fn write_all(stream: &Async<TcpStream>, mut bytes: &[u8]) -> Result<(), ClientError> {
    while !bytes.is_empty() {
        let n = stream.write_with(|mut s| s.write(bytes)).await?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "write zero").into());
        }
        bytes = bytes.get(n..).unwrap_or_default();
    }
    Ok(())
}

async fn read_some(stream: &Async<TcpStream>, buf: &mut [u8]) -> Result<usize, ClientError> {
    Ok(stream.read_with(|mut s| s.read(buf)).await?)
}

// ---------------------------------------------------------------------------------------------
// Events consumer
// ---------------------------------------------------------------------------------------------

/// Cursor shared with the consumer task, for tests and `zor/server.info`.
#[derive(Debug, Default)]
pub struct EventsCursor(AtomicU64);

impl EventsCursor {
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Spawns the consumer on `IoTaskPool`: streams `fux/events+watch` from `cursor` (0: from
/// now), forwards items as `Inbound::FuxEvent`/`FuxGap`, reports connection changes as
/// `Inbound::FuxLink`, re-reads the descriptor and reconnects with backoff until `inbound`
/// closes.
pub fn spawn_events_consumer(
    brp: PathBuf,
    cursor: u64,
    inbound: Sender<Inbound>,
) -> std::sync::Arc<EventsCursor> {
    let shared = std::sync::Arc::new(EventsCursor(AtomicU64::new(cursor)));
    let cursor = std::sync::Arc::clone(&shared);
    IoTaskPool::get()
        .spawn(async move {
            let mut backoff = BACKOFF_MIN;
            while !inbound.is_closed() {
                let Ok(descriptor) = read_descriptor(&brp) else {
                    Timer::after(backoff).await;
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                    continue;
                };
                match watch_once(&descriptor, &cursor, &inbound).await {
                    Ok(()) => backoff = BACKOFF_MIN,
                    Err(_) => {
                        let _ = inbound.send(Inbound::FuxLink { instance: None }).await;
                        Timer::after(backoff).await;
                        backoff = (backoff * 2).min(BACKOFF_MAX);
                    }
                }
            }
        })
        .detach();
    shared
}

/// One connection's worth of the stream; returns `Ok` when the server closed it cleanly.
async fn watch_once(
    descriptor: &Descriptor,
    cursor: &EventsCursor,
    inbound: &Sender<Inbound>,
) -> Result<(), ClientError> {
    let since = cursor.get();
    let mut params = json!({
        "token": descriptor.token,
        "instance": descriptor.instance,
    });
    if since > 0
        && let Value::Object(fields) = &mut params
    {
        fields.insert("cursor".into(), json!(since));
    }
    let body = encode_request(EVENTS_METHOD, params)?;
    let stream = connect(descriptor).await?;
    let header = format!(
        "POST / HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        descriptor.http.host,
        descriptor.http.port,
        body.len()
    );
    write_all(&stream, header.as_bytes()).await?;
    write_all(&stream, &body).await?;

    let mut frames = Frames::default();
    let mut buf = [0u8; 8192];
    let mut connected = false;
    loop {
        if inbound.is_closed() {
            return Ok(());
        }
        let n = read_some(&stream, &mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        frames.push(buf.get(..n).unwrap_or_default())?;
        if !connected && frames.headers_done {
            connected = true;
            let _ = inbound
                .send(Inbound::FuxLink {
                    instance: Some(descriptor.instance.clone()),
                })
                .await;
        }
        while let Some(item) = frames.next_item()? {
            let item = unwrap_reply(item)?;
            if let Some(gap) = item.get("gap").filter(|g| !g.is_null()) {
                let since = gap.get("since").and_then(Value::as_u64).unwrap_or(0);
                let resume = gap.get("resume").and_then(Value::as_u64).unwrap_or(0);
                cursor.0.store(resume, Ordering::Relaxed);
                if inbound
                    .send(Inbound::FuxGap { since, resume })
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
            for event in item
                .get("events")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let at = event.get("cursor").and_then(Value::as_u64).unwrap_or(0);
                let name = event
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let body = event.get("event").cloned().unwrap_or(Value::Null);
                cursor.0.store(at, Ordering::Relaxed);
                if inbound
                    .send(Inbound::FuxEvent {
                        cursor: at,
                        name,
                        body,
                    })
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
        }
    }
}

/// Incremental HTTP/1.1 + SSE parser: headers, then chunked or raw body, then `data:` lines.
#[derive(Default)]
struct Frames {
    buffer: Vec<u8>,
    headers_done: bool,
    chunked: bool,
    /// Decoded body bytes not yet split into records.
    body: Vec<u8>,
}

impl Frames {
    fn push(&mut self, bytes: &[u8]) -> Result<(), ClientError> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > MAX_FRAME_BYTES {
            return Err(ClientError::Malformed("stream frame too large".into()));
        }
        if !self.headers_done {
            let Some(end) = self.buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
                return Ok(());
            };
            let head =
                String::from_utf8_lossy(self.buffer.get(..end).unwrap_or_default()).into_owned();
            let mut lines = head.split("\r\n");
            let status: u16 = lines
                .next()
                .and_then(|l| l.split(' ').nth(1))
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| ClientError::Malformed("status line".into()))?;
            for (name, value) in lines.filter_map(|l| l.split_once(':')) {
                if name.eq_ignore_ascii_case("transfer-encoding") {
                    self.chunked = value.trim().eq_ignore_ascii_case("chunked");
                }
            }
            self.buffer.drain(..end + 4);
            self.headers_done = true;
            if status != 200 {
                return Err(ClientError::Http {
                    status,
                    body: String::from_utf8_lossy(&self.buffer).into_owned(),
                });
            }
        }
        self.decode()
    }

    /// Moves complete chunks (or everything, unchunked) from `buffer` into `body`.
    fn decode(&mut self) -> Result<(), ClientError> {
        if !self.chunked {
            self.body.append(&mut self.buffer);
            return Ok(());
        }
        loop {
            let Some(line_end) = self.buffer.windows(2).position(|w| w == b"\r\n") else {
                return Ok(());
            };
            let size_line =
                String::from_utf8_lossy(self.buffer.get(..line_end).unwrap_or_default())
                    .into_owned();
            let size_hex = size_line.split(';').next().unwrap_or_default().trim();
            let size = usize::from_str_radix(size_hex, 16)
                .map_err(|_| ClientError::Malformed("bad chunked encoding".into()))?;
            let needed = line_end + 2 + size + 2;
            if self.buffer.len() < needed {
                return Ok(());
            }
            if size > 0 {
                self.body.extend_from_slice(
                    self.buffer
                        .get(line_end + 2..line_end + 2 + size)
                        .unwrap_or_default(),
                );
            }
            self.buffer.drain(..needed);
        }
    }

    /// The next complete `data:` record as JSON, if one is buffered.
    fn next_item(&mut self) -> Result<Option<Value>, ClientError> {
        loop {
            let Some(end) = self.body.iter().position(|b| *b == b'\n') else {
                return Ok(None);
            };
            let line: Vec<u8> = self.body.drain(..=end).collect();
            let Some(data) = line.strip_prefix(b"data:") else {
                continue;
            };
            let item: Value = serde_json::from_slice(data.trim_ascii())
                .map_err(|e| ClientError::Malformed(e.to_string()))?;
            return Ok(Some(item));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_split_chunked_sse_records() {
        let mut frames = Frames::default();
        frames
            .push(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .unwrap();
        assert!(frames.headers_done && frames.chunked);
        let record = b"data: {\"result\":{\"events\":[]}}\n\n";
        let chunk = format!("{:x}\r\n", record.len());
        frames.push(chunk.as_bytes()).unwrap();
        assert!(frames.next_item().unwrap().is_none());
        frames.push(record.get(..5).unwrap()).unwrap();
        assert!(frames.next_item().unwrap().is_none());
        frames.push(record.get(5..).unwrap()).unwrap();
        frames.push(b"\r\n").unwrap();
        let item = frames.next_item().unwrap().unwrap();
        assert_eq!(item["result"]["events"], json!([]));
        assert!(frames.next_item().unwrap().is_none());
    }

    #[test]
    fn pid_validation_refuses_a_different_process() {
        let entry = PaneEntry {
            id: 1,
            node: None,
            state: "live".into(),
            pid: Some(42),
            exit_code: None,
            title: String::new(),
            rows: 24,
            cols: 80,
            seq: 0,
            argv: Vec::new(),
            cwd: None,
        };
        let mut handle = PaneHandle {
            pid: Some(42),
            ..PaneHandle::default()
        };
        assert!(ensure_pid(&entry, &handle).is_ok());
        handle.pid = Some(43);
        assert!(matches!(
            ensure_pid(&entry, &handle),
            Err(IdentityError::Pid { .. })
        ));
        handle.pid = None;
        assert!(ensure_pid(&entry, &handle).is_ok());
    }
}
