//! Bounded JSON-line service fixture connection; no retry or agent policy.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    path::Path,
    time::{Duration, Instant},
};

pub struct Peer {
    reader: BufReader<UnixStream>,
    timeout: Duration,
}
impl Peer {
    pub fn send(path: &Path, value: &Value, timeout: Duration) -> Result<Self> {
        let mut peer = super::local::connect(path, Instant::now() + timeout)
            .context("service fixture connect")?;
        peer.set_read_timeout(Some(timeout))?;
        peer.set_write_timeout(Some(timeout))?;
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        peer.write_all(&bytes).context("service fixture send")?;
        Ok(Self {
            reader: BufReader::new(peer),
            timeout,
        })
    }
    pub fn receive(self) -> Result<Value> {
        let deadline = Instant::now() + self.timeout;
        self.receive_before(deadline)
    }
    pub fn receive_before(mut self, deadline: Instant) -> Result<Value> {
        self.reader.get_ref().set_nonblocking(true)?;
        let mut bytes = Vec::new();
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
                .context("service reply deadline")?;
            let chunk = match self.reader.fill_buf() {
                Ok(chunk) => chunk,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    let mut polls = [nix::poll::PollFd::new(
                        self.reader.get_ref().as_fd(),
                        nix::poll::PollFlags::POLLIN,
                    )];
                    let milliseconds = u16::try_from(left.as_millis().clamp(1, 2000))?;
                    match nix::poll::poll(&mut polls, milliseconds) {
                        Ok(_) | Err(nix::errno::Errno::EINTR) => continue,
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            };
            ensure!(!chunk.is_empty(), "service reply EOF");
            let end = chunk.iter().position(|byte| *byte == b'\n');
            let count = end.map_or(chunk.len(), |end| end + 1);
            ensure!(bytes.len() + count <= 512 * 1024, "service reply bound");
            bytes.extend_from_slice(&chunk[..count]);
            self.reader.consume(count);
            if end.is_some() {
                return Ok(serde_json::from_slice(&bytes)?);
            }
        }
    }
    pub fn ready(peers: &[Self], milliseconds: u16) -> Result<Vec<usize>> {
        let mut polls: Vec<_> = peers
            .iter()
            .map(|p| {
                nix::poll::PollFd::new(p.reader.get_ref().as_fd(), nix::poll::PollFlags::POLLIN)
            })
            .collect();
        nix::poll::poll(&mut polls, milliseconds).context("service fixture readiness")?;
        Ok(polls
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                p.revents()
                    .is_some_and(|events| !events.is_empty())
                    .then_some(i)
            })
            .collect())
    }
}
pub fn rpc(path: &Path, request: &Value, timeout: Duration) -> Result<Value> {
    Peer::send(path, request, timeout)?.receive()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_reply_is_read_after_service_closes_connection() -> Result<()> {
        let (client, mut server) = UnixStream::pair()?;
        client.set_read_timeout(Some(Duration::from_secs(5)))?;
        server.write_all(b"{\"status\":\"completed\"}\n")?;
        drop(server);
        let peer = Peer {
            reader: BufReader::new(client),
            timeout: Duration::from_secs(5),
        };
        assert_eq!(peer.receive()?["status"], "completed");
        Ok(())
    }
}
