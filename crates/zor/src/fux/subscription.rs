//! Bounded generic invalidation streams. Events never establish agent or task outcomes.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Read,
    os::{fd::AsFd, unix::net::UnixStream},
    path::Path,
    time::{Duration, Instant},
};

pub(crate) const RESCAN: Duration = Duration::from_secs(3);
const COALESCE: Duration = Duration::from_millis(100);
const MAX_FRAME: usize = 65536;
const READ_BUDGET: usize = 262144;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cursor {
    pub stream: u64,
    pub sequence: u64,
}
#[derive(Clone, Debug)]
pub(crate) struct Boundary {
    pub instance: String,
    pub cursor: Cursor,
}
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
enum Acceptance {
    Accepted {
        id: u64,
    },
    Failed {
        id: u64,
        error: super::error::RemoteFailure,
    },
}

fn accept(frame: Value) -> Result<()> {
    super::error::reply(|| {
        match serde_json::from_value(frame).context("invalid subscription acceptance")? {
            Acceptance::Accepted { id } => {
                anyhow::ensure!(id == 1, "subscription request ID mismatch");
                Ok(())
            }
            Acceptance::Failed { id, error } => {
                anyhow::ensure!(id == 1, "subscription refusal ID mismatch");
                Err(error.into())
            }
        }
    })
}

struct Peer {
    socket: UnixStream,
    boundary: Boundary,
    buffer: Vec<u8>,
    partial_since: Option<Instant>,
}
impl Peer {
    fn open(path: &Path, boundary: &Boundary, deadline: Instant) -> Result<Self> {
        let mut socket = crate::fux::control(path, deadline).context("negotiate event control")?;
        let mut request = serde_json::to_vec(
            &json!({"id":1,"command":"subscribe","instance":boundary.instance,"after":boundary.cursor}),
        )?;
        request.push(b'\n');
        local_ipc::write_all_until(&mut socket, &request, deadline)?;
        socket
            .set_nonblocking(true)
            .context("nonblocking subscription")?;
        let mut peer = Self {
            socket,
            boundary: boundary.clone(),
            buffer: Vec::new(),
            partial_since: None,
        };
        loop {
            if let Some(frame) = peer.frame()? {
                accept(frame)?;
                return Ok(peer);
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .context("subscription acceptance timed out")?;
            let mut poll = [nix::poll::PollFd::new(
                peer.socket.as_fd(),
                nix::poll::PollFlags::POLLIN,
            )];
            nix::poll::poll(
                &mut poll,
                u16::try_from(remaining.as_millis().clamp(1, 2000))?,
            )?;
            peer.read()?;
        }
    }
    fn read(&mut self) -> Result<usize> {
        self.check_partial_deadline()?;
        let mut bytes = [0; 8192];
        let n = match self.socket.read(&mut bytes) {
            Ok(0) => anyhow::bail!("event stream closed; continuity lost"),
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(0);
            }
            Err(e) => return Err(e.into()),
        };
        anyhow::ensure!(
            self.buffer.len() + n <= MAX_FRAME + bytes.len(),
            "event frame limit exceeded"
        );
        self.buffer
            .extend_from_slice(bytes.get(..n).context("event read size")?);
        if self.buffer.contains(&b'\n') {
            self.partial_since = None;
        } else if self.partial_since.is_none() {
            self.partial_since = Some(Instant::now());
        }
        Ok(n)
    }
    fn check_partial_deadline(&self) -> Result<()> {
        anyhow::ensure!(
            self.partial_since
                .is_none_or(|start| start.elapsed() < Duration::from_secs(2)),
            "partial event frame deadline exceeded"
        );
        Ok(())
    }
    fn frame(&mut self) -> Result<Option<Value>> {
        self.check_partial_deadline()?;
        if let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            anyhow::ensure!(end <= MAX_FRAME, "event frame limit exceeded");
            let frame =
                serde_json::from_slice(self.buffer.get(..end).context("event frame range")?)?;
            self.buffer.drain(..=end);
            self.partial_since =
                (!self.buffer.is_empty() && !self.buffer.contains(&b'\n')).then(Instant::now);
            Ok(Some(frame))
        } else {
            anyhow::ensure!(self.buffer.len() <= MAX_FRAME, "event frame limit exceeded");
            Ok(None)
        }
    }
    /// Validates one frame and advances the cursor. Returns whether the event is one zor
    /// observes; a kind this version does not know is ignored (fux may add event kinds), but
    /// its cursor still counts toward continuity.
    fn event(&mut self, frame: Value) -> Result<bool> {
        anyhow::ensure!(
            frame.get("id").and_then(Value::as_u64) == Some(1),
            "event subscription ID changed"
        );
        let kind = frame
            .get("event")
            .and_then(Value::as_str)
            .context("missing event type")?;
        let observed = matches!(
            kind,
            "pane.opened"
                | "pane.closed"
                | "pane.output"
                | "tab.opened"
                | "tab.closed"
                | "workspace.changed"
        );
        if observed {
            let _: super::events::Event =
                serde_json::from_value(frame.clone()).context("malformed known event")?;
        }
        let cursor: Cursor =
            serde_json::from_value(frame.get("cursor").context("missing event cursor")?.clone())?;
        anyhow::ensure!(
            cursor.stream == self.boundary.cursor.stream
                && self.boundary.cursor.sequence.checked_add(1) == Some(cursor.sequence),
            "event cursor gap, duplicate or reordered frame"
        );
        self.boundary.cursor = cursor;
        Ok(observed)
    }
}

#[derive(Default)]
pub(crate) struct Events {
    peers: BTreeMap<String, Peer>,
    next: usize,
    next_read: usize,
    failures: u64,
}
impl Events {
    pub fn count(&self) -> usize {
        self.peers.len()
    }
    pub fn failures(&self) -> u64 {
        self.failures
    }
    fn failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }

    /// Subscribe from each authoritative listing boundary. Existing matching streams keep
    /// their own last processed cursor; a new listing must not skip still-buffered events.
    pub fn sync(
        &mut self,
        runtime: &Path,
        boundaries: &BTreeMap<String, Boundary>,
        deadline: Instant,
    ) -> BTreeMap<String, String> {
        self.peers.retain(|name, peer| {
            boundaries.get(name).is_some_and(|b| {
                b.instance == peer.boundary.instance
                    && b.cursor.stream == peer.boundary.cursor.stream
            })
        });
        let mut names: Vec<_> = boundaries.keys().cloned().collect();
        if !names.is_empty() {
            let offset = self.next % names.len();
            names.rotate_left(offset);
            self.next = offset + 1;
        }
        let mut problems = BTreeMap::new();
        for name in names {
            if self.peers.contains_key(&name) {
                continue;
            }
            let result = if Instant::now() >= deadline {
                Err(anyhow::anyhow!("subscription setup budget exhausted"))
            } else if self.peers.len() >= 64 {
                Err(anyhow::anyhow!("event workspace capacity exceeded"))
            } else {
                boundaries
                    .get(&name)
                    .context("missing listing boundary")
                    .and_then(|b| {
                        Peer::open(
                            &super::endpoint::Endpoint::new(runtime).workspace(&name)?,
                            b,
                            deadline,
                        )
                    })
            };
            match result {
                Ok(peer) => {
                    self.peers.insert(name, peer);
                }
                Err(error) => {
                    self.failure();
                    problems.insert(
                        name,
                        format!("event synchronization unavailable: {error}")
                            .chars()
                            .take(256)
                            .collect(),
                    );
                }
            }
        }
        problems
    }

    /// Poll with bounded work and coalescing anchored to the first dirty event. Returning
    /// changes asks the caller to invalidate and re-list, never to apply an agent verdict.
    pub fn wait(
        &mut self,
        until: Instant,
        interrupted: impl Fn() -> bool,
    ) -> BTreeMap<String, String> {
        let mut changed = BTreeMap::new();
        let mut deadline = until;
        let mut first = true;
        let mut initial_drain = true;
        while (initial_drain || Instant::now() < deadline) && !interrupted() {
            initial_drain = false;
            let mut polls: Vec<_> = self
                .peers
                .values()
                .map(|p| nix::poll::PollFd::new(p.socket.as_fd(), nix::poll::PollFlags::POLLIN))
                .collect();
            let remaining = deadline.saturating_duration_since(Instant::now());
            let timeout = u16::try_from(remaining.as_millis().clamp(1, 100)).unwrap_or(100);
            let buffered = self.peers.values().any(|p| p.buffer.contains(&b'\n'));
            let result = nix::poll::poll(
                &mut polls,
                if buffered || Instant::now() >= deadline {
                    0
                } else {
                    timeout
                },
            );
            drop(polls);
            if result.is_err_and(|e| e != nix::errno::Errno::EINTR) {
                for name in self.peers.keys() {
                    changed.insert(name.clone(), "event polling failed; continuity lost".into());
                }
                self.peers.clear();
                self.failure();
                break;
            }
            let mut failed = Vec::new();
            let mut bytes = 0;
            let mut names: Vec<_> = self.peers.keys().cloned().collect();
            if !names.is_empty() {
                let offset = self.next_read % names.len();
                names.rotate_left(offset);
                self.next_read = offset + 1;
            }
            for name in names {
                let Some(peer) = self.peers.get_mut(&name) else {
                    continue;
                };
                let result = (|| -> Result<()> {
                    for _ in 0..32 {
                        if bytes >= READ_BUDGET {
                            break;
                        }
                        if let Some(frame) = peer.frame()? {
                            if peer.event(frame)? {
                                changed.entry(name.clone()).or_insert_with(|| {
                                    "workspace event; observation refresh pending".into()
                                });
                            }
                        } else {
                            let n = peer.read()?;
                            bytes += n;
                            if n == 0 {
                                break;
                            }
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    changed.insert(
                        name.clone(),
                        format!("event continuity lost: {error}")
                            .chars()
                            .take(256)
                            .collect(),
                    );
                    failed.push(name.clone());
                }
            }
            for name in failed {
                self.peers.remove(&name);
                self.failure();
            }
            if first && !changed.is_empty() {
                deadline = deadline.min(Instant::now() + COALESCE);
                first = false;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn subscription_refusal_is_distinct_from_malformed_acceptance() -> Result<()> {
        use super::super::error::{Kind, kind, remote_code};
        accept(json!({"status":"accepted","id":1}))?;
        let refused =
            accept(json!({"status":"failed","id":1,"error":{"code":"gap","message":"expired"}}))
                .err()
                .context("refusal")?;
        assert_eq!(kind(&refused), Some(Kind::RemoteFailure));
        assert_eq!(remote_code(&refused), Some("gap"));
        for frame in [
            json!({"status":"accepted","id":2}),
            json!({"status":"failed","id":2,"error":{"code":"gap","message":"expired"}}),
            json!({"status":"failed","id":1,"error":{"code":"gap"}}),
            json!({"status":"completed","id":1,"result":{"kind":"unit"}}),
        ] {
            let error = accept(frame).err().context("malformed acceptance")?;
            assert_eq!(kind(&error), Some(Kind::MalformedReply));
            assert_eq!(remote_code(&error), None);
        }
        Ok(())
    }

    #[test]
    fn partial_frame_deadline_does_not_expire_idle_or_complete_buffered_frames() -> Result<()> {
        let (mut peer, mut writer) = peer();
        assert_eq!(peer.read()?, 0);
        assert!(
            peer.partial_since.is_none(),
            "idle subscriptions have no partial deadline"
        );
        writer.write_all(b"{\"id\":1")?;
        assert!(peer.read()? > 0);
        let started = peer.partial_since.context("partial start")?;
        writer.write_all(b" ")?;
        peer.read()?;
        assert_eq!(
            peer.partial_since,
            Some(started),
            "progress renewed deadline"
        );
        peer.partial_since = Some(Instant::now() - Duration::from_secs(3));
        writer.write_all(b"}\n")?;
        assert!(peer.read().is_err(), "late completion bypassed deadline");
        assert!(peer.frame().is_err());

        let (mut complete, mut writer) = self::peer();
        writer.write_all(b"{\"id\":1}\n{\"id\":2}\n")?;
        complete.read()?;
        assert!(complete.partial_since.is_none());
        complete.frame()?;
        assert!(
            complete.partial_since.is_none(),
            "already-complete replay was timed as partial"
        );
        assert!(complete.frame()?.is_some());
        Ok(())
    }
    fn peer() -> (Peer, UnixStream) {
        let (socket, writer) = UnixStream::pair().expect("socket pair");
        socket.set_nonblocking(true).expect("nonblocking");
        (
            Peer {
                socket,
                boundary: Boundary {
                    instance: "fixture".into(),
                    cursor: Cursor {
                        stream: 7,
                        sequence: 10,
                    },
                },
                buffer: Vec::new(),
                partial_since: None,
            },
            writer,
        )
    }
    fn event(sequence: u64) -> Value {
        json!({"id":1,"event":"workspace.changed","cursor":{"stream":7,"sequence":sequence}})
    }

    #[test]
    fn event_identity_requires_contiguous_unfiltered_history() {
        let (mut peer, _writer) = peer();
        peer.event(event(11)).expect("next event");
        assert!(peer.event(event(11)).is_err());
        assert!(peer.event(event(13)).is_err());
        for malformed in [
            json!({"id":2,"event":"workspace.changed","cursor":{"stream":7,"sequence":12}}),
            json!({"id":1,"event":"workspace.changed","cursor":{"stream":8,"sequence":12}}),
            json!({"id":1,"cursor":{"stream":7,"sequence":12}}),
        ] {
            assert!(peer.event(malformed).is_err());
        }
        assert_eq!(peer.boundary.cursor.sequence, 11);
        assert!(peer.event(event(12)).expect("correct event after refusal"));
    }
    #[test]
    fn unknown_event_kinds_are_ignored_but_keep_continuity() {
        let (mut peer, _writer) = peer();
        assert!(peer.event(event(11)).expect("known"));
        // A kind added by a newer fux is not an observation, and not a failure either.
        let unknown = |sequence: u64| json!({"id":1,"event":"pane.future","cursor":{"stream":7,"sequence":sequence}});
        assert!(!peer.event(unknown(12)).expect("unknown kind tolerated"));
        assert_eq!(peer.boundary.cursor.sequence, 12);
        // Its cursor still counts: a gap after it is a failure, and the next known event
        // must follow it exactly.
        assert!(peer.event(unknown(14)).is_err());
        assert!(peer.event(event(12)).is_err());
        assert!(peer.event(event(13)).expect("known after unknown"));
        assert!(!peer.event(unknown(14)).expect("unknown again"));
        assert_eq!(peer.boundary.cursor.sequence, 14);
    }
    #[test]
    fn frames_preserve_fragmentation_and_reject_oversize_or_malformed_json() {
        let (mut peer, _writer) = peer();
        peer.buffer.extend_from_slice(b"{\"id\":1}");
        assert!(peer.frame().expect("partial").is_none());
        peer.buffer.extend_from_slice(b"\n{\"id\":2}\n");
        assert_eq!(peer.frame().expect("first"), Some(json!({"id":1})));
        assert_eq!(peer.frame().expect("second"), Some(json!({"id":2})));
        peer.buffer = vec![b'x'; MAX_FRAME + 1];
        assert!(peer.frame().is_err());
        peer.buffer = b"not json\n".to_vec();
        assert!(peer.frame().is_err());
    }
    #[test]
    fn buffered_replay_invalidates_and_disconnect_discards_continuity() {
        let (mut peer, writer) = peer();
        peer.buffer = format!("{}\n{}\n", event(11), event(12)).into_bytes();
        let mut events = Events::default();
        events.peers.insert("default".into(), peer);
        let changed = events.wait(Instant::now() + Duration::from_secs(1), || false);
        assert!(changed.contains_key("default"));
        assert_eq!(
            events
                .peers
                .get("default")
                .expect("live peer")
                .boundary
                .cursor
                .sequence,
            12
        );
        drop(writer);
        let changed = events.wait(Instant::now() + Duration::from_secs(1), || false);
        assert!(
            changed
                .get("default")
                .expect("loss")
                .contains("continuity lost")
        );
        assert_eq!(events.count(), 0);
        assert_eq!(events.failures(), 1);
    }
    #[test]
    fn poll_can_be_interrupted_without_waiting_for_fallback_scan() {
        let mut events = Events::default();
        let start = Instant::now();
        assert!(events.wait(start + RESCAN, || true).is_empty());
        assert!(start.elapsed() < Duration::from_millis(100));
    }
    #[test]
    fn subscription_keeps_replay_buffered_with_the_acceptance() {
        use std::os::unix::net::UnixListener;
        let directory = tempfile::tempdir_in("/tmp").expect("private fixture");
        let path = directory.path().join("stream.sock");
        let listener = UnixListener::bind(&path).expect("listener");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let (done, finish) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || -> Result<()> {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut peer = loop {
                match listener.accept() {
                    Ok((peer, _)) => break peer,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => return Err(e.into()),
                }
            };
            peer.set_nonblocking(false)?;
            peer.set_read_timeout(Some(Duration::from_secs(2)))?;
            peer.set_write_timeout(Some(Duration::from_secs(2)))?;
            let mut preface = [0; 4];
            peer.read_exact(&mut preface)?;
            anyhow::ensure!(&preface == b"FUX\n", "preface");
            peer.write_all(&preface)?;
            let mut line = Vec::new();
            while !line.ends_with(b"\n") {
                let mut byte = [0];
                peer.read_exact(&mut byte)?;
                line.extend_from_slice(&byte);
                anyhow::ensure!(line.len() < 1024, "request cap");
            }
            let value: Value = serde_json::from_slice(&line)?;
            anyhow::ensure!(
                value.get("after") == Some(&json!({"stream":7,"sequence":10})),
                "snapshot cursor"
            );
            write!(
                peer,
                "{}\n{}\n",
                json!({"id":1,"status":"accepted"}),
                event(11)
            )?;
            let _ = finish.recv_timeout(Duration::from_secs(2));
            Ok(())
        });
        let boundary = Boundary {
            instance: "fixture".into(),
            cursor: Cursor {
                stream: 7,
                sequence: 10,
            },
        };
        let mut peer = Peer::open(&path, &boundary, Instant::now() + Duration::from_secs(2))
            .expect("subscribe");
        // Acceptance and replay may share one read or arrive separately.
        let until = Instant::now() + Duration::from_secs(1);
        let frame = loop {
            if let Some(frame) = peer.frame().expect("frame") {
                break frame;
            }
            assert!(Instant::now() < until);
            peer.read().expect("read");
            std::thread::sleep(Duration::from_millis(1));
        };
        peer.event(frame).expect("replay event");
        assert_eq!(peer.boundary.cursor.sequence, 11);
        let _ = done.send(());
        server
            .join()
            .expect("server thread")
            .expect("server result");
    }
}

#[cfg(test)]
mod overdue_tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn stalled_setup_cannot_skip_the_nonblocking_drain_of_healthy_streams() {
        use std::os::unix::net::UnixListener;
        let directory = tempfile::tempdir_in("/tmp").expect("fixture");
        let listener = UnixListener::bind(directory.path().join("stalled.sock")).expect("listener");
        listener.set_nonblocking(true).expect("nonblocking");
        let server = std::thread::spawn(move || -> Result<()> {
            let end = Instant::now() + Duration::from_secs(3);
            for _ in 0..2 {
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < end =>
                        {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(e) => return Err(e.into()),
                    }
                };
                socket.set_nonblocking(false)?;
                socket.set_read_timeout(Some(Duration::from_secs(1)))?;
                socket.set_write_timeout(Some(Duration::from_secs(1)))?;
                let mut preface = [0; 4];
                socket.read_exact(&mut preface)?;
                socket.write_all(&preface)?;
                // Read and discard request bytes until client deadline closes the stream.
                let mut bytes = [0; 1024];
                while socket.read(&mut bytes)? != 0 {}
            }
            Ok(())
        });
        let boundary = Boundary {
            instance: "fixture".into(),
            cursor: Cursor {
                stream: 7,
                sequence: 10,
            },
        };
        let (socket, mut writer) = UnixStream::pair().expect("pair");
        socket.set_nonblocking(true).expect("nonblocking");
        let mut events = Events::default();
        events.peers.insert(
            "healthy".into(),
            Peer {
                socket,
                boundary: boundary.clone(),
                buffer: Vec::new(),
                partial_since: None,
            },
        );
        let boundaries = BTreeMap::from([
            ("healthy".into(), boundary.clone()),
            ("stalled".into(), boundary),
        ]);
        for sequence in [11, 12] {
            let expired = Instant::now();
            let problems = events.sync(
                directory.path(),
                &boundaries,
                Instant::now() + Duration::from_millis(100),
            );
            assert!(problems.contains_key("stalled"));
            writeln!(writer, "{}", json!({"id":1,"event":"workspace.changed","cursor":{"stream":7,"sequence":sequence}})).expect("event");
            let changed = events.wait(expired, || false);
            assert!(changed.contains_key("healthy"));
            assert_eq!(
                events
                    .peers
                    .get("healthy")
                    .expect("healthy")
                    .boundary
                    .cursor
                    .sequence,
                sequence
            );
        }
        server.join().expect("server").expect("server result");
    }
}
