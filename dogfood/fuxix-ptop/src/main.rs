//! fuxix-ptop: a `top` for one job. It starts a command as the leader of a
//! session of its own, adopts the orphans the job leaves, and shows the
//! job's process tree, refreshed, with each process's group, session, state,
//! CPU, memory and working directory. Keys move a selection, change the
//! sort, and send a signal to the selected process's whole group.
//!
//! The processes come from `fuxix::process::tree`, which on Linux is this
//! process's descendants (and on macOS every process the system lists).

use fuxix::poll::{Events, PollFd};
use fuxix::process::{self, Pid, Signal};
use std::collections::HashMap;
use std::io::Write;
use std::os::fd::AsFd;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

type Res<T> = Result<T, String>;

/// Linux's `USER_HZ`, which `/proc/PID/stat` counts CPU time in. fuxix has
/// no `sysconf(_SC_CLK_TCK)`; every Linux ABI fux runs on uses 100.
const TICKS_PER_SECOND: f64 = 100.0;

const USAGE: &str =
    "usage: fuxix-ptop [-d SECONDS] [-n COUNT] [-b] [-s tree|cpu|mem|pid|name] -- COMMAND [ARGS...]
  -d  seconds between refreshes (default 1)
  -n  stop after this many refreshes (batch mode's default: 1)
  -b  batch: print plain snapshots to stdout instead of the interactive view
  -s  sort order (default tree)
keys: j/k or arrows move, s next sort, K TERM the selected group, 9 KILL it, q quit";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sort {
    Tree,
    Cpu,
    Mem,
    Pid,
    Name,
}

impl Sort {
    fn parse(s: &str) -> Option<Sort> {
        Some(match s {
            "tree" => Sort::Tree,
            "cpu" => Sort::Cpu,
            "mem" => Sort::Mem,
            "pid" => Sort::Pid,
            "name" => Sort::Name,
            _ => return None,
        })
    }
    fn next(self) -> Sort {
        match self {
            Sort::Tree => Sort::Cpu,
            Sort::Cpu => Sort::Mem,
            Sort::Mem => Sort::Pid,
            Sort::Pid => Sort::Name,
            Sort::Name => Sort::Tree,
        }
    }
}

struct Args {
    delay: Duration,
    count: Option<u64>,
    batch: bool,
    sort: Sort,
    command: Vec<String>,
}

fn parse_args(mut it: impl Iterator<Item = String>) -> Res<Args> {
    let mut args = Args {
        delay: Duration::from_secs(1),
        count: None,
        batch: false,
        sort: Sort::Tree,
        command: Vec::new(),
    };
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value\n{USAGE}"));
        match arg.as_str() {
            "-d" => {
                let secs: f64 = value("-d")?.parse().map_err(|e| format!("-d: {e}"))?;
                args.delay = Duration::try_from_secs_f64(secs).map_err(|e| format!("-d: {e}"))?;
            }
            "-n" => args.count = Some(value("-n")?.parse().map_err(|e| format!("-n: {e}"))?),
            "-s" => {
                let v = value("-s")?;
                args.sort = Sort::parse(&v).ok_or(format!("-s: unknown sort {v:?}"))?;
            }
            "-b" => args.batch = true,
            "-h" | "--help" => return Err(USAGE.to_owned()),
            "--" => {
                args.command = it.collect();
                break;
            }
            other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
        }
    }
    if args.command.is_empty() {
        return Err(format!("no command to run\n{USAGE}"));
    }
    Ok(args)
}

/// What `/proc/PID/stat` says that fuxix has no reader for.
#[derive(Clone, Debug, PartialEq)]
struct Stat {
    name: String,
    state: char,
    parent: i32,
    ticks: u64,
}

/// Parses a `/proc/PID/stat` line: `pid (comm) state ppid ...`, where comm
/// may hold spaces and parentheses, so it ends at the last `)`.
fn parse_stat(line: &str) -> Option<Stat> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    let name = line.get(open.checked_add(1)?..close)?.to_owned();
    let rest: Vec<&str> = line
        .get(close.checked_add(1)?..)?
        .split_whitespace()
        .collect();
    // After comm: state(3) ppid(4) pgrp session tty tpgid flags minflt
    // cminflt majflt cmajflt utime(14) stime(15).
    let state = rest.first()?.chars().next()?;
    let parent = rest.get(1)?.parse().ok()?;
    let utime: u64 = rest.get(11)?.parse().ok()?;
    let stime: u64 = rest.get(12)?.parse().ok()?;
    Some(Stat {
        name,
        state,
        parent,
        ticks: utime.saturating_add(stime),
    })
}

/// Resident memory in KiB, from `/proc/PID/status`'s `VmRSS`.
fn rss_kib(pid: Pid) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap_or(0)
}

#[derive(Clone, Debug)]
struct Proc {
    pid: Pid,
    parent: i32,
    group: Option<Pid>,
    session: Option<Pid>,
    state: char,
    name: String,
    ticks: u64,
    cpu: f64,
    rss: u64,
    cwd: String,
    depth: usize,
}

struct Snapshot {
    procs: Vec<Proc>,
    note: Option<String>,
}

/// One look at the job: every process `tree` finds that is still there.
fn snapshot(previous: &HashMap<Pid, u64>, elapsed: Duration) -> Snapshot {
    let tree = process::tree();
    let note = match tree.scan {
        process::Scan::Complete => None,
        process::Scan::Truncated => Some("tree truncated".to_owned()),
        process::Scan::Failed(e) => Some(format!("tree scan failed: errno {}", e.raw())),
    };
    let mut procs = Vec::new();
    for pid in tree.pids {
        let Some(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|l| parse_stat(&l))
        else {
            continue; // ended since the tree was read
        };
        // A fresh grant each: the path it returns keeps its charge.
        let cwd = match process::cwd_observed(pid, &mut 65536usize) {
            Ok(Ok(Some(p))) => p.display().to_string(),
            Ok(Ok(None)) => "?".to_owned(),
            Ok(Err(e)) => format!("<errno {}>", e.raw()),
            Err(e) => format!("<{e}>"),
        };
        let cpu = previous.get(&pid).map_or(0.0, |before| {
            let used = stat.ticks.saturating_sub(*before) as f64 / TICKS_PER_SECOND;
            let secs = elapsed.as_secs_f64();
            if secs > 0.0 { used / secs * 100.0 } else { 0.0 }
        });
        procs.push(Proc {
            pid,
            parent: stat.parent,
            group: process::group(pid),
            session: process::session(pid),
            state: stat.state,
            name: stat.name,
            ticks: stat.ticks,
            cpu,
            rss: rss_kib(pid),
            cwd,
            depth: 0,
        });
    }
    Snapshot { procs, note }
}

/// Orders the processes: in tree order (each after its parent, children by
/// pid, `depth` set), or by a column.
fn order(procs: &mut Vec<Proc>, sort: Sort) {
    match sort {
        Sort::Cpu => procs.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(a.pid.cmp(&b.pid))),
        Sort::Mem => procs.sort_by(|a, b| b.rss.cmp(&a.rss).then(a.pid.cmp(&b.pid))),
        Sort::Pid => procs.sort_by_key(|p| p.pid),
        Sort::Name => procs.sort_by(|a, b| a.name.cmp(&b.name).then(a.pid.cmp(&b.pid))),
        Sort::Tree => {
            procs.sort_by_key(|p| p.pid);
            let known: std::collections::HashSet<i32> =
                procs.iter().map(|p| p.pid.as_raw()).collect();
            let mut out = Vec::with_capacity(procs.len());
            let mut taken = vec![false; procs.len()];
            // Roots: those whose parent is not shown (the leader, orphans).
            let mut stack: Vec<(usize, usize)> = (0..procs.len())
                .rev()
                .filter(|i| procs.get(*i).is_some_and(|p| !known.contains(&p.parent)))
                .map(|i| (i, 0))
                .collect();
            while let Some((i, depth)) = stack.pop() {
                let (Some(p), Some(t)) = (procs.get(i), taken.get_mut(i)) else {
                    continue;
                };
                if std::mem::replace(t, true) {
                    continue;
                }
                let mut p = p.clone();
                p.depth = depth;
                let pid = p.pid.as_raw();
                out.push(p);
                for (j, c) in procs.iter().enumerate().rev() {
                    if c.parent == pid {
                        stack.push((j, depth.saturating_add(1)));
                    }
                }
            }
            *procs = out;
        }
    }
}

fn opt(pid: Option<Pid>) -> String {
    pid.map_or_else(|| "-".to_owned(), |p| p.to_string())
}

fn header() -> String {
    format!(
        "{:>7} {:>7} {:>7} {:>7} S {:>5} {:>8}  {:<24} COMMAND",
        "PID", "PPID", "PGID", "SID", "CPU%", "RSS-KiB", "CWD"
    )
}

fn line(p: &Proc, tree: bool) -> String {
    let indent = if tree {
        "  ".repeat(p.depth)
    } else {
        String::new()
    };
    format!(
        "{:>7} {:>7} {:>7} {:>7} {} {:>5.1} {:>8}  {:<24} {indent}{}",
        p.pid,
        p.parent,
        opt(p.group),
        opt(p.session),
        p.state,
        p.cpu,
        p.rss,
        p.cwd,
        p.name
    )
}

fn clip(s: &str, width: usize) -> String {
    s.chars().take(width).collect()
}

/// The job: its leader, whose end this process reaps, and how it ended.
struct Job {
    leader: Pid,
    ended: Option<process::Status>,
}

impl Job {
    fn start(command: &[String]) -> Res<Job> {
        let (program, rest) = command.split_first().ok_or("no command")?;
        let mut cmd = Command::new(program);
        cmd.args(rest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        process::lead_session(&mut cmd, false, process::Dispositions::Default);
        let child = cmd.spawn().map_err(|e| format!("{program}: {e}"))?;
        let leader = Pid::of(&child).ok_or("a child with no pid")?;
        Ok(Job {
            leader,
            ended: None,
        })
    }

    /// Reaps the leader if it ended, and every orphan that did.
    fn reap(&mut self) {
        if self.ended.is_none() {
            self.ended = process::reap(self.leader).ok().flatten();
        }
        let leader = self.leader;
        drop(process::reap_orphans(|pid| pid == leader));
    }

    fn status(&self) -> String {
        match self.ended {
            None => format!("job {} running", self.leader),
            Some(process::Status::Exited(code)) => format!("job {} exited {code}", self.leader),
            Some(process::Status::Signalled { signal, .. }) => {
                let name = Signal::from_raw(signal).map_or("?", Signal::name);
                format!("job {} killed by SIG{name}", self.leader)
            }
        }
    }
}

struct Monitor {
    sort: Sort,
    ticks: HashMap<Pid, u64>,
    last: Instant,
    procs: Vec<Proc>,
    note: Option<String>,
}

impl Monitor {
    fn refresh(&mut self, job: &mut Job) {
        job.reap();
        let now = Instant::now();
        let snap = snapshot(&self.ticks, now.saturating_duration_since(self.last));
        self.last = now;
        self.ticks = snap.procs.iter().map(|p| (p.pid, p.ticks)).collect();
        self.procs = snap.procs;
        self.note = snap.note;
        order(&mut self.procs, self.sort);
    }

    fn text(&self, job: &Job) -> String {
        let mut out = format!(
            "{} | {} processes | sort {:?}{}\n{}\n",
            job.status(),
            self.procs.len(),
            self.sort,
            self.note
                .as_ref()
                .map_or(String::new(), |n| format!(" | {n}")),
            header()
        );
        for p in &self.procs {
            out.push_str(&line(p, self.sort == Sort::Tree));
            out.push('\n');
        }
        out
    }
}

fn batch(args: &Args, job: &mut Job, monitor: &mut Monitor) -> Res<()> {
    let count = args.count.unwrap_or(1);
    let mut stdout = std::io::stdout().lock();
    for i in 0..count {
        if i > 0 {
            std::thread::sleep(args.delay);
        }
        monitor.refresh(job);
        writeln!(stdout, "{}", monitor.text(job)).map_err(|e| e.to_string())?;
        if job.ended.is_some() && monitor.procs.is_empty() {
            break;
        }
    }
    Ok(())
}

/// A key pressed, from raw input.
enum Key {
    Up,
    Down,
    Byte(u8),
}

fn keys(bytes: &[u8]) -> Vec<Key> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        match (
            b,
            bytes.get(i.saturating_add(1)),
            bytes.get(i.saturating_add(2)),
        ) {
            (0x1b, Some(b'[' | b'O'), Some(b'A')) => (out.push(Key::Up), i = i.saturating_add(3)),
            (0x1b, Some(b'[' | b'O'), Some(b'B')) => (out.push(Key::Down), i = i.saturating_add(3)),
            _ => (out.push(Key::Byte(b)), i = i.saturating_add(1)),
        };
    }
    out
}

fn interactive(args: &Args, job: &mut Job, monitor: &mut Monitor) -> Res<()> {
    let stdin = std::io::stdin();
    let raw = fuxix::terminal::Raw::enter(stdin.as_fd()).map_err(|e| format!("raw mode: {e}"))?;
    let signals =
        fuxix::signal::Pipe::new(&[Signal::Winch, Signal::Chld, Signal::Term, Signal::Int])
            .map_err(|e| format!("signals: {e}"))?;
    let mut out = std::io::stdout().lock();
    let w = |out: &mut std::io::StdoutLock, s: &str| -> Res<()> {
        out.write_all(s.as_bytes())
            .and_then(|()| out.flush())
            .map_err(|e| e.to_string())
    };
    w(&mut out, "\x1b[?1049h\x1b[?25l")?;
    let own_group =
        Pid::from_raw(i32::try_from(std::process::id()).unwrap_or(0)).and_then(process::group);
    let (mut selected, mut message, mut shown) = (0usize, String::new(), 0u64);
    let mut next = Instant::now();
    let result = loop {
        if Instant::now() >= next {
            monitor.refresh(job);
            shown = shown.saturating_add(1);
            next = Instant::now()
                .checked_add(args.delay)
                .unwrap_or_else(Instant::now);
            if args.count.is_some_and(|c| shown > c) {
                break Ok(());
            }
        }
        selected = selected.min(monitor.procs.len().saturating_sub(1));
        let (rows, cols) = fuxix::terminal::window_size(std::io::stdout().as_fd())
            .map_or((24, 80), |(s, _)| {
                (usize::from(s.rows.get()), usize::from(s.cols.get()))
            });
        let mut frame = String::from("\x1b[H\x1b[2J");
        let text = monitor.text(job);
        let mut lines = text.lines();
        for l in lines.by_ref().take(2) {
            frame.push_str(&format!("{}\r\n", clip(l, cols)));
        }
        let room = rows.saturating_sub(3);
        let first = selected.saturating_sub(room.saturating_sub(1));
        for (i, l) in lines.enumerate().skip(first).take(room) {
            let l = clip(l, cols);
            if i == selected {
                frame.push_str(&format!("\x1b[7m{l:<cols$}\x1b[m\r\n"));
            } else {
                frame.push_str(&format!("{l}\r\n"));
            }
        }
        frame.push_str(&format!("\x1b[{rows};1H{}", clip(&message, cols)));
        w(&mut out, &frame)?;

        let wait = next.saturating_duration_since(Instant::now());
        let mut fds = [
            PollFd::new(&stdin, Events::IN),
            PollFd::new(signals.reader(), Events::IN),
        ];
        match fuxix::poll::poll(&mut fds, Some(wait)) {
            Ok(_) | Err(fuxix::Errno::INTR) => {}
            Err(e) => break Err(format!("poll: {e:?}")),
        }
        let mut quit = false;
        while let Some(sig) = signals.next() {
            match sig {
                Signal::Term | Signal::Int => quit = true,
                Signal::Chld => next = Instant::now(),
                _ => {}
            }
        }
        if fds
            .first()
            .is_some_and(|f| f.revents().intersects(Events::IN | Events::HUP))
        {
            let mut buf = [0u8; 64];
            let n = match fuxix::io::read(&stdin, &mut buf) {
                Ok(0) => break Ok(()),
                Ok(n) => n,
                Err(fuxix::Errno::INTR | fuxix::Errno::AGAIN) => 0,
                Err(e) => break Err(format!("read: {e:?}")),
            };
            for key in keys(buf.get(..n).unwrap_or_default()) {
                match key {
                    Key::Byte(b'q') => quit = true,
                    Key::Up | Key::Byte(b'k') => selected = selected.saturating_sub(1),
                    Key::Down | Key::Byte(b'j') => selected = selected.saturating_add(1),
                    Key::Byte(b's') => {
                        monitor.sort = monitor.sort.next();
                        order(&mut monitor.procs, monitor.sort);
                    }
                    Key::Byte(b'K') => {
                        message = signal_group(monitor, selected, Signal::Term, own_group)
                    }
                    Key::Byte(b'9') => {
                        message = signal_group(monitor, selected, Signal::Kill, own_group)
                    }
                    Key::Byte(_) => {}
                }
            }
        }
        if quit {
            break Ok(());
        }
    };
    w(&mut out, "\x1b[?25h\x1b[?1049l")?;
    drop(raw);
    result
}

/// Sends `signal` to the selected process's group, unless it is this
/// program's own.
fn signal_group(monitor: &Monitor, selected: usize, signal: Signal, own: Option<Pid>) -> String {
    let Some(p) = monitor.procs.get(selected) else {
        return "nothing selected".to_owned();
    };
    let Some(group) = p.group.or_else(|| process::group(p.pid)) else {
        return format!("{} has no group (gone?)", p.pid);
    };
    if Some(group) == own {
        return format!("group {group} is fuxix-ptop's own; not signalled");
    }
    match process::kill_group(group, signal) {
        Ok(()) => format!("SIG{} sent to group {group}", signal.name()),
        Err(e) => format!("SIG{} to group {group}: errno {}", signal.name(), e.raw()),
    }
}

fn run() -> Res<()> {
    let args = parse_args(std::env::args().skip(1))?;
    // Orphans the job leaves stay in this process's tree.
    process::adopt_orphans().map_err(|e| format!("adopt orphans: {e:?}"))?;
    let mut job = Job::start(&args.command)?;
    let mut monitor = Monitor {
        sort: args.sort,
        ticks: HashMap::new(),
        last: Instant::now(),
        procs: Vec::new(),
        note: None,
    };
    let result = if args.batch {
        batch(&args, &mut job, &mut monitor)
    } else {
        interactive(&args, &mut job, &mut monitor)
    };
    // The job ends with the monitor: every group in its tree is told to,
    // those its processes moved to (setsid, job control) too.
    let own = Pid::from_raw(i32::try_from(std::process::id()).unwrap_or(0)).and_then(process::group);
    let mut groups: Vec<Pid> = process::tree().pids.into_iter().filter_map(process::group).collect();
    groups.sort_unstable();
    groups.dedup();
    for group in groups.into_iter().filter(|g| Some(*g) != own) {
        let _ = process::kill_group(group, Signal::Term);
    }
    result
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fuxix-ptop: {e}");
            std::process::ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stat_line_whose_name_holds_parens_and_spaces_is_read() {
        let line = "4242 (a (b) c) S 1 4242 4242 0 -1 4194560 100 0 0 0 7 3 0 0 20 0 1 0 1 2 3";
        assert_eq!(
            parse_stat(line),
            Some(Stat {
                name: "a (b) c".to_owned(),
                state: 'S',
                parent: 1,
                ticks: 10
            })
        );
        assert_eq!(parse_stat("garbage"), None);
    }
}
