use std::os::unix::net::{UnixListener, UnixStream};

/// One end of a link between fux's own processes, or a client's.
pub type Stream = UnixStream;
/// A connection, as accepted: to serve, or to hand to its connection's
/// process.
pub type Accepted = UnixStream;
/// A pane's process's socket, where its server and connections reach it.
pub type PaneListener = UnixListener;

/// `stream` read and written without waiting.
pub fn nonblocking(stream: &Stream) -> std::io::Result<()> {
    stream.set_nonblocking(true)
}

/// The next waiting at `listener`.
pub fn accept(listener: &mut PaneListener) -> std::io::Result<Accepted> {
    listener.accept().map(|(stream, _)| stream)
}

/// Who connected on `accepted`, as the system says.
pub fn peer(accepted: &Accepted) -> fuxix::Result<fuxix::socket::Peer> {
    fuxix::socket::peer(accepted)
}

/// `accepted` as a stream to serve here.
pub fn serve(accepted: Accepted) -> std::io::Result<Stream> {
    Ok(accepted)
}

/// A pane's process listens at `listener` itself, again: one that reaches
/// it is told its pid, not the server's that made the socket
/// (`SO_PEERCRED`).
pub fn pane_listen(listener: &PaneListener) -> Result<(), std::io::Error> {
    Ok(fuxix::socket::listen(listener, PANE_BACKLOG)?)
}

/// How many may wait at a pane's socket untaken: one more finds it busy.
const PANE_BACKLOG: i32 = 16;

/// The next to reach a pane's process at `listener`, waited for: its
/// server, till it has begun.
pub fn pane_next(listener: &mut PaneListener) -> std::io::Result<Stream> {
    listener.accept().map(|(stream, _)| stream)
}

/// A pane's process has begun: `listener` is read without waiting from
/// now on.
pub fn pane_begun(listener: &PaneListener) -> std::io::Result<()> {
    listener.set_nonblocking(true)
}
