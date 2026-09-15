//! The attachment stream client (prompt 3.10): reads `brp.json`, connects to the loopback
//! listener, sends `Hello`, then reads length-prefixed `ServerFrame`s on a thread (pushed to the
//! runner's wake channel) and writes encoded `ClientFrame`s from a bounded writer thread.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::Path;
use std::sync::mpsc::{self, Sender, SyncSender, TrySendError};
use std::thread::JoinHandle;

use bevy_ecs::error::BevyError;

use super::Wake;
use crate::model::Viewport;
use crate::remote::descriptor::{self, Descriptor};
use crate::wire::{self, ClientFrame, ExactTargetSpec, Hello, ServerFrame};

/// Frames the viewer can leave unsent before the server counts as stalled.
const WRITER_QUEUE: usize = 256;

#[derive(Debug)]
pub enum ConnectError {
    Descriptor(descriptor::DescriptorError),
    /// The server has not published an attachment endpoint.
    NoAttachEndpoint,
    Io(io::Error),
    Encode(serde_json::Error),
}

impl core::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Descriptor(e) => write!(f, "{e}"),
            Self::NoAttachEndpoint => f.write_str("server has no attachment endpoint yet"),
            Self::Io(e) => write!(f, "attachment connection: {e}"),
            Self::Encode(e) => write!(f, "encoding hello: {e}"),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<io::Error> for ConnectError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// A live attachment: the reader thread feeds `Wake::Frame`, `send` queues encoded frames.
pub struct Connection {
    writer: SyncSender<Vec<u8>>,
    stream: TcpStream,
    reader: Option<JoinHandle<()>>,
    encode_buf: Vec<u8>,
    pub descriptor: Descriptor,
}

impl Connection {
    /// Connects and sends `Hello`; the reader thread starts immediately.
    pub fn connect(
        brp: &Path,
        workspace: &str,
        viewport: Viewport,
        exact: Option<ExactTargetSpec>,
        wake: Sender<Wake>,
    ) -> Result<Self, ConnectError> {
        let descriptor = descriptor::read_descriptor(brp).map_err(ConnectError::Descriptor)?;
        let attach = descriptor
            .attach
            .as_ref()
            .ok_or(ConnectError::NoAttachEndpoint)?;
        let stream = TcpStream::connect((attach.host.as_str(), attach.port))?;
        stream.set_nodelay(true)?;
        let hello = Hello {
            token: attach.token.clone(),
            instance: descriptor.instance.clone(),
            workspace: workspace.into(),
            stream: String::new(),
            viewport,
            exact_target: exact,
        };
        let mut encode_buf = Vec::with_capacity(4096);
        wire::encode(&hello, &mut encode_buf).map_err(ConnectError::Encode)?;
        let mut writer_stream = stream.try_clone()?;
        writer_stream.write_all(&encode_buf)?;
        let reader_stream = stream.try_clone()?;
        let reader = std::thread::Builder::new()
            .name("fux-attach-reader".into())
            .spawn(move || read_frames(reader_stream, &wake))?;
        let (writer, rx) = mpsc::sync_channel::<Vec<u8>>(WRITER_QUEUE);
        std::thread::Builder::new()
            .name("fux-attach-writer".into())
            .spawn(move || {
                for bytes in rx {
                    if writer_stream.write_all(&bytes).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            writer,
            stream,
            reader: Some(reader),
            encode_buf,
            descriptor,
        })
    }

    /// Queues one frame; a full queue means the server stopped reading, which is fatal.
    pub fn send(&mut self, frame: &ClientFrame) -> Result<(), BevyError> {
        wire::encode(frame, &mut self.encode_buf)?;
        match self.writer.try_send(self.encode_buf.clone()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(io::Error::other("attachment writer stalled").into()),
            Err(TrySendError::Disconnected(_)) => {
                Err(io::Error::other("attachment connection closed").into())
            }
        }
    }

    /// Shuts the stream down and waits for the reader thread, so every wake it could send is
    /// already in the channel when this returns.
    pub fn close(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn read_frames(mut stream: TcpStream, wake: &Sender<Wake>) {
    let mut prefix = [0u8; wire::FRAME_PREFIX_BYTES];
    // Reused across frames; grows to the largest payload seen.
    let mut payload = Vec::with_capacity(64 * 1024);
    loop {
        if stream.read_exact(&mut prefix).is_err() {
            break;
        }
        let Ok(Some(len)) = wire::payload_len(&prefix) else {
            break;
        };
        payload.clear();
        payload.resize(len, 0);
        if stream.read_exact(&mut payload).is_err() {
            break;
        }
        let frame: ServerFrame = match serde_json::from_slice(&payload) {
            Ok(frame) => frame,
            Err(e) => {
                let _ = wake.send(Wake::Disconnected(format!("bad frame: {e}")));
                return;
            }
        };
        if wake.send(Wake::Frame(frame)).is_err() {
            return;
        }
    }
    let _ = wake.send(Wake::Disconnected("server closed the connection".into()));
}
