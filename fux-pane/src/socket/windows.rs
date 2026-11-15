/// One end of a link between fux's own processes, or a client's.
pub type Stream = fuxix::io::Stream;
/// A connection, as accepted: to serve, or to hand to its connection's
/// process.
pub type Accepted = fuxix::socket::Connection;
/// A pane's process's pipe, where its server and connections reach it.
pub type PaneListener = fuxix::socket::Listener;

/// Streams are always read and written without waiting here.
pub fn nonblocking(_stream: &Stream) -> std::io::Result<()> {
    Ok(())
}

/// The next connected at `listener`: `WouldBlock` while none has.
pub fn accept(listener: &mut PaneListener) -> std::io::Result<Accepted> {
    Ok(listener.accept()?)
}

/// Who connected on `accepted`, as the system says.
pub fn peer(accepted: &Accepted) -> fuxix::Result<fuxix::socket::Peer> {
    fuxix::socket::peer(accepted, accepted.connected()?)
}

/// `accepted` as a stream to serve here.
pub fn serve(accepted: Accepted) -> std::io::Result<Stream> {
    Ok(accepted.stream()?)
}

/// A pane's process made its pipe itself: nothing more to listen on.
pub fn pane_listen(_listener: &PaneListener) -> std::io::Result<()> {
    Ok(())
}

/// A pane's process has begun: its pipe is read without waiting already.
pub fn pane_begun(_listener: &PaneListener) -> std::io::Result<()> {
    Ok(())
}

/// The next to reach a pane's process at `listener`, waited for: its
/// server, till it has begun.
pub fn pane_next(listener: &mut PaneListener) -> std::io::Result<Stream> {
    use fuxix::poll::{Events, PollFd};
    loop {
        match listener.accept() {
            Ok(connection) => return Ok(connection.stream()?),
            Err(fuxix::Errno::AGAIN) => {}
            Err(error) => return Err(error.into()),
        }
        let mut fds = [PollFd::new(&*listener, Events::IN)];
        fuxix::poll::poll(&mut fds, None)?;
    }
}
