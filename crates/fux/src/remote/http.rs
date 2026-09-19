//! The bounded BRP transport (prompt 3.9 fallback): a hyper HTTP/1.1 acceptor on `IoTaskPool`
//! feeding `bevy_remote`'s `BrpSender` exactly as `RemoteHttpPlugin` does (`bevy_remote/src/
//! http.rs`), plus what that plugin lacks and `docs/verification.md` ("BRP resource
//! exhaustion") measured the need for: a body limit, a batch limit, a connection cap with the
//! attachment listener's backlog backpressure, header and body deadlines, and an accept loop
//! that survives `EMFILE` instead of ending the listener for the life of the process.
//!
//! The wire format is unchanged: JSON-RPC replies as `application/json`, `+watch` streams as
//! `text/event-stream` with one `data:` line per item.

use std::convert::Infallible;
use std::net::{TcpListener, TcpStream};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use async_channel::{Receiver, Sender};
use async_io::{Async, Timer};
use bevy_ecs::error::BevyError;
use bevy_ecs::prelude::*;
use bevy_log::warn;
use bevy_remote::http::{HostAddress, HostPort};
use bevy_remote::{
    BrpBatch, BrpError, BrpMessage, BrpRequest, BrpResponse, BrpResult, BrpSender, error_codes,
};
use bevy_tasks::IoTaskPool;
use bevy_tasks::futures_lite::{StreamExt, future};
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Bytes, Frame, Incoming};
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use hyper::server::conn::http1;
use hyper::{Request, Response, StatusCode, service};
use serde_json::Value;
use smol_hyper::rt::{FuturesIo, SmolTimer};

/// Largest request body dispatched. A longer one (announced by `Content-Length` or found while
/// reading) is never buffered: it is discarded until it ends or the deadline hits, so the
/// client reads a `413` instead of a reset, and then the connection closes.
pub const MAX_BODY: usize = 1024 * 1024;
/// Most requests in one batch.
pub const MAX_BATCH: usize = 64;
/// Connections served at once; further peers wait in the OS backlog (which, full, refuses).
pub const MAX_CONNECTIONS: usize = 256;
/// Deadline for a request head and, separately, for its body.
pub const READ_TIMEOUT: Duration = Duration::from_secs(10);
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

/// Bounded HTTP transport shared by the fux and zor BRP hosts.
/// Add after `RemotePlugin`; the bound ephemeral port is published through `HostPort`.
pub struct BoundedHttpPlugin;

impl bevy_app::Plugin for BoundedHttpPlugin {
    fn build(&self, app: &mut bevy_app::App) {
        app.insert_resource(HostAddress(std::net::IpAddr::V4(
            std::net::Ipv4Addr::LOCALHOST,
        )))
        .insert_resource(HostPort(0))
        .add_systems(bevy_app::Startup, start);
    }
}

/// `Startup`: binds the pre-probed [`HostPort`] (a fresh port if that one was taken in the
/// meantime, and [`HostPort`] follows) and spawns the acceptor.
pub(super) fn start(world: &mut World) -> Result<(), BevyError> {
    let address = world.resource::<HostAddress>().0;
    let probed = world.resource::<HostPort>().0;
    let listener = match Async::<TcpListener>::bind((address, probed)) {
        Ok(listener) => listener,
        Err(error) => {
            warn!("brp port {probed} was taken ({error}); binding a fresh one");
            Async::<TcpListener>::bind((address, 0))?
        }
    };
    let port = listener.get_ref().local_addr()?.port();
    world.insert_resource(HostPort(port));
    let sender = Sender::clone(world.resource::<BrpSender>());
    IoTaskPool::get()
        .spawn(accept_loop(listener, sender))
        .detach();
    Ok(())
}

/// One unit of the connection cap (the attachment listener's pattern): the channel holds one
/// unit per live connection, `send` blocks at the cap, and dropping the permit dequeues one.
struct Permit(Receiver<()>);

impl Drop for Permit {
    fn drop(&mut self) {
        let _ = self.0.try_recv();
    }
}

async fn accept_loop(listener: Async<TcpListener>, sender: Sender<BrpMessage>) {
    let (slots, released) = async_channel::bounded::<()>(MAX_CONNECTIONS);
    loop {
        // At the cap this waits for a connection to end; new peers sit in the OS backlog.
        if slots.send(()).await.is_err() {
            return;
        }
        let permit = Permit(released.clone());
        match listener.accept().await {
            Ok((client, _peer)) => {
                IoTaskPool::get()
                    .spawn(serve(client, sender.clone(), permit))
                    .detach();
            }
            Err(error) => {
                drop(permit);
                // EMFILE and friends: back off instead of spinning or, as
                // `RemoteHttpPlugin` does, returning and losing the listener.
                warn!("brp accept failed: {error}");
                Timer::after(ACCEPT_RETRY).await;
            }
        }
    }
}

/// One connection; the permit is held until hyper is done with it.
async fn serve(client: Async<TcpStream>, sender: Sender<BrpMessage>, _permit: Permit) {
    let _ = http1::Builder::new()
        .timer(SmolTimer::new())
        .header_read_timeout(READ_TIMEOUT)
        .serve_connection(
            FuturesIo::new(client),
            service::service_fn(|request| handle(request, &sender)),
        )
        .await;
}

/// Reads the body within the limits, dispatches the batch, and answers.
async fn handle(
    request: Request<Incoming>,
    sender: &Sender<BrpMessage>,
) -> Result<Response<BrpHttpBody>, Infallible> {
    let (parts, body) = request.into_parts();
    let announced = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    let read = future::or(
        read_body(body, announced.is_some_and(|n| n > MAX_BODY)),
        async {
            Timer::after(READ_TIMEOUT).await;
            Err(refuse(
                StatusCode::REQUEST_TIMEOUT,
                format!(
                    "request body not complete within {} s",
                    READ_TIMEOUT.as_secs()
                ),
            ))
        },
    )
    .await;
    let bytes = match read {
        Ok(bytes) => bytes,
        Err(response) => return Ok(response),
    };
    Ok(match serde_json::from_slice::<BrpBatch>(&bytes) {
        Ok(BrpBatch::Single(request)) => match process(request, sender).await {
            Reply::Complete(response) => complete(&response),
            Reply::Stream(stream) => {
                let mut response = Response::new(BrpHttpBody::Stream(stream));
                response
                    .headers_mut()
                    .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
                response
            }
        },
        Ok(BrpBatch::Batch(requests)) if requests.len() > MAX_BATCH => complete(&invalid(
            None,
            format!(
                "batch of {} requests exceeds the limit of {MAX_BATCH}",
                requests.len()
            ),
        )),
        Ok(BrpBatch::Batch(requests)) => {
            let mut responses = Vec::with_capacity(requests.len());
            for request in requests {
                responses.push(match process(request, sender).await {
                    Reply::Complete(response) => response,
                    Reply::Stream(BrpStream { id, .. }) => {
                        invalid(id, "Streaming can not be used in batch requests".to_owned())
                    }
                });
            }
            complete(&responses)
        }
        Err(error) => complete(&invalid(None, error.to_string())),
    })
}

/// The whole body, or the refusal it earned: over [`MAX_BODY`] (known up front from
/// `discard`, or discovered) the rest is read and dropped so the reply reaches the client.
async fn read_body(
    mut body: Incoming,
    mut discard: bool,
) -> Result<Vec<u8>, Response<BrpHttpBody>> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame =
            frame.map_err(|e| refuse(StatusCode::BAD_REQUEST, format!("request body: {e}")))?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        if discard {
            continue;
        }
        if bytes.len() + data.len() > MAX_BODY {
            discard = true;
            bytes = Vec::new();
            continue;
        }
        bytes.extend_from_slice(&data);
    }
    if discard {
        return Err(refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("request body exceeds {MAX_BODY} bytes"),
        ));
    }
    Ok(bytes)
}

enum Reply {
    Complete(BrpResponse),
    Stream(BrpStream),
}

/// Hands one request to the World through the mailbox and waits for its reply, or returns
/// the stream a `+watch` request answers on.
async fn process(request: Value, sender: &Sender<BrpMessage>) -> Reply {
    // The id first, so a malformed request is still answered under it.
    let id = request.as_object().and_then(|map| map.get("id")).cloned();
    let request: BrpRequest = match serde_json::from_value(request) {
        Ok(request) => request,
        Err(error) => return Reply::Complete(invalid(id, error.to_string())),
    };
    let watch = request.method.contains("+watch");
    let (result_sender, result_receiver) = async_channel::bounded(if watch { 8 } else { 1 });
    let _ = sender
        .send(BrpMessage {
            method: request.method,
            params: request.params,
            sender: result_sender,
        })
        .await;
    if watch {
        Reply::Stream(BrpStream {
            id: request.id,
            rx: Box::pin(result_receiver),
        })
    } else {
        let result = result_receiver.recv().await.unwrap_or_else(|_| {
            Err(BrpError {
                code: error_codes::INTERNAL_ERROR,
                message: "the request was dropped unanswered".to_owned(),
                data: None,
            })
        });
        Reply::Complete(BrpResponse::new(request.id, result))
    }
}

fn invalid(id: Option<Value>, message: String) -> BrpResponse {
    BrpResponse::new(
        id,
        Err(BrpError {
            code: error_codes::INVALID_REQUEST,
            message,
            data: None,
        }),
    )
}

/// A `200` JSON reply.
fn complete<T: serde::Serialize>(payload: &T) -> Response<BrpHttpBody> {
    let (status, body) = match serde_json::to_vec(payload) {
        Ok(body) => (StatusCode::OK, body),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{{\"error\":\"{error}\"}}").into_bytes(),
        ),
    };
    let mut response = Response::new(BrpHttpBody::Complete(Full::new(Bytes::from(body))));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

/// A refusal before dispatch: `status` with a JSON-RPC error body under a null id.
fn refuse(status: StatusCode, message: String) -> Response<BrpHttpBody> {
    let mut response = complete(&invalid(None, message));
    *response.status_mut() = status;
    response
}

struct BrpStream {
    id: Option<Value>,
    rx: Pin<Box<Receiver<BrpResult>>>,
}

impl Body for BrpStream {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.as_mut().rx.poll_next(cx) {
            Poll::Ready(Some(result)) => {
                let response = BrpResponse::new(self.id.clone(), result);
                match serde_json::to_string(&response) {
                    Ok(serialized) => Poll::Ready(Some(Ok(Frame::data(Bytes::from(format!(
                        "data: {serialized}\n\n"
                    )))))),
                    // An unserializable item ends the stream rather than corrupting it.
                    Err(_) => Poll::Ready(None),
                }
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.rx.is_closed()
    }
}

enum BrpHttpBody {
    Complete(Full<Bytes>),
    Stream(BrpStream),
}

impl Body for BrpHttpBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match &mut *self.get_mut() {
            BrpHttpBody::Complete(body) => Body::poll_frame(Pin::new(body), cx),
            BrpHttpBody::Stream(body) => Body::poll_frame(Pin::new(body), cx),
        }
    }
}
