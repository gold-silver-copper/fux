//! The system, through the baseline's and the current crates: fuxix's errnos
//! read and print alike; making pipes, sockets and PTYs nonblocking and back
//! returns the same, and leaves reads that do not wait; bytes go through a
//! pipe alike; and the config file's path comes out the same in every mix
//! of the variables it is found from.
use crate::rng::Rng;
use crate::{Outcome, bump, same, times};
use std::os::fd::{AsFd, OwnedFd};
use std::process::Command;

/// The hidden argument that prints both config paths: the environment is
/// the one `default_path` reads, and only a process of its own can have it
/// set without touching this one's.
pub const PROBE: &str = "--probe-config-path";

pub fn probe() -> String {
    format!(
        "{:?}\n{:?}",
        baseline::config::default_path(),
        fux::config::default_path()
    )
}

macro_rules! stack {
    ($name:ident, $ix:ident) => {
        mod $name {
            use std::os::fd::{AsFd, OwnedFd};
            use $ix::Errno;

            pub fn errno(n: i32) -> String {
                let error = std::io::Error::from_raw_os_error(n);
                match Errno::from_io_error(&error) {
                    Some(e) => format!("{e:?} {e} {}", e.raw()),
                    None => "none".into(),
                }
            }

            /// Each request's result, then, if the last asked not to wait,
            /// what reading the empty end gives.
            pub fn nonblocking(fd: impl AsFd, requests: &[bool]) -> String {
                let results: Vec<String> = requests
                    .iter()
                    .map(|on| format!("{:?}", $ix::io::set_nonblocking(fd.as_fd(), *on)))
                    .collect();
                let read = if requests.last().copied().unwrap_or(false) {
                    let mut buffer = [0u8; 8];
                    format!("{:?}", $ix::io::read(fd.as_fd(), &mut buffer))
                } else {
                    String::new()
                };
                format!("{results:?} {read}")
            }

            /// Writes `bytes` into a pipe and reads them back.
            pub fn pipe(write: &OwnedFd, read: &OwnedFd, bytes: &[u8]) -> String {
                let wrote = $ix::io::write(write, bytes);
                let mut buffer = vec![0u8; bytes.len().saturating_add(8)];
                let got = $ix::io::read(read, &mut buffer);
                let back = got.map(|n| buffer.get(..n).unwrap_or_default().to_vec());
                format!("{wrote:?} {back:?}")
            }
        }
    };
}

stack!(base, baseline_ix);
stack!(cur, fuxix);

/// A pipe, as two descriptors.
fn pipe() -> Result<(OwnedFd, OwnedFd), String> {
    let (read, write) = std::io::pipe().map_err(|e| format!("pipe: {e}"))?;
    Ok((OwnedFd::from(read), OwnedFd::from(write)))
}

/// Descriptors of every kind that matters: a pipe's read end, a socket and
/// a PTY master, each with what keeps its other end open.
fn descriptors() -> Result<Vec<(&'static str, OwnedFd, OwnedFd)>, String> {
    let (read, write) = pipe()?;
    let (socket, peer) =
        std::os::unix::net::UnixStream::pair().map_err(|e| format!("socketpair: {e}"))?;
    let mut out = vec![
        ("pipe", read, write),
        ("socket", OwnedFd::from(socket), OwnedFd::from(peer)),
    ];
    if let Some(Ok((master, slave))) = fuxix::terminal::Size::new(24, 80).map(fuxix::pty::open) {
        let slave = slave
            .as_fd()
            .try_clone_to_owned()
            .map_err(|e| e.to_string())?;
        out.push(("pty", master.into(), slave));
    }
    Ok(out)
}

pub fn run(r: &mut Rng, scale: usize) -> Outcome {
    for n in 0..=200 {
        same(&format!("errno {n}"), base::errno(n), cur::errno(n))?;
    }
    let mut requests = 0u64;
    for case in 0..times(300, scale) {
        let asked: Vec<bool> = (0..r.below(4).saturating_add(1))
            .map(|_| r.chance(50))
            .collect();
        // Each side its own descriptors, from the same state.
        for ((kind, a, _keep_a), (_, b, _keep_b)) in descriptors()?.into_iter().zip(descriptors()?)
        {
            same(
                &format!("set_nonblocking {case} on a {kind}: {asked:?}"),
                base::nonblocking(a.as_fd(), &asked),
                cur::nonblocking(b.as_fd(), &asked),
            )?;
            bump(&mut requests);
        }
    }
    let mut pipes = 0u64;
    for case in 0..times(300, scale) {
        let bytes: Vec<u8> = (0..r.below(4000))
            .map(|_| r.next().to_le_bytes()[0])
            .collect();
        let (read_a, write_a) = pipe()?;
        let (read_b, write_b) = pipe()?;
        same(
            &format!("pipe {case}: {} bytes", bytes.len()),
            base::pipe(&write_a, &read_a, &bytes),
            cur::pipe(&write_b, &read_b, &bytes),
        )?;
        bump(&mut pipes);
    }
    let exe = std::env::current_exe().map_err(|e| format!("this binary: {e}"))?;
    let mut paths = 0u64;
    for xdg in [None, Some(""), Some("/x/config"), Some("relative")] {
        for home in [None, Some(""), Some("/home/u")] {
            let mut command = Command::new(&exe);
            command.arg(PROBE).env_clear();
            if let Some(xdg) = xdg {
                command.env("XDG_CONFIG_HOME", xdg);
            }
            if let Some(home) = home {
                command.env("HOME", home);
            }
            let output = command
                .output()
                .map_err(|e| format!("{}: {e}", exe.display()))?;
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            let mut lines = text.lines();
            let (a, b) = (lines.next(), lines.next());
            if a.is_none() {
                return Err(format!("{PROBE} printed nothing: {text:?}"));
            }
            same(
                &format!("the config path, XDG_CONFIG_HOME {xdg:?} HOME {home:?}"),
                a,
                b,
            )?;
            bump(&mut paths);
        }
    }
    Ok(format!(
        "201 errnos, {requests} nonblocking requests, {pipes} pipes, the config path in {paths} environments"
    ))
}
