//! Bounded generic invalidation streams. Events never establish agent or task outcomes.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
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
struct Peer {
    socket: UnixStream,
    boundary: Boundary,
    buffer: Vec<u8>,
}
impl Peer {
    fn open(path: &Path, boundary: &Boundary, deadline: Instant) -> Result<Self> {
        let mut socket = crate::fux::control(path, deadline).context("negotiate event control")?;
        socket
            .set_write_timeout(Some(
                deadline
                    .checked_duration_since(Instant::now())
                    .context("subscription deadline")?,
            ))
            .context("set subscription write timeout")?;
        writeln!(
            socket,
            "{}",
            json!({"id":1,"command":"subscribe","instance":boundary.instance,"after":boundary.cursor})
        )?;
        socket
            .set_nonblocking(true)
            .context("nonblocking subscription")?;
        let mut peer = Self {
            socket,
            boundary: boundary.clone(),
            buffer: Vec::new(),
        };
        loop {
            if let Some(frame) = peer.frame()? {
                anyhow::ensure!(
                    frame.get("id").and_then(Value::as_u64) == Some(1)
                        && frame.get("status").and_then(Value::as_str) == Some("accepted"),
                    "subscription rejected (gap or incompatible response)"
                );
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
        Ok(n)
    }
    fn frame(&mut self) -> Result<Option<Value>> {
        if let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            anyhow::ensure!(end <= MAX_FRAME, "event frame limit exceeded");
            let frame =
                serde_json::from_slice(self.buffer.get(..end).context("event frame range")?)?;
            self.buffer.drain(..=end);
            Ok(Some(frame))
        } else {
            anyhow::ensure!(self.buffer.len() <= MAX_FRAME, "event frame limit exceeded");
            Ok(None)
        }
    }
    fn event(&mut self, frame: Value) -> Result<()> {
        anyhow::ensure!(
            frame.get("id").and_then(Value::as_u64) == Some(1),
            "event subscription ID changed"
        );
        let kind = frame
            .get("event")
            .and_then(Value::as_str)
            .context("missing event type")?;
        anyhow::ensure!(
            matches!(
                kind,
                "pane.opened"
                    | "pane.closed"
                    | "pane.title"
                    | "pane.output"
                    | "tab.opened"
                    | "tab.closed"
                    | "client.attached"
                    | "client.detached"
                    | "workspace.changed"
            ),
            "unknown event type"
        );
        let cursor: Cursor =
            serde_json::from_value(frame.get("cursor").context("missing event cursor")?.clone())?;
        anyhow::ensure!(
            cursor.stream == self.boundary.cursor.stream
                && self.boundary.cursor.sequence.checked_add(1) == Some(cursor.sequence),
            "event cursor gap, duplicate or reordered frame"
        );
        self.boundary.cursor = cursor;
        Ok(())
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
                    .and_then(|b| Peer::open(&runtime.join(format!("{name}.sock")), b, deadline))
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
                            peer.event(frame)?;
                            changed.entry(name.clone()).or_insert_with(|| {
                                "workspace event; observation refresh pending".into()
                            });
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
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
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
            json!({"id":1,"event":"agent.done","cursor":{"stream":7,"sequence":12}}),
        ] {
            assert!(peer.event(malformed).is_err());
        }
        assert_eq!(peer.boundary.cursor.sequence, 11);
        peer.event(event(12)).expect("correct event after refusal");
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
                        std::thread::sleep(Duration::from_millis(1))
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
#[allow(clippy::expect_used)]
mod overdue_tests {
    use super::*;
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
                            std::thread::sleep(Duration::from_millis(1))
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
