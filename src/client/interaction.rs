//! Transient interaction state belongs to one viewer, never to the workspace snapshot.
use super::hints::HintPanel;
use crate::commands::BuiltinAction;
use crate::control::{Request, TabAction};
use crate::state::WorkspaceState;
use unicode_segmentation::UnicodeSegmentation as _;

#[derive(Default)]
enum Mode {
    #[default]
    Pane,
    Copy(Box<super::copy::CopySession>),
    LoadingWorkspaces,
    Workspaces {
        names: Vec<String>,
        selected: usize,
    },
    Tabs {
        choices: Vec<(u32, String)>,
        selected: usize,
    },
    Rename {
        tab: u32,
        text: String,
    },
    Close {
        pane: u32,
    },
    Resize {
        pane: u32,
    },
}

#[derive(Default)]
pub struct Interaction {
    mode: Mode,
    escape: Vec<u8>,
    utf8: Vec<u8>,
    paste: bool,
    back: bool,
    error: Option<String>,
    workspace: Option<String>,
    loading_input: Vec<u8>,
    copy_clipboard: Option<(u64, String)>,
    mouse_layout: super::copy::MouseLayout,
}

impl Interaction {
    pub fn set_mouse_layout(&mut self, layout: super::copy::MouseLayout) {
        self.mouse_layout = layout;
    }
    pub fn mouse(&mut self, mouse: crate::host::MouseEvent, state: &WorkspaceState) -> bool {
        if !matches!(self.mode, Mode::Pane | Mode::Copy(_)) {
            return true;
        }
        let x = mouse.column.saturating_sub(1);
        let y = mouse.row.saturating_sub(1);
        let target = self
            .mouse_layout
            .iter()
            .find(|(_, rect)| {
                x > rect.x
                    && x < rect.right().saturating_sub(1)
                    && y > rect.y
                    && y < rect.bottom().saturating_sub(1)
            })
            .map(|(pane, rect)| (*pane, y - rect.y - 1, x - rect.x - 1));
        let Some((pane, row, column)) = target else {
            return mouse.shift()
                || matches!(self.mode, Mode::Copy(_))
                || (mouse.wheel()
                    && self.mouse_layout.iter().any(|(pane, rect)| {
                        rect.contains((x, y).into())
                            && state.pane(*pane).is_some_and(|pane| {
                                pane.modes.mouse_mode == crate::state::MouseMode::None
                            })
                    }));
        };
        let Some(view) = state.pane(pane) else {
            return true;
        };
        let owned = matches!(self.mode, Mode::Copy(_))
            || mouse.shift()
            || (mouse.wheel() && view.modes.mouse_mode == crate::state::MouseMode::None);
        if !owned {
            return false;
        }
        if matches!(self.mode, Mode::Pane) {
            if !mouse.wheel() && (mouse.code & 3 != 0 || mouse.release) {
                return true;
            }
            self.mode = super::copy::CopySession::new(pane, view.clone())
                .map_or(Mode::Pane, |copy| Mode::Copy(Box::new(copy)));
        }
        if let Mode::Copy(copy) = &mut self.mode
            && copy.pane() == pane
        {
            if mouse.wheel() {
                copy.key(if mouse.code & 1 == 0 { 'u' } else { 'd' });
            } else if mouse.code & 3 == 0 {
                copy.mouse(row, column, mouse.release);
            }
        }
        true
    }
    pub fn copy_ui(&self) -> super::copy::CopyUi {
        super::copy::CopyUi {
            view: match &self.mode {
                Mode::Copy(copy) => copy.view(),
                _ => None,
            },
            clipboard: self.copy_clipboard.clone(),
        }
    }
    pub fn reconcile_copy(&mut self, state: &WorkspaceState) {
        let removed = match &mut self.mode {
            Mode::Copy(copy) => !copy.reconcile(state),
            _ => false,
        };
        if removed {
            self.cancel();
            self.report_error("The pane being copied has closed.".into());
        }
    }
    pub fn take_copy_read(&mut self) -> Option<(u32, u32)> {
        match &mut self.mode {
            Mode::Copy(copy) => copy.take_read(),
            _ => None,
        }
    }
    pub fn install_copy_view(&mut self, reply: crate::local::CopyViewReply) {
        if let Mode::Copy(copy) = &mut self.mode {
            let valid =
                copy.pane().0 == reply.pane && reply.view.is_some_and(|view| copy.install(*view));
            if !valid {
                self.cancel();
                self.report_error("That pane is no longer available for copying.".into());
            }
        }
    }
    pub fn active(&self) -> bool {
        !matches!(self.mode, Mode::Pane)
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
    pub fn clear_error(&mut self) {
        self.error = None;
    }

    pub fn report_error(&mut self, error: String) {
        self.error = Some(
            error
                .chars()
                .filter(|c| !c.is_control())
                .take(512)
                .collect(),
        );
    }

    pub fn loading_workspaces(&mut self) {
        self.loading_input.clear();
        self.mode = Mode::LoadingWorkspaces;
        self.error = None;
        self.escape.clear();
        self.utf8.clear();
    }
    pub fn workspaces_loaded(&mut self, result: anyhow::Result<Vec<String>>) {
        if !matches!(self.mode, Mode::LoadingWorkspaces) {
            return;
        }
        match result {
            Ok(names) if !names.is_empty() => {
                // Buffered loading input is replayed from its first byte. Reset the
                // decoder so a partial escape or paste marker is not applied twice.
                self.escape.clear();
                self.utf8.clear();
                self.paste = false;
                self.mode = Mode::Workspaces { names, selected: 0 };
            }
            result => {
                self.mode = Mode::Pane;
                self.report_error(result.err().map_or_else(
                    || "No workspaces are available".into(),
                    |error| error.to_string(),
                ));
                self.back = true;
            }
        }
    }
    pub fn take_loading_input(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.loading_input)
    }

    pub fn take_workspace(&mut self) -> Option<String> {
        self.workspace.take()
    }

    pub fn enter(&mut self, action: BuiltinAction, state: &WorkspaceState) {
        self.escape.clear();
        self.utf8.clear();
        self.error = None;
        let tab = state
            .tabs()
            .iter()
            .find(|tab| Some(tab.id) == state.active_tab());
        let pane = state
            .popups()
            .iter()
            .max_by_key(|popup| popup.z_index)
            .map(|popup| popup.pane)
            .or_else(|| tab.map(|tab| tab.focused));
        self.mode = match action {
            BuiltinAction::CopyMode => pane
                .and_then(|pane| super::copy::CopySession::new(pane, state.pane(pane)?.clone()))
                .map_or(Mode::Pane, |copy| Mode::Copy(Box::new(copy))),
            BuiltinAction::TabPicker => Mode::Tabs {
                choices: state
                    .tabs()
                    .iter()
                    .map(|tab| (tab.id.0, tab.name.clone()))
                    .collect(),
                selected: state
                    .tabs()
                    .iter()
                    .position(|tab| Some(tab.id) == state.active_tab())
                    .unwrap_or(0),
            },
            BuiltinAction::RenameTab => tab.map_or(Mode::Pane, |tab| Mode::Rename {
                tab: tab.id.0,
                text: tab.name.clone(),
            }),
            BuiltinAction::ClosePane => {
                pane.map_or(Mode::Pane, |pane| Mode::Close { pane: pane.0 })
            }
            BuiltinAction::ResizeMode => {
                pane.map_or(Mode::Pane, |pane| Mode::Resize { pane: pane.0 })
            }
            _ => Mode::Pane,
        };
    }

    pub fn resolve_escape(&mut self) {
        if !self.escape_pending() {
            return;
        }
        if self.escape == [27] || self.escape.last() == Some(&27) {
            let cleared = match &mut self.mode {
                Mode::Copy(copy) => copy.escape(),
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

    pub fn feed(&mut self, byte: u8, state: &WorkspaceState) -> Option<Request> {
        if matches!(self.mode, Mode::LoadingWorkspaces) {
            if self.loading_input.len() < 4096 {
                self.loading_input.push(byte);
            } else {
                self.cancel();
                self.loading_input.clear();
                return None;
            }
            if byte != 27 && self.escape.is_empty() {
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
            if !self.paste
                && let Some(mouse) = crate::host::MouseEvent::parse(&sequence)
            {
                self.mouse(mouse, state);
                return None;
            }
            match sequence.as_slice() {
                b"\x1b[200~" => self.paste = true,
                b"\x1b[201~" => self.paste = false,
                b"\x1b[D" if !self.paste && matches!(self.mode, Mode::Copy(_)) => {
                    return self.key('h', state);
                }
                b"\x1b[C" if !self.paste && matches!(self.mode, Mode::Copy(_)) => {
                    return self.key('l', state);
                }
                b"\x1b[A" | b"\x1b[D"
                    if !self.paste && !matches!(self.mode, Mode::Rename { .. }) =>
                {
                    return self.key('k', state);
                }
                b"\x1b[B" | b"\x1b[C"
                    if !self.paste && !matches!(self.mode, Mode::Rename { .. }) =>
                {
                    return self.key('j', state);
                }
                _ => {}
            }
            return None;
        }
        if self.paste && !matches!(self.mode, Mode::Rename { .. }) {
            return None;
        }
        if byte.is_ascii() {
            self.utf8.clear();
            if self.paste && (byte.is_ascii_control()) {
                return None;
            }
            return self.key(char::from(byte), state);
        }
        self.utf8.push(byte);
        match std::str::from_utf8(&self.utf8) {
            Ok(text) => {
                let character = text.chars().next();
                self.utf8.clear();
                character.and_then(|character| self.key(character, state))
            }
            Err(error) if error.error_len().is_some() || self.utf8.len() >= 4 => {
                self.utf8.clear();
                None
            }
            Err(_) => None,
        }
    }

    fn key(&mut self, key: char, state: &WorkspaceState) -> Option<Request> {
        match &mut self.mode {
            Mode::Copy(copy) => {
                copy.key(key);
                if !copy.active() {
                    if !copy.clipboard().is_empty() {
                        let sequence = self
                            .copy_clipboard
                            .as_ref()
                            .map_or(1, |(id, _)| id.saturating_add(1));
                        self.copy_clipboard = Some((sequence, copy.clipboard().to_owned()));
                    }
                    self.mode = Mode::Pane;
                }
                None
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
                        self.workspace = names.get(*selected).cloned();
                        self.mode = Mode::Pane;
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
                        if !state.tabs().iter().any(|tab| tab.id.0 == target) {
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
            Mode::Rename { tab, text } => {
                match key {
                    '\r' | '\n' => {
                        let action = TabAction::Rename {
                            tab: *tab,
                            name: text.clone(),
                        };
                        self.mode = Mode::Pane;
                        return Some(Request::Tab { id: 0, action });
                    }
                    '\u{7f}' | '\u{8}' => {
                        if let Some((index, _)) = text.grapheme_indices(true).next_back() {
                            text.truncate(index);
                        }
                    }
                    '\u{15}' => text.clear(),
                    character
                        if !character.is_control() && text.len() + character.len_utf8() <= 128 =>
                    {
                        text.push(character)
                    }
                    _ => {}
                }
                None
            }
            Mode::Close { pane } => {
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
            Mode::Resize { pane } => {
                if self.paste {
                    return None;
                }
                let delta = match key {
                    'j' | 'l' | '+' => 250,
                    'k' | 'h' | '-' => -250,
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

    pub fn panel(&self) -> Option<HintPanel> {
        let (title, entries, footer, focus) = match &self.mode {
            Mode::Pane => {
                return self.error.as_ref().map(|error| {
                    HintPanel::context(
                        "Command failed".into(),
                        vec![error.clone()],
                        "Prefix ? for commands",
                        None,
                    )
                });
            }
            Mode::Copy(copy) => {
                if let Some(error) = copy.error() {
                    return Some(HintPanel::bar(error));
                }
                return Some(HintPanel::bar(if copy.selecting() {
                    "Copy selection · arrows/hjkl move · y/Enter copy · Esc clear · u/d scroll clears selection"
                } else {
                    "Copy · arrows/hjkl move · Space select · u/d scroll · q finish · Esc back"
                }));
            }
            Mode::LoadingWorkspaces => (
                "Choose workspace".into(),
                vec!["Loading workspaces…".into()],
                "Esc back",
                None,
            ),
            Mode::Workspaces { names, selected } => (
                "Choose workspace".into(),
                names
                    .iter()
                    .enumerate()
                    .map(|(index, name)| {
                        format!("{} {name}", if index == *selected { '>' } else { ' ' })
                    })
                    .collect(),
                "↑/↓ or j/k move · Enter select · Esc back",
                Some(*selected),
            ),
            Mode::Tabs { choices, selected } => (
                "Choose tab".into(),
                choices
                    .iter()
                    .enumerate()
                    .map(|(index, (_, name))| {
                        format!("{} {name}", if index == *selected { '>' } else { ' ' })
                    })
                    .collect(),
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
            Mode::Close { pane } => (
                format!("Close pane {pane}?"),
                vec!["Its process and unsaved work will be terminated.".into()],
                "y close · n/Esc back",
                None,
            ),
            Mode::Resize { pane } => {
                return Some(HintPanel::bar(&format!(
                    "Resize {pane} · ←/↑ shrink →/↓ grow · Enter finish · Esc back · changes kept{}",
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
}
