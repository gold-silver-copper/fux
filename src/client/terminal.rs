use super::Compositor;
use crate::state::{MAX_CLIPBOARD_BYTES, WorkspaceState};
use base64::Engine as _;
use koh::client::backend::{CellStyle as BackendStyle, KohBackend};
use koh::client::{ClientState as _, ClientTerminal, InputModes};
use koh::predict::Overlay;
use ratatui_core::buffer::Buffer;
use ratatui_core::style::{Color, Modifier};
use std::collections::BTreeMap;
use std::io;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MAX_TERMINAL_TITLE_BYTES: usize = 4096;

pub struct WorkspaceTerminal<B: KohBackend> {
    backend: B,
    compositor: Compositor,
    previous: Option<Buffer>,
    clipboard_enabled: bool,
    title_initialized: bool,
    last_title: String,
    last_clipboard: String,
    last_bell: u64,
    previous_modes: Option<InputModes>,
    notifications: Option<ClientNotificationGate>,
    notification_children: Vec<(Child, Instant)>,
}

impl<B: KohBackend> WorkspaceTerminal<B> {
    pub fn enter(mut backend: B, clipboard_enabled: bool) -> io::Result<Self> {
        backend.enter_raw_mode()?;
        if let Err(error) = backend.enter_alt_screen() {
            // The backend may have emitted only part of the enter sequence before failing.
            let _ = backend.end_frame();
            let _ = backend.leave_alt_screen();
            let _ = backend.leave_raw_mode();
            return Err(error);
        }
        Ok(Self {
            backend,
            compositor: Compositor::default(),
            previous: None,
            clipboard_enabled,
            title_initialized: false,
            last_title: String::new(),
            last_clipboard: String::new(),
            last_bell: 0,
            previous_modes: None,
            notifications: None,
            notification_children: Vec::new(),
        })
    }

    pub fn compositor_mut(&mut self) -> &mut Compositor {
        &mut self.compositor
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub fn invalidate(&mut self) {
        self.previous = None;
        self.title_initialized = false;
        self.last_title.clear();
        self.last_clipboard.clear();
        self.last_bell = 0;
        self.previous_modes = None;
    }

    fn emit_out_of_band(&mut self, state: &WorkspaceState) -> io::Result<()> {
        reap_notifications(&mut self.notification_children, false);
        if let Some(notifications) = &mut self.notifications {
            notifications.retain(state.panes().keys().map(|pane| pane.0));
            for (pane, view) in state.panes() {
                if notifications.observe(pane.0, view.agent.state)
                    && let Some(child) = spawn_notification(
                        view.agent.id.as_deref().unwrap_or("agent state changed"),
                    )
                {
                    track_notification(&mut self.notification_children, child);
                }
            }
        }
        let window = state.window();
        let title = sanitize_title(window.title);
        if (self.title_initialized || !title.is_empty())
            && (!self.title_initialized || title != self.last_title)
        {
            self.backend.set_window_title(&title)?;
            self.last_title = title;
            self.title_initialized = true;
        }
        if self.clipboard_enabled && window.clipboard != self.last_clipboard {
            self.last_clipboard = window.clipboard.to_owned();
            if valid_clipboard(window.clipboard) {
                self.backend.set_clipboard(window.clipboard)?;
            }
        }
        if window.bell_count > self.last_bell {
            self.backend.bell()?;
        }
        self.last_bell = self.last_bell.max(window.bell_count);
        let modes = state.input_modes();
        let bytes = self
            .previous_modes
            .map_or_else(|| modes.formatted(), |old| modes.diff(old));
        if !bytes.is_empty() {
            self.backend.write_input_modes(&bytes)?;
        }
        self.previous_modes = Some(modes);
        Ok(())
    }

    fn paint(&mut self, next: &Buffer, cursor: Option<(u16, u16)>) -> io::Result<()> {
        self.backend.begin_frame()?;
        let painted: io::Result<()> = (|| {
            let empty = Buffer::empty(next.area);
            let previous = self.previous.as_ref().unwrap_or(&empty);
            for (col, row, cell) in previous.diff(next) {
                self.backend.move_to(row, col)?;
                self.backend.set_style(backend_style(cell))?;
                self.backend.print(cell.symbol())?;
            }
            if let Some((row, col)) = cursor {
                self.backend.move_to(row, col)?;
                self.backend.show_cursor()?;
            } else {
                self.backend.write_bytes(b"\x1b[?25l")?;
            }
            Ok(())
        })();
        // DEC synchronized-output mode must never leak after a short/error frame.
        let ended = self.backend.end_frame();
        painted?;
        ended?;
        self.backend.flush()?;
        self.previous = Some(next.clone());
        Ok(())
    }

    /// Testable suspend transition without actually stopping the process.
    pub fn leave_for_suspend(&mut self) -> io::Result<()> {
        self.backend.end_frame()?;
        self.backend.leave_alt_screen()?;
        self.backend.leave_raw_mode()
    }

    /// Testable resume transition; the production wrapper raises SIGTSTP between these calls.
    pub fn reenter_after_resume(&mut self) -> io::Result<()> {
        self.backend.enter_raw_mode()?;
        self.backend.enter_alt_screen()?;
        self.invalidate();
        Ok(())
    }
}

fn track_notification(children: &mut Vec<(Child, Instant)>, child: Child) {
    if children.len() >= 16 {
        terminate_notification(&mut children.remove(0).0);
    }
    children.push((child, Instant::now()));
}

impl WorkspaceTerminal<koh::client::DefaultBackend> {
    pub fn enter_default(
        clipboard_enabled: bool,
        notifications: Option<crate::config::NotificationPolicy>,
    ) -> io::Result<Self> {
        let mut terminal = Self::enter(koh::client::DefaultBackend::new()?, clipboard_enabled)?;
        terminal.notifications = notifications.map(ClientNotificationGate::new);
        Ok(terminal)
    }
}

pub struct ClientNotificationGate {
    policy: crate::config::NotificationPolicy,
    states: BTreeMap<u32, crate::state::AgentState>,
}

impl ClientNotificationGate {
    pub fn new(policy: crate::config::NotificationPolicy) -> Self {
        Self {
            policy,
            states: BTreeMap::new(),
        }
    }

    pub fn observe(&mut self, pane: u32, next: crate::state::AgentState) -> bool {
        use crate::state::AgentState::{Blocked, Idle, Working};
        let previous = self.states.insert(pane, next);
        if !self.policy.enabled {
            return false;
        }
        match (previous, next) {
            (None, Blocked) => self.policy.notify_blocked,
            (Some(old), Blocked) => self.policy.notify_blocked && old != Blocked,
            (Some(Working | Blocked), Idle) => self.policy.notify_idle,
            _ => false,
        }
    }

    pub fn retain(&mut self, panes: impl IntoIterator<Item = u32>) {
        let panes: std::collections::BTreeSet<_> = panes.into_iter().collect();
        self.states.retain(|pane, _| panes.contains(pane));
    }

    pub fn tracked_count(&self) -> usize {
        self.states.len()
    }
}

fn spawn_notification(message: &str) -> Option<Child> {
    let termux = std::env::var_os("TERMUX_VERSION").is_some();
    let display =
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    let command = client_notification_command(
        message,
        termux,
        cfg!(target_os = "macos"),
        display,
        executable_on_path,
    );
    if let Some(command) = command
        && let Some((program, arguments)) = command.split_first()
        && let Ok(child) = Command::new(program)
            .args(arguments)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    {
        return Some(child);
    }
    None
}

/// Selects the client-side notifier without invoking a shell. `available` is injectable so the
/// Termux/macOS/Linux preference order is testable on every build host.
pub fn client_notification_command(
    message: &str,
    termux: bool,
    macos: bool,
    display: bool,
    available: impl Fn(&str) -> bool,
) -> Option<Vec<String>> {
    if termux && available("termux-notification") {
        return Some(vec![
            "termux-notification".into(),
            "-t".into(),
            "fux".into(),
            "-c".into(),
            message.into(),
        ]);
    }
    if macos && available("terminal-notifier") {
        return Some(vec![
            "terminal-notifier".into(),
            "-title".into(),
            "fux".into(),
            "-message".into(),
            message.into(),
        ]);
    }
    if macos && available("osascript") {
        let escaped = message.replace('\\', "\\\\").replace('"', "\\\"");
        return Some(vec![
            "osascript".into(),
            "-e".into(),
            format!("display notification \"{escaped}\" with title \"fux\""),
        ]);
    }
    if display && available("notify-send") {
        return Some(vec!["notify-send".into(), "fux".into(), message.into()]);
    }
    None
}

fn reap_notifications(children: &mut Vec<(Child, Instant)>, shutdown: bool) {
    children.retain_mut(|(child, started)| {
        if child.try_wait().ok().flatten().is_some() {
            return false;
        }
        if shutdown || started.elapsed() >= Duration::from_secs(5) {
            terminate_notification(child);
            return false;
        }
        true
    });
}

fn terminate_notification(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    let _ = child.wait();
}

#[cfg(test)]
mod notification_tests {
    use super::*;

    #[test]
    fn transition_storm_keeps_at_most_sixteen_owned_notifiers()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut children = Vec::new();
        for _ in 0..20 {
            let child = Command::new("/bin/sh")
                .args(["-c", "sleep 30"])
                .process_group(0)
                .spawn()?;
            track_notification(&mut children, child);
            assert!(children.len() <= 16);
        }
        reap_notifications(&mut children, true);
        assert!(children.is_empty());
        Ok(())
    }
}

fn executable_on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| directory.join(program).is_file())
    })
}

impl<B: KohBackend> ClientTerminal<WorkspaceState> for WorkspaceTerminal<B> {
    fn render(
        &mut self,
        state: &WorkspaceState,
        overlay: &Overlay,
        status: Option<&str>,
    ) -> io::Result<()> {
        self.emit_out_of_band(state)?;
        let (rows, cols) = self.backend.size()?;
        let frame = self.compositor.compose(state, overlay, status, rows, cols);
        self.paint(&frame.buffer, frame.cursor)
    }

    fn size(&self) -> io::Result<(u16, u16)> {
        self.backend.size()
    }

    fn suspend_resume(&mut self) -> io::Result<()> {
        self.leave_for_suspend()?;
        let _ = self
            .backend
            .write_bytes("\n[fux suspended — run `fg` to resume]\n".as_bytes());
        let _ = self.backend.flush();
        nix::sys::signal::raise(nix::sys::signal::Signal::SIGTSTP).map_err(io::Error::other)?;
        self.reenter_after_resume()
    }
}

impl<B: KohBackend> Drop for WorkspaceTerminal<B> {
    fn drop(&mut self) {
        reap_notifications(&mut self.notification_children, true);
        let _ = self.backend.end_frame();
        let _ = self.backend.leave_alt_screen();
        let _ = self.backend.leave_raw_mode();
    }
}

#[derive(Debug)]
pub struct CaptureBackend {
    pub bytes: Vec<u8>,
    pub rows: u16,
    pub cols: u16,
    pub raw: bool,
    pub alternate: bool,
    pub flushes: usize,
}

impl CaptureBackend {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            bytes: Vec::new(),
            rows,
            cols,
            raw: false,
            alternate: false,
            flushes: 0,
        }
    }
}

impl KohBackend for CaptureBackend {
    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.flushes = self.flushes.saturating_add(1);
        Ok(())
    }
    fn enter_raw_mode(&mut self) -> io::Result<()> {
        self.raw = true;
        Ok(())
    }
    fn leave_raw_mode(&mut self) -> io::Result<()> {
        self.raw = false;
        Ok(())
    }
    fn size(&self) -> io::Result<(u16, u16)> {
        Ok((self.rows, self.cols))
    }
    fn enter_alt_screen(&mut self) -> io::Result<()> {
        self.alternate = true;
        self.write_bytes(b"\x1b[?1049h\x1b[?25l")?;
        self.flush()
    }
    fn leave_alt_screen(&mut self) -> io::Result<()> {
        self.alternate = false;
        self.write_bytes(b"\x1b[?9l\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1l\x1b>\x1b[?25h\x1b[?1049l")?;
        self.flush()
    }
}

fn sanitize_title(title: &str) -> String {
    let mut output = String::new();
    for character in title.chars().filter(|value| !value.is_control()) {
        if output.len().saturating_add(character.len_utf8()) > MAX_TERMINAL_TITLE_BYTES {
            break;
        }
        output.push(character);
    }
    output
}

fn valid_clipboard(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CLIPBOARD_BYTES
        && base64::engine::general_purpose::STANDARD
            .decode(value)
            .is_ok()
}

fn backend_style(cell: &ratatui_core::buffer::Cell) -> BackendStyle {
    BackendStyle {
        fg: rat_to_vt(cell.fg),
        bg: rat_to_vt(cell.bg),
        bold: cell.modifier.contains(Modifier::BOLD),
        dim: cell.modifier.contains(Modifier::DIM),
        italic: cell.modifier.contains(Modifier::ITALIC),
        underline: cell.modifier.contains(Modifier::UNDERLINED),
        inverse: cell.modifier.contains(Modifier::REVERSED),
    }
}

const fn rat_to_vt(color: Color) -> vt100::Color {
    match color {
        Color::Reset => vt100::Color::Default,
        Color::Black => vt100::Color::Idx(0),
        Color::Red => vt100::Color::Idx(1),
        Color::Green => vt100::Color::Idx(2),
        Color::Yellow => vt100::Color::Idx(3),
        Color::Blue => vt100::Color::Idx(4),
        Color::Magenta => vt100::Color::Idx(5),
        Color::Cyan => vt100::Color::Idx(6),
        Color::Gray => vt100::Color::Idx(7),
        Color::DarkGray => vt100::Color::Idx(8),
        Color::LightRed => vt100::Color::Idx(9),
        Color::LightGreen => vt100::Color::Idx(10),
        Color::LightYellow => vt100::Color::Idx(11),
        Color::LightBlue => vt100::Color::Idx(12),
        Color::LightMagenta => vt100::Color::Idx(13),
        Color::LightCyan => vt100::Color::Idx(14),
        Color::White => vt100::Color::Idx(15),
        Color::Indexed(value) => vt100::Color::Idx(value),
        Color::Rgb(r, g, b) => vt100::Color::Rgb(r, g, b),
    }
}
