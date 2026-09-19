use super::{Result, output};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::process::Command;
use std::time::Duration;

#[derive(Clone, Serialize)]
pub(super) struct Counter {
    pub pid: u32,
    pub parent: u32,
    pub group: u32,
    pub command: String,
    pub rss_kib: u64,
    pub identity: u64,
    pub cpu_s: f64,
    pub interrupt_wakeups: Option<u64>,
    pub voluntary_switches: Option<u64>,
}

pub(super) fn snapshot(roots: &[u32], previous: &[Counter]) -> Result<Vec<Counter>> {
    let mut ps = Command::new("/bin/ps");
    ps.args(["-axo", "pid=,ppid=,pgid=,rss=,comm="]);
    let text = output(ps, Duration::from_secs(5))?;
    let text = std::str::from_utf8(&text)?;
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(pid) = fields.next() else {
            continue;
        };
        let parent = fields.next().ok_or("ps parent missing")?.parse::<u32>()?;
        let group = fields.next().ok_or("ps group missing")?.parse::<u32>()?;
        let rss = fields.next().ok_or("ps RSS missing")?.parse::<u64>()?;
        rows.push((
            pid.parse::<u32>()?,
            parent,
            group,
            rss,
            fields.collect::<Vec<_>>().join(" "),
        ));
    }
    let mut owned: BTreeSet<u32> = roots.iter().copied().collect();
    for prior in previous {
        if native(prior.pid).is_ok_and(|c| c.0 == prior.identity) {
            owned.insert(prior.pid);
        }
    }
    loop {
        let before = owned.len();
        for (pid, parent, ..) in &rows {
            if owned.contains(parent) {
                owned.insert(*pid);
            }
        }
        if owned.len() == before {
            break;
        }
    }
    let mut result = Vec::new();
    for (pid, parent, group, rss_kib, command) in rows {
        if !owned.contains(&pid) {
            continue;
        }
        // A process that exits during the snapshot is not silently treated as zero usage.
        let (identity, cpu_s, interrupt_wakeups, voluntary_switches) = native(pid)?;
        result.push(Counter {
            pid,
            parent,
            group,
            rss_kib,
            command,
            identity,
            cpu_s,
            interrupt_wakeups,
            voluntary_switches,
        });
    }
    if roots
        .iter()
        .any(|pid| !result.iter().any(|c| c.pid == *pid))
    {
        return Err("a measurement root disappeared from the process snapshot".into());
    }
    Ok(result)
}

pub(super) fn same_process(counter: &Counter) -> bool {
    native(counter.pid).is_ok_and(|c| c.0 == counter.identity)
}

type Native = (u64, f64, Option<u64>, Option<u64>);
#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn native(pid: u32) -> Result<Native> {
    let mut info = std::mem::MaybeUninit::<nix::libc::rusage_info_v2>::zeroed();
    // SAFETY: flavor and writable structure agree, and only successful output is read.
    let status = unsafe {
        nix::libc::proc_pid_rusage(
            i32::try_from(pid)?,
            nix::libc::RUSAGE_INFO_V2,
            info.as_mut_ptr().cast::<nix::libc::rusage_info_t>(),
        )
    };
    if status != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let info = unsafe { info.assume_init() };
    let mut timebase = nix::libc::mach_timebase_info { numer: 0, denom: 0 };
    // SAFETY: timebase is an initialized writable out parameter.
    if unsafe { nix::libc::mach_timebase_info(&mut timebase) } != 0 || timebase.denom == 0 {
        return Err("mach timebase unavailable".into());
    }
    let seconds = (info.ri_user_time as f64 + info.ri_system_time as f64)
        * f64::from(timebase.numer)
        / f64::from(timebase.denom)
        / 1e9;
    Ok((
        info.ri_proc_start_abstime,
        seconds,
        Some(info.ri_interrupt_wkups),
        None,
    ))
}

#[cfg(target_os = "linux")]
fn native(pid: u32) -> Result<Native> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .ok_or("invalid proc stat")?
        .1
        .split_whitespace()
        .collect();
    let parse = |i: usize| -> Result<u64> { Ok(fields.get(i).ok_or("short proc stat")?.parse()?) };
    // SAFETY: sysconf has no pointer arguments.
    let ticks = unsafe { nix::libc::sysconf(nix::libc::_SC_CLK_TCK) };
    if ticks <= 0 {
        return Err("clock tick rate unavailable".into());
    }
    let mut voluntary = 0;
    for task in std::fs::read_dir(format!("/proc/{pid}/task"))? {
        let text = std::fs::read_to_string(task?.path().join("status"))?;
        let value = text
            .lines()
            .find_map(|l| l.strip_prefix("voluntary_ctxt_switches:"))
            .ok_or("thread voluntary context switches missing")?
            .trim()
            .parse::<u64>()?;
        voluntary += value;
    }
    Ok((
        parse(19)?,
        (parse(11)? + parse(12)?) as f64 / ticks as f64,
        None,
        Some(voluntary),
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn native(_pid: u32) -> Result<Native> {
    Err("native process counters unavailable on this platform".into())
}

pub(super) fn delta(before: &[Counter], after: &[Counter], window: Duration) -> Value {
    let stable = before.len() == after.len()
        && before.iter().all(|a| {
            after
                .iter()
                .any(|b| a.pid == b.pid && a.identity == b.identity)
        });
    let unavailable = |reason: &str| json!({"status":"unavailable", "reason":reason});
    let difference = |f: fn(&Counter) -> Option<u64>| -> Value {
        if !stable {
            return unavailable(
                "process membership changed; exited/new process accounting incomplete",
            );
        }
        let mut total = 0u64;
        for a in before {
            let b = after
                .iter()
                .find(|b| a.pid == b.pid)
                .expect("stable membership");
            let Some(d) = f(a).zip(f(b)).and_then(|(x, y)| y.checked_sub(x)) else {
                return unavailable(
                    "counter unsupported or reset (thread churn can reset context-switch totals)",
                );
            };
            total += d;
        }
        json!({"status":"measured", "value":total, "per_second":total as f64/window.as_secs_f64()})
    };
    let cpu = if stable {
        let seconds = after.iter().map(|p| p.cpu_s).sum::<f64>()
            - before.iter().map(|p| p.cpu_s).sum::<f64>();
        if seconds < 0.0 {
            unavailable("CPU counter decreased")
        } else {
            json!({"status":"measured", "seconds":seconds, "percent_one_core":100.0*seconds/window.as_secs_f64()})
        }
    } else {
        unavailable("process membership changed; CPU lifetime totals incomplete")
    };
    json!({
        "window_s":window.as_secs_f64(), "stable_membership":stable,
        "cpu":cpu,
        "scheduler_wakeups":unavailable("no privileged scheduler tracing; context switches and interrupt wakeups are not total wakeups"),
        "interrupt_wakeups":difference(|c| c.interrupt_wakeups),
        "voluntary_context_switches":difference(|c| c.voluntary_switches),
        "rss_before_kib":before.iter().map(|p| p.rss_kib).sum::<u64>(),
        "rss_after_kib":after.iter().map(|p| p.rss_kib).sum::<u64>(),
        "rss_semantics":"sum of all owned live process RSS, shared pages double-counted; boundary samples, not peak",
        "before":before, "after":after,
    })
}
