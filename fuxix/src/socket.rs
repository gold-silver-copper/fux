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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::{UnixListener, UnixStream};

    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("fuxix-{name}-{}", std::process::id()))
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
