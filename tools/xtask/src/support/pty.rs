//! PTY descriptors remain fixture-owned; only requested stdio crosses exec.
use anyhow::Result;

pub fn open(rows: u16, columns: u16) -> Result<nix::pty::OpenptyResult> {
    let pty = nix::pty::openpty(
        Some(&nix::pty::Winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }),
        None,
    )?;
    for fd in [&pty.master, &pty.slave] {
        nix::fcntl::fcntl(
            fd,
            nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
        )?;
    }
    Ok(pty)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn originals_do_not_leak_through_exec() -> Result<()> {
        let pty = open(24, 80)?;
        for fd in [&pty.master, &pty.slave] {
            assert_ne!(
                nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFD)?
                    & nix::fcntl::FdFlag::FD_CLOEXEC.bits(),
                0
            );
        }
        Ok(())
    }
}
