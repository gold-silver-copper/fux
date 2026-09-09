//! Persistent public control connections for subscriptions and dropped replies.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::{Duration, Instant},
};

pub struct Peer(BufReader<UnixStream>);
impl Peer {
    pub fn connect(path: &Path) -> Result<Self> {
        let mut stream = super::local::connect(path, Instant::now() + Duration::from_secs(3))?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;
        stream.write_all(b"FUX\n")?;
        let mut preface = [0; 4];
        stream.read_exact(&mut preface)?;
        ensure!(&preface == b"FUX\n", "control preface mismatch");
        Ok(Self(BufReader::new(stream)))
    }
    pub fn send(&mut self, value: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        self.0.get_mut().write_all(&bytes)?;
        Ok(())
    }
    pub fn read(&mut self) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut bytes = Vec::new();
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
                .context("control frame deadline")?;
            self.0.get_ref().set_read_timeout(Some(left))?;
            let chunk = self.0.fill_buf()?;
            ensure!(!chunk.is_empty(), "control frame EOF");
            let end = chunk.iter().position(|byte| *byte == b'\n');
            let count = end.map_or(chunk.len(), |index| index + 1);
            ensure!(
                bytes.len() + count <= 1024 * 1024,
                "control frame size limit"
            );
            bytes.extend_from_slice(&chunk[..count]);
            self.0.consume(count);
            if end.is_some() {
                return Ok(serde_json::from_slice(&bytes)?);
            }
        }
    }
}
