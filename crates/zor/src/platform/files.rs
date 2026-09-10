//! Descriptor-relative opens keep untrusted child names beneath an opened directory.
use std::{
    fs::File,
    os::fd::{AsRawFd, FromRawFd},
};

#[allow(unsafe_code)]
pub(crate) fn child(parent: &File, name: &str, directory: bool) -> anyhow::Result<File> {
    anyhow::ensure!(
        !name.is_empty() && name != "." && name != ".." && !name.contains('/'),
        "invalid child name"
    );
    let mut flags = nix::fcntl::OFlag::O_RDONLY
        | nix::fcntl::OFlag::O_CLOEXEC
        | nix::fcntl::OFlag::O_NOFOLLOW
        | nix::fcntl::OFlag::O_NONBLOCK;
    if directory {
        flags |= nix::fcntl::OFlag::O_DIRECTORY;
    }
    let fd = nix::fcntl::openat(
        Some(parent.as_raw_fd()),
        name,
        flags,
        nix::sys::stat::Mode::empty(),
    )?;
    // SAFETY: successful openat returned a fresh owned descriptor; transfer it exactly once.
    Ok(unsafe { File::from_raw_fd(fd) })
}
