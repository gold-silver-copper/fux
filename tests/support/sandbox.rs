//! A sandbox like Codex's on Linux: a seccomp filter under which `connect`,
//! `bind`, `sendto` (`send`), `setsockopt` and `shutdown` fail with
//! `EPERM`, while socket pairs, FIFOs, read(2) and write(2) and sendmsg(2)
//! keep working.
//!
//! The filter goes on a thread of its own, installed with a safe call (the
//! tests forbid unsafe code), and every process that thread spawns inherits
//! it: a fork copies the forking thread's filters, and they hold across
//! exec. The test's other threads are not filtered.
use std::collections::BTreeMap;

/// Runs `work` on a thread under the filter; what it spawns is sandboxed.
pub fn sandboxed<T: Send>(work: impl FnOnce() -> Result<T, String> + Send) -> Result<T, String> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                deny_sockets()?;
                work()
            })
            .join()
            .map_err(|_| "the sandboxed thread panicked".to_owned())?
    })
}

/// Installs the filter on the calling thread: `no_new_privs`, then a BPF
/// program that checks the architecture (another is killed) and answers the
/// five calls `EPERM`, allowing the rest.
fn deny_sockets() -> Result<(), String> {
    let denied = [
        libc::SYS_connect,
        libc::SYS_bind,
        libc::SYS_sendto,
        libc::SYS_setsockopt,
        libc::SYS_shutdown,
    ];
    let rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> =
        denied.into_iter().map(|call| (call, Vec::new())).collect();
    let arch = seccompiler::TargetArch::try_from(std::env::consts::ARCH).map_err(super::e)?;
    let eperm = u32::try_from(libc::EPERM).map_err(super::e)?;
    let program: seccompiler::BpfProgram = seccompiler::SeccompFilter::new(
        rules,
        seccompiler::SeccompAction::Allow,
        seccompiler::SeccompAction::Errno(eperm),
        arch,
    )
    .map_err(super::e)?
    .try_into()
    .map_err(super::e)?;
    seccompiler::apply_filter(&program).map_err(super::e)
}

/// Whether process `pid` runs under a seccomp filter: that a test's
/// sandbox reached it.
pub fn filtered(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .is_ok_and(|status| status.lines().any(|line| line == "Seccomp:\t2"))
}
