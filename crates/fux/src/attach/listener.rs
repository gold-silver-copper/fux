//! The loopback listener (prompt 3.10): an `Async<TcpListener>` accepted on `IoTaskPool`, one
//! reader task and one writer task per connection. Tasks own the sockets; the World only ever
//! sees channel ends: an [`Accepted`] handshake per connection and, after admission, the
//! viewer's `ClientFrame`s as `Inbound::ViewerRequest` on the runner's channel.

use std::io;
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use async_channel::{Receiver, Sender};
use async_io::{Async, Timer};
use bevy_ecs::error::BevyError;
use bevy_ecs::prelude::*;
use bevy_log::{debug, warn};
use bevy_tasks::IoTaskPool;
use bevy_tasks::futures_lite::future;
use bevy_tasks::futures_lite::io::{AsyncReadExt, AsyncWriteExt};
use serde::de::DeserializeOwned;

use crate::model::{Inbound, ServerInstance};
use crate::wire::{self, ByeReason, ClientFrame, FRAME_PREFIX_BYTES, Hello, ServerFrame};

/// A viewer that has not sent `Hello` within this long is closed.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Encoded frames queued for one viewer's socket; a viewer that lets the queue fill is stalled
/// and its attachment is closed (`AttachAdapter`).
pub const WRITER_QUEUE: usize = 64;

/// Where viewers connect; published in `brp.json` by the remote module.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct AttachEndpoint {
    pub host: String,
    pub port: u16,
}

/// The attachment token (hex256) every `Hello` must carry; published beside the endpoint.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct AttachToken(pub String);

/// A connection whose `Hello` passed the token and instance checks. The ingest system decides
/// admission against the World (workspace, exact pane) and either sends the viewer entity on
/// `admit` or drops it, which ends the reader task; the writer sender goes to the adapter.
pub(crate) struct Accepted {
    pub hello: Hello,
    /// Encoded frames for the writer task.
    pub writer: Sender<Vec<u8>>,
    /// Buffers the writer task has finished with, for reuse by the encoder.
    pub recycle: Receiver<Vec<u8>>,
    /// The reader task learns which viewer it feeds; dropped on refusal.
    pub admit: Sender<Entity>,
}

/// Channel ends the accept loop and the ingest system share.
#[derive(Resource)]
pub(crate) struct Inbox {
    pub accepted: Receiver<Accepted>,
    sender: Sender<Accepted>,
    inbound: Sender<Inbound>,
}

impl Inbox {
    pub fn new(inbound: Sender<Inbound>) -> Self {
        let (sender, accepted) = async_channel::unbounded();
        Self {
            accepted,
            sender,
            inbound,
        }
    }
}

/// Token and instance nonce, snapshotted for the accept loop.
struct Gate {
    token: String,
    instance: String,
}

impl Gate {
    fn admits(&self, hello: &Hello) -> bool {
        constant_time_eq(hello.token.as_bytes(), self.token.as_bytes())
            & constant_time_eq(hello.instance.as_bytes(), self.instance.as_bytes())
    }
}

/// Equal-length comparison whose duration does not depend on where the inputs differ.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 32 random bytes from the OS as lowercase hex.
pub fn random_hex256() -> Result<String, io::Error> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let mut hex = String::with_capacity(64);
    for byte in bytes {
        hex.push(HEX.get(usize::from(byte >> 4)).copied().unwrap_or(b'0') as char);
        hex.push(HEX.get(usize::from(byte & 0xf)).copied().unwrap_or(b'0') as char);
    }
    Ok(hex)
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Startup: bind `127.0.0.1:0`, publish the endpoint and token, start the accept loop.
pub(crate) fn start(world: &mut World) -> Result<(), BevyError> {
    let listener = Async::<TcpListener>::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.get_ref().local_addr()?.port();
    let token = random_hex256()?;
    let instance = world.resource::<ServerInstance>().nonce.clone();
    let inbox = world.resource::<Inbox>();
    let gate = Arc::new(Gate {
        token: token.clone(),
        instance,
    });
    IoTaskPool::get()
        .spawn(accept_loop(
            listener,
            gate,
            inbox.sender.clone(),
            inbox.inbound.clone(),
        ))
        .detach();
    world.insert_resource(AttachEndpoint {
        host: Ipv4Addr::LOCALHOST.to_string(),
        port,
    });
    world.insert_resource(AttachToken(token));
    Ok(())
}

async fn accept_loop(
    listener: Async<TcpListener>,
    gate: Arc<Gate>,
    accepted: Sender<Accepted>,
    inbound: Sender<Inbound>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _peer)) => {
                IoTaskPool::get()
                    .spawn(serve(
                        stream,
                        Arc::clone(&gate),
                        accepted.clone(),
                        inbound.clone(),
                    ))
                    .detach();
            }
            Err(error) => {
                // EMFILE and friends: back off instead of spinning.
                warn!("attach accept failed: {error}");
                Timer::after(Duration::from_millis(100)).await;
            }
        }
    }
}

/// One connection: handshake, then the reader loop; the writer loop is its own task.
async fn serve(
    stream: Async<TcpStream>,
    gate: Arc<Gate>,
    accepted: Sender<Accepted>,
    inbound: Sender<Inbound>,
) {
    let stream = Arc::new(stream);
    let mut reader = FrameReader::default();
    let hello = match future::or(reader.read::<Hello>(&stream), hello_deadline()).await {
        Ok(Some(hello)) => hello,
        Ok(None) => return,
        Err(error) => {
            debug!("attach handshake failed: {error}");
            refuse(&stream, ByeReason::Protocol, "expected Hello").await;
            return;
        }
    };
    if !gate.admits(&hello) {
        refuse(&stream, ByeReason::Refused, "token or instance mismatch").await;
        return;
    }
    let (writer, frames) = async_channel::bounded(WRITER_QUEUE);
    let (recycle_tx, recycle) = async_channel::bounded(WRITER_QUEUE);
    let (admit_tx, admit) = async_channel::bounded(1);
    IoTaskPool::get()
        .spawn(write_loop(Arc::clone(&stream), frames, recycle_tx))
        .detach();
    let handoff = Accepted {
        hello,
        writer,
        recycle,
        admit: admit_tx,
    };
    if accepted.send(handoff).await.is_err() {
        return;
    }
    // The runner sleeps on the inbound channel; nothing else would make it run the ingest.
    if inbound.send(Inbound::Wake).await.is_err() {
        return;
    }
    // Refused by the World: the writer task delivers its `Bye`, this task just ends.
    let Ok(viewer) = admit.recv().await else {
        return;
    };
    loop {
        match reader.read::<ClientFrame>(&stream).await {
            Ok(Some(ClientFrame::Request { request })) => {
                if inbound
                    .send(Inbound::ViewerRequest { viewer, request })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            // Acks are informational; the outstanding window is the bounded writer queue.
            Ok(Some(ClientFrame::Ack { .. })) => {}
            Ok(None) => break,
            Err(error) => {
                debug!("attach reader for {viewer} closed: {error}");
                break;
            }
        }
    }
    let _ = inbound.send(Inbound::ViewerGone { viewer }).await;
}

async fn hello_deadline() -> io::Result<Option<Hello>> {
    Timer::after(HELLO_TIMEOUT).await;
    Err(io::Error::new(io::ErrorKind::TimedOut, "no Hello"))
}

/// Writes queued frames; when the queue closes (adapter dropped the connection) or a write
/// fails, shuts the socket down so the reader task sees EOF and reports `ViewerGone`.
async fn write_loop(
    stream: Arc<Async<TcpStream>>,
    frames: Receiver<Vec<u8>>,
    recycle: Sender<Vec<u8>>,
) {
    let mut out = &*stream;
    while let Ok(buf) = frames.recv().await {
        if out.write_all(&buf).await.is_err() {
            break;
        }
        // A full recycle queue just drops the buffer.
        let _ = recycle.try_send(buf);
    }
    let _ = stream.get_ref().shutdown(Shutdown::Both);
}

async fn refuse(stream: &Async<TcpStream>, reason: ByeReason, message: &str) {
    let mut buf = Vec::new();
    let frame = ServerFrame::Bye {
        reason,
        message: message.to_owned(),
    };
    if wire::encode(&frame, &mut buf).is_ok() {
        let mut out = stream;
        let _ = out.write_all(&buf).await;
    }
    let _ = stream.get_ref().shutdown(Shutdown::Both);
}

/// Length-prefixed frame reader with one reusable payload buffer.
#[derive(Default)]
struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    /// `Ok(None)` on a clean EOF between frames; oversize or malformed frames are errors.
    async fn read<T: DeserializeOwned>(
        &mut self,
        stream: &Async<TcpStream>,
    ) -> io::Result<Option<T>> {
        let mut input = stream;
        let mut prefix = [0u8; FRAME_PREFIX_BYTES];
        match input.read_exact(&mut prefix).await {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error),
        }
        let Some(len) = wire::payload_len(&prefix)? else {
            return Ok(None);
        };
        self.buf.clear();
        self.buf.resize(len, 0);
        input.read_exact(&mut self.buf).await?;
        serde_json::from_slice(&self.buf)
            .map(Some)
            .map_err(io::Error::other)
    }
}
