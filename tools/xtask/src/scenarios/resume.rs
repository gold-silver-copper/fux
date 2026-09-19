//! Real-process native resume against a deterministic external provider, not an upstream
//! OpenCode compatibility claim. The sidecar speaks zor's documented JSONL adapter protocol.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::brp::Descriptor;
use super::stack::Stack;
use super::{
    Binaries, Fixture, Outcome, Result, TARGET, WAIT, err, live_attempt, nonce, require_target,
    until,
};

const SESSION: &str = "ses_native_resume_acceptance";
const TASK: &str = "native-resume";
const PROMPT: &str = "native-original-prompt";
const TEXT: &str = "NATIVE_RECEIPT_MARKER_DO_NOT_REPLAY";

// This executable is an external process on both sides of the product adapter: a raw PTY
// consumer, and a JSONL sidecar. A bound/report is emitted only after the PTY consumer has
// actually received the armed text. Gate files control natural exits, not product state.
const PROVIDER: &str = r#"#!/usr/bin/env python3
import json, os, pathlib, select, sys, time, tty
root = pathlib.Path(os.environ['NATIVE_EVIDENCE'])
session = os.environ['NATIVE_SESSION']
sidecar = len(sys.argv) > 1 and sys.argv[1] == '--provider'
role = 'provider' if sidecar else 'terminal'
kind = sys.argv[2] if sidecar else 'opencode'
pid = os.getpid()
log = root / (role + '-' + str(pid) + '.jsonl')
def record(event, **fields):
    with log.open('a') as f:
        f.write(json.dumps(dict(event=event, pid=pid, **fields)) + '\n')
def send(frame):
    record('out', frame=frame)
    print(json.dumps(frame), flush=True)
record('start', argv=sys.argv, cwd=os.getcwd(), storage={k: os.environ.get(k) for k in
    ['HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME']})
if not sidecar:
    if sys.argv[1:] not in ([], ['--session', session]):
        raise SystemExit('unexpected native session argv')
    tty.setraw(0)
    record('ready')
    while not (root / 'exit-terminal').exists():
        if select.select([0], [], [], 0.025)[0]:
            chunk = os.read(0, 65536)
            if not chunk:
                break
            record('input', hex=chunk.hex())
            with (root / ('input-' + str(pid) + '.bin')).open('ab') as f:
                f.write(chunk)
else:
    if kind == 'opencode':
        send(dict(t='hello', session=session))
    pending = None
    buffer = b''
    while not (root / 'exit-provider').exists():
        if select.select([0], [], [], 0.025)[0]:
            chunk = os.read(0, 65536)
            if not chunk:
                break
            buffer += chunk
            while b'\n' in buffer:
                line, buffer = buffer.split(b'\n', 1)
                frame = json.loads(line)
                record('in', frame=frame)
                if frame.get('t') == 'arm':
                    pending = frame
                elif frame.get('method') == 'initialize':
                    send(dict(id=frame['id'], result={}))
                elif frame.get('method') == 'thread/start':
                    send(dict(id=frame['id'], result=dict(thread=dict(id=session))))
        if pending and any(pending['text'].encode() in p.read_bytes() for p in root.glob('input-*.bin')):
            send(dict(t='bound', operation=pending['operation'], token=pending['token'],
                session=session, message='msg_acceptance_original'))
            send(dict(t='report', operation=pending['operation'], token=pending['token'], kind='response'))
            pending = None
record('exit', code=0)
"#;

fn save(root: &Path, name: &str, value: &Value) -> Result<()> {
    std::fs::write(root.join(name), serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn records(root: &Path, role: &str) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&format!("{role}-")) && name.ends_with(".jsonl"))
        {
            let bytes = std::fs::read(path)?;
            // A writer can be between write(2)s; only complete JSONL records are observable.
            for line in bytes.split_inclusive(|byte| *byte == b'\n') {
                if line.last() == Some(&b'\n') {
                    rows.push(serde_json::from_slice(line)?);
                }
            }
        }
    }
    Ok(rows)
}

fn starts(root: &Path, role: &str) -> Result<Vec<Value>> {
    let mut rows: Vec<Value> = records(root, role)?
        .into_iter()
        .filter(|row| row["event"] == "start")
        .collect();
    rows.sort_by_key(|row| row["pid"].as_u64());
    Ok(rows)
}

fn inspect(zor: &Descriptor, task: &str) -> Result<Value> {
    zor.call("zor/task.inspect", json!({"task": task}))
}

fn finished(zor: &Descriptor, task: &str, attempt: &Value) -> Result<Value> {
    until(WAIT, "verified terminal process exit", || {
        let view = inspect(zor, task)?;
        Ok(view["attempts"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| {
                row["attempt"] == attempt["attempt"]
                    && row["state"] == "finished"
                    && row.pointer("/final/exit_code") == Some(&json!(0))
            })
            .cloned())
    })
}

fn launch(
    stack: &Stack,
    root: &Path,
    executable: &Path,
    task: &str,
    kind: Option<&str>,
) -> Result<Value> {
    std::fs::create_dir_all(root)?;
    let cwd = stack.path().join("home").join(task);
    std::fs::create_dir_all(&cwd)?;
    let zor = stack.zor()?;
    zor.call(
        "zor/task.create",
        json!({"task": task, "title": task, "cwd": cwd}),
    )?;
    zor.call(
        "zor/task.launch",
        json!({
            "task": task, "operation": format!("launch-{task}"), "argv": [executable],
            "env": [["NATIVE_EVIDENCE", root], ["NATIVE_SESSION", SESSION]],
            "workspace": format!("workspace-{task}"), "ephemeral": true,
            "integration": kind.map(|kind| json!({
                "kind": if kind == "opencode" { "open_code" } else { kind },
                "argv": [executable, "--provider", kind],
            })),
        }),
    )?;
    let attempt = until(WAIT, "managed fixture child live", || {
        let view = inspect(&zor, task)?;
        Ok(view["attempts"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| row["state"] == "live" && row["pid"].as_u64().is_some())
            .cloned())
    })?;
    until(WAIT, "terminal fixture ready", || {
        Ok(records(root, "terminal")?
            .iter()
            .any(|row| row["event"] == "ready")
            .then_some(()))
    })?;
    if matches!(kind, Some("opencode" | "codex")) {
        until(WAIT, "native hello consumed by zor", || {
            let agents = zor.call("zor/agent.list", json!({}))?;
            Ok(agents["agents"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|row| row["task"] == task && row["last_event"] == "hello")
                .then_some(()))
        })?;
    }
    save(root, "launched.json", &attempt)?;
    Ok(attempt)
}

fn exit_children(
    zor: &Descriptor,
    root: &Path,
    task: &str,
    attempt: &Value,
    native: bool,
) -> Result<Value> {
    if native {
        std::fs::write(root.join("exit-provider"), b"exit naturally\n")?;
        until(WAIT, "provider positive exit observed by zor", || {
            let agents = zor.call("zor/agent.list", json!({}))?;
            Ok(agents["agents"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|row| row["task"] == task && row["last_event"] == "exited 0")
                .then_some(()))
        })?;
    }
    std::fs::write(root.join("exit-terminal"), b"exit naturally\n")?;
    let ended = finished(zor, task, attempt)?;
    save(root, "finished.json", &ended)?;
    Ok(ended)
}

fn refused(zor: &Descriptor, root: &Path, name: &str, request: Value) -> Result<()> {
    let task = request["task"]
        .as_str()
        .ok_or_else(|| err("resume task missing"))?;
    let before = inspect(zor, task)?;
    let terminal = starts(root, "terminal")?;
    let provider = starts(root, "provider")?;
    let failure = match zor.call("zor/task.resume", request.clone()) {
        Ok(value) => {
            return Err(err(format!(
                "{name}: resume unexpectedly accepted: {value}"
            )));
        }
        Err(error) => error.to_string(),
    };
    // Observe beyond a provider heartbeat, not merely the immediate rejection reply.
    let until_at = Instant::now() + Duration::from_secs(1);
    while Instant::now() < until_at {
        if inspect(zor, task)?["attempts"] != before["attempts"]
            || starts(root, "terminal")? != terminal
            || starts(root, "provider")? != provider
        {
            return Err(err(format!(
                "{name}: refused resume changed process or attempt identity"
            )));
        }
        std::thread::sleep(super::TICK);
    }
    save(
        root,
        &format!("refused-{name}.json"),
        &json!({"request": request, "error": failure, "before": before,
        "after": inspect(zor, task)?, "terminal_starts": terminal, "provider_starts": provider}),
    )
}

pub(super) fn native_resume(fixture: &mut Fixture) -> Result<Outcome> {
    let target = require_target(fixture)?;
    let root = fixture.artifacts.join(format!("native-resume-{}", nonce()));
    std::fs::create_dir_all(&root)?;
    save(
        &root,
        "coverage.json",
        &json!({
            "provider": "deterministic external executable; zor OpenCode JSONL sidecar protocol",
            "upstream_compatibility": "not claimed",
            "upstream_prerequisite": "A real OpenCode bridge translating native session/message ancestry into zor hello/arm/bound/report frames, plus a configured upstream service and session database. The upstream interactive CLI alone does not speak this sidecar protocol.",
        }),
    )?;
    let executable = root.join("opencode");
    std::fs::write(&executable, PROVIDER)?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
    let bins = Binaries {
        fux: fixture.local.fux_binary().to_path_buf(),
        zor: fixture.local.zor_binary().to_path_buf(),
        dir: fixture
            .local
            .fux_binary()
            .parent()
            .ok_or_else(|| err("binary directory"))?
            .to_path_buf(),
    };
    let mut stack = Stack::start("NativeResume", &bins)?;
    let original = root.join(TASK);
    let cases = [
        ("resume-claude", Some("claude")),
        ("resume-codex", Some("codex")),
        ("resume-no-provider", None),
        ("resume-unbound", Some("opencode")),
    ];
    let exercised = (|| -> Result<(u64, u64)> {
        let fux = stack.fux()?;
        let zor = stack.zor()?;
        let old = launch(&stack, &original, &executable, TASK, Some("opencode"))?;
        zor.call(
            "zor/task.prompt",
            json!({"task": TASK, "prompt": PROMPT, "text": TEXT, "timeout_ms": 15000}),
        )?;
        let prompt = until(WAIT, "real receipt and native session binding", || {
            let row = zor.call("zor/prompt.status", json!({"prompt": PROMPT}))?;
            Ok((row.pointer("/binding/session") == Some(&json!(SESSION))
                && row.pointer("/response/kind") == Some(&json!("response"))
                && row["receipt"]["instance"] == fux.instance
                && row["receipt"]["pane"] == old["pane"]
                && row["receipt"]["operation"] == row["binding"]["input_operation"])
                .then_some(row))
        })?;
        save(&original, "prompt-bound.json", &prompt)?;
        exit_children(&zor, &original, TASK, &old, true)?;
        let eligible = until(WAIT, "retired provider native resume authority", || {
            let status = zor.call("zor/task.resume-status", json!({"task": TASK}))?;
            Ok(
                (status["eligible"] == true && status["native_session"] == SESSION)
                    .then_some(status),
            )
        })?;
        save(&original, "eligible-before-restart.json", &eligible)?;
        stack.kill_zor()?;
        std::fs::copy(
            stack.path().join("state/zor/journal.scn.ron"),
            root.join("retained-journal.scn.ron"),
        )?;
        stack.start_zor()?;
        let zor = stack.zor()?;
        let restored = zor.call("zor/prompt.status", json!({"prompt": PROMPT}))?;
        if restored["receipt"] != prompt["receipt"]
            || restored["binding"] != prompt["binding"]
            || restored["response"] != prompt["response"]
        {
            return Err(err("restart changed retained receipt/native ancestry"));
        }
        let status = zor.call("zor/task.resume-status", json!({"task": TASK}))?;
        save(
            &original,
            "restored.json",
            &json!({"prompt": restored, "status": status}),
        )?;
        if status["eligible"] != true
            || status["previous_attempt"] != old["attempt"]
            || status["native_session"] != SESSION
            || stack.fux()?.instance != fux.instance
        {
            return Err(err(
                "journal restart lost native authority or changed fux incarnation",
            ));
        }
        refused(
            &zor,
            &original,
            "wrong-incarnation",
            json!({"task": TASK,
            "operation": "resume-wrong-incarnation", "fux_instance": "not-the-live-fux"}),
        )?;
        std::fs::remove_file(original.join("exit-provider"))?;
        std::fs::remove_file(original.join("exit-terminal"))?;
        let request = json!({"task": TASK, "operation": "resume-explicit-1", "fux_instance": fux.instance,
            "guard": {"attempt": null, "pane": null}});
        let resumed = zor.call("zor/task.resume", request.clone())?;
        let new = until(WAIT, "fresh resumed managed child", || {
            let view = inspect(&zor, TASK)?;
            Ok(view["attempts"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|row| row["attempt"] == resumed["attempt"] && row["state"] == "live")
                .cloned())
        })?;
        if new["attempt"] == old["attempt"]
            || new["pane"] == old["pane"]
            || new["pid"] == old["pid"]
        {
            return Err(err("resume did not create a distinct managed child"));
        }
        until(WAIT, "resumed external processes started", || {
            Ok((starts(&original, "terminal")?.len() == 2
                && starts(&original, "provider")?.len() == 2)
                .then_some(()))
        })?;
        let terminal = starts(&original, "terminal")?;
        let resumed_child = terminal
            .iter()
            .find(|row| row["argv"].as_array().is_some_and(|argv| argv.len() == 3))
            .ok_or_else(|| err("fresh child did not receive native session argv"))?;
        if resumed_child["argv"] != json!([executable, "--session", SESSION]) {
            return Err(err("resumed child received wrong native session argv"));
        }
        let initial_child = terminal
            .iter()
            .find(|row| row["argv"].as_array().is_some_and(|argv| argv.len() == 1))
            .ok_or_else(|| err("original external child missing"))?;
        if initial_child["storage"] != resumed_child["storage"]
            || initial_child["cwd"] != resumed_child["cwd"]
        {
            return Err(err("resume changed native storage namespace"));
        }
        let retry = zor.call("zor/task.resume", request.clone())?;
        if retry["attempt"] != resumed["attempt"] {
            return Err(err("same resume operation selected another attempt"));
        }
        let quiet = Instant::now() + Duration::from_secs(1);
        while Instant::now() < quiet {
            let arms = records(&original, "provider")?
                .into_iter()
                .filter(|row| row["event"] == "in" && row["frame"]["t"] == "arm")
                .count();
            let input = records(&original, "terminal")?
                .iter()
                .any(|row| row["event"] == "input" && row["pid"] == resumed_child["pid"]);
            if starts(&original, "terminal")?.len() != 2
                || starts(&original, "provider")?.len() != 2
                || arms != 1
                || input
            {
                return Err(err(
                    "resume/retry spawned again or replayed the original prompt",
                ));
            }
            std::thread::sleep(super::TICK);
        }
        save(
            &original,
            "resumed-and-retried.json",
            &json!({"request": request, "new": new,
            "retry": retry, "terminal_starts": terminal, "provider_starts": starts(&original, "provider")?}),
        )?;
        for (task, kind) in cases {
            let evidence = root.join(task);
            let attempt = launch(&stack, &evidence, &executable, task, kind)?;
            exit_children(
                &zor,
                &evidence,
                task,
                &attempt,
                matches!(kind, Some("opencode" | "codex")),
            )?;
            let status = zor.call("zor/task.resume-status", json!({"task": task}))?;
            save(&evidence, "ineligible.json", &status)?;
            if status["eligible"] != false {
                return Err(err(format!(
                    "{task}: unsupported/unbound resume reported eligible"
                )));
            }
            refused(
                &zor,
                &evidence,
                task,
                json!({"task": task, "operation": format!("refused-{task}"),
                "fux_instance": fux.instance, "guard": {"attempt": null, "pane": null}}),
            )?;
        }
        Ok((
            old["attempt"]
                .as_u64()
                .ok_or_else(|| err("old attempt ID"))?,
            new["attempt"]
                .as_u64()
                .ok_or_else(|| err("new attempt ID"))?,
        ))
    })();
    // Always request natural exits; Stack's bounded Drop also owns both servers and groups
    // if a BRP assertion or fixture interpreter failed before these gates were reached.
    for task in std::iter::once(TASK).chain(cases.iter().map(|(task, _)| *task)) {
        let evidence = root.join(task);
        if evidence.is_dir() {
            let _ = std::fs::write(evidence.join("exit-provider"), b"cleanup\n");
            let _ = std::fs::write(evidence.join("exit-terminal"), b"cleanup\n");
        }
    }
    for name in ["fux-serve.log", "zor-serve.log"] {
        let _ = std::fs::copy(stack.path().join(name), root.join(name));
    }
    drop(stack);
    if live_attempt(fixture, TARGET)? != Some(target) {
        return Err(err(
            "native resume scenario changed unrelated Remote target",
        ));
    }
    let (old, new) = exercised?;
    Ok(Outcome::Pass(format!(
        "native fixture {old} -> {new}; retained session, no replay, stable retry; Claude/Codex/no authority refused; upstream service compatibility not claimed"
    )))
}
