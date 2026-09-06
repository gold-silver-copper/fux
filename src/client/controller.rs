//! Viewer-local interaction modes: history/copy, tab and workspace choosers, rename, confirmed
//! closes, repeated resize and workspace naming. Transient state never leaves this process.

use super::copy::{CopyKey, CopyOutcome, CopySession};
use super::hints::HintPanel;
use crate::commands::Action;
use crate::ids::{PaneId, TabId};
use crate::proto::attach::{MouseEvent, ViewReply};
use crate::proto::control::{Request, TabAction, WorkspaceAction};
use crate::view::{Frame, MouseMode};
use unicode_segmentation::UnicodeSegmentation as _;

const MAX_TEXT_BYTES: usize = 128;
pub const RESIZE_STEP: i16 = 250;

enum Mode {
    Pane,
    Copy(Box<CopySession>),
    LoadingWorkspaces,
    Workspaces {
        names: Vec<String>,
        selected: usize,
    },
    Tabs {
        choices: Vec<(TabId, String)>,
        selected: usize,
    },
    Rename {
        tab: TabId,
        text: String,
    },
    NewWorkspace {
        text: String,
    },
    ClosePane {
        pane: PaneId,
    },
    CloseTab {
        tab: TabId,
        label: String,
        panes: usize,
    },
    Resize {
        pane: PaneId,
    },
}

pub struct Controller {
    mode: Mode,
    escape: Vec<u8>,
    utf8: Vec<u8>,
    paste: bool,
    back: bool,
    error: Option<String>,
    info: Option<String>,
    copied: Option<String>,
    workspaces_enabled: bool,
    loading_input: Vec<u8>,
}

/// What a mouse report should do.
pub enum MouseDisposition {
    /// Handled locally (history, selection); nothing goes to the server.
    Local,
    /// Forward to the server, which focuses or re-encodes for the application.
    Forward,
    /// Consumed and dropped (a mode that ignores the mouse).
    Ignore,
}

impl Controller {
    #[must_use]
    pub fn new(workspaces_enabled: bool) -> Self {
        Self {
            mode: Mode::Pane,
            escape: Vec::new(),
            utf8: Vec::new(),
            paste: false,
            back: false,
            error: None,
            info: None,
            copied: None,
            workspaces_enabled,
            loading_input: Vec::new(),
        }
    }

    pub fn active(&self) -> bool {
        !matches!(self.mode, Mode::Pane)
    }

    /// A cancelled mode still owns an unfinished paste or sequence; its tail must never be
    /// reinterpreted as commands or forwarded to a pane.
    pub fn owns_input(&self) -> bool {
        self.active() || self.paste || !self.escape.is_empty() || !self.utf8.is_empty()
    }

    pub fn in_copy(&self) -> bool {
        matches!(self.mode, Mode::Copy(_))
    }

    pub fn escape_pending(&self) -> bool {
        !self.escape.is_empty()
            && !self.paste
            && (self.escape.last() == Some(&27)
                || !(self.escape.starts_with(b"\x1b[") || self.escape.starts_with(b"\x1bO")))
    }

    pub fn take_back(&mut self) -> bool {
        std::mem::take(&mut self.back)
    }

    pub fn take_copied(&mut self) -> Option<String> {
        self.copied.take()
    }

    pub fn clear_error(&mut self) {
        self.error = None;
        self.info = None;
    }

    /// A transient confirmation shown as a thin bar until the next key.
    pub fn report_info(&mut self, message: impl Into<String>) {
        self.info = Some(
            message
                .into()
                .chars()
                .filter(|c| !c.is_control())
                .take(256)
                .collect(),
        );
    }

    pub fn report_error(&mut self, error: impl Into<String>) {
        self.error = Some(
            error
                .into()
                .chars()
                .filter(|c| !c.is_control())
                .take(256)
                .collect(),
        );
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The copy session's view and selection for the compositor.
    pub fn local_view(&self) -> Option<super::render::LocalView<'_>> {
        match &self.mode {
            Mode::Copy(copy) => Some(super::render::LocalView {
                pane: copy.pane(),
                view: copy.view(),
                cursor: copy.cursor(),
                anchor: copy.anchor(),
            }),
            _ => None,
        }
    }

    pub fn take_read(&mut self) -> Option<(u64, PaneId, u32)> {
        match &mut self.mode {
            Mode::Copy(copy) => copy.take_read(),
            _ => None,
        }
    }

    pub fn awaiting_read(&self) -> bool {
        matches!(&self.mode, Mode::Copy(copy) if copy.awaiting_read())
    }

    pub fn install_view(&mut self, reply: ViewReply) {
        if let Mode::Copy(copy) = &mut self.mode
            && !copy.install(reply)
        {
            self.cancel();
            self.report_error("That pane is no longer available for copying.");
        }
    }

    /// Reconciles the mode with a new frame: stale targets cancel with feedback, live copy views
    /// follow new output.
    pub fn reconcile(&mut self, frame: &Frame) {
        if let Some(message) = &frame.message {
            self.report_error(message.clone());
        }
        let stale = match &mut self.mode {
            Mode::Copy(copy) => match frame.pane(copy.pane()) {
                Some(live) => {
                    copy.refresh_live(live);
                    false
                }
                None => true,
            },
            Mode::ClosePane { pane } | Mode::Resize { pane } => frame.pane(*pane).is_none(),
            Mode::CloseTab { tab, .. } | Mode::Rename { tab, .. } => {
                !frame.tabs.iter().any(|entry| entry.id == *tab)
            }
            Mode::Tabs { .. }
            | Mode::Pane
            | Mode::LoadingWorkspaces
            | Mode::Workspaces { .. }
            | Mode::NewWorkspace { .. } => false,
        };
        if stale {
            self.cancel();
            self.report_error("The target of that command has closed.");
        }
    }

    pub fn loading_workspaces(&mut self) {
        self.loading_input.clear();
        self.mode = Mode::LoadingWorkspaces;
        self.error = None;
        self.escape.clear();
        self.utf8.clear();
    }

    pub fn workspaces_loaded(&mut self, result: anyhow::Result<Vec<String>>, current: &str) {
        if !matches!(self.mode, Mode::LoadingWorkspaces) {
            return;
        }
        match result {
            Ok(names) if !names.is_empty() => {
                self.escape.clear();
                self.utf8.clear();
                self.paste = false;
                let selected = names.iter().position(|name| name == current).unwrap_or(0);
                self.mode = Mode::Workspaces { names, selected };
            }
            result => {
                self.mode = Mode::Pane;
                self.loading_input.clear();
                self.report_error(result.err().map_or_else(
                    || "No workspaces are available".to_owned(),
                    |error| error.to_string(),
                ));
                self.back = true;
            }
        }
    }

    /// Bytes typed while the chooser was loading, replayed once it is ready.
    pub fn take_loading_input(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.loading_input)
    }

    /// Enters a mode for a modal action; returns false for actions that need no mode.
    pub fn enter(&mut self, action: Action, frame: &Frame) -> bool {
        self.escape.clear();
        self.utf8.clear();
        self.error = None;
        let tab = frame.active_tab;
        let pane = frame.focused;
        self.mode = match action {
            Action::CopyMode => match pane
                .and_then(|pane| frame.pane(pane).map(|view| (pane, view)))
            {
                Some((pane, view)) => Mode::Copy(Box::new(CopySession::new(pane, view.clone()))),
                None => return false,
            },
            Action::ChooseTab => Mode::Tabs {
                choices: frame
                    .tabs
                    .iter()
                    .map(|entry| (entry.id, entry.label.clone()))
                    .collect(),
                selected: frame
                    .tabs
                    .iter()
                    .position(|entry| Some(entry.id) == tab)
                    .unwrap_or(0),
            },
            Action::RenameTab => {
                match tab.and_then(|tab| frame.tabs.iter().find(|entry| entry.id == tab)) {
                    Some(entry) => Mode::Rename {
                        tab: entry.id,
                        text: entry.label.clone(),
                    },
                    None => return false,
                }
            }
            Action::ClosePane => match pane {
                Some(pane) => Mode::ClosePane { pane },
                None => return false,
            },
            Action::CloseTab => {
                match tab.and_then(|tab| frame.tabs.iter().find(|entry| entry.id == tab)) {
                    Some(entry) => Mode::CloseTab {
                        tab: entry.id,
                        label: entry.label.clone(),
                        panes: frame.layout.len(),
                    },
                    None => return false,
                }
            }
            Action::ResizeMode => match pane {
                Some(pane) => Mode::Resize { pane },
                None => return false,
            },
            Action::NewWorkspace => Mode::NewWorkspace {
                text: String::new(),
            },
            Action::ChooseWorkspace => {
                self.loading_workspaces();
                return true;
            }
            _ => return false,
        };
        true
    }

    pub fn resolve_escape(&mut self) {
        if !self.escape_pending() {
            return;
        }
        if self.escape == [27] || self.escape.last() == Some(&27) {
            let cleared = match &mut self.mode {
                Mode::Copy(copy) => matches!(copy.key(CopyKey::Escape), CopyOutcome::Continue),
                _ => false,
            };
            if !cleared {
                self.cancel();
            }
        }
        self.escape.clear();
    }

    fn cancel(&mut self) {
        self.mode = Mode::Pane;
        self.back = true;
        self.utf8.clear();
        self.error = None;
    }

    /// Routes a mouse report while a mode is active or the pointer is over pane content.
    pub fn mouse(&mut self, mouse: MouseEvent, frame: &Frame) -> MouseDisposition {
        if !matches!(self.mode, Mode::Pane | Mode::Copy(_)) {
            return MouseDisposition::Ignore;
        }
        let x = mouse.column.saturating_sub(1);
        let y = mouse.row.saturating_sub(1);
        let Some(entry) = frame.pane_at(x, y) else {
            return if self.in_copy() {
                MouseDisposition::Ignore
            } else {
                MouseDisposition::Forward
            };
        };
        let content = entry.rect.inner();
        let Some(view) = frame.pane(entry.pane) else {
            return MouseDisposition::Ignore;
        };
        let app_owns_mouse = view.modes.mouse_mode != MouseMode::None;
        // Shift is the documented override: fux history/selection even when the app owns the mouse.
        let local = self.in_copy()
            || mouse.shift()
            || (!app_owns_mouse && (mouse.wheel() || mouse.motion() && mouse.button() == 0));
        if !local {
            return MouseDisposition::Forward;
        }
        if !content.contains(x, y) {
            return MouseDisposition::Ignore;
        }
        let row = y.saturating_sub(content.y);
        let column = x.saturating_sub(content.x);
        if matches!(self.mode, Mode::Pane) {
            if mouse.release {
                return MouseDisposition::Ignore;
            }
            self.mode = Mode::Copy(Box::new(CopySession::new(entry.pane, view.clone())));
        }
        if let Mode::Copy(copy) = &mut self.mode {
            if copy.pane() != entry.pane {
                return MouseDisposition::Ignore;
            }
            if mouse.wheel() {
                copy.scroll(if mouse.code & 1 == 0 { 3 } else { -3 });
            } else if mouse.button() == 0 {
                copy.drag(row, column, mouse.release);
            }
        }
        MouseDisposition::Local
    }

    /// Feeds one input byte to the active mode. Returns a request to send, if any.
    pub fn feed(&mut self, byte: u8, frame: &Frame) -> Option<Request> {
        if matches!(self.mode, Mode::LoadingWorkspaces) {
            if self.loading_input.len() < 4096 {
                self.loading_input.push(byte);
            } else {
                self.cancel();
                self.loading_input.clear();
            }
            if matches!(self.mode, Mode::LoadingWorkspaces) && byte != 27 && self.escape.is_empty()
            {
                return None;
            }
        }
        if !self.escape.is_empty() || byte == 27 {
            self.escape.push(byte);
            let complete = self.escape.len() > 1
                && match self.escape.get(1) {
                    Some(b'[' | b'O') => self.escape.len() > 2 && (0x40..=0x7e).contains(&byte),
                    _ => true,
                };
            if !complete && self.escape.len() < 64 {
                return None;
            }
            let sequence = std::mem::take(&mut self.escape);
            if self.active()
                && !self.paste
                && let Some(mouse) = MouseEvent::parse(&sequence)
            {
                let _ = self.mouse(mouse, frame);
                return None;
            }
            match sequence.as_slice() {
                b"\x1b[200~" => self.paste = true,
                b"\x1b[201~" => self.paste = false,
                b"\x1bOM" if !self.paste => return self.key('\r', frame),
                b"\x1b[D" | b"\x1bOD" if !self.paste && self.in_copy() => {
                    return self.key('h', frame);
                }
                b"\x1b[C" | b"\x1bOC" if !self.paste && self.in_copy() => {
                    return self.key('l', frame);
                }
                b"\x1b[5~" if !self.paste && self.in_copy() => {
                    return self.copy_key(CopyKey::PageUp);
                }
                b"\x1b[6~" if !self.paste && self.in_copy() => {
                    return self.copy_key(CopyKey::PageDown);
                }
                b"\x1b[A" | b"\x1b[D" | b"\x1bOA" | b"\x1bOD"
                    if !self.paste && !self.text_entry() =>
                {
                    return self.key('k', frame);
                }
                b"\x1b[B" | b"\x1b[C" | b"\x1bOB" | b"\x1bOC"
                    if !self.paste && !self.text_entry() =>
                {
                    return self.key('j', frame);
                }
                _ => {}
            }
            return None;
        }
        if self.paste && !self.text_entry() {
            return None;
        }
        if byte.is_ascii() {
            self.utf8.clear();
            if self.paste && byte.is_ascii_control() {
                return None;
            }
            return self.key(char::from(byte), frame);
        }
        self.utf8.push(byte);
        match std::str::from_utf8(&self.utf8) {
            Ok(text) => {
                let character = text.chars().next();
                self.utf8.clear();
                character.and_then(|character| self.key(character, frame))
            }
            Err(error) if error.error_len().is_some() || self.utf8.len() >= 4 => {
                self.utf8.clear();
                None
            }
            Err(_) => None,
        }
    }

    fn text_entry(&self) -> bool {
        matches!(self.mode, Mode::Rename { .. } | Mode::NewWorkspace { .. })
    }

    fn copy_key(&mut self, key: CopyKey) -> Option<Request> {
        if let Mode::Copy(copy) = &mut self.mode {
            match copy.key(key) {
                CopyOutcome::Continue => {}
                CopyOutcome::Copied(text) => {
                    self.copied = Some(text);
                    self.mode = Mode::Pane;
                }
                CopyOutcome::Finished => {
                    self.mode = Mode::Pane;
                }
            }
        }
        None
    }

    fn key(&mut self, key: char, frame: &Frame) -> Option<Request> {
        match &mut self.mode {
            Mode::Copy(_) => {
                let mapped = match key {
                    'h' => CopyKey::Left,
                    'l' => CopyKey::Right,
                    'k' => CopyKey::Up,
                    'j' => CopyKey::Down,
                    'u' => return self.scroll_copy(3),
                    'd' => return self.scroll_copy(-3),
                    ' ' => CopyKey::Anchor,
                    'y' | '\r' | '\n' => CopyKey::Copy,
                    'g' => CopyKey::Live,
                    'q' => CopyKey::Quit,
                    _ => return None,
                };
                self.copy_key(mapped)
            }
            Mode::Pane | Mode::LoadingWorkspaces => None,
            Mode::Workspaces { names, selected } => {
                if self.paste {
                    return None;
                }
                match key {
                    'j' => {
                        *selected = selected
                            .saturating_add(1)
                            .min(names.len().saturating_sub(1))
                    }
                    'k' => *selected = selected.saturating_sub(1),
                    '\r' | '\n' => {
                        let name = names.get(*selected).cloned()?;
                        self.mode = Mode::Pane;
                        if name == frame.workspace {
                            return None;
                        }
                        return Some(Request::Workspace {
                            id: 0,
                            action: WorkspaceAction::Select { name },
                        });
                    }
                    _ => {}
                }
                None
            }
            Mode::Tabs { choices, selected } => {
                match key {
                    'j' => {
                        *selected = selected
                            .saturating_add(1)
                            .min(choices.len().saturating_sub(1))
                    }
                    'k' => *selected = selected.saturating_sub(1),
                    '\r' | '\n' => {
                        let target = choices.get(*selected)?.0;
                        if !frame.tabs.iter().any(|entry| entry.id == target) {
                            self.error =
                                Some("That tab no longer exists; Esc returns to commands.".into());
                            return None;
                        }
                        self.mode = Mode::Pane;
                        return Some(Request::Tab {
                            id: 0,
                            action: TabAction::SelectId { tab: target },
                        });
                    }
                    _ => {}
                }
                None
            }
            Mode::Rename { tab, text } => match key {
                '\r' | '\n' => {
                    let action = TabAction::Rename {
                        tab: *tab,
                        name: text.clone(),
                    };
                    self.mode = Mode::Pane;
                    Some(Request::Tab { id: 0, action })
                }
                _ => {
                    edit_text(text, key);
                    None
                }
            },
            Mode::NewWorkspace { text } => match key {
                '\r' | '\n' => {
                    let name = text.trim().to_owned();
                    if !name.is_empty() && crate::ids::validate_workspace_name(&name).is_err() {
                        self.error = Some(crate::ids::InvalidName.to_string());
                        return None;
                    }
                    self.mode = Mode::Pane;
                    Some(Request::Workspace {
                        id: 0,
                        action: WorkspaceAction::New {
                            name: (!name.is_empty()).then_some(name),
                        },
                    })
                }
                _ => {
                    edit_text(text, key);
                    None
                }
            },
            Mode::ClosePane { pane } => {
                if self.paste {
                    return None;
                }
                match key {
                    'y' | 'Y' => {
                        let pane = *pane;
                        self.mode = Mode::Pane;
                        Some(Request::Kill { id: 0, pane })
                    }
                    'n' | 'N' => {
                        self.cancel();
                        None
                    }
                    _ => None,
                }
            }
            Mode::CloseTab { tab, .. } => {
                if self.paste {
                    return None;
                }
                match key {
                    'y' | 'Y' => {
                        let tab = *tab;
                        self.mode = Mode::Pane;
                        Some(Request::Tab {
                            id: 0,
                            action: TabAction::Close { tab },
                        })
                    }
                    'n' | 'N' => {
                        self.cancel();
                        None
                    }
                    _ => None,
                }
            }
            Mode::Resize { pane } => {
                if self.paste {
                    return None;
                }
                let delta = match key {
                    'j' | 'l' | '+' => RESIZE_STEP,
                    'k' | 'h' | '-' => -RESIZE_STEP,
                    '\r' | '\n' => {
                        self.mode = Mode::Pane;
                        return None;
                    }
                    _ => return None,
                };
                Some(Request::Resize {
                    id: 0,
                    pane: *pane,
                    delta,
                })
            }
        }
    }

    fn scroll_copy(&mut self, delta: i64) -> Option<Request> {
        if let Mode::Copy(copy) = &mut self.mode {
            copy.scroll(delta);
        }
        None
    }

    /// The panel this mode wants painted, if any.
    pub fn panel(&self) -> Option<HintPanel> {
        let (title, entries, footer, focus) = match &self.mode {
            Mode::Pane => {
                if let Some(error) = &self.error {
                    return Some(HintPanel::context(
                        "Command failed".into(),
                        vec![error.clone()],
                        "Esc dismiss · prefix ? for commands",
                        None,
                    ));
                }
                return self
                    .info
                    .as_ref()
                    .map(|info| HintPanel::bar(&format!("{info} · any key dismisses")));
            }
            Mode::Copy(copy) => return Some(HintPanel::bar(&copy.hint())),
            Mode::LoadingWorkspaces => (
                "Choose workspace".into(),
                vec!["Loading workspaces…".into()],
                "Esc back",
                None,
            ),
            Mode::Workspaces { names, selected } => (
                "Choose workspace".into(),
                names.clone(),
                "↑/↓ or j/k move · Enter switch · Esc back",
                Some(*selected),
            ),
            Mode::Tabs { choices, selected } => (
                "Choose tab".into(),
                choices.iter().map(|(_, name)| name.clone()).collect(),
                "↑/↓ or j/k move · Enter select · Esc back",
                Some(*selected),
            ),
            Mode::Rename { text, .. } => {
                return Some(HintPanel::text_input(
                    "Rename tab",
                    text,
                    "Enter save · Esc back · Ctrl-U clear · Backspace delete",
                ));
            }
            Mode::NewWorkspace { text } => {
                return Some(HintPanel::text_input(
                    "New workspace (empty = automatic name)",
                    text,
                    "Enter create · Esc back · Ctrl-U clear",
                ));
            }
            Mode::ClosePane { pane } => (
                format!("Close pane {pane}?"),
                vec!["Its process and unsaved work will be terminated.".into()],
                "y close · n/Esc back",
                None,
            ),
            Mode::CloseTab { tab, label, panes } => (
                format!("Close tab {label} ({tab})?"),
                vec![format!(
                    "{panes} pane{} and their processes will be terminated.",
                    if *panes == 1 { "" } else { "s" }
                )],
                "y close · n/Esc back",
                None,
            ),
            Mode::Resize { pane } => {
                return Some(HintPanel::bar(&format!(
                    "Resize {pane} · ←/↑ shrink →/↓ grow · Enter finish · Esc back · changes are kept{}",
                    self.error
                        .as_ref()
                        .map_or(String::new(), |error| format!(" · {error}")),
                )));
            }
        };
        let mut entries: Vec<String> = entries;
        if let Some(error) = &self.error {
            entries.push(error.clone());
        }
        Some(HintPanel::context(title, entries, footer, focus))
    }

    pub fn workspaces_enabled(&self) -> bool {
        self.workspaces_enabled
    }
}

fn edit_text(text: &mut String, key: char) {
    match key {
        '\u{7f}' | '\u{8}' => {
            if let Some((index, _)) = text.grapheme_indices(true).next_back() {
                text.truncate(index);
            }
        }
        '\u{15}' => text.clear(),
        character
            if !character.is_control() && text.len() + character.len_utf8() <= MAX_TEXT_BYTES =>
        {
            text.push(character);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{PaneRect, PaneView, TabEntry};

    fn frame() -> Frame {
        let mut parser = vt100::Parser::new(4, 10, 0);
        parser.process(b"hello");
        let view = PaneView::from_screen(parser.screen(), "", 0, None).unwrap_or_default();
        let mut frame = Frame {
            workspace: "default".into(),
            ..Frame::default()
        };
        frame.tabs.push(TabEntry {
            id: TabId(1),
            label: "main".into(),
        });
        frame.active_tab = Some(TabId(1));
        frame.focused = Some(PaneId(1));
        frame.layout.push(PaneRect {
            pane: PaneId(1),
            rect: crate::layout::Rect {
                x: 0,
                y: 0,
                width: 12,
                height: 6,
            },
        });
        frame.panes.insert(PaneId(1), view);
        frame
    }

    fn feed(controller: &mut Controller, bytes: &[u8], frame: &Frame) -> Vec<Request> {
        bytes
            .iter()
            .filter_map(|byte| controller.feed(*byte, frame))
            .collect()
    }

    #[test]
    fn rename_submits_fragmented_unicode_and_cancels_without_mutation() {
        let frame = frame();
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::RenameTab, &frame));
        let requests = feed(&mut controller, "\u{15}renamed界\r".as_bytes(), &frame);
        assert_eq!(
            requests,
            vec![Request::Tab {
                id: 0,
                action: TabAction::Rename {
                    tab: TabId(1),
                    name: "renamed界".into()
                }
            }]
        );
        assert!(!controller.active());
        assert!(controller.enter(Action::RenameTab, &frame));
        assert!(feed(&mut controller, b"discard\x1b", &frame).is_empty());
        controller.resolve_escape();
        assert!(!controller.active() && controller.take_back());
    }

    #[test]
    fn confirmations_carry_the_original_target_and_ignore_paste() {
        let frame = frame();
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::ClosePane, &frame));
        assert!(feed(&mut controller, b"\x1b[200~y\r\x1b[201~", &frame).is_empty());
        assert!(controller.active(), "pasted confirmation ignored");
        assert_eq!(
            feed(&mut controller, b"y", &frame),
            vec![Request::Kill {
                id: 0,
                pane: PaneId(1)
            }]
        );
        assert!(controller.enter(Action::CloseTab, &frame));
        assert_eq!(
            feed(&mut controller, b"Y", &frame),
            vec![Request::Tab {
                id: 0,
                action: TabAction::Close { tab: TabId(1) }
            }]
        );
        // A stale target cancels with feedback when the frame no longer has it.
        assert!(controller.enter(Action::ClosePane, &frame));
        controller.reconcile(&Frame::default());
        assert!(!controller.active());
        assert!(controller.error().is_some());
    }

    #[test]
    fn resize_repeats_with_arrows_and_application_cursor_keys() {
        let frame = frame();
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::ResizeMode, &frame));
        let requests = feed(&mut controller, b"j\x1b[A\x1bOC\x1bOD\r", &frame);
        let deltas: Vec<i16> = requests
            .iter()
            .filter_map(|request| match request {
                Request::Resize { delta, .. } => Some(*delta),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec![250, -250, 250, -250]);
        assert!(!controller.active());
    }

    #[test]
    fn workspace_chooser_replays_buffered_input_and_switches() {
        let frame = frame();
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::ChooseWorkspace, &frame));
        assert!(feed(&mut controller, b"j\r", &frame).is_empty());
        controller.workspaces_loaded(Ok(vec!["default".into(), "other".into()]), "default");
        let replay = controller.take_loading_input();
        assert_eq!(
            feed(&mut controller, &replay, &frame),
            vec![Request::Workspace {
                id: 0,
                action: WorkspaceAction::Select {
                    name: "other".into()
                }
            }]
        );
        let mut controller = Controller::new(true);
        controller.enter(Action::ChooseWorkspace, &frame);
        controller.workspaces_loaded(Err(anyhow::anyhow!("lookup failed")), "default");
        assert!(!controller.active() && controller.take_back());
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::NewWorkspace, &frame));
        assert_eq!(
            feed(&mut controller, b"proj\r", &frame),
            vec![Request::Workspace {
                id: 0,
                action: WorkspaceAction::New {
                    name: Some("proj".into())
                }
            }]
        );
    }

    #[test]
    fn copy_mode_selection_and_mouse_routing() {
        let frame = frame();
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::CopyMode, &frame));
        assert!(feed(&mut controller, b"\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D ", &frame).is_empty());
        feed(&mut controller, b"llll", &frame);
        feed(&mut controller, b"y", &frame);
        assert_eq!(controller.take_copied(), Some("hello".into()));
        assert!(!controller.active());
        // A wheel over a pane whose application does not own the mouse browses locally.
        let wheel = MouseEvent {
            code: 64,
            column: 3,
            row: 3,
            release: false,
        };
        assert!(matches!(
            controller.mouse(wheel, &frame),
            MouseDisposition::Local
        ));
        assert!(controller.in_copy());
        assert_eq!(controller.take_read().map(|(_, _, offset)| offset), Some(3));
        // A click without shift on the pane is the server's business.
        let mut plain = Controller::new(true);
        let click = MouseEvent {
            code: 0,
            column: 3,
            row: 3,
            release: false,
        };
        assert!(matches!(
            plain.mouse(click, &frame),
            MouseDisposition::Forward
        ));
        // Shift-drag selects locally even when the application owns the mouse.
        let mut owned = frame.clone();
        if let Some(view) = owned.panes.get_mut(&PaneId(1)) {
            view.modes.mouse_mode = MouseMode::AnyMotion;
        }
        let mut shift = Controller::new(true);
        let press = MouseEvent {
            code: 4,
            column: 2,
            row: 2,
            release: false,
        };
        assert!(matches!(
            shift.mouse(press, &owned),
            MouseDisposition::Local
        ));
        let drag = MouseEvent {
            code: 36,
            column: 6,
            row: 2,
            release: false,
        };
        assert!(matches!(shift.mouse(drag, &owned), MouseDisposition::Local));
        let release = MouseEvent {
            code: 4,
            column: 6,
            row: 2,
            release: true,
        };
        assert!(matches!(
            shift.mouse(release, &owned),
            MouseDisposition::Local
        ));
        feed(&mut shift, b"y", &owned);
        assert_eq!(shift.take_copied(), Some("hello".into()));
        assert!(matches!(
            Controller::new(true).mouse(wheel, &owned),
            MouseDisposition::Forward
        ));
    }

    #[test]
    fn canceled_modes_keep_owning_unfinished_pastes() {
        let frame = frame();
        let mut controller = Controller::new(true);
        controller.enter(Action::CopyMode, &frame);
        for byte in b"\x1b[200~ab" {
            assert!(controller.feed(*byte, &frame).is_none());
        }
        controller.reconcile(&Frame::default());
        assert!(!controller.active());
        assert!(
            controller.owns_input(),
            "the paste tail still belongs to the controller"
        );
        for byte in b"t\x01xy\x1b[201~" {
            assert!(controller.feed(*byte, &frame).is_none());
        }
        assert!(!controller.owns_input());
    }
}
