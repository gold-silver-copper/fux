#![allow(unsafe_code)]
use super::{Job, Pid, Process};
use std::{ffi::CStr, io};
const PROC_PGRP_ONLY: u32 = 2;

pub(crate) fn process_identity(pid: Pid) -> io::Result<super::ProcessRef> {
    let value = info(pid).ok_or_else(|| io::Error::other("process identity unavailable"))?;
    Ok(super::ProcessRef {
        pid,
        birth: (value.pbi_start_tvsec, value.pbi_start_tvusec),
    })
}

pub(crate) fn process_children(pid: Pid) -> io::Result<Vec<super::ProcessRef>> {
    if pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid process ID",
        ));
    }
    let before = info(pid).ok_or_else(|| io::Error::other("parent process unavailable"))?;
    // PROC_PPID_ONLY is defined as 6 in the platform sys/proc_info.h.
    // One spare slot detects truncation at the 256-process traversal bound.
    let mut pids = [0; 257];
    let size = std::mem::size_of_val(&pids) as i32;
    // SAFETY: errno is thread-local; proc_listpids writes at most size bytes
    // into this live array and retains no pointer.
    let written = unsafe {
        *libc::__error() = 0;
        let written = libc::proc_listpids(6, pid as u32, pids.as_mut_ptr().cast(), size);
        if written < 0 || (written == 0 && *libc::__error() != 0) {
            return Err(io::Error::last_os_error());
        }
        written
    };
    if written >= size || written % std::mem::size_of::<Pid>() as i32 != 0 {
        return Err(io::Error::other(
            "child process list exceeds bound or is incomplete",
        ));
    }
    let mut children = Vec::new();
    for child in pids
        .into_iter()
        .take(written as usize / std::mem::size_of::<Pid>())
    {
        let entry = info(child).ok_or_else(|| io::Error::other("child process unavailable"))?;
        if child <= 0 || entry.pbi_ppid as Pid != pid {
            return Err(io::Error::other("child process changed during inspection"));
        }
        children.push(super::ProcessRef {
            pid: child,
            birth: (entry.pbi_start_tvsec, entry.pbi_start_tvusec),
        });
    }
    let after = info(pid).ok_or_else(|| io::Error::other("parent process disappeared"))?;
    if (before.pbi_start_tvsec, before.pbi_start_tvusec)
        != (after.pbi_start_tvsec, after.pbi_start_tvusec)
    {
        return Err(io::Error::other("parent process replaced"));
    }
    Ok(children)
}

pub fn process_cwd(pid: Pid) -> io::Result<std::path::PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    if pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid process ID",
        ));
    }
    // SAFETY: the zero-initialized C structure has no invalid bit patterns;
    // proc_pidinfo receives its exact allocated size and retains no pointer.
    let value: libc::proc_vnodepathinfo = unsafe {
        let mut value = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as i32;
        let written = libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            (&mut value as *mut libc::proc_vnodepathinfo).cast(),
            size,
        );
        if written <= 0 {
            return Err(io::Error::last_os_error());
        }
        if written != size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incomplete process cwd metadata",
            ));
        }
        value
    };
    // Do not trust the kernel path to be NUL terminated: bound all reads to
    // the fixed buffer, rejecting missing termination and empty paths.
    let bytes: Vec<u8> = value
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .map(|c| *c as u8)
        .collect();
    let end = bytes
        .iter()
        .position(|b| *b == 0)
        .filter(|end| *end > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid process cwd path"))?;
    let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(
        bytes.into_iter().take(end).collect(),
    ));
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "process cwd is not absolute",
        ));
    }
    Ok(path)
}

pub fn foreground_pgid(child: Pid, _: Option<i32>) -> Option<Pid> {
    info(child)
        .map(|value| value.e_tpgid as Pid)
        .filter(|value| *value > 0)
}
pub fn leader(pgid: Pid) -> Option<Process> {
    process(pgid).filter(|_| info(pgid).is_some_and(|value| value.pbi_pgid as Pid == pgid))
}
pub fn job(_: Pid, pgid: Pid) -> Job {
    unsafe {
        // SAFETY: proc_listpids writes at most the supplied allocated byte count.
        let bytes = libc::proc_listpids(PROC_PGRP_ONLY, pgid as u32, std::ptr::null_mut(), 0);
        if bytes <= 0 {
            return Job {
                leader: pgid,
                processes: Vec::new(),
            };
        }
        let count = usize::try_from(bytes).unwrap_or_default() / std::mem::size_of::<Pid>();
        let mut pids = vec![0; count];
        let size = i32::try_from(pids.len() * std::mem::size_of::<Pid>()).unwrap_or(i32::MAX);
        let written =
            libc::proc_listpids(PROC_PGRP_ONLY, pgid as u32, pids.as_mut_ptr().cast(), size);
        pids.truncate(usize::try_from(written).unwrap_or_default() / std::mem::size_of::<Pid>());
        Job {
            leader: pgid,
            processes: pids
                .into_iter()
                .filter_map(process)
                .filter(|value| info(value.pid).is_some_and(|entry| entry.pbi_pgid as Pid == pgid))
                .collect(),
        }
    }
}
fn info(pid: Pid) -> Option<libc::proc_bsdinfo> {
    unsafe {
        // SAFETY: proc_pidinfo writes at most the supplied structure size.
        let mut value = std::mem::zeroed();
        let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
        (libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut value as *mut libc::proc_bsdinfo).cast(),
            size,
        ) == size)
            .then_some(value)
    }
}
fn process(pid: Pid) -> Option<Process> {
    let value = info(pid)?;
    let comm = unsafe {
        // SAFETY: pbi_comm is a kernel-provided NUL-terminated fixed buffer.
        CStr::from_ptr(value.pbi_comm.as_ptr())
    }
    .to_string_lossy()
    .into_owned();
    let (argv0, argv, env_agent) =
        arguments(pid).unwrap_or_else(|| (Some(comm.clone()), vec![comm.clone()], None));
    Some(Process {
        pid,
        ppid: value.pbi_ppid as Pid,
        comm: comm.clone(),
        argv0,
        argv,
        env_agent,
    })
}

fn arguments(pid: Pid) -> Option<(Option<String>, Vec<String>, Option<String>)> {
    unsafe {
        // SAFETY: sysctl is first queried for size, then writes only into that allocated byte buffer.
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut size = 0usize;
        if libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        ) != 0
            || size < 4
        {
            return None;
        }
        let mut bytes = vec![0u8; size];
        if libc::sysctl(
            mib.as_mut_ptr(),
            3,
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return None;
        }
        bytes.truncate(size);
        let argc = i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?);
        let mut fields = bytes
            .get(4..)?
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty());
        let _executable = fields.next()?;
        let mut argv = Vec::new();
        for _ in 0..argc.max(0) {
            argv.push(String::from_utf8_lossy(fields.next()?).into_owned());
        }
        let argv0 = argv.first().cloned();
        let env_agent = bytes
            .split(|byte| *byte == 0)
            .find_map(|field| field.strip_prefix(b"ZOR_AGENT="))
            .map(|value| String::from_utf8_lossy(value).into_owned());
        Some((argv0, argv, env_agent))
    }
}
pub struct Guard {
    fd: i32,
    original: nix::sys::termios::Termios,
}
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: Guard owns a valid borrowed descriptor for its lifetime.
            let fd = std::os::fd::BorrowedFd::borrow_raw(self.fd);
            let _ = nix::sys::termios::tcsetattr(
                fd,
                nix::sys::termios::SetArg::TCSANOW,
                &self.original,
            );
        }
    }
}
pub fn set_raw(fd: i32) -> io::Result<Guard> {
    unsafe {
        // SAFETY: nix borrows but does not retain the descriptor.
        let borrowed = std::os::fd::BorrowedFd::borrow_raw(fd);
        let original = nix::sys::termios::tcgetattr(borrowed)?;
        let mut raw = original.clone();
        nix::sys::termios::cfmakeraw(&mut raw);
        nix::sys::termios::tcsetattr(borrowed, nix::sys::termios::SetArg::TCSANOW, &raw)?;
        Ok(Guard { fd, original })
    }
}
pub fn winsize(fd: i32) -> portable_pty::PtySize {
    unsafe {
        // SAFETY: ioctl writes to a correctly sized winsize value.
        let mut size: libc::winsize = std::mem::zeroed();
        if libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) == 0 {
            portable_pty::PtySize {
                rows: size.ws_row.max(1),
                cols: size.ws_col.max(1),
                pixel_width: size.ws_xpixel,
                pixel_height: size.ws_ypixel,
            }
        } else {
            portable_pty::PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[allow(clippy::panic)]
    fn raw_guard_restores_terminal_attributes() {
        // Phase Z §4: raw mode is restored when its guard drops.
        unsafe {
            // SAFETY: openpty initializes both descriptors; all termios buffers have valid sizes.
            let mut master = -1;
            let mut fd = -1;
            assert_eq!(
                libc::openpty(
                    &mut master,
                    &mut fd,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                0
            );
            let borrowed = std::os::fd::BorrowedFd::borrow_raw(fd);
            let before = nix::sys::termios::tcgetattr(borrowed)
                .unwrap_or_else(|error| panic!("tcgetattr: {error}"));
            let Ok(guard) = set_raw(fd) else { return };
            drop(guard);
            let after = nix::sys::termios::tcgetattr(borrowed)
                .unwrap_or_else(|error| panic!("tcgetattr: {error}"));
            assert_eq!(before.input_flags, after.input_flags);
            assert_eq!(before.output_flags, after.output_flags);
            assert_eq!(before.control_flags, after.control_flags);
            assert_eq!(
                before.local_flags,
                after.local_flags & !nix::sys::termios::LocalFlags::PENDIN
            );
            assert_eq!(before.control_chars, after.control_chars);
            libc::close(master);
            libc::close(fd);
        }
    }

    #[test]
    fn spawned_job_exposes_processes_and_arguments() -> anyhow::Result<()> {
        // Phase Z §4: the real platform adapter lists a spawned job and its argv.
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 2"])
            .env("ZOR_AGENT", "claude")
            .spawn()?;
        let pid = i32::try_from(child.id())?;
        let mut child_process = None;
        for _ in 0..20 {
            child_process = process(pid);
            if child_process.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let child_process = child_process.ok_or_else(|| anyhow::anyhow!("child unavailable"))?;
        let pgid = info(pid)
            .map(|value| value.pbi_pgid as Pid)
            .ok_or_else(|| anyhow::anyhow!("child has no process group"))?;
        let listing = job(pid, pgid);
        assert!(child_process.argv.iter().any(|arg| arg == "sleep 2"));
        assert!(listing.processes.iter().any(|process| process.pid == pid));
        child.kill()?;
        let _ = child.wait()?;
        Ok(())
    }
}
