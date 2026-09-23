//! The BRP transport: HTTP/1 over a Unix domain socket, and nothing else.
//!
//! `bevy_remote` 0.19.1 binds its HTTP server with `Async::<TcpListener>::bind`
//! and offers no way to supply a listener, so fux owns this serving loop. It
//! is modelled on the crate's `http.rs` (MIT OR Apache-2.0, copyright the
//! Bevy contributors) with `Async<UnixListener>` in place of the TCP listener.
//! Everything above the listener is stock: requests go into `BrpSender`'s
//! mailbox as `BrpMessage`s, and the method registry, the BRP types and the
//! watching bookkeeping are the crate's own.
//!
//! A Unix socket is a filesystem object, not a network address, so a web page
//! cannot reach it and its file permissions decide which local users can.
//! There is no authentication step beyond that: anyone who can open the socket
//! has the whole API.
use async_channel::{Receiver, Sender};
use async_io::{Async, Timer};
use bevy_remote::{
    BrpBatch, BrpError, BrpMessage, BrpRequest, BrpResponse, BrpResult, error_codes,
};
use bevy_tasks::{IoTaskPool, Task, futures_lite::Stream};
use http_body_util::{BodyExt as _, Full, LengthLimitError, Limited};
use hyper::{
    Request, Response,
    body::{Body, Bytes, Frame, Incoming},
    header::{CONTENT_TYPE, HeaderValue},
    server::conn::http1,
    service,
};
use nix::{
    fcntl::{Flock, FlockArg},
    sys::socket::{AddressFamily, Backlog, SockFlag, SockType, UnixAddr, bind, listen, socket},
    unistd::geteuid,
};
use serde_json::Value;
use smol_hyper::rt::{FuturesIo, SmolTimer};
use std::{
    convert::Infallible,
    fs::{self, DirBuilder, File, OpenOptions},
    io,
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    pin::Pin,
    sync::mpsc,
    task::{Context, Poll},
    thread,
    time::{Duration, Instant},
};

/// The file name used inside a default socket directory.
const DEFAULT_NAME: &str = "server.sock";
/// How long a probe of an existing socket may take before it counts as
/// ambiguous rather than stale.
const PROBE: Duration = Duration::from_secs(1);

/// Operational commands refuse to run while the retired variable is set, so a
/// script written for the TCP endpoint fails loudly instead of reaching
/// whatever server listens on the default socket.
pub fn reject_retired_environment() -> Result<(), String> {
    if std::env::var_os("FUX_ENDPOINT").is_some() {
        return Err(
            "FUX_ENDPOINT is no longer supported: fux serves only on a Unix domain socket. \
             Unset FUX_ENDPOINT and, to choose a socket, set FUX_SOCKET to its absolute path"
                .into(),
        );
    }
    Ok(())
}

/// The socket a command uses: `flag` (the server's `--socket`), else
/// `FUX_SOCKET`, else `$XDG_RUNTIME_DIR/fux/server.sock`, else
/// `$TMPDIR/fux/server.sock`. Every candidate is validated, not skipped.
pub fn socket_path(flag: Option<&str>) -> Result<PathBuf, String> {
    if let Some(flag) = flag {
        return checked(flag, "--socket");
    }
    if let Some(value) = std::env::var_os("FUX_SOCKET") {
        let value = value
            .into_string()
            .map_err(|_| "FUX_SOCKET is not valid UTF-8".to_string())?;
        return checked(&value, "FUX_SOCKET");
    }
    for base in ["XDG_RUNTIME_DIR", "TMPDIR"] {
        if let Some(value) = std::env::var_os(base) {
            let value = value
                .into_string()
                .map_err(|_| format!("{base} is not valid UTF-8"))?;
            return socket_path_from(base, &value);
        }
    }
    Err(
        "no socket location: neither XDG_RUNTIME_DIR nor TMPDIR is set; \
         set FUX_SOCKET to an absolute socket path"
            .into(),
    )
}

/// Longest socket path, in bytes, that this platform's `sockaddr_un` holds
/// with its terminating NUL.
pub fn max_path_bytes() -> usize {
    // SAFETY: sockaddr_un is a plain C struct; all-zero is a valid value.
    let address: nix::libc::sockaddr_un = unsafe { std::mem::zeroed() };
    std::mem::size_of_val(&address.sun_path) - 1
}

/// The socket path under a directory variable such as `XDG_RUNTIME_DIR`, and
/// its validation. Split out so a test can drive it without setting the
/// environment, and so the length error counts the socket path rather than the
/// directory it was built from.
fn socket_path_from(base: &str, value: &str) -> Result<PathBuf, String> {
    if value.is_empty() {
        return Err(format!(
            "{base} is set but empty; set it to a directory or set FUX_SOCKET"
        ));
    }
    if !Path::new(value).is_absolute() {
        return Err(format!(
            "{base} is {value:?}, not an absolute path; fix it or set FUX_SOCKET"
        ));
    }
    let path = Path::new(value).join("fux").join(DEFAULT_NAME);
    checked(&path.to_string_lossy(), base)
}

fn checked(value: &str, source: &str) -> Result<PathBuf, String> {
    if value.is_empty() {
        return Err(format!(
            "{source} is empty; it must be an absolute socket path"
        ));
    }
    if value.contains("://") {
        return Err(format!(
            "{source} is {value:?}, which looks like a URL. fux no longer listens on TCP: \
             {source} takes the absolute path of a Unix domain socket"
        ));
    }
    if value.contains('\0') {
        return Err(format!("{source} contains a NUL byte"));
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(format!(
            "{source} is {value:?}; it must be an absolute path"
        ));
    }
    if path.file_name().is_none() || path.parent().is_none() {
        return Err(format!("{source} is {value:?}; it must name a socket file"));
    }
    let limit = max_path_bytes();
    if value.len() > limit {
        return Err(format!(
            "the socket path is {} bytes, longer than the {limit}-byte limit for a Unix \
             domain socket on this platform (from {source}): {value}",
            value.len()
        ));
    }
    Ok(path)
}

fn octal(mode: u32) -> String {
    format!("{:04o}", mode & 0o7777)
}

/// The directory that holds the socket must be ours and private, reached only
/// through directories no other user can rewrite. `create` makes it (mode
/// 0700, one level) when it is missing; nothing that already exists is ever
/// modified.
fn private_directory(directory: &Path, create: bool) -> Result<(), String> {
    let euid = geteuid().as_raw();
    let shown = directory.display();
    let above = directory
        .parent()
        .ok_or_else(|| format!("{shown} has no parent directory"))?;
    let canonical = above
        .canonicalize()
        .map_err(|error| format!("socket directory parent {}: {error}", above.display()))?;
    for ancestor in canonical.ancestors() {
        let meta =
            fs::metadata(ancestor).map_err(|error| format!("{}: {error}", ancestor.display()))?;
        let mode = meta.permissions().mode();
        let owned = meta.uid() == 0 || meta.uid() == euid;
        let shared = mode & 0o022 != 0;
        if !owned || (shared && mode & 0o1000 == 0) {
            return Err(format!(
                "{} (mode {}, owner uid {}) could be changed by another user, so the socket \
                 beneath it would not be private; choose another location with FUX_SOCKET",
                ancestor.display(),
                octal(mode),
                meta.uid()
            ));
        }
    }
    match fs::symlink_metadata(directory) {
        Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
            match DirBuilder::new().mode(0o700).create(directory) {
                // Set explicitly: the umask must not decide who can reach the socket.
                Ok(()) => fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                    .map_err(|error| format!("{shown}: {error}"))?,
                // A concurrent start made it first; it is checked below like any other.
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(format!("creating {shown}: {error}")),
            }
        }
        Err(error) => return Err(format!("{shown}: {error}")),
        Ok(_) => {}
    }
    let meta = fs::symlink_metadata(directory).map_err(|error| format!("{shown}: {error}"))?;
    let mode = meta.permissions().mode();
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{shown} is a symbolic link; the socket directory must be a real directory"
        ));
    }
    if !meta.is_dir() || meta.uid() != euid || mode & 0o077 != 0 {
        return Err(format!(
            "{shown} must be a directory owned by you (uid {euid}) with mode 0700; it is {} \
             with mode {} owned by uid {}. fux does not change it",
            if meta.is_dir() {
                "a directory"
            } else {
                "not a directory"
            },
            octal(mode),
            meta.uid()
        ));
    }
    Ok(())
}

/// A client refuses a socket that is not in a private directory of its own
/// user, or not a socket of its own user: keystrokes must not go to a socket
/// another user planted.
pub fn check_client_socket(path: &Path) -> Result<(), String> {
    let meta = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(format!(
                "no fux server socket at {} (start one with `fux server`, or set FUX_SOCKET)",
                path.display()
            ));
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
        Ok(meta) => meta,
    };
    let directory = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    private_directory(directory, false)?;
    if !meta.file_type().is_socket() || meta.uid() != geteuid().as_raw() {
        return Err(format!(
            "{} is not a socket owned by you; refusing to connect",
            path.display()
        ));
    }
    Ok(())
}

/// Identity of a filesystem object, so cleanup removes only what it created.
fn identity(meta: &fs::Metadata) -> (u64, u64) {
    (meta.dev(), meta.ino())
}

/// A socket file's identity, held so that it keeps meaning that file.
///
/// `(device, inode)` names a file only while its inode is allocated. ext4
/// hands a freed inode number to the next file created, 200 times out of 200
/// in a socket's case, so once the listener closes, a socket another program
/// binds at the same path can carry the same pair, and a check by identity
/// would remove it (hunt 8 finding 014). On Linux an `O_PATH` descriptor keeps
/// the inode allocated for as long as this is held, so its number cannot be
/// reused. APFS numbers files from a 64-bit counter and does not reuse them,
/// and macOS has no `O_PATH`; there the identity alone is sound.
struct Pinned {
    identity: (u64, u64),
    #[cfg(target_os = "linux")]
    _inode: std::os::fd::OwnedFd,
}

impl Pinned {
    #[cfg(target_os = "linux")]
    fn new(path: &Path) -> io::Result<Self> {
        use nix::fcntl::{OFlag, open};
        let inode = open(
            path,
            OFlag::O_PATH | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )?;
        let stat = nix::sys::stat::fstat(&inode)?;
        Ok(Self {
            identity: (stat.st_dev, stat.st_ino),
            _inode: inode,
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn new(path: &Path) -> io::Result<Self> {
        fs::symlink_metadata(path).map(|meta| Self {
            identity: identity(&meta),
        })
    }

    /// Whether the file at `path` is still the one pinned.
    fn still_at(&self, path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok_and(|meta| identity(&meta) == self.identity)
    }
}

/// The bound socket. Owns the lock that makes this server the socket's only
/// owner for its whole life. Dropping it removes the socket, if the file at
/// the path is still the one this server bound, then releases the lock.
pub struct Endpoint {
    path: PathBuf,
    socket: Pinned,
    _lock: Flock<File>,
}

impl Endpoint {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if self.socket.still_at(&self.path) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Connects with a deadline. Unix connects do not normally block, but a probe
/// must never be able to hang startup.
fn probe(path: &Path) -> io::Result<()> {
    let (sender, receiver) = mpsc::channel();
    let target = path.to_owned();
    thread::spawn(move || {
        let _ = sender.send(UnixStream::connect(target).map(drop));
    });
    receiver
        .recv_timeout(PROBE)
        .unwrap_or_else(|_| Err(io::Error::new(io::ErrorKind::TimedOut, "probe timed out")))
}

/// Takes ownership of `path` and binds it. Refuses a live server's socket and
/// anything that is not a socket; replaces only a socket proven stale.
pub fn bind_socket(path: &Path) -> Result<(Endpoint, UnixListener), String> {
    let shown = path.display().to_string();
    let directory = path
        .parent()
        .ok_or_else(|| format!("{shown} has no parent directory"))?;
    private_directory(directory, true)?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("{shown} names no file"))?;
    let euid = geteuid().as_raw();
    let foreign = |meta: &fs::Metadata| !meta.file_type().is_socket() || meta.uid() != euid;
    let refused =
        || format!("{shown} exists and is not a socket owned by you; fux will not replace it");
    // Refuse what can never be ours before creating anything beside it; the
    // same check is repeated under the lock.
    if fs::symlink_metadata(path).is_ok_and(|meta| foreign(&meta)) {
        return Err(refused());
    }
    // Every server for this path locks the same file, which is never removed,
    // so two starts cannot both believe they own the path.
    let lock_path = directory.join(format!("{}.lock", name.to_string_lossy()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(&lock_path)
        .map_err(|error| format!("{}: {error}", lock_path.display()))?;
    let lock = Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(|(_, errno)| {
        if errno == nix::errno::Errno::EWOULDBLOCK {
            format!("another fux server is already using {shown}")
        } else {
            format!("locking {}: {errno}", lock_path.display())
        }
    })?;
    // Pinned before the probe, so the file the probe found dead is the one
    // removed, even where inode numbers are reused.
    let stale = Pinned::new(path);
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{shown}: {error}")),
        Ok(meta) if foreign(&meta) => return Err(refused()),
        Ok(_) => match probe(path) {
            Ok(()) => {
                return Err(format!(
                    "another fux server is already listening on {shown}"
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                // Nothing listens: a server that died without cleanup left it.
                // The lock excludes every fux starting on this path, and the
                // private directory every other user, so it is still ours.
                if stale.as_ref().is_ok_and(|stale| stale.still_at(path)) {
                    fs::remove_file(path)
                        .map_err(|error| format!("removing stale socket {shown}: {error}"))?;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot tell whether {shown} is in use ({error}); it was left in place"
                ));
            }
        },
    }
    let fd = socket(
        AddressFamily::Unix,
        SockType::Stream,
        SockFlag::empty(),
        None,
    )
    .map_err(|error| format!("socket: {error}"))?;
    nix::fcntl::fcntl(
        &fd,
        nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
    )
    .map_err(|error| format!("socket: {error}"))?;
    let address = UnixAddr::new(path).map_err(|error| format!("{shown}: {error}"))?;
    bind(std::os::fd::AsRawFd::as_raw_fd(&fd), &address)
        .map_err(|error| format!("binding {shown}: {error}"))?;
    let endpoint = Endpoint {
        path: path.to_owned(),
        socket: Pinned::new(path).map_err(|error| format!("{shown}: {error}"))?,
        _lock: lock,
    };
    // From here a failure drops `endpoint`, which removes the bound socket.
    // Permissions are set before `listen`, so no connection is ever accepted
    // on a socket the umask left open.
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("{shown}: {error}"))?;
    listen(&fd, Backlog::new(128).map_err(|error| error.to_string())?)
        .map_err(|error| format!("listening on {shown}: {error}"))?;
    Ok((endpoint, UnixListener::from(fd)))
}

/// Accept errors that pass on their own: a peer that went away between its
/// connect and this accept, or an interrupted call. Retrying is the answer.
fn momentary(error: &io::Error) -> bool {
    use nix::libc;
    matches!(
        error.raw_os_error(),
        Some(libc::ECONNABORTED | libc::EINTR | libc::EAGAIN)
    )
}

/// Accept errors that persist until something frees a resource. Retrying the
/// same accept cannot free one, so these need their own handling; anything
/// that is neither this nor `momentary` ends the server.
fn starved(error: &io::Error) -> bool {
    use nix::libc;
    matches!(
        error.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM)
    )
}

/// A descriptor held back so the accept loop has one to spend when it runs
/// out. Without it a starved loop cannot accept, so it cannot close, so the
/// listener's backlog stays full and clients wait in silence.
fn reserve() -> Option<File> {
    File::open("/dev/null").ok()
}

/// How long descriptor pressure may last before it is reported again.
const PRESSURE_REPORT: Duration = Duration::from_secs(5);

/// The largest request body that is read. A caller chooses the size, and the
/// body is held whole before it can be looked at, so without a bound one
/// connection can make the server hold whatever it likes.
///
/// The largest legitimate request is far below this. The biggest fux itself
/// sends is a paste, bounded by `paste::LIMIT` at 64 KiB, which reaches about
/// 400 KiB if every byte needs a six-character JSON escape. A one-megabyte
/// name through `rename` or a raw `Name` insert, which the harness exercises,
/// is about 1 MiB. Four leaves room for both and still bounds the 256 MB body
/// hunt 6 sent by a factor of sixty-four.
pub const MAX_BODY: usize = 4 << 20;

/// The most requests one batch may hold. A body limit alone does not bound the
/// reply, because a small body holds many requests: 10 000 `rpc.discover`
/// calls fit in 478 KiB and were answered with 12.49 MB. fux's own clients
/// send no batches at all, and the harness's largest is 1000.
pub const MAX_BATCH: usize = 1024;

/// The most a batch reply may serialize to. The count cap alone does not bound
/// it either, because a single method can answer with a lot: `registry.schema`
/// alone is about 120 KiB. Requests past this point are answered with an error
/// instead of a result, so the reply keeps its shape.
pub const MAX_BATCH_RESPONSE: usize = 8 << 20;

/// Serves BRP on `listener` until the returned task is dropped. A fatal accept
/// error is passed to `failed`, which must make the server exit.
pub fn serve(
    listener: UnixListener,
    requests: Sender<BrpMessage>,
    failed: impl FnOnce(String) + Send + 'static,
) -> Result<Task<()>, String> {
    let listener = Async::new(listener).map_err(|error| format!("socket: {error}"))?;
    Ok(IoTaskPool::get().spawn(async move {
        let mut spare = reserve();
        // When the pressure started, and when it was last reported.
        let mut pressure: Option<(Instant, Instant)> = None;
        loop {
            match listener.accept().await {
                Ok((client, _)) => {
                    if let Some((since, _)) = pressure.take() {
                        bevy_log::error!(
                            "BRP socket accept recovered after {:.1?} of descriptor pressure.",
                            since.elapsed()
                        );
                    }
                    let requests = requests.clone();
                    IoTaskPool::get()
                        .spawn(async move {
                            let _ = http1::Builder::new()
                                .timer(SmolTimer::new())
                                .serve_connection(
                                    FuturesIo::new(client),
                                    service::service_fn(|request| batch(request, requests.clone())),
                                )
                                .await;
                        })
                        .detach();
                }
                Err(error) if starved(&error) => {
                    // Report once when it starts and at intervals afterwards.
                    // The condition can last as long as whatever holds the
                    // descriptors, and one line per attempt would be a flood.
                    let now = Instant::now();
                    match &mut pressure {
                        None => {
                            bevy_log::error!(
                                "BRP socket accept is out of descriptors ({error}). \
                                 New connections are being refused until this clears."
                            );
                            pressure = Some((now, now));
                        }
                        Some((since, reported)) if now.duration_since(*reported) >= PRESSURE_REPORT => {
                            bevy_log::warn!(
                                "BRP socket accept still out of descriptors after {:.1?} ({error}).",
                                since.elapsed()
                            );
                            *reported = now;
                        }
                        Some(_) => {}
                    }
                    // Spend the reserve to drain every connection already
                    // waiting, closing each at once. Accepting one per tick
                    // left a client behind up to a backlog (128) of others,
                    // one shed every 50 ms, so on Linux the first client under
                    // pressure waited about six seconds; the whole backlog is
                    // waiting now, so drain it now (hunt 7 finding 012). Each
                    // shed frees the descriptor again, so this is bounded by
                    // the listener's backlog and ends when nothing waits.
                    if let Some(held) = spare.take() {
                        drop(held);
                        // The listener is non-blocking, so accept takes only a
                        // connection already waiting and returns WouldBlock
                        // once the backlog is empty.
                        while let Ok((shed, _)) = listener.get_ref().accept() {
                            drop(shed);
                        }
                        spare = reserve();
                    }
                    Timer::after(Duration::from_millis(50)).await;
                }
                Err(error) if momentary(&error) => {
                    bevy_log::debug!("BRP socket accept: {error}");
                }
                Err(error) => {
                    failed(format!("BRP socket accept failed: {error}"));
                    return;
                }
            }
        }
    }))
}

enum Reply {
    Complete(BrpResponse),
    Stream(Watch),
}

/// The response body: one JSON document, or a watch's server-sent events.
pub enum Payload {
    Complete(Full<Bytes>),
    Stream(Watch),
}

impl Body for Payload {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        match self.get_mut() {
            Payload::Complete(body) => Pin::new(body).poll_frame(cx),
            Payload::Stream(body) => Pin::new(body).poll_frame(cx),
        }
    }
}

/// A `+watch` response. Dropping it (hyper does when the client goes) closes
/// the result channel, which is how the ECS side learns the watcher left.
pub struct Watch {
    id: Option<Value>,
    results: Pin<Box<Receiver<BrpResult>>>,
}

fn json(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|error| {
        format!(
            r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":{},"message":{}}}}}"#,
            error_codes::INTERNAL_ERROR,
            serde_json::Value::from(format!("response serialization: {error}"))
        )
    })
}

impl Body for Watch {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        match self.results.as_mut().poll_next(cx) {
            Poll::Ready(Some(result)) => {
                let response = BrpResponse::new(self.id.clone(), result);
                let event = format!("data: {}\n\n", json(&response));
                Poll::Ready(Some(Ok(Frame::data(Bytes::from(event)))))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.results.is_closed()
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

async fn batch(
    request: Request<Incoming>,
    requests: Sender<BrpMessage>,
) -> Result<Response<Payload>, Infallible> {
    let body = match Limited::new(request.into_body(), MAX_BODY).collect().await {
        Ok(body) => body.to_bytes(),
        Err(error) => {
            // `Limited` reports the cap through its own error type; anything
            // else is an ordinary read failure.
            let message = if error.downcast_ref::<LengthLimitError>().is_some() {
                format!("Request body exceeds the {MAX_BODY} byte limit")
            } else {
                error.to_string()
            };
            return Ok(complete(json(&invalid(None, message))));
        }
    };
    let response = match serde_json::from_slice::<BrpBatch>(&body) {
        Ok(BrpBatch::Single(request)) => match single(request, &requests).await {
            Reply::Complete(response) => complete(json(&response)),
            Reply::Stream(watch) => {
                let mut response = Response::new(Payload::Stream(watch));
                response
                    .headers_mut()
                    .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
                response
            }
        },
        Ok(BrpBatch::Batch(batch)) => {
            if batch.len() > MAX_BATCH {
                complete(json(&invalid(
                    None,
                    format!(
                        "Batch holds {} requests, more than the limit of {MAX_BATCH}",
                        batch.len()
                    ),
                )))
            } else {
                // Serialized as they are produced, so the reply can be bounded
                // without serializing any response twice.
                let mut responses: Vec<String> = Vec::with_capacity(batch.len());
                let mut budget = MAX_BATCH_RESPONSE;
                for request in batch {
                    let id = request.as_object().and_then(|map| map.get("id")).cloned();
                    let response = if budget == 0 {
                        invalid(
                            id,
                            format!(
                                "Batch reply exceeds the {MAX_BATCH_RESPONSE} byte limit; \
                                 this request was not run"
                            ),
                        )
                    } else if streaming(&request) {
                        // Refused here rather than dispatched and then refused.
                        // Dispatching opens a response channel that is dropped
                        // at once, and the runner reads that as a watcher going
                        // away, which detaches a viewer; a refused request must
                        // leave the world alone. The reply is unchanged.
                        invalid(
                            id,
                            "Streaming can not be used in batch requests".to_string(),
                        )
                    } else {
                        match single(request, &requests).await {
                            Reply::Complete(response) => response,
                            Reply::Stream(Watch { id, .. }) => invalid(
                                id,
                                "Streaming can not be used in batch requests".to_string(),
                            ),
                        }
                    };
                    let serialized = json(&response);
                    budget = budget.saturating_sub(serialized.len());
                    responses.push(serialized);
                }
                complete(format!("[{}]", responses.join(",")))
            }
        }
        Err(error) => complete(json(&invalid(None, error.to_string()))),
    };
    Ok(response)
}

fn complete(serialized: String) -> Response<Payload> {
    let mut response = Response::new(Payload::Complete(Full::new(Bytes::from(serialized))));
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

/// Whether a request in a batch names a watching method, read before the
/// request is dispatched.
fn streaming(request: &Value) -> bool {
    request
        .get("method")
        .and_then(Value::as_str)
        .is_some_and(|method| method.contains("+watch"))
}

async fn single(request: Value, requests: &Sender<BrpMessage>) -> Reply {
    // Take the id first so a request that fails to parse still reports it.
    let id = request.as_object().and_then(|map| map.get("id")).cloned();
    let request: BrpRequest = match serde_json::from_value(request) {
        Ok(request) => request,
        Err(error) => return Reply::Complete(invalid(id, error.to_string())),
    };
    let watch = request.method.contains("+watch");
    let (sender, results) = async_channel::bounded(if watch { 8 } else { 1 });
    let _ = requests
        .send(BrpMessage {
            method: request.method,
            params: request.params,
            sender,
        })
        .await;
    if watch {
        return Reply::Stream(Watch {
            id: request.id,
            results: Box::pin(results),
        });
    }
    let result = results
        .recv()
        .await
        .unwrap_or_else(|error| Err(BrpError::internal(error)));
    Reply::Complete(BrpResponse::new(request.id, result))
}

#[cfg(test)]
mod tests;
