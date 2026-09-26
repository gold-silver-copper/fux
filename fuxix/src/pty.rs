//! Pseudoterminals.
use crate::errno::{Errno, check};
use crate::terminal;
use std::fmt;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

/// A step of opening a PTY that failed, and why.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Error {
    pub call: &'static str,
    pub errno: Errno,
}

/// "grantpt: Permission denied (os error 13)".
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.call, self.errno)
    }
}

impl std::error::Error for Error {}

fn at(call: &'static str) -> impl FnOnce(Errno) -> Error {
    move |errno| Error { call, errno }
}

/// Opens a PTY of `rows` by `cols`: its master and its slave, both
/// close-on-exec, neither the controlling terminal of this process.
///
/// macOS, when PTYs are allocated and freed quickly, by any processes, has
/// two races this works around (`docs/apple-feedback-ptmx-eredriveopen.md` in
/// the fux repository has the details and reproducers):
/// - Concurrent opens of `/dev/ptmx` can be handed the same PTY number, and
///   the kernel retries the loser only ten times, sleeping up to 100 ms
///   between tries, before it returns its own internal code, `EREDRIVEOPEN`
///   (errno -6). So the master is opened by one thread of the process at a
///   time, and retried on that code; if the retries run out, the error is
///   `AGAIN`.
/// - A master can open with no replica node in `/dev`; `grantpt` on it then
///   never returns, as the kernel restarts it forever. So the replica is
///   looked up first, and `grantpt` is watched: a master without a replica,
///   or whose grant stalls, is closed, and another PTY opened in its place;
///   after `ALLOCATE_TRIES` of those, the error is `AGAIN` from `grantpt`.
pub fn open(rows: u16, cols: u16) -> Result<(OwnedFd, OwnedFd), Error> {
    let master = granted_master()?;
    // SAFETY: unlockpt takes a descriptor and touches no memory.
    check(unsafe { libc::unlockpt(master.as_raw_fd()) }).map_err(at("unlockpt"))?;
    let name = slave_name(&master).map_err(at("ptsname"))?;
    // std opens close-on-exec, atomically, on every platform.
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(&name)
        .map_err(|e| Errno::from_io_error(&e).unwrap_or(Errno::INVAL))
        .map_err(at("opening the PTY slave"))?;
    terminal::set_window_size(&master, rows.max(1), cols.max(1)).map_err(at("TIOCSWINSZ"))?;
    Ok((master, OwnedFd::from(slave)))
}

/// A new PTY master, granted.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn granted_master() -> Result<OwnedFd, Error> {
    let master = open_master().map_err(at("posix_openpt"))?;
    // SAFETY: grantpt takes a descriptor and touches no memory.
    check(unsafe { libc::grantpt(master.as_raw_fd()) }).map_err(at("grantpt"))?;
    Ok(master)
}

/// A new PTY master, granted, with a replica in `/dev` (see `open`). A master
/// that fails either is dropped, which frees its PTY, and another opened.
#[cfg(target_os = "macos")]
fn granted_master() -> Result<OwnedFd, Error> {
    for _ in 0..ALLOCATE_TRIES {
        let master = open_master().map_err(at("posix_openpt"))?;
        // The name comes from the PTY's number, whether or not its node exists.
        let name = slave_name(&master).map_err(at("ptsname"))?;
        if std::fs::symlink_metadata(&name).is_err() {
            continue;
        }
        // SAFETY: grantpt takes a descriptor and touches no memory.
        let grant = || check(unsafe { libc::grantpt(master.as_raw_fd()) });
        match watched(master.as_fd(), GRANT_LIMIT, grant) {
            Watched::Done(granted) => return granted.map(|_| master).map_err(at("grantpt")),
            // The master is /dev/null now; dropping it closes that.
            Watched::Stalled => {}
        }
    }
    Err(Error {
        call: "grantpt",
        errno: Errno::AGAIN,
    })
}

/// How many PTYs `granted_master` opens before it gives up on finding one
/// with a replica whose grant returns.
#[cfg(target_os = "macos")]
const ALLOCATE_TRIES: u32 = 4;

/// How long `grantpt` may take before it is taken to be the kernel's endless
/// restart. It changes a node's owner and mode, which takes microseconds.
#[cfg(target_os = "macos")]
const GRANT_LIMIT: std::time::Duration = std::time::Duration::from_secs(1);

/// What `watched` saw of its call.
#[cfg(any(target_os = "macos", test))]
enum Watched<T> {
    /// It returned within the limit, and this is what it returned.
    Done(T),
    /// It had not returned within the limit, so `fd` was replaced with
    /// `/dev/null`; whatever it returned is from `/dev/null`.
    Stalled,
}

/// Runs `call`, a call on `fd` that could otherwise never return: if it has not
/// returned within `limit`, a watchdog thread replaces `fd` with `/dev/null`, so
/// the call, which the kernel keeps restarting, fails with `/dev/null`'s error
/// and returns. The descriptor stays open throughout, as the caller holds it;
/// only the file behind it changes. If the watchdog cannot be started, `call`
/// runs unwatched.
#[cfg(any(target_os = "macos", test))]
fn watched<T>(
    fd: std::os::fd::BorrowedFd<'_>,
    limit: std::time::Duration,
    call: impl FnOnce() -> T,
) -> Watched<T> {
    use std::sync::{Arc, Condvar, Mutex, PoisonError};
    let raw = fd.as_raw_fd();
    // (the call returned, fd was replaced), and the watchdog's wake-up.
    let shared = Arc::new((Mutex::new((false, false)), Condvar::new()));
    let watchdog_shared = Arc::clone(&shared);
    let watchdog = std::thread::Builder::new()
        .name("fuxix-watchdog".to_owned())
        .spawn(move || {
            let (state, wake) = &*watchdog_shared;
            let mut state = wake
                .wait_timeout_while(
                    state.lock().unwrap_or_else(PoisonError::into_inner),
                    limit,
                    |(returned, _)| !*returned,
                )
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            // The lock is held until the replacement is recorded, so the caller
            // sees either a call that returned first or a replaced descriptor.
            if !state.0 {
                state.1 = replace_with_null(raw);
            }
            drop(state);
        });
    let result = call();
    let replaced = {
        let (state, wake) = &*shared;
        let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
        state.0 = true;
        wake.notify_one();
        state.1
    };
    if let Ok(watchdog) = watchdog {
        let _ = watchdog.join();
    }
    if replaced {
        Watched::Stalled
    } else {
        Watched::Done(result)
    }
}

/// Makes the descriptor `raw` refer to `/dev/null`, closing what it referred
/// to; whether it did.
#[cfg(any(target_os = "macos", test))]
fn replace_with_null(raw: libc::c_int) -> bool {
    let Ok(null) = std::fs::File::open("/dev/null") else {
        return false;
    };
    // SAFETY: dup2 takes two descriptor numbers and touches no memory. `raw`
    // is open: `watched`'s caller holds it, and closes it only after `watched`
    // returns, which is after this thread has finished.
    let replaced = unsafe { libc::dup2(null.as_raw_fd(), raw) };
    replaced >= 0
}

/// A new PTY master, close-on-exec: atomically where the system allows,
/// and on macOS, which has no flag for it, straight after.
fn open_master() -> crate::Result<OwnedFd> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    // SAFETY: posix_openpt takes flags and touches no memory.
    let raw =
        check(unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) })?;
    #[cfg(target_os = "macos")]
    let raw = open_master_serialized()?;
    // SAFETY: `raw` was just opened, is valid, and nothing else owns it.
    let master = unsafe { OwnedFd::from_raw_fd(raw) };
    #[cfg(target_os = "macos")]
    crate::io::set_cloexec(&master)?;
    Ok(master)
}

/// `posix_openpt` on macOS, by one thread of this process at a time and
/// retried while the kernel reports the PTY allocation race (`redrive`).
///
/// The lock keeps this process's threads from racing each other into the
/// kernel's retry loop, whose sleeps made single opens take over 400 ms. It is
/// held through the retries' own sleeps too: while one opener is losing to
/// another process, this process's other threads would only join the race.
#[cfg(target_os = "macos")]
fn open_master_serialized() -> crate::Result<libc::c_int> {
    static OPENING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // A poisoned lock only means an opener panicked; there is nothing to repair.
    let _one = OPENING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    redrive(
        // SAFETY: posix_openpt takes flags and touches no memory.
        || check(unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) }),
        std::thread::sleep,
    )
}

/// How many times `redrive` calls `open`: sleeping 1, 2, … 7 ms between
/// tries, 28 ms in all, on top of the kernel's own retries of each.
#[cfg(any(target_os = "macos", test))]
const REDRIVE_TRIES: u32 = 8;

/// `open`, called again while it fails with `EREDRIVEOPEN`, after `sleep`ing
/// a millisecond longer each time; `AGAIN` if it still fails after
/// `REDRIVE_TRIES` calls. Every other result, `INTR` included, is returned
/// at once: this retries only the open the kernel meant to retry itself.
#[cfg(any(target_os = "macos", test))]
fn redrive<T>(
    mut open: impl FnMut() -> crate::Result<T>,
    mut sleep: impl FnMut(std::time::Duration),
) -> crate::Result<T> {
    let mut tries: u32 = 1;
    loop {
        match open() {
            Err(errno) if errno == Errno::REDRIVEOPEN => {
                if tries >= REDRIVE_TRIES {
                    return Err(Errno::AGAIN);
                }
                sleep(std::time::Duration::from_millis(u64::from(tries)));
                tries = tries.saturating_add(1);
            }
            result => return result,
        }
    }
}

/// The path of the slave of the PTY `master`.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn slave_name(master: impl AsFd) -> crate::Result<PathBuf> {
    let mut buffer: [libc::c_char; 128] = [0; 128];
    // SAFETY: the buffer is writable for its length, which is passed; the
    // call reports its errors as a number rather than in errno.
    let error = unsafe {
        libc::ptsname_r(
            master.as_fd().as_raw_fd(),
            buffer.as_mut_ptr(),
            buffer.len(),
        )
    };
    if error != 0 {
        return Err(
            Errno::from_io_error(&std::io::Error::from_raw_os_error(error)).unwrap_or(Errno::INVAL),
        );
    }
    Ok(path(&buffer))
}

/// The path of the slave of the PTY `master`. macOS's `ptsname` returns a
/// shared buffer; its ioctl fills one of ours.
#[cfg(target_os = "macos")]
fn slave_name(master: impl AsFd) -> crate::Result<PathBuf> {
    let mut buffer: [libc::c_char; 128] = [0; 128];
    let get = terminal::request(libc::TIOCPTYGNAME)?;
    // SAFETY: TIOCPTYGNAME writes at most 128 bytes, NUL included, and the
    // buffer is 128 bytes.
    check(unsafe { libc::ioctl(master.as_fd().as_raw_fd(), get, buffer.as_mut_ptr()) })?;
    Ok(path(&buffer))
}

/// A NUL-terminated C string as a path; the whole buffer if there is no NUL.
fn path(buffer: &[libc::c_char]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let bytes: Vec<u8> = buffer
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| u8::from_ne_bytes(c.to_ne_bytes()))
        .collect();
    PathBuf::from(std::ffi::OsStr::from_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_cloexec(fd: impl AsFd) -> bool {
        // SAFETY: F_GETFD on a valid descriptor touches no memory.
        let flags = unsafe { libc::fcntl(fd.as_fd().as_raw_fd(), libc::F_GETFD) };
        flags >= 0 && flags & libc::FD_CLOEXEC != 0
    }

    /// Bytes written to the master arrive at the slave and back, and both
    /// ends are close-on-exec.
    #[test]
    fn a_pty_carries_bytes_both_ways() -> std::result::Result<(), String> {
        let (master, slave) = open(10, 40).map_err(|e| e.to_string())?;
        assert!(is_cloexec(&master) && is_cloexec(&slave));
        let mut modes = terminal::attributes(&slave).map_err(|e| e.to_string())?;
        modes.make_raw();
        terminal::set_attributes(&slave, &modes).map_err(|e| e.to_string())?;
        assert_eq!(crate::io::write(&master, b"in"), Ok(2));
        let mut buffer = [0u8; 8];
        assert_eq!(crate::io::read(&slave, &mut buffer), Ok(2));
        assert_eq!(buffer.get(..2), Some(&b"in"[..]));
        assert_eq!(crate::io::write(&slave, b"out"), Ok(3));
        assert_eq!(crate::io::read(&master, &mut buffer), Ok(3));
        assert_eq!(buffer.get(..3), Some(&b"out"[..]));
        Ok(())
    }

    /// `redrive` over a scripted sequence of results: what it returned, how
    /// many times it called the open, and the sleeps it asked for.
    fn redrive_script(
        results: &[crate::Result<u8>],
    ) -> (crate::Result<u8>, usize, Vec<std::time::Duration>) {
        let mut next = results.iter().copied();
        let mut calls = 0usize;
        let mut sleeps = Vec::new();
        let result = redrive(
            || {
                calls = calls.saturating_add(1);
                next.next().unwrap_or(Err(Errno::REDRIVEOPEN))
            },
            |d| sleeps.push(d),
        );
        (result, calls, sleeps)
    }

    #[test]
    fn a_redriven_open_is_tried_again_and_its_success_returned() {
        let (result, calls, sleeps) = redrive_script(&[Err(Errno::REDRIVEOPEN), Ok(7)]);
        assert_eq!(result, Ok(7));
        assert_eq!(calls, 2);
        assert_eq!(sleeps, [std::time::Duration::from_millis(1)]);
    }

    #[test]
    fn an_open_redriven_every_time_gives_again_after_the_bound() {
        let (result, calls, sleeps) = redrive_script(&[]);
        assert_eq!(result, Err(Errno::AGAIN), "never the kernel's -6");
        assert_eq!(calls, usize::try_from(REDRIVE_TRIES).unwrap_or(0));
        let slept: std::time::Duration = sleeps.iter().sum();
        assert_eq!(
            slept,
            std::time::Duration::from_millis(28),
            "the documented worst case"
        );
    }

    #[test]
    fn other_results_are_returned_at_once() {
        for first in [
            Ok(3),
            Err(Errno::INTR),
            Err(Errno::MFILE),
            Err(Errno::AGAIN),
        ] {
            let (result, calls, sleeps) = redrive_script(&[first]);
            assert_eq!(result, first);
            assert_eq!(calls, 1, "{first:?} is not retried");
            assert!(sleeps.is_empty(), "{first:?} does not sleep");
        }
    }

    /// The file `fd` refers to now: its type and device.
    fn behind(fd: std::os::fd::BorrowedFd<'_>) -> std::result::Result<(bool, u64), String> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let file = std::fs::File::from(fd.try_clone_to_owned().map_err(|e| e.to_string())?);
        let meta = file.metadata().map_err(|e| e.to_string())?;
        Ok((meta.file_type().is_char_device(), meta.rdev()))
    }

    fn dev_null() -> std::result::Result<(bool, u64), String> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let meta = std::fs::metadata("/dev/null").map_err(|e| e.to_string())?;
        Ok((meta.file_type().is_char_device(), meta.rdev()))
    }

    /// A call that returns in time is left alone: its result comes back, and
    /// the descriptor still refers to what it did.
    #[test]
    fn a_call_that_returns_in_time_keeps_its_descriptor() -> std::result::Result<(), String> {
        let (reader, _writer) = std::io::pipe().map_err(|e| e.to_string())?;
        let before = behind(reader.as_fd())?;
        let seen = watched(reader.as_fd(), std::time::Duration::from_secs(5), || 42);
        assert!(matches!(seen, Watched::Done(42)));
        assert_eq!(behind(reader.as_fd())?, before);
        assert_ne!(before, dev_null()?);
        Ok(())
    }

    /// A call still running at the limit has its descriptor replaced with
    /// /dev/null, and is reported as stalled whatever it returns.
    #[test]
    fn a_call_that_stalls_has_its_descriptor_replaced_with_dev_null()
    -> std::result::Result<(), String> {
        let (reader, _writer) = std::io::pipe().map_err(|e| e.to_string())?;
        let seen = watched(reader.as_fd(), std::time::Duration::from_millis(20), || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            42
        });
        assert!(matches!(seen, Watched::Stalled));
        assert_eq!(behind(reader.as_fd())?, dev_null()?);
        Ok(())
    }

    /// What a stress run saw: failures by message, and the opens slower than
    /// `SLOW_OPEN`.
    struct Stress {
        opens: u64,
        failures: Vec<String>,
        slow: u64,
        slowest: std::time::Duration,
    }

    /// `threads` threads, released together each round, each opening a PTY
    /// pair and closing it, `rounds` times.
    fn stress(threads: usize, rounds: usize) -> Stress {
        use std::sync::{Arc, Barrier, Mutex};
        let seen = Arc::new(Mutex::new(Stress {
            opens: 0,
            failures: Vec::new(),
            slow: 0,
            slowest: std::time::Duration::ZERO,
        }));
        let barrier = Arc::new(Barrier::new(threads));
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (seen, barrier) = (Arc::clone(&seen), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    for _ in 0..rounds {
                        barrier.wait();
                        let started = std::time::Instant::now();
                        // The pair closes as soon as the open returns, as a
                        // short-lived process's would.
                        let failure = open(24, 80).err();
                        let took = started.elapsed();
                        let mut all = seen
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        all.opens = all.opens.saturating_add(1);
                        all.slowest = all.slowest.max(took);
                        if took > SLOW_OPEN {
                            all.slow = all.slow.saturating_add(1);
                        }
                        if let Some(error) = failure {
                            all.failures.push(error.to_string());
                        }
                    }
                })
            })
            .collect();
        let mut panicked = Vec::new();
        for handle in handles {
            if handle.join().is_err() {
                panicked.push("a stress thread panicked".to_owned());
            }
        }
        let mut all = seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Stress {
            opens: all.opens,
            slow: all.slow,
            failures: std::mem::take(&mut all.failures)
                .into_iter()
                .chain(panicked)
                .collect(),
            slowest: all.slowest,
        }
    }

    /// An open slower than this has been through the kernel's retry sleeps.
    /// Without the lock, a third of the in-process test's opens were, some for
    /// over 400 ms; with it, only the odd one, when another process races us.
    const SLOW_OPEN: std::time::Duration = std::time::Duration::from_millis(50);

    /// Held by each stress test, so neither runs while the other's opens are
    /// contending in the kernel. The cross-process test holds it while its
    /// workers run.
    static STRESS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Many threads of one process opening PTYs at once: none fails, and on
    /// macOS none is slowed by the kernel's retry loop.
    #[test]
    fn concurrent_opens_in_one_process_neither_fail_nor_stall() -> std::result::Result<(), String> {
        let _alone = STRESS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let seen = stress(16, 300);
        if !seen.failures.is_empty() {
            return Err(format!(
                "{} of {} opens failed: {:?}",
                seen.failures.len(),
                seen.opens,
                seen.failures.first()
            ));
        }
        // Fewer than one open in a hundred slowed by the kernel's retries.
        if cfg!(target_os = "macos") && seen.slow.saturating_mul(100) >= seen.opens {
            return Err(format!(
                "{} of {} opens took over {SLOW_OPEN:?}, the slowest {:?}",
                seen.slow, seen.opens, seen.slowest
            ));
        }
        Ok(())
    }

    /// Set in a worker process of the cross-process test: its rounds.
    const STRESS_WORKER: &str = "FUXIX_PTY_STRESS_WORKER";

    /// Many processes opening PTYs at once, one thread each, so no lock of
    /// ours stands between them: none fails. The test runs its own binary as
    /// the workers.
    #[test]
    fn concurrent_opens_across_processes_never_fail() -> std::result::Result<(), String> {
        const NAME: &str = "pty::tests::concurrent_opens_across_processes_never_fail";
        if let Some(rounds) = std::env::var_os(STRESS_WORKER) {
            let rounds: usize = rounds
                .to_str()
                .and_then(|r| r.parse().ok())
                .ok_or("a worker's rounds")?;
            let seen = stress(1, rounds);
            println!(
                "fuxix-pty-worker opens={} failures={} slowest_ms={}",
                seen.opens,
                seen.failures.len(),
                seen.slowest.as_millis()
            );
            return match seen.failures.first() {
                None => Ok(()),
                Some(first) => Err(format!("{} failed: {first}", seen.failures.len())),
            };
        }
        let _alone = STRESS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let workers: Vec<_> = (0..STRESS_WORKERS)
            .map(|_| {
                std::process::Command::new(&exe)
                    .args([
                        NAME,
                        "--exact",
                        "--include-ignored",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env(STRESS_WORKER, STRESS_WORKER_ROUNDS.to_string())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
            })
            .collect::<std::io::Result<_>>()
            .map_err(|e| e.to_string())?;
        // A worker that stops making progress fails the test instead of hanging it. The
        // deadline is generous: a worker's opens take about 3 s under full contention.
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(STRESS_DEADLINE_SECS))
            .ok_or("a deadline")?;
        let mut failed = Vec::new();
        for mut worker in workers {
            let status = loop {
                match worker.try_wait().map_err(|e| e.to_string())? {
                    Some(status) => break Some(status),
                    None if std::time::Instant::now() > deadline => {
                        let _ = worker.kill();
                        let _ = worker.wait();
                        break None;
                    }
                    None => std::thread::sleep(std::time::Duration::from_millis(20)),
                }
            };
            // The harness prints the report after `test NAME ... `, on the same line.
            let mut stdout = String::new();
            let mut stderr = String::new();
            if let Some(mut out) = worker.stdout.take() {
                let _ = std::io::Read::read_to_string(&mut out, &mut stdout);
            }
            if let Some(mut err) = worker.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut err, &mut stderr);
            }
            let report = stdout
                .lines()
                .find_map(|l| l.split_once("fuxix-pty-worker").map(|(_, r)| r))
                .unwrap_or("no report: the worker did not run the test")
                .to_owned();
            match status {
                None => failed.push(format!("a worker still ran after {STRESS_DEADLINE_SECS} s")),
                Some(status) if !status.success() || !report.contains("failures=0") => {
                    failed.push(format!("{report}; {stderr}"));
                }
                Some(_) => {}
            }
        }
        match failed.first() {
            None => Ok(()),
            Some(first) => Err(format!(
                "{} of {STRESS_WORKERS} workers failed: {first}",
                failed.len()
            )),
        }
    }

    const STRESS_WORKERS: usize = 16;
    const STRESS_WORKER_ROUNDS: usize = 1500;
    const STRESS_DEADLINE_SECS: u64 = 120;

    #[test]
    fn a_name_stops_at_its_nul() {
        let raw: Vec<libc::c_char> = b"/dev/pts/7\0junk"
            .iter()
            .map(|b| libc::c_char::from_ne_bytes([*b]))
            .collect();
        assert_eq!(path(&raw), PathBuf::from("/dev/pts/7"));
        let error = Error {
            call: "grantpt",
            errno: Errno::INVAL,
        };
        assert!(error.to_string().starts_with("grantpt: "));
    }
}
