//! Isolated controller and real koh forwarding for the resume composition fixture.
use crate::support::{
    local::{Root, until},
    process::{self, Guard},
    terminal::Terminal,
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

pub(super) struct Remote {
    // Reap the endpoint before deleting credentials and runtime files.
    gateway: Guard,
    gate: Option<super::resume_reply_gate::Gate>,
    root: Root,
    zor: PathBuf,
    koh: PathBuf,
}

fn output(root: &Root, binary: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = root.command(binary);
    command.args(args);
    // No automatic replay: this also runs mutation commands.
    let reply = process::output(command, Duration::from_secs(15), 1024 * 1024)?;
    ensure!(
        reply.status.success(),
        "remote fixture {args:?}: {}",
        String::from_utf8_lossy(&reply.stderr)
    );
    Ok(reply.stdout)
}

impl Remote {
    pub(super) fn start(zor: &Path, koh: &Path, socket: &Path, lose_reply: bool) -> Result<Self> {
        let mut root = Root::new("zresume-controller-", &["/bin/cat".into()])?;
        for name in ["KOH_KEY_PASSPHRASE", "KOH_KEY_NEW_PASSPHRASE"] {
            root.env
                .insert(name.into(), "disposable-resume-fixture".into());
        }
        let key = root.path().join("client.key");
        let key_text = key.to_str().context("key path")?;
        let identity = String::from_utf8(output(&root, koh, &["id", "--key-file", key_text])?)?;
        let ad = root.path().join("advertisement.json");
        let log = root.path().join("gateway.log");
        let gate_path = root.path().join("reply-gate.sock");
        let gate = lose_reply
            .then(|| super::resume_reply_gate::Gate::start(&gate_path, socket))
            .transpose()?;
        let socket = if gate.is_some() {
            gate_path.as_path()
        } else {
            socket
        };
        let mut gateway = Guard(
            root.command(koh)
                .args(["gateway", "serve", "--local", "--socket"])
                .arg(socket)
                .arg("--key-file")
                .arg(root.path().join("server.key"))
                .args(["--allow", identity.trim()])
                .stdin(Stdio::null())
                .stdout(fs::File::create(&ad)?)
                .stderr(fs::File::create(&log)?)
                .spawn()?,
        );
        let advertisement: Value = until(Duration::from_secs(10), || {
            ensure!(
                gateway.0.try_wait()?.is_none(),
                "gateway exited: {}",
                fs::read_to_string(&log)?
            );
            ensure!(fs::metadata(&ad)?.len() <= 65536, "advertisement too large");
            let bytes = fs::read(&ad)?;
            Ok(if bytes.ends_with(b"\n") {
                Some(serde_json::from_slice(&bytes)?)
            } else {
                None
            })
        })?;
        output(
            &root,
            zor,
            &[
                "machine",
                "add",
                "resume-host",
                "--endpoint",
                advertisement["endpoint_id"]
                    .as_str()
                    .context("endpoint ID")?,
                "--key-file",
                key_text,
                "--direct",
                advertisement["direct_addr"]
                    .as_str()
                    .context("direct address")?,
            ],
        )?;
        Ok(Self {
            gateway,
            gate,
            root,
            zor: zor.into(),
            koh: koh.into(),
        })
    }

    /// Drive the interactive dashboard's `u` control for one successful guarded resume:
    /// machine-scoped entry, selection by task identity, the typed operation/incarnation
    /// form, one dispatch, the returned detail, and a clean quit. Returns the inspection.
    pub(super) fn dashboard_resume(
        &mut self,
        fux: &Path,
        operation: &str,
        instance: &str,
    ) -> Result<Value> {
        ensure!(self.gateway.0.try_wait()?.is_none(), "gateway exited");
        let koh = self.koh.to_str().context("koh path")?.to_owned();
        let view: Value = serde_json::from_slice(&output(
            &self.root,
            &self.zor,
            &["--machine", "resume-host", "--koh-binary", &koh, "dashboard", "--once"],
        )?)?;
        let index = view["view"]["rows"]
            .as_array()
            .context("rows")?
            .iter()
            .position(|row| row["expected"]["task"] == "worker")
            .context("worker task row missing")?;
        let wait = Duration::from_secs(15);
        let mut terminal = Terminal::start_with_size(
            &self.root,
            &self.zor,
            &[
                "--koh-binary",
                &koh,
                "--fux-binary",
                fux.to_str().context("fux path")?,
                "--machine",
                "resume-host",
                "dashboard",
            ],
            30,
            180,
        )?;
        terminal.wait_for("resume-host:live", wait)?;
        terminal.wait_for("zor dashboard | resume-host", wait)?;
        let mut mark = terminal.raw_len()?;
        for _ in 0..index {
            terminal.send(b"j")?;
        }
        terminal.wait_for_since("> resume-host", mark, wait)?;
        mark = terminal.raw_len()?;
        terminal.send(b"u")?;
        terminal.wait_for_since("Resume selected task", mark, wait)?;
        terminal.send(format!("{operation} {instance}").as_bytes())?;
        terminal.wait_for_since(instance, mark, wait)?;
        terminal.checkpoint("remote-resume-form")?;
        mark = terminal.raw_len()?;
        terminal.send(b"\r")?;
        // One guarded dispatch; the detail view shows the resumed task once the service
        // replies. The header and the finished new attempt are visible at the top; the
        // long launch JSON (phase closed) scrolls below the fold and is asserted through
        // the service inspection this function returns.
        terminal.wait_for_since("resume-host / worker / attempt", mark, Duration::from_secs(30))?;
        terminal.wait_for_since("\"state\": \"finished\"", mark, wait)?;
        terminal.checkpoint("remote-resume-success")?;
        ensure!(
            !terminal.raw_contains_since(b"outcome unconfirmed", mark)?
                && !terminal.raw_contains_since(b"action failed", mark)?
                && !terminal.raw_contains_since(b"Resume unavailable", mark)?,
            "dashboard resume reported a failure"
        );
        mark = terminal.raw_len()?;
        terminal.send(b"\x1b")?;
        terminal.wait_for_since("zor dashboard | resume-host", mark, wait)?;
        terminal.send(b"q")?;
        let status = terminal.wait(Duration::from_secs(10))?;
        ensure!(status.success(), "dashboard exited with {status}");
        let intents = self.retained_intents()?;
        let intent = intents
            .as_array()
            .filter(|items| items.len() == 1)
            .and_then(|items| items.first())
            .context("exactly one durable dashboard intent expected")?;
        ensure!(
            intent["operation"] == operation
                && intent["fux_instance"] == instance
                && intent["expected"]["task"] == "worker",
            "dashboard intent mismatch: {intents}"
        );
        let inspection = self.resume(&["inspect", "worker"])?;
        ensure!(
            inspection["launch"]["phase"] == "closed"
                && inspection["attempt"]["state"] == "finished",
            "dashboard resume did not reach a closed launch: {inspection}"
        );
        Ok(inspection)
    }

    pub(super) fn retained_intents(&self) -> Result<Value> {
        Ok(serde_json::from_slice(&output(
            &self.root,
            &self.zor,
            &["machine", "resume-intents"],
        )?)?)
    }
    pub(super) fn verify_lost_reply(&self, mutations: usize) -> Result<()> {
        self.gate
            .as_ref()
            .context("reply gate missing")?
            .verify(mutations)
    }
    pub(super) fn finish_gate(&mut self) -> Result<()> {
        if let Some(gate) = &mut self.gate {
            gate.finish()?;
        }
        Ok(())
    }
    pub(super) fn resume(&mut self, args: &[&str]) -> Result<Value> {
        ensure!(self.gateway.0.try_wait()?.is_none(), "gateway exited");
        let mut remote = vec![
            "--machine",
            "resume-host",
            "--koh-binary",
            self.koh.to_str().context("koh path")?,
            "task",
        ];
        remote.extend_from_slice(args);
        let result = output(&self.root, &self.zor, &remote);
        if let Some(gate) = &mut self.gate {
            gate.check()?;
        }
        let reply: Value = serde_json::from_slice(&result?)?;
        ensure!(
            !self.root.path().join("state/zor/journal.json").exists(),
            "remote resume created a controller task store"
        );
        ensure!(
            reply["value"].is_object(),
            "remote resume envelope: {reply}"
        );
        Ok(reply["value"].clone())
    }
}
