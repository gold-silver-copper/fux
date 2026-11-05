//! The Windows event loop's probes (spikes S2a and S2b), each one property,
//! each with a deadline so that a lost wake-up fails instead of hanging.
//! They take turns ([`turn`]): one thread polls at a time, and the idle
//! probe counts the whole process's CPU time.
#![cfg(windows)]
use fuxix::Errno;
use fuxix::io::{Stream, pipe, pipe_for_child, read, write_what_fits};
use fuxix::poll::{Events, Poll, PollFd};
use fuxix::process::{Signal, watch};
use fuxix::signal::{self, Pipe};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

type Outcome = Result<(), String>;

fn turn() -> MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock().unwrap_or_else(PoisonError::into_inner)
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn after(duration: Duration) -> Result<Instant, String> {
    Instant::now()
        .checked_add(duration)
        .ok_or_else(|| "a deadline".to_owned())
}

/// Byte `i` of stream `seed`: no two streams, and no two nearby offsets,
/// alike, so a byte lost, doubled or moved is seen.
fn pattern(seed: u64, i: usize) -> u8 {
    let i = u64::try_from(i).unwrap_or(0);
    let mixed = i.wrapping_mul(0x9E37_79B9).wrapping_add(seed) ^ (i >> 11);
    u8::try_from(mixed % 251).unwrap_or(0)
}

fn bytes(seed: u64, count: usize) -> Vec<u8> {
    (0..count).map(|i| pattern(seed, i)).collect()
}

/// What one side of a flood has sent and taken.
struct Side {
    out: Vec<u8>,
    sent: usize,
    seed_in: u64,
    taken: usize,
    expected: usize,
}

impl Side {
    fn interest(&self) -> Events {
        match self.sent < self.out.len() {
            true => Events::IN | Events::OUT,
            false => Events::IN,
        }
    }

    /// Writes what fits and takes what came, checking each byte.
    fn turn(&mut self, stream: &Stream, ready: Events) -> Outcome {
        if ready.intersects(Events::OUT) {
            let rest = self.out.get(self.sent..).unwrap_or_default();
            self.sent = self
                .sent
                .saturating_add(write_what_fits(stream, rest).map_err(text)?);
        }
        let mut buffer = [0u8; 8192];
        loop {
            match read(stream, &mut buffer) {
                Ok(0) => return Err("the stream ended early".into()),
                Ok(n) => {
                    for byte in buffer.iter().take(n) {
                        let want = pattern(self.seed_in, self.taken);
                        if *byte != want {
                            return Err(format!("byte {} is {byte}, not {want}", self.taken));
                        }
                        self.taken = self.taken.saturating_add(1);
                    }
                }
                Err(Errno::AGAIN) => return Ok(()),
                Err(errno) => return Err(errno.to_string()),
            }
        }
    }

    fn done(&self) -> bool {
        self.sent == self.out.len() && self.taken == self.expected
    }
}

/// Raises `signals` in turn from another thread, each once the loop has
/// counted the one before (`seen`), so every one must be told, once and
/// in order. Gives up at `deadline`.
fn raiser(
    signals: Vec<Signal>,
    seen: Arc<AtomicUsize>,
    deadline: Instant,
) -> std::thread::JoinHandle<Outcome> {
    std::thread::spawn(move || {
        for (i, signal) in signals.into_iter().enumerate() {
            while seen.load(Ordering::SeqCst) < i {
                if Instant::now() > deadline {
                    return Err(format!("signal {i} was not seen in time"));
                }
                std::thread::yield_now();
            }
            signal::raise(signal).map_err(text)?;
        }
        Ok(())
    })
}

/// 4 MiB one way and 1 MiB the other through one pipe, at once, with 500
/// signals raised from another thread meanwhile: every byte comes, in
/// order, and every signal, once, in order. Nothing to read says `AGAIN`.
#[test]
fn a_flood_both_ways_with_signals_arrives_whole_and_in_order() -> Outcome {
    let _turn = turn();
    let (a, b) = pipe().map_err(text)?;
    let mut buffer = [0u8; 16];
    assert_eq!(read(&a, &mut buffer), Err(Errno::AGAIN));
    let (big, small) = (4 << 20, 1 << 20);
    let mut ours = Side {
        out: bytes(1, big),
        sent: 0,
        seed_in: 2,
        taken: 0,
        expected: small,
    };
    let mut theirs = Side {
        out: bytes(2, small),
        sent: 0,
        seed_in: 1,
        taken: 0,
        expected: big,
    };
    let kinds = [Signal::Usr1, Signal::Usr2, Signal::Winch];
    let order: Vec<Signal> = kinds.iter().copied().cycle().take(500).collect();
    let signals = Pipe::new(&kinds).map_err(text)?;
    let seen = Arc::new(AtomicUsize::new(0));
    let deadline = after(Duration::from_secs(25))?;
    let raising = raiser(order.clone(), Arc::clone(&seen), deadline);
    let mut poll = Poll::default();
    let mut turns = 0u64;
    while !(ours.done() && theirs.done() && seen.load(Ordering::SeqCst) == order.len()) {
        if Instant::now() > deadline {
            return Err(format!(
                "not done in time: sent {}/{}, taken {}/{}, signals {}",
                ours.sent,
                theirs.sent,
                ours.taken,
                theirs.taken,
                seen.load(Ordering::SeqCst)
            ));
        }
        let mut list = poll.list();
        let at_a = list.push(PollFd::new(&a, ours.interest()));
        let at_b = list.push(PollFd::new(&b, theirs.interest()));
        let at_signals = list.push(PollFd::new(signals.reader(), Events::IN));
        if list.poll(Some(Duration::from_secs(5))).map_err(text)? == 0 {
            return Err("nothing was ready for 5 s: a lost wake-up".into());
        }
        let (on_a, on_b) = (list.events(at_a), list.events(at_b));
        let on_signals = list.events(at_signals);
        drop(list);
        ours.turn(&a, on_a)?;
        theirs.turn(&b, on_b)?;
        if on_signals.intersects(Events::IN) {
            while let Some(signal) = signals.next() {
                let at = seen.load(Ordering::SeqCst);
                if order.get(at) != Some(&signal) {
                    return Err(format!("signal {at} is {signal:?}"));
                }
                seen.store(at.saturating_add(1), Ordering::SeqCst);
            }
        }
        turns = turns.saturating_add(1);
    }
    raising.join().map_err(|_| "the raiser panicked")??;
    let _ = writeln!(
        std::io::stderr(),
        "FLOOD out={big} in={small} signals={} turns={turns}",
        order.len()
    );
    Ok(())
}

/// Four threads each raise their own signal 500 times, each raise once
/// the one before was seen: every one wakes the loop, which waits with
/// no timeout but a 5 s guard.
#[test]
fn every_raise_from_another_thread_wakes_the_loop() -> Outcome {
    let _turn = turn();
    let kinds = [Signal::Usr1, Signal::Usr2, Signal::Winch, Signal::Alrm];
    let signals = Pipe::new(&kinds).map_err(text)?;
    let deadline = after(Duration::from_secs(25))?;
    let seen: Vec<Arc<AtomicUsize>> = kinds
        .iter()
        .map(|_| Arc::new(AtomicUsize::new(0)))
        .collect();
    let raising: Vec<_> = (kinds.iter().zip(&seen))
        .map(|(kind, seen)| raiser(vec![*kind; 500], Arc::clone(seen), deadline))
        .collect();
    let mut poll = Poll::default();
    let started = Instant::now();
    let mut wakes = 0u64;
    while seen.iter().any(|seen| seen.load(Ordering::SeqCst) < 500) {
        if Instant::now() > deadline {
            return Err("not every raise was seen in time".into());
        }
        let mut list = poll.list();
        list.push(PollFd::new(signals.reader(), Events::IN));
        if list.poll(Some(Duration::from_secs(5))).map_err(text)? == 0 {
            return Err("no wake for 5 s: a raise was lost".into());
        }
        drop(list);
        wakes = wakes.saturating_add(1);
        while let Some(signal) = signals.next() {
            let at = kinds
                .iter()
                .position(|kind| *kind == signal)
                .ok_or("a stray signal")?;
            let count = seen.get(at).ok_or("a stray signal")?;
            count.fetch_add(1, Ordering::SeqCst);
        }
    }
    for thread in raising {
        thread.join().map_err(|_| "a raiser panicked")??;
    }
    let _ = writeln!(
        std::io::stderr(),
        "WAKES raises=2000 wakes={wakes} elapsed_ms={}",
        started.elapsed().as_millis()
    );
    Ok(())
}

/// The CPU time this process has used, all its threads.
fn cpu_time() -> Result<Duration, String> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let mut times = [FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    }; 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: it takes nothing, and gives this process's pseudo handle.
    let this = unsafe { GetCurrentProcess() };
    // SAFETY: the pseudo handle is this process's; each FILETIME is
    // writable for the call.
    let done = unsafe { GetProcessTimes(this, created, exited, kernel, user) };
    if done == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let ticks =
        |time: &FILETIME| u64::from(time.dwHighDateTime) << 32 | u64::from(time.dwLowDateTime);
    let hundreds = ticks(kernel).saturating_add(ticks(user));
    Ok(Duration::from_nanos(hundreds.saturating_mul(100)))
}

/// A loop with nothing to do, waiting with no timeout, uses under 1 % of
/// a core over 2 s, and wakes when a byte comes.
#[test]
fn an_idle_loop_uses_no_cpu_and_wakes_when_a_byte_comes() -> Outcome {
    let _turn = turn();
    let (a, b) = pipe().map_err(text)?;
    let signals = Pipe::new(&[Signal::Usr1]).map_err(text)?;
    let idle = Duration::from_secs(2);
    let writer = std::thread::spawn(move || {
        std::thread::sleep(idle);
        write_what_fits(&b, b"x").map(|_| b)
    });
    let (cpu, started) = (cpu_time()?, Instant::now());
    let mut poll = Poll::default();
    let mut list = poll.list();
    let byte = list.push(PollFd::new(&a, Events::IN));
    list.push(PollFd::new(signals.reader(), Events::IN));
    let ready = list.poll(None).map_err(text)?;
    let (slept, used) = (started.elapsed(), cpu_time()?.saturating_sub(cpu));
    assert_eq!(ready, 1);
    assert!(list.events(byte).intersects(Events::IN));
    drop(list);
    let _b = writer
        .join()
        .map_err(|_| "the writer panicked")?
        .map_err(text)?;
    let mut buffer = [0u8; 4];
    assert_eq!(read(&a, &mut buffer), Ok(1));
    let percent = used.as_secs_f64() * 100.0 / slept.as_secs_f64();
    let _ = writeln!(
        std::io::stderr(),
        "IDLE slept_ms={} cpu_us={} cpu_pct={percent:.3}",
        slept.as_millis(),
        used.as_micros()
    );
    assert!(
        slept >= idle.saturating_sub(Duration::from_millis(50)),
        "woke after {slept:?}"
    );
    assert!(percent < 1.0, "{percent:.3} % of a core while idle");
    Ok(())
}

/// A 1 ms timeout, 200 times: never early, and late by under 3 ms at the
/// 99th percentile.
#[test]
fn a_short_timeout_is_never_early_and_hardly_late() -> Outcome {
    let _turn = turn();
    let (a, _b) = pipe().map_err(text)?;
    let want = Duration::from_millis(1);
    let mut poll = Poll::default();
    let mut late = Vec::new();
    for _ in 0..200 {
        let mut list = poll.list();
        list.push(PollFd::new(&a, Events::IN));
        let started = Instant::now();
        let ready = list.poll(Some(want)).map_err(text)?;
        let took = started.elapsed();
        assert_eq!(ready, 0);
        assert!(took >= want, "woke early, after {took:?}");
        late.push(took.saturating_sub(want));
    }
    late.sort();
    let at = |percent: usize| late.get(late.len().saturating_mul(percent) / 100).copied();
    let (p50, p99) = (at(50).unwrap_or_default(), at(99).unwrap_or_default());
    let max = late.last().copied().unwrap_or_default();
    let _ = writeln!(
        std::io::stderr(),
        "TIMER want=1ms n=200 early=0 late_us p50={} p99={} max={}",
        p50.as_micros(),
        p99.as_micros(),
        max.as_micros()
    );
    assert!(p99 < Duration::from_millis(3), "p99 late by {p99:?}");
    Ok(())
}

/// The bytes a child writes in [`a_watched_childs_exit_comes_as_chld_after_its_output`].
const CHILD_BYTES: usize = 1 << 20;
const CHILD: &str = "FUXIX_TEST_CHILD";

/// The child itself, run by the probe below (with `CHILD` set) and by no
/// one else: it writes its bytes to stderr, which is the pipe.
#[test]
#[ignore = "the child of a_watched_childs_exit_comes_as_chld_after_its_output"]
fn child_writes_its_bytes() -> Outcome {
    if std::env::var_os(CHILD).is_some() {
        std::io::stderr()
            .write_all(&bytes(3, CHILD_BYTES))
            .map_err(text)?;
    }
    Ok(())
}

/// A watched child writes 1 MiB and exits: all its bytes are read, to the
/// end, and its exit comes as `Chld`, after which its status is there.
#[test]
fn a_watched_childs_exit_comes_as_chld_after_its_output() -> Outcome {
    let _turn = turn();
    let children = Pipe::new(&[Signal::Chld]).map_err(text)?;
    let (output, theirs) = pipe_for_child().map_err(text)?;
    let mut child = {
        let mut command = std::process::Command::new(std::env::current_exe().map_err(text)?);
        command
            .args([
                "child_writes_its_bytes",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(theirs));
        // Dropped with the command: the child's copy is then the only one.
        command.spawn().map_err(text)?
    };
    let _watch = watch(&child).map_err(text)?;
    let deadline = after(Duration::from_secs(25))?;
    let mut poll = Poll::default();
    let (mut taken, mut ended, mut exited) = (0usize, false, false);
    let mut buffer = vec![0u8; 65536];
    while !(ended && exited) {
        if Instant::now() > deadline {
            return Err(format!("taken {taken}, ended {ended}, exited {exited}"));
        }
        let mut list = poll.list();
        let out = (!ended).then(|| list.push(PollFd::new(&output, Events::IN)));
        let chld = list.push(PollFd::new(children.reader(), Events::IN));
        if list.poll(Some(Duration::from_secs(5))).map_err(text)? == 0 {
            return Err("nothing for 5 s: a lost wake-up".into());
        }
        let out = out.map_or(Events::NONE, |out| list.events(out));
        let chld = list.events(chld);
        drop(list);
        while out.intersects(Events::IN | Events::HUP) && !ended {
            match read(&output, &mut buffer) {
                Ok(0) => ended = true,
                Ok(n) => {
                    for byte in buffer.iter().take(n) {
                        if *byte != pattern(3, taken) {
                            return Err(format!("byte {taken} is {byte}"));
                        }
                        taken = taken.saturating_add(1);
                    }
                }
                Err(Errno::AGAIN) => break,
                Err(errno) => return Err(errno.to_string()),
            }
        }
        if chld.intersects(Events::IN) {
            while let Some(signal) = children.next() {
                assert_eq!(signal, Signal::Chld);
                exited = true;
            }
        }
    }
    assert_eq!(taken, CHILD_BYTES);
    let status = child
        .try_wait()
        .map_err(text)?
        .ok_or("Chld came, but it runs")?;
    assert!(status.success(), "{status:?}");
    let _ = writeln!(
        std::io::stderr(),
        "CHILD bytes={taken} chld=1 status={status:?}"
    );
    Ok(())
}
