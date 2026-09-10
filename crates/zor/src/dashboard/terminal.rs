use super::{View, snapshot};
use anyhow::Result;
use std::{
    collections::BTreeSet,
    fs::File,
    io::{IsTerminal, Read, Write},
    os::fd::{AsFd, AsRawFd},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

fn clean(text: &str, width: usize) -> String {
    text.chars()
        .take(width)
        .map(|ch| {
            if ch.is_ascii_graphic() || ch == ' ' {
                ch
            } else {
                '?'
            }
        })
        .collect()
}
fn fresh(stale: bool, age: Option<u64>, elapsed: Duration) -> bool {
    !stale
        && elapsed <= Duration::from_secs(5)
        && age.is_none_or(|age| {
            Duration::from_millis(age).saturating_add(elapsed) <= Duration::from_secs(5)
        })
}
fn paint(
    view: Option<&View>,
    selected: usize,
    attention: bool,
    problem: &str,
    elapsed: Duration,
    cols: usize,
    rows: usize,
) -> Vec<u8> {
    let stale = view.is_none_or(|view| !fresh(view.stale, None, elapsed));
    let width = cols.saturating_sub(1).clamp(1, 160);
    let height = rows.clamp(1, 60);
    let mut lines = vec![format!(
        "zor dashboard  {}  [j/k] select [Enter] focus [a] attention [q] quit",
        if stale { "STALE" } else { "live" }
    )];
    let filtered: Vec<_> = view
        .into_iter()
        .flat_map(|view| &view.rows)
        .filter(|row| !attention || row.attention)
        .collect();
    let page = height.saturating_sub(4).max(1);
    let start = selected / page * page;
    for (index, row) in filtered.iter().enumerate().skip(start).take(page) {
        lines.push(format!(
            "{} {} {:12} {:11} {}",
            if selected == index { ">" } else { " " },
            if row.attention { "!" } else { " " },
            if row.age_upper_bound_ms.is_some() && !fresh(stale, row.age_upper_bound_ms, elapsed) {
                "unknown"
            } else {
                &row.status
            },
            row.kind,
            row.label
        ));
    }
    if filtered.is_empty() {
        lines.push("No matching tasks or observed panes".into());
    }
    if let Some(row) = filtered.get(selected) {
        lines.push(if !fresh(stale, row.age_upper_bound_ms, elapsed) {
            "Evidence stale; focus disabled until a fresh snapshot arrives".into()
        } else {
            row.detail.clone()
        });
    }
    lines.push(if problem.is_empty() {
        format!(
            "{} rows; {}",
            filtered.len(),
            if attention {
                "attention only"
            } else {
                "all evidence"
            }
        )
    } else {
        problem.into()
    });
    let mut frame = String::from("\x1b[H");
    for (index, line) in lines.iter().take(height).enumerate() {
        frame.push_str("\x1b[2K");
        frame.push_str(&clean(line, width));
        if index + 1 < height {
            frame.push_str("\r\n");
        }
    }
    frame.push_str("\x1b[J");
    frame.into_bytes()
}

struct Screen {
    output: File,
    flags: nix::fcntl::OFlag,
    _raw: crate::platform::Guard,
    signals: Vec<signal_hook::SigId>,
}
impl Screen {
    fn open(stop: &Arc<AtomicBool>) -> Result<Self> {
        anyhow::ensure!(
            std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
            "dashboard requires a terminal; use dashboard --once for JSON"
        );
        let output = File::from(std::io::stdout().as_fd().try_clone_to_owned()?);
        let flags = nix::fcntl::OFlag::from_bits_truncate(nix::fcntl::fcntl(
            output.as_raw_fd(),
            nix::fcntl::FcntlArg::F_GETFL,
        )?);
        let raw = crate::platform::set_raw(0)?;
        let mut screen = Self {
            output,
            flags,
            _raw: raw,
            signals: Vec::new(),
        };
        nix::fcntl::fcntl(
            screen.output.as_raw_fd(),
            nix::fcntl::FcntlArg::F_SETFL(flags | nix::fcntl::OFlag::O_NONBLOCK),
        )?;
        for signal in [
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGHUP,
        ] {
            screen
                .signals
                .push(signal_hook::flag::register(signal, Arc::clone(stop))?);
        }
        screen.write(b"\x1b[?1049h\x1b[?25l\x1b[2J")?;
        Ok(screen)
    }
    fn write(&mut self, mut bytes: &[u8]) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(750);
        while !bytes.is_empty() {
            anyhow::ensure!(
                Instant::now() < deadline,
                "dashboard terminal output stalled"
            );
            match self.output.write(bytes) {
                Ok(0) => anyhow::bail!("dashboard terminal closed"),
                Ok(count) => {
                    bytes = bytes
                        .get(count..)
                        .ok_or_else(|| anyhow::anyhow!("invalid write count"))?
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    let mut polls = [nix::poll::PollFd::new(
                        self.output.as_fd(),
                        nix::poll::PollFlags::POLLOUT,
                    )];
                    nix::poll::poll(&mut polls, 50_u16)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = self.write(b"\x1b[?25h\x1b[?1049l");
        let _ = nix::fcntl::fcntl(
            self.output.as_raw_fd(),
            nix::fcntl::FcntlArg::F_SETFL(self.flags),
        );
        for id in &self.signals {
            signal_hook::low_level::unregister(*id);
        }
    }
}
struct Fetcher {
    stop: mpsc::SyncSender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Fetcher {
    fn drop(&mut self) {
        let _ = self.stop.try_send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub(super) fn run(
    directory: Option<PathBuf>,
    bell: bool,
    notify: bool,
    notification_command: Option<PathBuf>,
) -> Result<u8> {
    let stop = Arc::new(AtomicBool::new(false));
    let mut screen = Screen::open(&stop)?;
    let (sender, receiver) = mpsc::sync_channel(1);
    let (end, ended) = mpsc::sync_channel(1);
    let thread = std::thread::Builder::new()
        .name("zor-dashboard".into())
        .spawn(move || {
            loop {
                let view = snapshot(directory.clone()).map_err(|error| format!("{error:#}"));
                if sender.try_send((Instant::now(), view)).is_err() && ended.try_recv().is_ok() {
                    break;
                }
                match ended.recv_timeout(Duration::from_secs(1)) {
                    Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        })?;
    let fetcher = Fetcher {
        stop: end,
        thread: Some(thread),
    };
    let mut view: Option<View> = None;
    let mut at = Instant::now();
    let mut selected = 0usize;
    let mut attention = false;
    let mut problem = "Connecting to zor service".to_string();
    let mut notifications = super::attention::Attention::new();
    let mut delivery =
        notify.then(|| crate::platform::notification::Delivery::new(notification_command));
    let mut notification_problem = None;
    let mut last = Vec::new();
    let mut input = std::io::stdin();
    let result = (|| -> Result<u8> {
        while !stop.load(Ordering::Acquire) {
            if let Ok((produced, next)) = receiver.try_recv() {
                at = produced;
                match next {
                    Ok(next) => {
                        let current: BTreeSet<_> = next
                            .rows
                            .iter()
                            .filter(|row| {
                                row.attention
                                    && fresh(next.stale, row.age_upper_bound_ms, at.elapsed())
                            })
                            .map(|row| row.key.clone())
                            .collect();
                        if notifications.observe(current) && bell {
                            screen.write(b"\x07")?;
                        }
                        problem = next.problems.join("; ");
                        // Preserve selection by identity when attention sorting changes.
                        let key = view
                            .as_ref()
                            .and_then(|view| {
                                view.rows
                                    .iter()
                                    .filter(|row| !attention || row.attention)
                                    .nth(selected)
                            })
                            .map(|row| &row.key);
                        selected = key
                            .and_then(|key| {
                                next.rows
                                    .iter()
                                    .filter(|row| !attention || row.attention)
                                    .position(|row| &row.key == key)
                            })
                            .unwrap_or(0);
                        view = Some(next);
                    }
                    Err(error) => {
                        notifications.lost();
                        problem = error;
                        view = None;
                        selected = 0;
                    }
                }
            }
            if let Some(delivery) = &mut delivery {
                if let Some(result) = delivery.poll() {
                    notification_problem = result
                        .err()
                        .map(|error| format!("Notification failed: {error:#}"));
                }
                if !delivery.busy() {
                    let current = view
                        .as_ref()
                        .map(|view| {
                            view.rows
                                .iter()
                                .filter(|row| {
                                    row.attention
                                        && fresh(view.stale, row.age_upper_bound_ms, at.elapsed())
                                })
                                .map(|row| row.key.clone())
                                .collect()
                        })
                        .unwrap_or_default();
                    if let Some(count) = notifications.take(&current, Instant::now())
                        && let Err(error) = delivery.start(
                            "Zor needs attention",
                            &format!(
                                "{count} new attention entries. Open zor dashboard for details."
                            ),
                        )
                    {
                        notification_problem = Some(format!("Notification failed: {error:#}"));
                    }
                }
            }
            let size = crate::platform::winsize(1);
            let display_problem = match &notification_problem {
                Some(notification) if !problem.is_empty() => format!("{problem}; {notification}"),
                Some(notification) => notification.clone(),
                None => problem.clone(),
            };
            let frame = paint(
                view.as_ref(),
                selected,
                attention,
                &display_problem,
                at.elapsed(),
                usize::from(size.cols),
                usize::from(size.rows),
            );
            if frame != last {
                screen.write(&frame)?;
                last = frame;
            }
            let mut polls = [nix::poll::PollFd::new(
                input.as_fd(),
                nix::poll::PollFlags::POLLIN,
            )];
            match nix::poll::poll(&mut polls, 100_u16) {
                Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
                Ok(_) => {}
                Err(error) => return Err(error.into()),
            }
            let mut bytes = [0; 64];
            let count = input.read(&mut bytes)?;
            if count == 0 {
                break;
            }
            for byte in bytes.iter().take(count) {
                let filtered: Vec<_> = view
                    .as_ref()
                    .into_iter()
                    .flat_map(|view| &view.rows)
                    .filter(|row| !attention || row.attention)
                    .collect();
                match byte {
                    b'q' | 3 | 4 => {
                        stop.store(true, Ordering::Release);
                        break;
                    }
                    b'j' => selected = (selected + 1).min(filtered.len().saturating_sub(1)),
                    b'k' => selected = selected.saturating_sub(1),
                    b'a' => {
                        attention = !attention;
                        selected = 0;
                    }
                    b'\r' | b'\n' => {
                        if view.as_ref().is_none_or(|view| {
                            !fresh(
                                view.stale,
                                filtered
                                    .get(selected)
                                    .and_then(|row| row.age_upper_bound_ms),
                                at.elapsed(),
                            )
                        }) {
                            problem = "Evidence stale; focus disabled".into();
                            continue;
                        }
                        problem = if let Some(target) =
                            filtered.get(selected).and_then(|row| row.target.as_ref())
                        {
                            match crate::tasks::focus::target(target) {
                                Ok(_) => {
                                    format!("Focused {} pane {}", target.workspace, target.pane)
                                }
                                Err(error) => format!("Focus refused: {error:#}"),
                            }
                        } else {
                            "No live target for this row".into()
                        };
                    }
                    _ => {}
                }
            }
        }
        Ok(0)
    })();
    // Restore the terminal before waiting for a bounded in-flight snapshot RPC, including errors.
    drop(screen);
    drop(delivery);
    drop(fetcher);
    result
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    #[test]
    fn queued_and_near_expiry_evidence_never_gets_a_new_freshness_window() {
        assert!(super::fresh(false, Some(4900), Duration::from_millis(50)));
        assert!(!super::fresh(false, Some(4900), Duration::from_millis(101)));
        assert!(!super::fresh(false, None, Duration::from_secs(6)));
        assert!(!super::fresh(true, Some(0), Duration::ZERO));
    }
    #[test]
    fn tiny_frames_preserve_header_without_bottom_row_scrolling() {
        let frame = super::paint(None, 0, false, "unavailable", Duration::ZERO, 20, 3);
        let mut parser = vt100::Parser::new(3, 20, 0);
        parser.process(&frame);
        assert!(parser.screen().contents().starts_with("zor dashboard"));
        assert!(!frame.windows(2).any(|pair| pair == b"\x1b]"));
    }
    #[test]
    fn labels_cannot_inject_terminal_controls_or_wide_layout() {
        assert_eq!(
            super::clean("hello\x1b]52;c;evil\x07界\n", 80),
            "hello?]52;c;evil???"
        );
        assert_eq!(super::clean("abcdef", 3), "abc");
    }
}
