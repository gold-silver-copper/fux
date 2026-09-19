//! Real owner mutations across the controller's intent/dispatch and reply-loss boundaries.
//! The proxy forwards authentic HTTP requests; it never manufactures a product reply.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::brp::Descriptor;
use super::stack::Stack;
use super::{Binaries, Fixture, Outcome, Result, WAIT, err, nonce, require_target, until};

const IO: Duration = Duration::from_secs(8);
const GATE: Duration = Duration::from_secs(25);
const BOUND: usize = 2 * 1024 * 1024;
const CONNECTIONS: usize = 16;

fn save(path: &Path, value: &Value) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum Boundary {
    BeforeForward,
    AfterReply,
}
struct Gate {
    method: String,
    selector: Option<(String, Value)>,
    boundary: Boundary,
    reached: bool,
    released: bool,
    finished: bool,
}
#[derive(Default)]
struct State {
    gate: Option<Gate>,
    requests: usize,
    forwarded: usize,
    completed: usize,
    methods: BTreeMap<String, usize>,
    streams: usize,
    stream_bytes: usize,
    offline: bool,
    errors: Vec<String>,
}

/// Finite calls have a whole-exchange deadline. Event streams are relayed incrementally
/// with bounded buffers and interruptible reads; Drop joins every owned worker.
struct Proxy {
    port: u16,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Proxy {
    fn start(owner: &Descriptor, evidence: &Path) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let address: SocketAddr = format!("{}:{}", owner.host, owner.port).parse()?;
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let shared = state.clone();
        let shutdown = stop.clone();
        let evidence = evidence.to_owned();
        let thread = std::thread::spawn(move || {
            let mut workers: Vec<JoinHandle<()>> = Vec::new();
            while !shutdown.load(Ordering::Acquire) {
                let mut i = 0;
                while i < workers.len() {
                    if workers[i].is_finished() {
                        let _ = workers.swap_remove(i).join();
                    } else {
                        i += 1;
                    }
                }
                if workers.len() >= CONNECTIONS {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                match listener.accept() {
                    Ok((client, _)) => {
                        let state = shared.clone();
                        let stop = shutdown.clone();
                        let evidence = evidence.clone();
                        workers.push(std::thread::spawn(move || {
                            if let Err(error) = exchange(client, address, &state, &stop, &evidence)
                            {
                                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                                if state.errors.len() < 32 {
                                    state.errors.push(error.to_string());
                                }
                            }
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => {
                        shared
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .errors
                            .push(error.to_string());
                        break;
                    }
                }
            }
            for worker in workers {
                let _ = worker.join();
            }
        });
        Ok(Self {
            port,
            state,
            stop,
            thread: Some(thread),
        })
    }
    fn arm(&self, task: &str, boundary: Boundary) {
        self.arm_method("zor/task.stop", Some(("task", json!(task))), boundary);
    }
    fn arm_method(&self, method: &str, selector: Option<(&str, Value)>, boundary: Boundary) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).gate = Some(Gate {
            method: method.into(),
            selector: selector.map(|(key, value)| (key.into(), value)),
            boundary,
            reached: false,
            released: false,
            finished: false,
        });
    }
    fn offline(&self, offline: bool) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).offline = offline;
    }
    fn reached(&self) -> Result<()> {
        until(WAIT, "fault proxy boundary", || {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.errors.is_empty() {
                return Err(err(format!("proxy: {:?}", state.errors)));
            }
            Ok(state
                .gate
                .as_ref()
                .is_some_and(|gate| gate.reached)
                .then_some(()))
        })
    }
    fn release(&self) -> Result<()> {
        if let Some(gate) = &mut self.state.lock().unwrap_or_else(|e| e.into_inner()).gate {
            gate.released = true;
        }
        until(IO, "fault proxy gate cleanup", || {
            Ok(self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .gate
                .as_ref()
                .is_some_and(|gate| gate.finished)
                .then_some(()))
        })
    }
    fn counts(&self) -> Result<Value> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.errors.is_empty() {
            return Err(err(format!("proxy: {:?}", state.errors)));
        }
        Ok(
            json!({"mutation_requests":state.requests,"forwarded_mutations":state.forwarded,"accepted_replies":state.completed,
            "methods":state.methods,"event_streams":state.streams,"event_stream_bytes":state.stream_bytes}),
        )
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn timeout(start: Instant) -> Result<Duration> {
    IO.checked_sub(start.elapsed())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| err("proxy HTTP deadline"))
}

/// BRP JSON replies use Content-Length or chunked framing; no EOF/keepalive assumption.
fn frame(stream: &mut TcpStream, start: Instant) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut raw = Vec::new();
    loop {
        if let Some(split) = raw.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let header = std::str::from_utf8(&raw[..split])?;
            let body = &raw[split + 4..];
            let mut length = None;
            let mut chunked = false;
            for (key, value) in header.split("\r\n").filter_map(|line| line.split_once(':')) {
                if key.eq_ignore_ascii_case("content-length") {
                    length = Some(value.trim().parse::<usize>()?);
                }
                if key.eq_ignore_ascii_case("transfer-encoding") {
                    chunked = value.trim().eq_ignore_ascii_case("chunked");
                }
            }
            if let Some(length) = length {
                if length > BOUND {
                    return Err(err("proxy HTTP body bound"));
                }
                if body.len() >= length {
                    let body = body[..length].to_vec();
                    return Ok((raw, body));
                }
            } else if chunked {
                if let Some(body) = chunks(body)? {
                    return Ok((raw, body));
                }
            } else {
                return Err(err("proxy requires bounded HTTP framing"));
            }
        }
        stream.set_read_timeout(Some(timeout(start)?))?;
        let mut buffer = [0; 8192];
        let n = stream.read(&mut buffer)?;
        if n == 0 {
            return Err(err("proxy truncated HTTP frame"));
        }
        if raw.len() + n > BOUND {
            return Err(err("proxy HTTP frame bound"));
        }
        raw.extend_from_slice(&buffer[..n]);
    }
}
fn chunks(mut bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut body = Vec::new();
    loop {
        let Some(end) = bytes.windows(2).position(|part| part == b"\r\n") else {
            return Ok(None);
        };
        let text = std::str::from_utf8(&bytes[..end])?;
        let size = usize::from_str_radix(text.split(';').next().unwrap_or_default(), 16)?;
        if size > BOUND {
            return Err(err("proxy chunk bound"));
        }
        bytes = &bytes[end + 2..];
        if size == 0 {
            return Ok(bytes.starts_with(b"\r\n").then_some(body));
        }
        if bytes.len() < size + 2 {
            return Ok(None);
        }
        if &bytes[size..size + 2] != b"\r\n" {
            return Err(err("proxy malformed chunk"));
        }
        body.extend_from_slice(&bytes[..size]);
        bytes = &bytes[size + 2..];
    }
}
fn hold(state: &Mutex<State>, stop: &AtomicBool) -> Result<()> {
    state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .gate
        .as_mut()
        .ok_or_else(|| err("missing proxy gate"))?
        .reached = true;
    let start = Instant::now();
    while !stop.load(Ordering::Acquire) {
        let released = {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(gate) = &mut state.gate {
                if gate.released {
                    gate.finished = true;
                }
                gate.released
            } else {
                true
            }
        };
        if released {
            return Ok(());
        }
        if start.elapsed() > GATE {
            return Err(err("fault gate deadline"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
fn relay_events(
    client: &mut TcpStream,
    upstream: &mut TcpStream,
    state: &Mutex<State>,
    stop: &AtomicBool,
) -> Result<()> {
    upstream.set_read_timeout(Some(Duration::from_millis(100)))?;
    client.set_write_timeout(Some(IO))?;
    client.set_read_timeout(Some(Duration::from_millis(1)))?;
    let mut buffer = [0; 8192];
    let mut opened = false;
    while !stop.load(Ordering::Acquire) {
        if state.lock().unwrap_or_else(|e| e.into_inner()).offline {
            break;
        }
        match upstream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                if client.write_all(&buffer[..n]).is_err() {
                    break;
                }
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                if !opened {
                    state.streams += 1;
                    opened = true;
                }
                state.stream_bytes += n;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => break,
            Err(error) => return Err(error.into()),
        }
        if matches!(client.peek(&mut [0]), Ok(0)) {
            break;
        }
    }
    Ok(())
}

fn exchange(
    mut client: TcpStream,
    owner: SocketAddr,
    state: &Mutex<State>,
    stop: &AtomicBool,
    evidence: &Path,
) -> Result<()> {
    let started = Instant::now();
    let (request, body) = frame(&mut client, started)?;
    let rpc: Value = serde_json::from_slice(&body)?;
    let method = rpc["method"]
        .as_str()
        .ok_or_else(|| err("proxy request has no method"))?;
    let mutation = matches!(
        method,
        "zor/task.stop" | "fux/root.new" | "fux/workspace.new" | "fux/input.submit"
    );
    let (number, boundary) = {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        *state.methods.entry(method.into()).or_default() += 1;
        if mutation {
            state.requests += 1;
        }
        if state.offline {
            let _ = client.shutdown(Shutdown::Both);
            return Ok(());
        }
        let boundary = state
            .gate
            .as_ref()
            .filter(|gate| {
                method == gate.method
                    && !gate.reached
                    && !gate.released
                    && gate
                        .selector
                        .as_ref()
                        .is_none_or(|(key, value)| &rpc["params"][key] == value)
            })
            .map(|gate| gate.boundary);
        (state.requests, boundary)
    };
    if mutation {
        save(
            &evidence.join(format!("mutation-{number}-request.json")),
            &json!({"http":String::from_utf8_lossy(&request),"rpc":rpc}),
        )?;
    }
    if boundary == Some(Boundary::BeforeForward) {
        hold(state, stop)?;
        let _ = client.shutdown(Shutdown::Both);
        return Ok(());
    }
    let mut upstream = TcpStream::connect_timeout(&owner, timeout(started)?)?;
    upstream.set_write_timeout(Some(timeout(started)?))?;
    upstream.write_all(&request)?;
    if mutation {
        state.lock().unwrap_or_else(|e| e.into_inner()).forwarded += 1;
    }
    if method == "fux/events+watch" {
        return relay_events(&mut client, &mut upstream, state, stop);
    }
    let (response, body) = frame(&mut upstream, started)?;
    if mutation {
        let reply: Value = serde_json::from_slice(&body)?;
        save(
            &evidence.join(format!("mutation-{number}-reply.json")),
            &json!({"http":String::from_utf8_lossy(&response),"rpc":reply}),
        )?;
        if reply.get("error").is_some() || reply.get("result").is_none() {
            return Err(err(format!("real owner refused mutation: {reply}")));
        }
        state.lock().unwrap_or_else(|e| e.into_inner()).completed += 1;
    }
    if boundary == Some(Boundary::AfterReply) {
        hold(state, stop)?;
        let _ = client.shutdown(Shutdown::Both);
        return Ok(());
    }
    client.set_write_timeout(Some(timeout(started)?))?;
    // A controller killed during an unrelated observation may close first; that is not
    // an owner/proxy failure and must not hide the retained mutation evidence.
    let _ = client.write_all(&response);
    Ok(())
}

fn inspect(owner: &Stack, task: &str) -> Result<Value> {
    owner.zor()?.call("zor/task.inspect", json!({"task":task}))
}
fn intent(controller: &Stack, operation: &str) -> Result<Value> {
    let status = controller
        .zor()?
        .call("zor/machine.status", json!({"operation":operation}))?;
    status["intents"]
        .as_array()
        .and_then(|rows| rows.first())
        .cloned()
        .ok_or_else(|| err(format!("missing retained intent {operation}: {status}")))
}
fn guard(controller: &Stack, task: &str, expected_attempt: Option<u64>) -> Result<Value> {
    until(super::FRESHNESS_WAIT, "fresh fault task selection", || {
        let status = controller
            .zor()?
            .call("zor/machine.inspect", json!({"machine":"fault-owner"}))?;
        let machine = &status["machine"];
        if machine["freshness"]["freshness"].as_str() != Some("fresh") {
            return Ok(None);
        }
        let view = &machine["view"];
        let row = view["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| row["id"].as_str() == Some(task));
        let Some(row) = row else {
            return Ok(None);
        };
        if row["current_attempt"].as_u64() != expected_attempt {
            return Ok(None);
        }
        let pane = if let Some(attempt) = expected_attempt {
            let agent = view["agents"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|agent| {
                    agent["task"].as_str() == Some(task)
                        && agent["attempt"].as_u64() == Some(attempt)
                });
            let Some(agent) = agent else {
                return Ok(None);
            };
            json!({"instance":agent["instance"],"workspace":agent["workspace"],"pane":agent["pane"],"pid":agent["pid"]})
        } else {
            Value::Null
        };
        Ok(Some(
            json!({"instance":view["instance"],"attempt":expected_attempt,"pane":pane}),
        ))
    })
}

fn attempt(owner: &Stack, task: &str) -> Result<Value> {
    let view = inspect(owner, task)?;
    let rows = view["attempts"]
        .as_array()
        .ok_or_else(|| err("missing attempts"))?;
    if rows.len() != 1 {
        return Err(err(format!("expected one retained attempt: {view}")));
    }
    Ok(rows[0].clone())
}

fn launch_record<'a>(attempt: &'a Value, operation: &str) -> Result<&'a Value> {
    attempt["operations"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["operation"].as_str() == Some(operation))
        .ok_or_else(|| err(format!("missing launch {operation}: {attempt}")))
}

fn owner_reply(evidence: &Path, number: usize) -> Result<Value> {
    let saved: Value = serde_json::from_slice(&std::fs::read(
        evidence.join(format!("mutation-{number}-reply.json")),
    )?)?;
    Ok(saved["rpc"]["result"].clone())
}

fn exact_counts(proxy: &Proxy, launches: u64, inputs: u64) -> Result<Value> {
    let counts = proxy.counts()?;
    let methods = &counts["methods"];
    if methods["fux/workspace.new"].as_u64().unwrap_or(0) != launches
        || methods["fux/root.new"].as_u64().unwrap_or(0) != 0
        || methods["fux/input.submit"].as_u64().unwrap_or(0) != inputs
        || counts["mutation_requests"].as_u64() != Some(launches + inputs)
        || counts["forwarded_mutations"].as_u64() != Some(launches + inputs)
        || counts["accepted_replies"].as_u64() != Some(launches + inputs)
    {
        return Err(err(format!(
            "launch/input duplicate dispatch or missing owner acceptance: {counts}"
        )));
    }
    Ok(counts)
}

fn retained_journal(owner: &Stack, evidence: &Path, label: &str) -> Result<()> {
    let path = owner.path().join("state/zor/journal.scn.ron");
    if std::fs::metadata(&path)?.permissions().mode() & 0o077 != 0 {
        return Err(err("journal is not private"));
    }
    save(
        &evidence.join(format!("{label}-journal.json")),
        &json!({"journal":std::fs::read_to_string(path)?}),
    )
}

/// The fux owner survives both controller deaths. While restarting, an explicit transport
/// outage exposes the original uncertainty before real listing/receipt observations resolve it.
/// No mutating retry is used, even with the same operation id.
fn launch_input_recovery(bins: &Binaries, root: &Path) -> Result<Value> {
    let evidence = root.join("launch-input");
    std::fs::create_dir(&evidence)?;
    std::fs::set_permissions(&evidence, std::fs::Permissions::from_mode(0o700))?;
    let mut owner = Stack::start("ReceiptFaultOwner", bins)?;
    owner.kill_zor()?;
    let fux = owner.fux()?;
    let proxy = Proxy::start(&fux, &evidence)?;
    let mut descriptor = fux.raw.clone();
    descriptor["http"]["port"] = json!(proxy.port);
    let replacement = owner.fux_descriptor_path().with_extension("proxy.json");
    save(&replacement, &descriptor)?;
    std::fs::rename(&replacement, owner.fux_descriptor_path())?;
    owner.start_zor()?;
    until(WAIT, "proxied real fux event stream", || {
        Ok((proxy.counts()?["event_streams"].as_u64().unwrap_or(0) > 0).then_some(()))
    })?;
    let task = "receipt-fault";
    let operation = format!("receipt-launch-{}", nonce());
    let workspace = "receipt-fault-workspace";
    // Disable terminal echo: each INPUT line is emitted by the actual reader, not the PTY.
    let command = "stty -echo; printf 'READY:%s:%s\\n' \"$ZOR_LAUNCH_ID\" \"$$\"; while IFS= read -r line; do printf 'INPUT:%s\\n' \"$line\"; done";
    owner.zor()?.call(
        "zor/task.create",
        json!({"task":task,"title":task,"cwd":owner.path().join("home")}),
    )?;
    proxy.arm_method(
        "fux/workspace.new",
        Some(("name", json!(workspace))),
        Boundary::AfterReply,
    );
    let accepted = owner.zor()?.call("zor/task.launch", json!({
        "task":task,"operation":operation,"argv":["/bin/sh","-c",command],"workspace":workspace,"ephemeral":true
    }))?;
    proxy.reached()?;
    let retained = attempt(&owner, task)?;
    if launch_record(&retained, &operation)?["phase"] != "submitting" || retained["pane"] != 0 {
        return Err(err(format!(
            "launch reply was not withheld at submitting boundary: {retained}"
        )));
    }
    let marker = retained["marker"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| err("launch marker"))?;
    let tag = format!("ZOR_LAUNCH_ID={marker}");
    let request: Value =
        serde_json::from_slice(&std::fs::read(evidence.join("mutation-1-request.json"))?)?;
    let template = &request["rpc"]["params"]["template"];
    if template["stream"] != operation
        || !template["argv"]
            .as_array()
            .is_some_and(|argv| argv.contains(&json!(tag)))
    {
        return Err(err("forwarded launch lost its exact operation/marker"));
    }
    let reply = owner_reply(&evidence, 1)?;
    let pane = reply["pane"]
        .as_u64()
        .ok_or_else(|| err(format!("accepted launch has no pane: {reply}")))?;
    let process = until(WAIT, "accepted launch real process", || {
        let row = super::pane_row(&fux, pane)?;
        Ok((row["state"] == "live" && row["pid"].as_u64().is_some()).then_some(row))
    })?;
    let ready = format!(
        "READY:{marker}:{}",
        process["pid"].as_u64().ok_or_else(|| err("process pid"))?
    );
    let capture = until(WAIT, "launch marker from actual process", || {
        let text = super::capture(&fux, pane)?;
        Ok(text.contains(&ready).then_some(text))
    })?;
    let counts = exact_counts(&proxy, 1, 0)?;
    save(
        &evidence.join("launch-before-crash.json"),
        &json!({
            "accepted":accepted,"retained":retained,"owner_reply":reply,"process":process,"capture":capture,"counts":counts
        }),
    )?;
    let killed = owner.kill_zor()?;
    retained_journal(&owner, &evidence, "launch")?;
    proxy.release()?;
    proxy.offline(true);
    owner.start_zor()?;
    let restored = attempt(&owner, task)?;
    if launch_record(&restored, &operation)?["phase"] != "uncertain"
        || restored["attempt"] != retained["attempt"]
        || restored["marker"] != retained["marker"]
        || launch_record(&restored, &operation)?["launch"]
            != launch_record(&retained, &operation)?["launch"]
        || restored["pane"] != retained["pane"]
    {
        return Err(err(format!(
            "restart did not retain exact uncertain launch: {restored}"
        )));
    }
    proxy.offline(false);
    let reconciled = until(WAIT, "launch marker reconciliation", || {
        let row = attempt(&owner, task)?;
        Ok((row["state"] == "live"
            && row["pane"] == pane
            && row["pid"] == process["pid"]
            && row["instance"] == fux.instance
            && row["workspace"] == workspace
            && row["marker"] == retained["marker"]
            && row["stream"] == operation
            && launch_record(&row, &operation)?["phase"] == "attached")
            .then_some(row))
    })?;
    let listing = fux.call("fux/workspace.list", json!({}))?;
    let tagged = listing["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|w| w["roots"].as_array().into_iter().flatten())
        .flat_map(|r| r["panes"].as_array().into_iter().flatten())
        .filter(|p| {
            p["argv"]
                .as_array()
                .is_some_and(|argv| argv.contains(&json!(tag)))
        })
        .count();
    if tagged != 1 {
        return Err(err(format!("launch marker identifies {tagged} real panes")));
    }
    let launch = json!({"action":"pane-launch","boundary":"accepted-reply-lost","killed_controller":killed,
        "restored_uncertain":restored,"reconciliation":reconciled,"tagged_panes":tagged,"counts":exact_counts(&proxy,1,0)?});
    save(&evidence.join("launch-reconciled.json"), &launch)?;

    let prompt = format!("receipt-input-{}", nonce());
    let text = format!("RECEIPT_INPUT_{}", nonce());
    proxy.arm_method("fux/input.submit", None, Boundary::AfterReply);
    let accepted = owner.zor()?.call(
        "zor/task.prompt",
        json!({"task":task,"prompt":prompt,"text":text,"timeout_ms":60000}),
    )?;
    proxy.reached()?;
    let retained = owner
        .zor()?
        .call("zor/prompt.status", json!({"prompt":prompt}))?;
    let receipt = &retained["receipt"];
    if retained["delivery"] != "submitting"
        || receipt["instance"] != fux.instance
        || receipt["pane"] != pane
        || retained["attempt"] != reconciled["attempt"]
        || receipt["operation"].as_u64().is_none()
    {
        return Err(err(format!(
            "input boundary lost exact submitting receipt: {retained}"
        )));
    }
    let request: Value =
        serde_json::from_slice(&std::fs::read(evidence.join("mutation-2-request.json"))?)?;
    if request["rpc"]["params"]["operation"] != receipt["operation"]
        || request["rpc"]["params"]["keys"] != format!("{text}\\r")
    {
        return Err(err("forwarded input differs from its retained intent"));
    }
    let reply = owner_reply(&evidence, 2)?;
    if reply["operation"] != receipt["operation"]
        || reply["pane"] != pane
        || reply["state"] != "submitted"
        || reply["bytes_written"].as_u64() != Some((text.len() + 1) as u64)
        || reply["seq"].as_u64().is_none()
    {
        return Err(err(format!("owner did not accept exact input: {reply}")));
    }
    let visible = format!("INPUT:{text}");
    let capture = until(WAIT, "input consumed by actual process", || {
        let capture = super::capture(&fux, pane)?;
        Ok(capture.contains(&visible).then_some(capture))
    })?;
    if capture.matches(&visible).count() != 1 {
        return Err(err("input was visibly delivered more than once"));
    }
    save(
        &evidence.join("input-before-crash.json"),
        &json!({
            "accepted":accepted,"retained":retained,"owner_reply":reply,"capture":capture,"counts":exact_counts(&proxy,1,1)?
        }),
    )?;
    let killed = owner.kill_zor()?;
    retained_journal(&owner, &evidence, "input")?;
    proxy.release()?;
    proxy.offline(true);
    owner.start_zor()?;
    let restored = owner
        .zor()?
        .call("zor/prompt.status", json!({"prompt":prompt}))?;
    if restored["delivery"] != "uncertain"
        || restored["receipt"] != retained["receipt"]
        || restored["attempt"] != retained["attempt"]
        || restored["text"] != text
        || restored["prompt"] != prompt
    {
        return Err(err(format!(
            "restart did not retain exact uncertain input: {restored}"
        )));
    }
    proxy.offline(false);
    let reconciled = until(WAIT, "receipt observation reconciliation", || {
        let row = owner
            .zor()?
            .call("zor/prompt.status", json!({"prompt":prompt}))?;
        Ok((row["delivery"] == "delivered"
            && row["receipt"]["instance"] == receipt["instance"]
            && row["receipt"]["operation"] == reply["operation"]
            && row["receipt"]["pane"] == reply["pane"]
            && row["receipt"]["state"] == reply["state"]
            && row["receipt"]["seq"] == reply["seq"]
            && row["receipt"]["bytes_written"] == reply["bytes_written"]
            && row["receipt"]["expires_ms"] == reply["expires_ms"])
            .then_some(row))
    })?;
    let observed = fux.call(
        "fux/input.status",
        json!({"operation":receipt["operation"]}),
    )?;
    if observed != reply {
        return Err(err(
            "retained owner receipt differs from captured accepted reply",
        ));
    }
    let live = attempt(&owner, task)?;
    if live["pane"] != pane || live["pid"] != process["pid"] || live["instance"] != fux.instance {
        return Err(err("input recovery changed the actual process identity"));
    }
    // Leave normal streaming, listing, and heartbeat observations running long enough to
    // catch any replay after reconciliation, not merely the first restored projection.
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        exact_counts(&proxy, 1, 1)?;
        std::thread::sleep(super::TICK);
    }
    let capture = super::capture(&fux, pane)?;
    if capture.matches(&visible).count() != 1 {
        return Err(err("restart duplicated or lost visible input"));
    }
    let counts = exact_counts(&proxy, 1, 1)?;
    if counts["methods"]["fux/input.status"].as_u64().unwrap_or(0) == 0
        || counts["methods"]["fux/workspace.list"]
            .as_u64()
            .unwrap_or(0)
            == 0
        || counts["event_streams"].as_u64().unwrap_or(0) < 3
    {
        return Err(err(format!(
            "missing authentic restart observation traffic: {counts}"
        )));
    }
    let input = json!({"action":"input-submit","boundary":"accepted-reply-lost","killed_controller":killed,
        "restored_uncertain":restored,"reconciliation":reconciled,"owner_receipt":observed,
        "capture":capture,"visible_deliveries":1,"counts":counts});
    save(&evidence.join("input-reconciled.json"), &input)?;
    Ok(json!({"launch":launch,"input":input}))
}

pub(super) fn recovery(fixture: &mut Fixture) -> Result<Outcome> {
    let target = require_target(fixture)?;
    let evidence = fixture.artifacts.join("crash-boundaries");
    std::fs::create_dir(&evidence)?;
    std::fs::set_permissions(&evidence, std::fs::Permissions::from_mode(0o700))?;
    let bins = Binaries {
        fux: fixture.local.fux_binary().into(),
        zor: fixture.local.zor_binary().into(),
        dir: fixture
            .local
            .zor_binary()
            .parent()
            .ok_or_else(|| err("binary directory"))?
            .into(),
    };
    let owner = Stack::start("FaultOwner", &bins)?;
    let mut controller = Stack::start("FaultController", &bins)?;
    let original = owner.zor()?;
    let proxy = Proxy::start(&original, &evidence)?;
    let mut descriptor = original.raw.clone();
    descriptor["http"]["port"] = json!(proxy.port);
    let proxied = evidence.join("owner.brp.json");
    save(&proxied, &descriptor)?;
    controller.zor_run(&[
        "machine",
        "add",
        "fault-owner",
        "--control-brp",
        proxied.to_str().ok_or_else(|| err("descriptor path"))?,
    ])?;
    let mut report = Vec::new();
    for (label, boundary, forwarded) in [
        ("before-forward", Boundary::BeforeForward, 0),
        ("reply-lost", Boundary::AfterReply, 1),
    ] {
        let task = format!("fault-{label}");
        let operation = format!("fault-{}-{label}", nonce());
        let home = owner.path().join("home");
        owner.zor_run(&[
            "task",
            "create",
            &task,
            "--title",
            &task,
            "--cwd",
            home.to_str().ok_or_else(|| err("owner home"))?,
        ])?;
        let launch = format!("fault-launch-{}", nonce());
        let marker = format!("FAULT_PROCESS_{label}");
        let command = format!("printf '{marker}\\n'; exec /bin/cat");
        owner.zor_run(&[
            "task",
            "launch",
            &task,
            "--operation",
            &launch,
            "--",
            "/bin/sh",
            "-c",
            &command,
        ])?;
        let before = until(WAIT, "fault task live", || {
            let view = inspect(&owner, &task)?;
            Ok(view["attempts"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|attempt| attempt["state"].as_str() == Some("live"))
                .cloned())
        })?;
        let attempt = before["attempt"]
            .as_u64()
            .ok_or_else(|| err("attempt identity"))?;
        let pane = before["pane"]
            .as_u64()
            .ok_or_else(|| err("pane identity"))?;
        let capture = until(WAIT, "real process output", || {
            let text = super::capture(&owner.fux()?, pane)?;
            Ok(text.contains(&marker).then_some(text))
        })?;
        save(
            &evidence.join(format!("{label}-process.json")),
            &json!({"attempt":before,"capture":capture}),
        )?;
        let selected = guard(&controller, &task, Some(attempt))?;
        proxy.arm(&task, boundary);
        let accepted = controller.zor()?.call(
            "zor/machine.stop",
            json!({"machine":"fault-owner","task":task,"operation":operation,"guard":selected}),
        )?;
        proxy.reached()?;
        let retained = intent(&controller, &operation)?;
        if retained["record"]["phase"].as_str() != Some("submitting") {
            return Err(err(format!("fault intent not submitting: {retained}")));
        }
        let durable_path = controller
            .config_dir()
            .join("zor/machines.json.action-intents.json");
        if std::fs::metadata(&durable_path)?.permissions().mode() & 0o077 != 0 {
            return Err(err("action intents are not private"));
        }
        let durable: Value = serde_json::from_slice(&std::fs::read(&durable_path)?)?;
        if !durable["intents"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|row| row == &retained)
        {
            return Err(err("proxy boundary preceded durable intent"));
        }
        let counts = proxy.counts()?;
        if counts["forwarded_mutations"].as_u64() != Some(forwarded)
            || counts["accepted_replies"].as_u64() != Some(forwarded)
        {
            return Err(err(format!("wrong fault boundary counters: {counts}")));
        }
        save(
            &evidence.join(format!("{label}-before-crash.json")),
            &json!({"accepted":accepted,"retained":retained,"durable":durable,"counts":counts}),
        )?;
        let killed = controller.kill_zor()?;
        proxy.release()?;
        controller.start_zor()?;
        let restored = intent(&controller, &operation)?;
        if restored["record"]["phase"].as_str() != Some("uncertain")
            || restored["operation"] != retained["operation"]
            || restored["record"]["pane"] != retained["record"]["pane"]
            || restored["record"]["instance"] != retained["record"]["instance"]
        {
            return Err(err(format!(
                "restart lost exact uncertain intent: {restored}"
            )));
        }
        let after = until(WAIT, "owner evidence after crash", || {
            let view = inspect(&owner, &task)?;
            let Some(row) = view["attempts"].as_array().and_then(|rows| rows.first()) else {
                return Ok(None);
            };
            let state = if boundary == Boundary::BeforeForward {
                "live"
            } else {
                "finished"
            };
            Ok((row["state"].as_str() == Some(state)).then_some(view))
        })?;
        let expected = if boundary == Boundary::BeforeForward {
            Some(attempt)
        } else {
            None
        };
        let reconciled_guard = guard(&controller, &task, expected)?;
        let reconciliation = format!("fault-reconcile-{}", nonce());
        controller.zor()?.call("zor/machine.reconcile", json!({"machine":"fault-owner","task":task,"operation":reconciliation,"guard":reconciled_guard}))?;
        let reconciled = until(WAIT, "explicit evidence reconciliation", || {
            let record = intent(&controller, &reconciliation)?;
            match record["record"]["phase"].as_str() {
                Some("submitting") => Ok(None),
                Some("done") => Ok(Some(record)),
                _ => Err(err(format!("reconciliation failed: {record}"))),
            }
        })?;
        let result = &reconciled["record"]["result"];
        if result["attempts"]
            .as_array()
            .and_then(|rows| rows.first())
            .map(|row| &row["state"])
            != after["attempts"]
                .as_array()
                .and_then(|rows| rows.first())
                .map(|row| &row["state"])
        {
            return Err(err("explicit reconciliation disagrees with owner evidence"));
        }
        let original_uncertain = intent(&controller, &operation)?;
        if original_uncertain != restored {
            return Err(err(
                "read-only reconciliation rewrote original uncertain action",
            ));
        }
        // Keep the proxy in the catalog through restart and fresh supervision. Multiple
        // new HTTP observations and explicit reconciliation must not emit a second stop.
        let counts = proxy.counts()?;
        if counts["mutation_requests"].as_u64() != Some(if forwarded == 0 { 1 } else { 2 })
            || counts["forwarded_mutations"].as_u64() != Some(forwarded)
        {
            return Err(err(format!(
                "controller replayed retained mutation: {counts}"
            )));
        }
        let item = json!({"action":"remote-stop","boundary":label,"killed_controller":killed,"restored":restored,
            "original_after_reconciliation":original_uncertain,"owner":after,"reconciliation":reconciled,"counts":counts});
        save(&evidence.join(format!("{label}-reconciled.json")), &item)?;
        report.push(item);
    }
    let receipts = launch_input_recovery(&bins, &evidence)?;
    save(
        &evidence.join("summary.json"),
        &json!({"remote_actions":report,"receipts":receipts,
        "coverage":"remote stop before dispatch and accepted reply lost; pane launch and input submit accepted reply lost; SIGKILL/restart retains uncertainty, authentic listing/receipt reconciliation, no mutation retries"}),
    )?;
    if super::live_attempt(fixture, super::TARGET)? != Some(target) {
        return Err(err("fault scenario changed original target"));
    }
    Ok(Outcome::Pass("real HTTP proxy: remote stop pre-forward (0 forwards) and accepted reply lost (1 forward); pane launch and input submit accepted replies lost (exactly 1 forward each). SIGKILL/restarts retain exact uncertainty before authentic reconciliation; one marker-tagged live process and one visible input delivery, no replay. Event streams forwarded, private HTTP/intent/receipt evidence: crash-boundaries/.".into()))
}
