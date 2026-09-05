use crate::state::{CellKind, PaneId, PaneView, WorkspaceState};
use base64::Engine as _;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CopyPoint {
    pub row: u16,
    pub column: u16,
}

/// Shared-v1 copy state. The pane viewport lives in `WorkspaceState`, so every viewer observes it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CopyMode {
    active: bool,
    cursor: CopyPoint,
    anchor: Option<CopyPoint>,
    dragging: bool,
    target: Option<PaneId>,
    pending: Vec<u8>,
}

impl CopyMode {
    #[must_use]
    pub const fn active(&self) -> bool {
        self.active
    }

    pub fn enter(&mut self, pane: &PaneView) {
        self.active = true;
        self.cursor = CopyPoint {
            row: pane.cursor.row.min(pane.rows.saturating_sub(1)),
            column: pane.cursor.column.min(pane.columns.saturating_sub(1)),
        };
        self.anchor = None;
        self.target = None;
        self.pending.clear();
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
        self.dragging = false;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn reset_synced(&mut self, state: &mut WorkspaceState) {
        let target = self.target;
        self.reset();
        if let Some(target) = target {
            self.sync(state, target);
        }
    }

    pub fn bind_target(&mut self, pane: PaneId) {
        self.target = Some(pane);
    }

    #[must_use]
    pub fn targets(&self, pane: PaneId) -> bool {
        self.target == Some(pane)
    }

    pub fn sync(&self, state: &mut WorkspaceState, pane_id: PaneId) {
        let _ = state.update_pane(pane_id, |pane| {
            pane.copy.active = self.active;
            pane.copy.cursor_row = self.cursor.row.min(pane.rows.saturating_sub(1));
            pane.copy.cursor_column = self.cursor.column.min(pane.columns.saturating_sub(1));
            pane.copy.anchor = self.anchor.map(|point| {
                (
                    point.row.min(pane.rows.saturating_sub(1)),
                    point.column.min(pane.columns.saturating_sub(1)),
                )
            });
        });
    }

    /// Handles copy-mode keys. Returns true when the bytes were consumed locally.
    pub fn key(&mut self, input: &[u8], state: &mut WorkspaceState, pane_id: PaneId) -> bool {
        self.key_with_remainder(input, state, pane_id).0
    }

    /// Handles copy-mode keys and returns bytes following a command that exits the mode.
    pub fn key_with_remainder(
        &mut self,
        input: &[u8],
        state: &mut WorkspaceState,
        pane_id: PaneId,
    ) -> (bool, Vec<u8>) {
        if !self.active {
            return (false, input.to_vec());
        }
        let Some(pane) = state.pane(pane_id).cloned() else {
            self.reset();
            return (true, Vec::new());
        };
        self.target = Some(pane_id);
        self.pending.extend_from_slice(input);
        let mut offset = 0;
        while offset < self.pending.len() && self.active {
            let Some(&byte) = self.pending.get(offset) else {
                break;
            };
            let (command, used) = if byte == 0x1b {
                if self.pending.len() - offset < 2 {
                    break;
                }
                if self.pending.get(offset + 1) != Some(&b'[') {
                    (b'q', 1)
                } else if self.pending.len() - offset < 3 {
                    break;
                } else {
                    let Some(&command) = self.pending.get(offset + 2) else {
                        break;
                    };
                    (
                        match command {
                            b'A' => 0x11,
                            b'B' => 0x12,
                            b'C' => 0x13,
                            b'D' => 0x14,
                            _ => 0,
                        },
                        3,
                    )
                }
            } else {
                (byte, 1)
            };
            offset += used;
            match command {
                b'q' => {
                    self.active = false;
                    self.anchor = None;
                }
                b' ' => self.anchor = Some(self.cursor),
                b'h' | 0x14 => self.cursor.column = self.cursor.column.saturating_sub(1),
                b'l' | 0x13 => {
                    self.cursor.column = self
                        .cursor
                        .column
                        .saturating_add(1)
                        .min(pane.columns.saturating_sub(1))
                }
                b'k' | 0x11 => self.cursor.row = self.cursor.row.saturating_sub(1),
                b'j' | 0x12 => {
                    self.cursor.row = self
                        .cursor
                        .row
                        .saturating_add(1)
                        .min(pane.rows.saturating_sub(1))
                }
                b'u' => {
                    let _ = state.update_pane(pane_id, |pane| {
                        pane.viewport_offset = pane.viewport_offset.saturating_add(3)
                    });
                }
                b'd' => {
                    let _ = state.update_pane(pane_id, |pane| {
                        pane.viewport_offset = pane.viewport_offset.saturating_sub(3)
                    });
                }
                b'y' | b'\r' if self.anchor.is_some() => {
                    let text = self.selected_text(&pane);
                    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
                    if state
                        .update_metadata(|metadata| metadata.clipboard_base64 = encoded)
                        .is_ok()
                    {
                        self.active = false;
                    }
                }
                _ => {}
            }
        }
        self.pending.drain(..offset);
        let remainder = if self.active {
            Vec::new()
        } else {
            std::mem::take(&mut self.pending)
        };
        if !self.active {
            self.target = None;
        }
        self.sync(state, pane_id);
        (true, remainder)
    }

    /// Updates a selection from an SGR mouse event. Coordinates are pane-content coordinates.
    pub fn shift_drag(&mut self, row: u16, column: u16, release: bool, pane: &PaneView) -> bool {
        if !self.active || pane.rows == 0 || pane.columns == 0 {
            return false;
        }
        let point = CopyPoint {
            row: row.min(pane.rows - 1),
            column: column.min(pane.columns - 1),
        };
        if !self.dragging {
            self.anchor = Some(point);
        }
        self.cursor = point;
        self.dragging = !release;
        true
    }

    #[must_use]
    pub fn selected_text(&self, pane: &PaneView) -> String {
        let Some(anchor) = self.anchor else {
            return String::new();
        };
        let (start, end) = if (anchor.row, anchor.column) <= (self.cursor.row, self.cursor.column) {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        };
        let mut output = String::new();
        for row in start.row..=end.row.min(pane.rows.saturating_sub(1)) {
            let first = if row == start.row { start.column } else { 0 };
            let last = if row == end.row {
                end.column
            } else {
                pane.columns.saturating_sub(1)
            };
            let mut line = String::new();
            for column in first..=last.min(pane.columns.saturating_sub(1)) {
                if let Some(cell) = pane.cell(row, column)
                    && cell.kind != CellKind::WideContinuation
                {
                    line.push_str(&cell.text);
                }
            }
            output.push_str(line.trim_end_matches(' '));
            let wrapped = pane
                .wrapped_rows
                .get(usize::from(row))
                .copied()
                .unwrap_or(false);
            if row != end.row && !wrapped {
                output.push('\n');
            }
        }
        output
    }
}

/// Stateful mapping from workspace-configured viewer shortcuts to client actions.
#[derive(Clone, Debug)]
pub struct DetachFilter {
    prefix: Vec<u8>,
    bindings: crate::commands::ClientBindings,
    matched: Vec<u8>,
    command_pending: bool,
    contextual: bool,
    reveal_hints: bool,
    prefix_epoch: u64,
    hint_page: usize,
    viewer_action: Option<crate::commands::BuiltinAction>,
    mouse: Option<(crate::host::MouseEvent, Vec<u8>)>,
    external_binding: Option<u8>,
    paste: bool,
    paste_marker: Vec<u8>,
    detach: bool,
    workspace_picker: bool,
    workspace_picker_enabled: bool,
}

impl DetachFilter {
    pub fn new(prefix: Vec<u8>) -> Option<Self> {
        (!prefix.is_empty() && prefix.len() <= 16).then_some(Self {
            prefix,
            bindings: crate::commands::ClientBindings::default(),
            matched: Vec::new(),
            command_pending: false,
            contextual: false,
            reveal_hints: false,
            prefix_epoch: 0,
            hint_page: 0,
            viewer_action: None,
            mouse: None,
            external_binding: None,
            paste: false,
            paste_marker: Vec::new(),
            detach: false,
            workspace_picker: false,
            workspace_picker_enabled: false,
        })
    }

    pub fn enable_contextual_help(&mut self) {
        self.contextual = true;
    }

    pub fn take_external_binding(&mut self) -> Option<u8> {
        self.external_binding.take()
    }

    pub fn take_mouse(&mut self) -> Option<(crate::host::MouseEvent, Vec<u8>)> {
        self.mouse.take()
    }
    pub fn take_viewer_action(&mut self) -> Option<crate::commands::BuiltinAction> {
        self.viewer_action.take()
    }
    pub fn show_commands(&mut self) {
        self.command_pending = true;
        self.reveal_hints = true;
        self.prefix_epoch = self.prefix_epoch.wrapping_add(1);
    }

    pub fn command_pending(&self) -> bool {
        self.command_pending
    }

    pub fn prefix_epoch(&self) -> u64 {
        self.prefix_epoch
    }

    pub fn hint_page(&self) -> usize {
        self.hint_page
    }

    pub fn hints_requested(&self) -> bool {
        self.reveal_hints && self.command_pending
    }

    pub fn bindings(&self) -> &crate::commands::ClientBindings {
        &self.bindings
    }

    pub fn escape_pending(&self) -> bool {
        self.paste_marker.first() == Some(&0x1b) && !self.paste
            // Once CSI/SS3 is identified, keep its bounded sequence together.
            // A pause in a terminal report must not reinterpret its parameters
            // (or a bracketed-paste delimiter) as configurable command keys.
            && !(self.contextual && self.paste_marker.last() != Some(&0x1b)
                && (self.paste_marker.starts_with(b"\x1b[")
                    || self.paste_marker.starts_with(b"\x1bO")))
    }

    pub fn resolve_escape(&mut self) -> Vec<u8> {
        if !self.escape_pending() {
            return Vec::new();
        }
        let pending = std::mem::take(&mut self.paste_marker);
        if self.contextual && self.command_pending && pending.len() > 1 {
            if pending.last() == Some(&0x1b) {
                self.command_pending = false;
                self.reveal_hints = false;
            } else {
                self.reveal_hints = true;
            }
            Vec::new()
        } else {
            self.process(&pending, false)
        }
    }

    /// Recognizes bracketed-paste boundaries across chunks and applies detach mapping only to
    /// bytes outside the pasted payload. Boundary sequences themselves are forwarded unchanged.
    pub fn process_terminal_input(&mut self, input: &[u8]) -> Vec<u8> {
        const BEGIN: &[u8] = b"\x1b[200~";
        const END: &[u8] = b"\x1b[201~";
        let mut output = Vec::with_capacity(input.len());
        for &byte in input {
            self.paste_marker.push(byte);
            if self.contextual
                && !self.command_pending
                && !self.paste
                && self.paste_marker.starts_with(b"\x1b[<")
            {
                let complete = self.paste_marker.len() > 3 && (0x40..=0x7e).contains(&byte);
                if complete || self.paste_marker.len() >= 64 {
                    let bytes = std::mem::take(&mut self.paste_marker);
                    if let Some(mouse) = crate::host::MouseEvent::parse(&bytes) {
                        self.mouse = Some((mouse, bytes));
                    } else {
                        output.extend(bytes);
                    }
                }
                continue;
            }
            if self.contextual
                && !self.command_pending
                && !self.paste
                && (self.paste_marker.starts_with(b"\x1b[")
                    || self.paste_marker.starts_with(b"\x1bO"))
                && !BEGIN.starts_with(&self.paste_marker)
            {
                if (self.paste_marker.len() > 2 && (0x40..=0x7e).contains(&byte))
                    || self.paste_marker.len() >= 64
                {
                    output.extend(std::mem::take(&mut self.paste_marker));
                }
                continue;
            }
            if self.contextual
                && self.command_pending
                && !self.paste
                && self.paste_marker.first() == Some(&0x1b)
                && !BEGIN.starts_with(&self.paste_marker)
            {
                // Consume a complete escape sequence as one unknown command, never leak its tail.
                let complete = match self.paste_marker.get(1) {
                    Some(b'[' | b'O') => {
                        self.paste_marker.len() >= 3 && (0x40..=0x7e).contains(&byte)
                    }
                    Some(_) => true,
                    None => false,
                };
                if complete || self.paste_marker.len() >= 64 {
                    match self.paste_marker.as_slice() {
                        b"\x1b[6~" | b"\x1b[B" => self.hint_page = self.hint_page.saturating_add(1),
                        b"\x1b[5~" | b"\x1b[A" => self.hint_page = self.hint_page.saturating_sub(1),
                        _ => {}
                    }
                    self.paste_marker.clear();
                    self.reveal_hints = true;
                }
                continue;
            }
            loop {
                let marker = if self.paste { END } else { BEGIN };
                if marker.starts_with(&self.paste_marker) {
                    if self.paste_marker == marker {
                        if self.contextual && self.command_pending {
                            self.command_pending = false;
                            self.reveal_hints = false;
                        }
                        let complete = std::mem::take(&mut self.paste_marker);
                        self.flush_pending_into(&mut output);
                        output.extend(complete);
                        self.paste = !self.paste;
                    }
                    break;
                }
                let Some(first) = self.paste_marker.first().copied() else {
                    break;
                };
                self.paste_marker.remove(0);
                output.extend(self.process(&[first], self.paste));
                if self.paste_marker.is_empty() {
                    break;
                }
            }
        }
        output
    }

    /// Maps detach outside bracketed paste. All other bytes, including other prefix commands,
    /// remain byte-for-byte unchanged.
    pub fn process(&mut self, input: &[u8], bracketed_paste: bool) -> Vec<u8> {
        let mut output = Vec::with_capacity(input.len().saturating_add(self.prefix.len()));
        if bracketed_paste {
            self.flush_pending_into(&mut output);
            output.extend_from_slice(input);
            return output;
        }
        for &byte in input {
            if self.command_pending {
                if self.contextual {
                    if byte == 0x1b {
                        self.command_pending = false;
                        self.reveal_hints = false;
                        continue;
                    }
                    let literal = self.prefix == [byte];
                    if !literal
                        && (!self.bindings.is_bound(byte)
                            || self.bindings.action(byte)
                                == Some(crate::commands::BuiltinAction::Help))
                    {
                        self.reveal_hints = true;
                        continue;
                    }
                }
                self.reveal_hints = false;
                self.command_pending = false;
                if self.contextual
                    && ((!self.workspace_picker_enabled
                        && self.bindings.action(byte)
                            == Some(crate::commands::BuiltinAction::WorkspacePicker))
                        || self
                            .bindings
                            .action(byte)
                            .and_then(|action| action.command())
                            .and_then(|command| command.request(None))
                            .is_some()
                        || matches!(
                            self.bindings.action(byte),
                            Some(
                                crate::commands::BuiltinAction::ClosePane
                                    | crate::commands::BuiltinAction::CopyMode
                                    | crate::commands::BuiltinAction::TabPicker
                                    | crate::commands::BuiltinAction::RenameTab
                                    | crate::commands::BuiltinAction::ResizeMode
                            )
                        ))
                {
                    self.viewer_action = self.bindings.action(byte);
                } else if self.bindings.action(byte) == Some(crate::commands::BuiltinAction::Detach)
                {
                    self.detach = true;
                } else if self.bindings.action(byte)
                    == Some(crate::commands::BuiltinAction::WorkspacePicker)
                    && self.workspace_picker_enabled
                {
                    self.workspace_picker = true;
                } else if self.contextual {
                    if self.prefix == [byte] {
                        output.push(byte);
                    } else {
                        self.external_binding = Some(byte);
                    }
                } else {
                    output.extend_from_slice(&self.prefix);
                    output.push(byte);
                }
                continue;
            }
            self.matched.push(byte);
            while !self.prefix.starts_with(&self.matched) {
                if let Some(first) = self.matched.first().copied() {
                    output.push(first);
                    self.matched.remove(0);
                } else {
                    break;
                }
            }
            if self.matched == self.prefix {
                self.matched.clear();
                self.command_pending = true;
                self.prefix_epoch = self.prefix_epoch.wrapping_add(1);
                self.hint_page = 0;
            }
        }
        output
    }

    pub fn flush(&mut self) -> Vec<u8> {
        let mut output = Vec::new();
        let marker = std::mem::take(&mut self.paste_marker);
        if self.contextual && !self.command_pending {
            output.extend(marker);
        } else {
            output.extend(self.process(&marker, self.paste));
        }
        self.flush_pending_into(&mut output);
        output
    }

    /// Flush a partial prefix under its old meaning when live bindings change.
    pub fn configure(&mut self, bindings: crate::commands::ClientBindings) -> Vec<u8> {
        if self.bindings == bindings && self.prefix == [bindings.prefix()] {
            return Vec::new();
        }
        let mut pending = Vec::new();
        if self.contextual && self.command_pending {
            self.command_pending = false;
            self.reveal_hints = false;
            self.paste_marker.clear();
        }
        self.flush_pending_into(&mut pending);
        self.prefix = vec![bindings.prefix()];
        self.bindings = bindings;
        pending
    }

    pub fn take_detach(&mut self) -> bool {
        std::mem::take(&mut self.detach)
    }

    pub fn take_workspace_picker(&mut self) -> bool {
        std::mem::take(&mut self.workspace_picker)
    }

    pub fn set_workspace_picker_enabled(&mut self, enabled: bool) {
        self.workspace_picker_enabled = enabled;
    }

    fn flush_pending_into(&mut self, output: &mut Vec<u8>) {
        if self.command_pending {
            if !self.contextual {
                output.extend_from_slice(&self.prefix);
            }
            self.command_pending = false;
            self.reveal_hints = false;
        }
        output.append(&mut self.matched);
    }
}
