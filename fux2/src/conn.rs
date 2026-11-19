//! One connection, either side: bytes in and out of a non-blocking stream,
//! cut into frames. It never waits; its owner polls.
use crate::sys::Stream;
use crate::wire::{Code, Failure};
use fuxix::Errno;
use fuxix::poll::Events;

const HEADER: usize = 4;

/// Why a connection cannot go on.
#[derive(Debug)]
pub enum Broken {
    /// The other end closed it.
    Closed,
    /// It sent what is no frame.
    Frame(Failure),
    Io(Errno),
}

impl From<Broken> for Failure {
    fn from(broken: Broken) -> Failure {
        match broken {
            Broken::Closed => Failure::new(Code::Indeterminate, "the connection closed"),
            Broken::Frame(failure) => failure,
            Broken::Io(errno) => Failure::new(Code::Indeterminate, format!("connection: {errno}")),
        }
    }
}

/// A stream and what is on its way in and out of it.
pub struct Conn {
    pub stream: Stream,
    limit: usize,
    input: Vec<u8>,
    output: Vec<u8>,
    sent: usize,
}

impl Conn {
    /// `stream`, already non-blocking, taking frames of up to `limit` bytes.
    pub fn new(stream: Stream, limit: usize) -> Conn {
        Conn {
            stream,
            limit,
            input: Vec::new(),
            output: Vec::new(),
            sent: 0,
        }
    }

    /// Adds a frame holding `body` to what is to be written.
    pub fn queue(&mut self, body: &[u8]) -> Result<(), Failure> {
        let len = u32::try_from(body.len())
            .map_err(|_| Failure::new(Code::Limit, "a frame past 4 GiB"))?;
        self.output.extend_from_slice(&len.to_be_bytes());
        self.output.extend_from_slice(body);
        Ok(())
    }

    /// Bytes still to be written.
    pub fn pending(&self) -> usize {
        self.output.len().saturating_sub(self.sent)
    }

    /// Whether part of a frame has come and the rest has not.
    pub fn mid_frame(&self) -> bool {
        !self.input.is_empty()
    }

    /// What to wait for.
    pub fn events(&self) -> Events {
        // One request at a time: nothing is read while an answer is going
        // out, so asking for input then would only wake the poll for nothing.
        match self.pending() {
            0 => Events::IN,
            _ => Events::OUT,
        }
    }

    /// Writes what fits; true if anything went.
    pub fn flush(&mut self) -> Result<bool, Broken> {
        let mut moved = false;
        while let Some(rest) = self.output.get(self.sent..).filter(|r| !r.is_empty()) {
            match fuxix::io::write(&self.stream, rest) {
                Ok(0) | Err(Errno::AGAIN) => break,
                Err(Errno::INTR) => {}
                Ok(n) => {
                    self.sent = self.sent.saturating_add(n);
                    moved = true;
                }
                Err(errno) if errno.is_peer_gone() => return Err(Broken::Closed),
                Err(errno) => return Err(Broken::Io(errno)),
            }
        }
        if self.pending() == 0 {
            self.output = Vec::new();
            self.sent = 0;
        }
        Ok(moved)
    }

    /// Reads toward the next frame: its body once it is whole.
    pub fn receive(&mut self) -> Result<Option<Vec<u8>>, Broken> {
        loop {
            let have = self.input.len();
            let want = match self.input.first_chunk::<HEADER>() {
                None => HEADER,
                Some(header) => {
                    let len = usize::try_from(u32::from_be_bytes(*header)).unwrap_or(usize::MAX);
                    if len == 0 || len > self.limit {
                        let why = format!("a frame of {len} bytes, at most {}", self.limit);
                        return Err(Broken::Frame(Failure::new(Code::Protocol, why)));
                    }
                    len.saturating_add(HEADER)
                }
            };
            if have == want {
                let body = self.input.get(HEADER..).unwrap_or_default().to_vec();
                self.input = Vec::new();
                return Ok(Some(body));
            }
            self.input.resize(want, 0);
            let room = self.input.get_mut(have..).unwrap_or_default();
            let read = fuxix::io::read(&self.stream, room);
            let got = *match &read {
                Ok(n) => n,
                Err(_) => &0,
            };
            self.input.truncate(have.saturating_add(got));
            match read {
                Ok(0) => return Err(Broken::Closed),
                Ok(_) | Err(Errno::INTR) => {}
                Err(Errno::AGAIN) => return Ok(None),
                Err(errno) if errno.is_peer_gone() => return Err(Broken::Closed),
                Err(errno) => return Err(Broken::Io(errno)),
            }
        }
    }
}
