//! What a viewer does when the running session server speaks an older protocol: explain, list
//! the workspaces it recorded, and offer to stop it (after a typed confirmation) or to run
//! alongside it. Nothing here ever stops a server without that confirmation.

use anyhow::{Result, bail};

/// True when the manager answered with a different control preface: an older fux is running.
pub fn incompatible_server(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::Unsupported)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationChoice {
    /// Stop the old server (terminating its panes) and start this version.
    Stop,
    /// Leave it running; explain how to use a separate runtime directory.
    Alongside,
    Quit,
}

impl MigrationChoice {
    pub fn parse(input: &str) -> Option<Self> {
        match input.trim() {
            "k" | "K" | "stop" => Some(Self::Stop),
            "s" | "S" | "alongside" => Some(Self::Alongside),
            "" | "q" | "Q" | "quit" => Some(Self::Quit),
            _ => None,
        }
    }
}

/// Explains the incompatible server and asks the operator what to do. Without a terminal on both
/// stdin and stderr nothing is asked: the caller reports the mismatch and leaves the server alone.
pub fn migration_dialog(paths: &crate::daemon::DaemonPaths) -> Result<MigrationChoice> {
    use std::io::{BufRead, IsTerminal, Write};
    let servers = crate::daemon::recorded_servers(paths);
    let mut err = std::io::stderr().lock();
    writeln!(
        err,
        "fux: the running session server speaks an older protocol; this fux needs control {} and attachment v{}.",
        String::from_utf8_lossy(crate::proto::control::CONTROL_PREFACE).trim_end(),
        crate::proto::attach::VERSION
    )?;
    writeln!(err, "  runtime directory: {}", paths.runtime_dir.display())?;
    if servers.is_empty() {
        writeln!(err, "  its workspaces: none recorded")?;
    } else {
        let listed: Vec<String> = servers
            .iter()
            .map(|(name, pid)| format!("{name} (pid {pid})"))
            .collect();
        writeln!(err, "  its workspaces: {}", listed.join(", "))?;
    }
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        writeln!(
            err,
            "Run `fux` in a terminal to choose what to do, or point XDG_RUNTIME_DIR at a fresh \
             directory to run this version alongside it."
        )?;
        return Ok(MigrationChoice::Quit);
    }
    writeln!(err, "Choose:")?;
    writeln!(
        err,
        "  k  stop the old server and start this one; this TERMINATES every pane listed above"
    )?;
    writeln!(
        err,
        "  s  leave it running and show how to run this fux alongside it"
    )?;
    writeln!(err, "  q  quit and leave it running (default)")?;
    let stdin = std::io::stdin();
    loop {
        write!(err, "[k/s/q] ")?;
        err.flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            return Ok(MigrationChoice::Quit);
        }
        match MigrationChoice::parse(&line) {
            Some(MigrationChoice::Stop) => {
                write!(
                    err,
                    "Type \"stop\" to confirm terminating the old server and its panes: "
                )?;
                err.flush()?;
                let mut confirmation = String::new();
                if stdin.lock().read_line(&mut confirmation)? == 0 || confirmation.trim() != "stop"
                {
                    writeln!(err, "Not confirmed; the old server keeps running.")?;
                    return Ok(MigrationChoice::Quit);
                }
                return Ok(MigrationChoice::Stop);
            }
            Some(choice) => return Ok(choice),
            None => writeln!(err, "Please answer k, s or q.")?,
        }
    }
}

pub fn print_alongside_instructions(paths: &crate::daemon::DaemonPaths) {
    eprintln!(
        "Leave the old server running and start this fux in a separate runtime directory:\n\n  \
         XDG_RUNTIME_DIR=\"$HOME/.fux-runtime\" fux\n\nUse the same variable for every later fux, \
         koh gateway and zor command that should reach the new server. The old server stays \
         at {}.",
        paths.runtime_dir.display()
    );
}

/// Sends SIGTERM to the recorded server processes and waits for the manager socket to close.
/// Never escalates to SIGKILL: an unresponsive old server keeps its panes, and the operator is told.
pub fn stop_old_server(paths: &crate::daemon::DaemonPaths) -> Result<()> {
    use std::os::unix::net::UnixStream;
    let mut pids: Vec<u32> = crate::daemon::recorded_servers(paths)
        .into_iter()
        .map(|(_, pid)| pid)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    if pids.is_empty() {
        bail!(
            "no server descriptors under {}; stop the old server yourself, then run fux again",
            paths.descriptors_dir.display()
        );
    }
    for pid in &pids {
        let target = nix::unistd::Pid::from_raw(i32::try_from(*pid)?);
        match nix::sys::signal::kill(target, nix::sys::signal::Signal::SIGTERM) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
            Err(error) => bail!("signalling the old server (pid {pid}): {error}"),
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match UnixStream::connect(&paths.manager_socket) {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                eprintln!("The old server has stopped; starting this version.");
                return Ok(());
            }
            _ => {}
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "the old server (pid {}) did not stop within 10 s; its panes are untouched",
                pids.iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_choices_parse_defaults_and_aliases() {
        assert_eq!(MigrationChoice::parse("k\n"), Some(MigrationChoice::Stop));
        assert_eq!(
            MigrationChoice::parse(" s "),
            Some(MigrationChoice::Alongside)
        );
        assert_eq!(MigrationChoice::parse(""), Some(MigrationChoice::Quit));
        assert_eq!(MigrationChoice::parse("q"), Some(MigrationChoice::Quit));
        assert_eq!(MigrationChoice::parse("x"), None);
    }
}
