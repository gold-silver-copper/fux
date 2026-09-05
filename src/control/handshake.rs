//! Control v1 negotiation precedes every JSON command or subscription.
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

pub const CONTROL_VERSION: u32 = 1;
pub const CONTROL_PREFACE: &[u8; 8] = b"FUXCTL1\n";
const DEADLINE: Duration = Duration::from_secs(2);

pub fn negotiate_client(stream: &mut UnixStream) -> io::Result<()> {
    super::authorize_peer(stream)?;
    negotiate(stream, true)
}
pub fn negotiate_server(stream: &mut UnixStream) -> io::Result<()> {
    negotiate(stream, false)
}
fn negotiate(stream: &mut UnixStream, client: bool) -> io::Result<()> {
    let read_timeout = stream.read_timeout()?;
    let write_timeout = stream.write_timeout()?;
    let result = (|| {
        stream.set_write_timeout(Some(DEADLINE))?;
        if client {
            stream.write_all(CONTROL_PREFACE)?;
        }
        let deadline = Instant::now() + DEADLINE;
        let mut received = [0; 8];
        let mut used = 0;
        while used < received.len() {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "control negotiation timed out")
                })?;
            stream.set_read_timeout(Some(remaining))?;
            let target = received
                .get_mut(used..)
                .ok_or_else(|| io::Error::other("invalid preface offset"))?;
            let length = stream.read(target)?;
            if length == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "control server closed during version negotiation",
                ));
            }
            used += length;
        }
        if !client {
            stream.write_all(CONTROL_PREFACE)?;
        }
        if &received != CONTROL_PREFACE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incompatible fux control protocol; expected FUXCTL1; use matching versions or restart the server after saving work",
            ));
        }
        Ok(())
    })();
    stream.set_read_timeout(read_timeout)?;
    stream.set_write_timeout(write_timeout)?;
    result
}
