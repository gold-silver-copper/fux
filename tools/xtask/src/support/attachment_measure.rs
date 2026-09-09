//! Concurrent benchmark readers: count wire bytes and inspect every delta frame.
use anyhow::{Result, ensure};
use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, poll},
    sys::socket::{MsgFlags, recv},
};
use serde_json::Value;
use std::{
    os::{
        fd::{AsFd, AsRawFd},
        unix::net::UnixStream,
    },
    time::{Duration, Instant},
};

const MAX_FRAME: usize = 16 * 1024 * 1024;
const PUMP_BUDGET: usize = 1024 * 1024;

pub struct Viewer {
    pub peer: UnixStream,
    pub bytes: u64,
    pending: Vec<u8>,
}

impl Viewer {
    pub fn new(peer: UnixStream) -> Self {
        Self {
            peer,
            bytes: 0,
            pending: Vec::new(),
        }
    }

    /// The socket stays blocking for bounded writes; only reads use DONTWAIT.
    /// Limit each turn so a continuously producing viewer cannot starve others.
    pub fn pump(&mut self, mut frame: impl FnMut(Value) -> Result<()>) -> Result<usize> {
        let mut read = 0;
        let mut frames = 0;
        let mut buffer = [0; 65536];
        while read < PUMP_BUDGET {
            let target = if self.pending.len() < 4 {
                4
            } else {
                let length = u32::from_be_bytes(self.pending[..4].try_into()?) as usize;
                ensure!(length <= MAX_FRAME, "attachment benchmark frame limit");
                4 + length
            };
            if self.pending.len() == target {
                frame(serde_json::from_slice(&self.pending[4..])?)?;
                self.pending.clear();
                frames += 1;
                continue;
            }
            let count = (target - self.pending.len())
                .min(buffer.len())
                .min(PUMP_BUDGET - read);
            match recv(
                self.peer.as_raw_fd(),
                &mut buffer[..count],
                MsgFlags::MSG_DONTWAIT,
            ) {
                Ok(0) => anyhow::bail!("server closed benchmark attachment"),
                Ok(count) => {
                    self.pending.extend_from_slice(&buffer[..count]);
                    self.bytes += count as u64;
                    read += count;
                }
                Err(Errno::EAGAIN) => break,
                Err(Errno::EINTR) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        // A frame ending exactly at the byte budget must not wait for new socket data.
        if self.pending.len() >= 4 {
            let length = u32::from_be_bytes(self.pending[..4].try_into()?) as usize;
            ensure!(length <= MAX_FRAME, "attachment benchmark frame limit");
            if self.pending.len() == 4 + length {
                frame(serde_json::from_slice(&self.pending[4..])?)?;
                self.pending.clear();
                frames += 1;
            }
        }
        Ok(frames)
    }
}

fn ready(viewers: &[Viewer], deadline: Instant) -> Result<()> {
    let mut polls: Vec<_> = viewers
        .iter()
        .map(|v| PollFd::new(v.peer.as_fd(), PollFlags::POLLIN))
        .collect();
    let remaining = deadline.saturating_duration_since(Instant::now());
    let timeout = u16::try_from(remaining.as_millis().clamp(1, 1000))?;
    match poll(&mut polls, timeout) {
        Ok(_) | Err(Errno::EINTR) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn wait_text(
    viewers: &mut [Viewer],
    predicate: impl Fn(&str) -> bool,
    timeout: Duration,
) -> Result<()> {
    ensure!(!viewers.is_empty(), "benchmark requires a viewer");
    let deadline = Instant::now() + timeout;
    loop {
        ensure!(Instant::now() < deadline, "benchmark frame did not arrive");
        ready(viewers, deadline)?;
        let mut matched = false;
        for (index, viewer) in viewers.iter_mut().enumerate() {
            viewer.pump(|frame| {
                if index == 0 && predicate(&super::attachment::text(&frame)?) {
                    matched = true;
                }
                Ok(())
            })?;
        }
        if matched {
            return Ok(());
        }
    }
}

pub fn drain_all(viewers: &mut [Viewer], quiet: Duration) -> Result<()> {
    ensure!(!viewers.is_empty(), "benchmark requires a viewer");
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut last = Instant::now();
    while last.elapsed() < quiet {
        ensure!(
            Instant::now() < deadline,
            "benchmark attachment did not become quiet"
        );
        ready(viewers, (last + quiet).min(deadline))?;
        for viewer in viewers.iter_mut() {
            if viewer.pump(|_| Ok(()))? > 0 {
                last = Instant::now();
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn fragmented_frames_and_every_delta_are_observed() -> Result<()> {
        let (mut sender, receiver) = UnixStream::pair()?;
        let first = json!({"state":{"state":{"panes":{"1":{"cells":[{"text":"MARK"}]}}}}});
        let body = serde_json::to_vec(&first)?;
        let prefix = (body.len() as u32).to_be_bytes();
        let mut viewer = Viewer::new(receiver);
        sender.write_all(&prefix[..2])?;
        assert_eq!(viewer.pump(|_| anyhow::bail!("incomplete prefix"))?, 0);
        sender.write_all(&prefix[2..])?;
        sender.write_all(&body[..3])?;
        assert_eq!(viewer.pump(|_| anyhow::bail!("incomplete body"))?, 0);
        sender.write_all(&body[3..])?;
        super::super::attachment::send_server(&mut sender, &json!({"state":{}}))?;
        wait_text(
            std::slice::from_mut(&mut viewer),
            |text| text.contains("MARK"),
            Duration::from_secs(1),
        )?;
        assert_eq!(
            viewer.bytes,
            (4 + body.len() + 4 + serde_json::to_vec(&json!({"state":{}}))?.len()) as u64
        );
        Ok(())
    }

    #[test]
    fn rejects_oversize_before_reading_body() -> Result<()> {
        let (mut sender, receiver) = UnixStream::pair()?;
        sender.write_all(&((MAX_FRAME + 1) as u32).to_be_bytes())?;
        let mut viewer = Viewer::new(receiver);
        assert!(
            viewer
                .pump(|_| Ok(()))
                .unwrap_err()
                .to_string()
                .contains("frame limit")
        );
        assert_eq!(viewer.pending.len(), 4);
        assert_eq!(viewer.bytes, 4);
        Ok(())
    }
}
