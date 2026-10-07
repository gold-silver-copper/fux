//! Unix stream sockets: made one step at a time, because fux sets a
//! socket's mode between binding it and listening on it, which std's
//! `UnixListener::bind` does in one.
use crate::errno::{Errno, Result, check};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// A new Unix stream socket, close-on-exec: atomically where the system
/// allows, and on macOS, which has no flag for it, straight after.
pub fn stream() -> Result<OwnedFd> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let kind = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
    #[cfg(target_os = "macos")]
    let kind = libc::SOCK_STREAM;
    // SAFETY: socket takes three numbers and touches no memory.
    let raw = check(unsafe { libc::socket(libc::AF_UNIX, kind, 0) })?;
    // SAFETY: `raw` was just opened, is valid, and nothing else owns it.
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };
    #[cfg(target_os = "macos")]
    crate::io::set_cloexec(&socket)?;
    Ok(socket)
}

/// `path` as a socket address, and the address's length. A path with a
/// NUL, or too long for the address, is refused.
fn address(path: &Path) -> Result<(libc::sockaddr_un, libc::socklen_t)> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.contains(&0) {
        return Err(Errno::INVAL);
    }
    // SAFETY: an all-zero sockaddr_un is a valid, empty one.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    // The path and its NUL must fit.
    if bytes.len() >= address.sun_path.len() {
        return Err(Errno::NAMETOOLONG);
    }
    address.sun_family = libc::sa_family_t::try_from(libc::AF_UNIX).map_err(|_| Errno::INVAL)?;
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = libc::c_char::from_ne_bytes([*byte]);
    }
    let length = std::mem::offset_of!(libc::sockaddr_un, sun_path)
        .checked_add(bytes.len())
        .and_then(|n| n.checked_add(1))
        .ok_or(Errno::NAMETOOLONG)?;
    #[cfg(target_os = "macos")]
    {
        address.sun_len = u8::try_from(length).map_err(|_| Errno::NAMETOOLONG)?;
    }
    let length = libc::socklen_t::try_from(length).map_err(|_| Errno::NAMETOOLONG)?;
    Ok((address, length))
}

/// Binds `socket` to `path`, which must not exist.
pub fn bind(socket: impl AsFd, path: &Path) -> Result<()> {
    let (address, length) = address(path)?;
    // SAFETY: the pointer is to a whole sockaddr_un, of which `length`
    // bytes are the address.
    let bound = unsafe {
        libc::bind(
            socket.as_fd().as_raw_fd(),
            std::ptr::from_ref(&address).cast(),
            length,
        )
    };
    check(bound).map(drop)
}

/// Starts listening on `socket`, with room for `backlog` connections
/// waiting to be accepted.
pub fn listen(socket: impl AsFd, backlog: i32) -> Result<()> {
    // SAFETY: listen takes two numbers and touches no memory.
    check(unsafe { libc::listen(socket.as_fd().as_raw_fd(), backlog) }).map(drop)
}

/// Connects `socket` to `path`. On a nonblocking socket a connection that
/// cannot finish at once fails with `AGAIN` or `INPROGRESS`.
pub fn connect(socket: impl AsFd, path: &Path) -> Result<()> {
    let (address, length) = address(path)?;
    // SAFETY: the pointer is to a whole sockaddr_un, of which `length`
    // bytes are the address.
    let connected = unsafe {
        libc::connect(
            socket.as_fd().as_raw_fd(),
            std::ptr::from_ref(&address).cast(),
            length,
        )
    };
    check(connected).map(drop)
}

/// The effective user ID of the process at the other end of the connected
/// Unix socket `socket`, as the kernel recorded it.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn peer_uid(socket: impl AsFd) -> Result<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length =
        libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).map_err(|_| Errno::INVAL)?;
    // SAFETY: the pointers are to a whole ucred and its length, which the
    // call fills and updates.
    let got = unsafe {
        libc::getsockopt(
            socket.as_fd().as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut credentials).cast(),
            &mut length,
        )
    };
    check(got)?;
    Ok(credentials.uid)
}

/// The effective user ID of the process at the other end of the connected
/// Unix socket `socket`, as the kernel recorded it.
#[cfg(target_os = "macos")]
pub fn peer_uid(socket: impl AsFd) -> Result<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: the descriptor is valid for the call's duration and both
    // out-pointers point at initialised locals.
    check(unsafe { libc::getpeereid(socket.as_fd().as_raw_fd(), &mut uid, &mut gid) })?;
    Ok(uid)
}

/// Room for a control message of a few descriptors, aligned as `cmsghdr`
/// is.
#[repr(C, align(8))]
struct Control([u8; 64]);

/// `value` as the type a field of a control message has, which differs
/// between platforms (`usize` on Linux, `u32` on macOS).
fn fit<T: TryFrom<U>, U>(value: U) -> Result<T> {
    T::try_from(value).map_err(|_| Errno::INVAL)
}

/// The size of one descriptor, as the CMSG macros take sizes.
fn int_size() -> Result<libc::c_uint> {
    libc::c_uint::try_from(std::mem::size_of::<libc::c_int>()).map_err(|_| Errno::INVAL)
}

/// Sends `bytes` on the connected Unix socket `socket` with `fd` attached
/// (`SCM_RIGHTS`): the peer receives a descriptor for the same open file
/// with the bytes. How many of the bytes were sent; the descriptor goes
/// with the first.
pub fn send_with_fd(socket: impl AsFd, bytes: &[u8], fd: impl AsFd) -> Result<usize> {
    let raw: libc::c_int = fd.as_fd().as_raw_fd();
    let mut control = Control([0; 64]);
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    // SAFETY: CMSG_SPACE and CMSG_LEN compute sizes from a number.
    let space = unsafe { libc::CMSG_SPACE(int_size()?) };
    // SAFETY: as above.
    let length = unsafe { libc::CMSG_LEN(int_size()?) };
    // SAFETY: an all-zero msghdr is a valid, empty one.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    // The header and the descriptor fit the buffer, as the SAFETY below says.
    if usize::try_from(space).map_or(true, |space| space > control.0.len()) {
        return Err(Errno::INVAL);
    }
    message.msg_control = control.0.as_mut_ptr().cast();
    message.msg_controllen = fit(space)?;
    // SAFETY: `message` has a control buffer of `space` bytes, at most its
    // 64 (checked above): its first header is in it, or null.
    let first = unsafe { libc::CMSG_FIRSTHDR(&message) };
    // SAFETY: a non-null first header points into `control`, aligned.
    let header = unsafe { first.as_mut() }.ok_or(Errno::INVAL)?;
    header.cmsg_level = libc::SOL_SOCKET;
    header.cmsg_type = libc::SCM_RIGHTS;
    header.cmsg_len = fit(length)?;
    // SAFETY: the header's data follows it in `control`, with room for one
    // descriptor, as CMSG_SPACE reserved.
    let data = unsafe { libc::CMSG_DATA(header) };
    // SAFETY: as above; the data may be unaligned for an int.
    unsafe { std::ptr::write_unaligned(data.cast::<libc::c_int>(), raw) };
    // SAFETY: the message's buffers live for the call.
    let sent = unsafe { libc::sendmsg(socket.as_fd().as_raw_fd(), &message, 0) };
    usize::try_from(sent).map_err(|_| Errno::last())
}

/// Receives into `buffer` from the Unix socket `socket`, and a descriptor
/// sent with the bytes (`SCM_RIGHTS`), if one was: close-on-exec. Any more
/// descriptors than one are closed. How many bytes were received.
pub fn recv_with_fd(socket: impl AsFd, buffer: &mut [u8]) -> Result<(usize, Option<OwnedFd>)> {
    let mut control = Control([0; 64]);
    let mut iov = libc::iovec {
        iov_base: buffer.as_mut_ptr().cast(),
        iov_len: buffer.len(),
    };
    // SAFETY: an all-zero msghdr is a valid, empty one.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.0.as_mut_ptr().cast();
    message.msg_controllen = fit(control.0.len())?;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let flags = libc::MSG_CMSG_CLOEXEC;
    #[cfg(target_os = "macos")]
    let flags = 0;
    // SAFETY: the message's buffers live for the call, of the sizes given.
    let got = unsafe { libc::recvmsg(socket.as_fd().as_raw_fd(), &mut message, flags) };
    let got = usize::try_from(got).map_err(|_| Errno::last())?;
    // SAFETY: CMSG_LEN computes a size from a number.
    let empty = usize::try_from(unsafe { libc::CMSG_LEN(0) }).map_err(|_| Errno::INVAL)?;
    let size = std::mem::size_of::<libc::c_int>();
    let mut received = Vec::new();
    // SAFETY: the kernel filled `msg_controllen` bytes of the control buffer
    // with whole headers: the first is in it, or null.
    let mut next = unsafe { libc::CMSG_FIRSTHDR(&message) };
    // SAFETY: each header the macros give points into `control`, aligned.
    while let Some(header) = unsafe { next.as_ref() } {
        if header.cmsg_level == libc::SOL_SOCKET && header.cmsg_type == libc::SCM_RIGHTS {
            // SAFETY: the header's data follows it in `control`.
            let data = unsafe { libc::CMSG_DATA(header) }.cast::<libc::c_int>();
            let length = fit::<usize, _>(header.cmsg_len)
                .unwrap_or(0)
                .saturating_sub(empty);
            for i in 0..length.checked_div(size).unwrap_or(0) {
                // SAFETY: descriptor `i` of the header's data is within its
                // `cmsg_len` bytes; it may be unaligned.
                let raw = unsafe { std::ptr::read_unaligned(data.wrapping_add(i)) };
                if raw >= 0 {
                    // SAFETY: the kernel made `raw` for this process, and
                    // nothing else owns it.
                    received.push(unsafe { OwnedFd::from_raw_fd(raw) });
                }
            }
        }
        // SAFETY: `header` is a header of `message`'s control buffer.
        next = unsafe { libc::CMSG_NXTHDR(&message, header) };
    }
    let fd = received.into_iter().next();
    #[cfg(target_os = "macos")]
    if let Some(fd) = &fd {
        crate::io::set_cloexec(fd)?;
    }
    Ok((got, fd))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::{UnixListener, UnixStream};

    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("fuxix-{name}-{}", std::process::id()))
    }

    /// A descriptor sent with bytes arrives with them, for the same open
    /// file; bytes sent without one arrive without one.
    #[test]
    fn a_descriptor_goes_with_the_bytes_it_was_sent_with() -> std::result::Result<(), String> {
        use std::io::{Read, Write};
        let (a, b) = UnixStream::pair().map_err(|e| e.to_string())?;
        let (mut read_end, mut write_end) = UnixStream::pair().map_err(|e| e.to_string())?;
        assert_eq!(send_with_fd(&a, b"hello", &write_end), Ok(5));
        let mut buffer = [0u8; 16];
        let (n, fd) = recv_with_fd(&b, &mut buffer).map_err(|e| e.to_string())?;
        assert_eq!(buffer.get(..n), Some(&b"hello"[..]));
        let fd = fd.ok_or("no descriptor came")?;
        // Writing to it writes to the other end of the pair it came from.
        let mut sent = std::fs::File::from(fd);
        sent.write_all(b"via").map_err(|e| e.to_string())?;
        let mut got = [0u8; 3];
        read_end.read_exact(&mut got).map_err(|e| e.to_string())?;
        assert_eq!(&got, b"via");
        (&a).write_all(b"plain").map_err(|e| e.to_string())?;
        let (n, fd) = recv_with_fd(&b, &mut buffer).map_err(|e| e.to_string())?;
        assert_eq!((buffer.get(..n), fd.is_none()), (Some(&b"plain"[..]), true));
        write_end.flush().map_err(|e| e.to_string())?;
        Ok(())
    }

    #[test]
    fn a_socket_binds_listens_and_is_connected_to() -> std::result::Result<(), String> {
        let dir = scratch("bind");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("s.sock");
        let socket = stream().map_err(|e| e.to_string())?;
        bind(&socket, &path).map_err(|e| e.to_string())?;
        listen(&socket, 8).map_err(|e| e.to_string())?;
        let listener = UnixListener::from(socket);
        let client = stream().map_err(|e| e.to_string())?;
        connect(&client, &path).map_err(|e| e.to_string())?;
        let (served, _) = listener.accept().map_err(|e| e.to_string())?;
        assert_eq!(peer_uid(&served), Ok(crate::process::geteuid()));
        let client = UnixStream::from(client);
        assert_eq!(peer_uid(&client), Ok(crate::process::geteuid()));
        // Nowhere to connect: an error. (Not "the listener is dropped": on
        // macOS the socket is made close-on-exec a step after it is made,
        // and a child another test spawns in between keeps it listening.)
        drop((listener, served));
        let nowhere = stream().map_err(|e| e.to_string())?;
        assert!(connect(&nowhere, &dir.join("none.sock")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn a_path_that_does_not_fit_is_refused() {
        let long = Path::new("/tmp").join(std::iter::repeat_n('x', 200).collect::<String>());
        assert_eq!(address(&long).map(|(_, n)| n), Err(Errno::NAMETOOLONG));
        assert_eq!(
            address(Path::new("/tmp/a\0b")).map(|(_, n)| n),
            Err(Errno::INVAL)
        );
    }
}
