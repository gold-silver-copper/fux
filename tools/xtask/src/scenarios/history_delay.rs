//! Real viewer against a task-owned attachment peer with deliberately delayed history replies.
use crate::support::{
    attachment::{connect, receive, send, send_server},
    local::{Root, until},
    terminal::Terminal,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::Path,
    time::{Duration, Instant},
};

fn pump(terminal: &mut Terminal, parser: &mut vt100::Parser) -> Result<String> {
    terminal.pump_for(Duration::from_millis(20), |bytes| parser.process(bytes))?;
    Ok(parser.screen().contents())
}
fn observe(
    terminal: &mut Terminal,
    parser: &mut vt100::Parser,
    label: &str,
    check: impl Fn(&str) -> bool,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let text = pump(terminal, parser)?;
        if check(&text) && !terminal.frame_pending()? {
            terminal.checkpoint(label)?;
            return Ok(());
        }
        ensure!(Instant::now() < deadline, "{label}: {text}");
    }
}
fn reply(request: &Value, view: &Value, offset: u64) -> Value {
    let mut view = view.clone();
    view["offset"] = json!(offset);
    json!({"view":{"reply":{"request":request["request"],"pane":request["pane"],"view":view,"history":100}}})
}
#[derive(Default)]
struct Incoming(Vec<u8>);
impl Incoming {
    fn next(
        &mut self,
        peer: &mut std::os::unix::net::UnixStream,
        terminal: &mut Terminal,
        parser: &mut vt100::Parser,
        label: &str,
    ) -> Result<Value> {
        use std::io::Read as _;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            // A real PTY has bounded output buffers: service rendering while awaiting
            // input frames, rather than blocking the viewer inside its paint call.
            pump(terminal, parser)?;
            let mut bytes = [0; 16384];
            loop {
                match peer.read(&mut bytes) {
                    Ok(0) => anyhow::bail!("{label}: attachment closed"),
                    Ok(count) => self.0.extend_from_slice(&bytes[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.into()),
                }
                ensure!(self.0.len() <= 16 * 1024 * 1024, "fixture input bound");
            }
            if let Some(prefix) = self.0.get(..4) {
                let length = u32::from_be_bytes(prefix.try_into()?) as usize;
                ensure!(length <= 65536, "oversized viewer frame");
                if let Some(bytes) = self.0.get(4..4 + length) {
                    let value = serde_json::from_slice(bytes)?;
                    self.0.drain(..4 + length);
                    return Ok(value);
                }
            }
            ensure!(
                Instant::now() < deadline,
                "{label}: no viewer message; screen: {}",
                parser.screen().contents()
            );
        }
    }
}

fn expect_input(
    peer: &mut std::os::unix::net::UnixStream,
    incoming: &mut Incoming,
    terminal: &mut Terminal,
    parser: &mut vt100::Parser,
    expected: &[u8],
) -> Result<()> {
    let mut actual = Vec::new();
    while actual.len() < expected.len() {
        let message = incoming.next(peer, terminal, parser, "application input")?;
        ensure!(
            message["type"] == "input",
            "expected only application input: {message}"
        );
        actual.extend(serde_json::from_value::<Vec<u8>>(message["bytes"].clone())?);
        ensure!(
            expected.starts_with(&actual),
            "input duplicated, changed or leaked Escape: {actual:?}"
        );
    }
    ensure!(actual == expected, "wrong application input");
    Ok(())
}

pub(super) fn run(binary: &Path) -> Result<()> {
    let root = Root::new("fhdelay-", &["/bin/cat".into()])?;
    let mut server = root.server(binary)?;
    let mut source = connect(&root.path().join("fux/default.attach.sock"))?;
    send(&mut source, &json!({"type":"hello","rows":12,"columns":48}))?;
    ensure!(
        receive(&mut source).context("source hello")? == json!({"hello":{}}),
        "source hello"
    );
    let bindings = receive(&mut source).context("source bindings")?;
    let mut state = receive(&mut source).context("source full frame")?;
    drop(source);
    let frame = state.pointer_mut("/state/state").context("source frame")?;
    let view = frame.pointer("/panes/1").context("source pane")?.clone();
    frame["panes"]["2"] = view.clone();
    frame["layout"][0]["rect"]["width"] = json!(23);
    let mut second = frame["layout"][0].clone();
    second["pane"] = json!(2);
    second["rect"]["x"] = json!(24);
    second["rect"]["width"] = json!(24);
    frame["layout"]
        .as_array_mut()
        .context("source layout")?
        .push(second);
    let mut generation = frame["generation"].as_u64().context("generation")?;
    let path = root.path().join("delayed.sock");
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let mut terminal = Terminal::start_with_size(
        &root,
        binary,
        &["attach", "--socket", path.to_str().context("socket path")?],
        12,
        48,
    )?;
    let mut parser = vt100::Parser::new(12, 48, 0);
    let (mut peer, _) = until(Duration::from_secs(3), || match listener.accept() {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error.into()),
    })?;
    peer.set_nonblocking(true)?;
    let mut incoming = Incoming::default();
    peer.set_read_timeout(Some(Duration::from_secs(2)))?;
    peer.set_write_timeout(Some(Duration::from_secs(2)))?;
    ensure!(
        incoming.next(
            &mut peer,
            &mut terminal,
            &mut parser,
            "fake peer: viewer hello"
        )?["type"]
            == "hello",
        "viewer hello"
    );
    send(&mut peer, &json!({"hello":{}}))?;
    send_server(&mut peer, &bindings)?;
    send_server(&mut peer, &state)?;
    ensure!(
        incoming.next(
            &mut peer,
            &mut terminal,
            &mut parser,
            "fake peer: initial viewer resize"
        )?["type"]
            == "resize",
        "viewer resize"
    );
    observe(&mut terminal, &mut parser, "delayed peer ready", |text| {
        text.contains("default")
    })?;

    terminal.send(b"\x1b[<64;3;3M")?;
    let first = incoming.next(&mut peer, &mut terminal, &mut parser, "history A request")?;
    ensure!(
        first["type"] == "view" && first["pane"] == 1,
        "first history: {first}"
    );
    terminal.send(b"\x1b[<64;30;3M")?;
    let other = incoming.next(
        &mut peer,
        &mut terminal,
        &mut parser,
        "history B request while A withheld",
    )?;
    ensure!(
        other["type"] == "view" && other["pane"] == 2,
        "B blocked behind A: {other}"
    );
    send_server(&mut peer, &reply(&other, &view, 3))?;
    observe(
        &mut terminal,
        &mut parser,
        "B history while A reply withheld",
        |text| text.contains("offset 3"),
    )?;
    terminal.send(b"\x1b")?;
    observe(
        &mut terminal,
        &mut parser,
        "Escape leaves pending A independently",
        |text| text.contains("History pane 1"),
    )?;
    terminal.send(b"\x1b")?;
    observe(
        &mut terminal,
        &mut parser,
        "Escape dismisses pending history",
        |text| !text.contains("History pane") && !text.contains("split side"),
    )?;
    terminal.send(b"EXACT")?;
    expect_input(
        &mut peer,
        &mut incoming,
        &mut terminal,
        &mut parser,
        b"EXACT",
    )?;

    terminal.send(b"\x1b[<64;3;3M")?;
    let fresh = incoming.next(
        &mut peer,
        &mut terminal,
        &mut parser,
        "reentered history request",
    )?;
    ensure!(
        fresh["type"] == "view" && fresh["request"] != first["request"],
        "reused history identity: {fresh}"
    );
    send_server(&mut peer, &reply(&first, &view, 99))?;
    observe(
        &mut terminal,
        &mut parser,
        "late old reply cannot restore viewport",
        |text| text.contains("offset 0") && !text.contains("offset 99"),
    )?;
    send_server(&mut peer, &reply(&fresh, &view, 3))?;
    observe(
        &mut terminal,
        &mut parser,
        "current reply installs history",
        |text| text.contains("offset 3"),
    )?;
    terminal.send(b"\x1b[<64;3;3M")?;
    let withheld = incoming.next(
        &mut peer,
        &mut terminal,
        &mut parser,
        "second scroll request",
    )?;
    ensure!(
        withheld["type"] == "view" && withheld["offset"] == 6,
        "next scroll: {withheld}"
    );
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        generation += 1;
        state["state"]["state"]["generation"] = json!(generation);
        send_server(&mut peer, &state)?;
        let text = pump(&mut terminal, &mut parser)?;
        if text.contains("History request timed out") {
            observe(
                &mut terminal,
                &mut parser,
                "history timeout despite continuing state traffic",
                |text| text.contains("timed out"),
            )?;
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "state traffic postponed fixed history deadline: {text}"
        );
    }
    terminal.send(b"AFTER_TIMEOUT")?;
    expect_input(
        &mut peer,
        &mut incoming,
        &mut terminal,
        &mut parser,
        b"AFTER_TIMEOUT",
    )?;
    // A withheld mutation reply must not prevent local history or cancellation.
    terminal.send(b"\x01*")?;
    let control = incoming.next(
        &mut peer,
        &mut terminal,
        &mut parser,
        "control before delay",
    )?;
    ensure!(control["type"] == "control", "expected control: {control}");
    let control_id = control["request"]["id"].as_u64().context("control id")?;
    terminal.send(b"\x1b[<64;3;3M")?;
    let during = incoming.next(
        &mut peer,
        &mut terminal,
        &mut parser,
        "history during control",
    )?;
    ensure!(
        during["type"] == "view",
        "history gated by control: {during}"
    );
    send_server(&mut peer, &reply(&during, &view, 3))?;
    observe(
        &mut terminal,
        &mut parser,
        "history during withheld control",
        |text| text.contains("offset 3"),
    )?;
    terminal.send(b"\x1b")?;
    observe(
        &mut terminal,
        &mut parser,
        "Escape during withheld control",
        |text| !text.contains("History pane"),
    )?;
    terminal.send(b"\x01,discarded")?;
    observe(
        &mut terminal,
        &mut parser,
        "dependent command waits cancellably",
        |text| text.contains("Waiting for previous"),
    )?;
    terminal.send(b"\x1b")?;
    observe(
        &mut terminal,
        &mut parser,
        "cancel dependent command before reply",
        |text| !text.contains("Waiting for previous"),
    )?;
    terminal.send(b"CONTROL_INPUT")?;
    send_server(
        &mut peer,
        &json!({"reply":{"reply":{"status":"accepted","id":control_id+100}}}),
    )?;
    terminal.pump_for(Duration::from_millis(100), |bytes| parser.process(bytes))?;
    {
        use std::io::Read as _;
        let mut byte = [0];
        ensure!(
            incoming.0.is_empty(),
            "unexpected buffered input before acknowledgment"
        );
        ensure!(
            matches!(peer.read(&mut byte), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "stale control reply released external input"
        );
    }
    send_server(
        &mut peer,
        &json!({"reply":{"reply":{"status":"accepted","id":control_id}}}),
    )?;
    expect_input(
        &mut peer,
        &mut incoming,
        &mut terminal,
        &mut parser,
        b"CONTROL_INPUT",
    )?;
    observe(
        &mut terminal,
        &mut parser,
        "late control reply leaves canceled command dismissed",
        |text| !text.contains("Rename tab") && !text.contains("split side by side"),
    )?;

    terminal.send(b"\x01*")?;
    let next_control = incoming.next(
        &mut peer,
        &mut terminal,
        &mut parser,
        "second delayed control",
    )?;
    ensure!(
        next_control["type"] == "control",
        "second control: {next_control}"
    );
    terminal.send(b"\x01,\x15preserved")?;
    observe(
        &mut terminal,
        &mut parser,
        "dependent command before completion",
        |text| text.contains("Waiting for previous"),
    )?;
    send_server(
        &mut peer,
        &json!({"reply":{"reply":{"status":"accepted","id":next_control["request"]["id"]}}}),
    )?;
    observe(
        &mut terminal,
        &mut parser,
        "dependent command resumes with its text",
        |text| text.contains("Rename tab") && text.contains("preserved"),
    )?;
    terminal.send(b"\x1b")?;
    observe(
        &mut terminal,
        &mut parser,
        "resumed command dismisses normally",
        |text| !text.contains("Rename tab"),
    )?;

    // A reply can observe a buffer switch before its state frame arrives.
    // Reject its viewport rather than briefly installing it into the old session.
    terminal.send(b"\x1b[<64;3;3M")?;
    let before_buffer = incoming.next(
        &mut peer,
        &mut terminal,
        &mut parser,
        "read before buffer switch",
    )?;
    ensure!(
        before_buffer["type"] == "view",
        "buffer read: {before_buffer}"
    );
    let mut alternate = view.clone();
    alternate["modes"]["alternate_screen"] = json!(true);
    send_server(&mut peer, &reply(&before_buffer, &alternate, 0))?;
    observe(
        &mut terminal,
        &mut parser,
        "other buffer reply dismisses history before state",
        |text| !text.contains("History pane") && text.contains("History view changed"),
    )?;
    generation += 1;
    state["state"]["state"]["generation"] = json!(generation);
    state["state"]["state"]["panes"]["1"]["modes"]["alternate_screen"] = json!(true);
    state["state"]["state"]["message"] = json!("ALTERNATE_STATE_READY");
    send_server(&mut peer, &state)?;
    observe(
        &mut terminal,
        &mut parser,
        "alternate state barrier",
        |text| text.contains("ALTERNATE_STATE_READY"),
    )?;
    terminal.send(b"\x1b[<68;3;3M")?;
    let alternate_read = incoming.next(
        &mut peer,
        &mut terminal,
        &mut parser,
        "new alternate buffer read",
    )?;
    ensure!(
        alternate_read["type"] == "view" && alternate_read["request"] != before_buffer["request"],
        "alternate read identity: {alternate_read}"
    );
    send_server(&mut peer, &reply(&before_buffer, &view, 3))?;
    send_server(&mut peer, &reply(&alternate_read, &alternate, 0))?;
    observe(
        &mut terminal,
        &mut parser,
        "new alternate history ignores old primary reply",
        |text| text.contains("History pane 1") && text.contains("offset 0"),
    )?;
    generation += 1;
    state["state"]["state"]["generation"] = json!(generation);
    state["state"]["state"]["panes"]["1"]["modes"]["alternate_screen"] = json!(false);
    send_server(&mut peer, &state)?;
    observe(
        &mut terminal,
        &mut parser,
        "primary state invalidates alternate history",
        |text| !text.contains("History pane"),
    )?;

    terminal.send(b"\x1b[<64;3;3M")?;
    let before_workspace = incoming.next(
        &mut peer,
        &mut terminal,
        &mut parser,
        "history pending before workspace change",
    )?;
    ensure!(
        before_workspace["type"] == "view",
        "workspace history read: {before_workspace}"
    );
    generation += 1;
    state["state"]["state"]["generation"] = json!(generation);
    state["state"]["state"]["workspace"] = json!("transferred");
    let stream = state["state"]["state"]["workspace_stream"]
        .as_u64()
        .context("workspace stream")?;
    state["state"]["state"]["workspace_stream"] = json!(stream + 1);
    state["state"]["state"]["message"] = json!("WORKSPACE_STATE_READY");
    send_server(&mut peer, &state)?;
    observe(
        &mut terminal,
        &mut parser,
        "workspace change discards pending history",
        |text| text.contains("WORKSPACE_STATE_READY") && !text.contains("History pane"),
    )?;
    terminal.send(b"\x1b[<64;3;3M")?;
    let after_workspace = incoming.next(
        &mut peer,
        &mut terminal,
        &mut parser,
        "new workspace history read",
    )?;
    ensure!(
        after_workspace["type"] == "view"
            && after_workspace["request"] != before_workspace["request"],
        "workspace read identity: {after_workspace}"
    );
    send_server(&mut peer, &reply(&before_workspace, &view, 99))?;
    send_server(&mut peer, &reply(&after_workspace, &view, 3))?;
    observe(
        &mut terminal,
        &mut parser,
        "new workspace history ignores prior stream reply",
        |text| text.contains("offset 3") && !text.contains("offset 99"),
    )?;

    // Availability is frame-local: an exited pane can be read only while the
    // attachment still retains it. Its old mouse mode must not own wheel input.
    generation += 1;
    state["state"]["state"]["generation"] = json!(generation);
    state["state"]["state"]["panes"]["2"]["exit"] = json!(0);
    state["state"]["state"]["panes"]["2"]["modes"]["mouse_mode"] = json!("any-motion");
    state["state"]["state"]["message"] = json!("EXITED_PANE_READY");
    send_server(&mut peer, &state)?;
    observe(
        &mut terminal,
        &mut parser,
        "retained exited pane state",
        |text| text.contains("EXITED_PANE_READY"),
    )?;
    terminal.send(b"\x1b[<64;30;3M")?;
    let exited_read = incoming.next(
        &mut peer,
        &mut terminal,
        &mut parser,
        "retained exited pane wheel",
    )?;
    ensure!(
        exited_read["type"] == "view" && exited_read["pane"] == 2,
        "exited application captured wheel: {exited_read}"
    );
    let exited_view = state["state"]["state"]["panes"]["2"].clone();
    send_server(&mut peer, &reply(&exited_read, &exited_view, 3))?;
    observe(
        &mut terminal,
        &mut parser,
        "retained exited pane history remains readable",
        |text| text.contains("History pane 2") && text.contains("offset 3"),
    )?;
    generation += 1;
    state["state"]["state"]["generation"] = json!(generation);
    state["state"]["state"]["panes"]
        .as_object_mut()
        .context("panes")?
        .remove("2");
    state["state"]["state"]["layout"]
        .as_array_mut()
        .context("layout")?
        .retain(|entry| entry["pane"] != 2);
    state["state"]["state"]["message"] = json!("EXITED_PANE_REMOVED");
    send_server(&mut peer, &state)?;
    observe(
        &mut terminal,
        &mut parser,
        "removed exited pane discards its private history",
        |text| text.contains("EXITED_PANE_REMOVED") && !text.contains("History pane 2"),
    )?;

    terminal.send(b"\x01*")?;
    let failed_control = incoming.next(
        &mut peer,
        &mut terminal,
        &mut parser,
        "control rejected after local interaction",
    )?;
    ensure!(
        failed_control["type"] == "control",
        "failed control request: {failed_control}"
    );
    send_server(
        &mut peer,
        &json!({"reply":{"reply":{"status":"failed","id":failed_control["request"]["id"],"error":{"code":"conflict","message":"CONTROL_EXPECTED_FAILURE"}}}}),
    )?;
    observe(
        &mut terminal,
        &mut parser,
        "failed control is a notice without commands",
        |text| text.contains("CONTROL_EXPECTED_FAILURE") && !text.contains("split side by side"),
    )?;
    terminal.send(b"AFTER_CONTROL_FAILURE")?;
    expect_input(
        &mut peer,
        &mut incoming,
        &mut terminal,
        &mut parser,
        b"AFTER_CONTROL_FAILURE",
    )?;
    observe(
        &mut terminal,
        &mut parser,
        "normal input resumes after rejected control",
        |text| !text.contains("History pane") && !text.contains("split side by side"),
    )?;

    terminal.send(b"\x01d")?;
    ensure!(
        incoming.next(
            &mut peer,
            &mut terminal,
            &mut parser,
            "detach after exact input"
        )?["type"]
            == "detach",
        "unexpected duplicate application input or command"
    );
    send(&mut peer, &json!({"exited":{"code":0}}))?;
    ensure!(
        terminal.wait(Duration::from_secs(2))?.success(),
        "viewer did not detach"
    );
    server.finish()?;
    println!(
        "PASS delayed history/control, local cancellation, exact ordered input, stale replies and fixed deadline under state traffic"
    );
    Ok(())
}
