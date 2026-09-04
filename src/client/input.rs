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
        if !self.active {
            return false;
        }
        let Some(pane) = state.pane(pane_id).cloned() else {
            self.reset();
            return true;
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
                    let _ = state.update_metadata(|metadata| metadata.clipboard_base64 = encoded);
                    self.active = false;
                }
                _ => {}
            }
        }
        self.pending.drain(..offset);
        if !self.active {
            self.pending.clear();
            self.target = None;
        }
        self.sync(state, pane_id);
        true
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

/// Stateful client-side mapping from the configured fux prefix + `d` to koh's detach escape.
#[derive(Clone, Debug)]
pub struct DetachFilter {
    prefix: Vec<u8>,
    matched: Vec<u8>,
    command_pending: bool,
    paste: bool,
    paste_marker: Vec<u8>,
    workspace_picker: bool,
    workspace_picker_enabled: bool,
}

impl DetachFilter {
    pub fn new(prefix: Vec<u8>) -> Option<Self> {
        (!prefix.is_empty() && prefix.len() <= 16).then_some(Self {
            prefix,
            matched: Vec::new(),
            command_pending: false,
            paste: false,
            paste_marker: Vec::new(),
            workspace_picker: false,
            workspace_picker_enabled: false,
        })
    }

    /// Recognizes bracketed-paste boundaries across chunks and applies detach mapping only to
    /// bytes outside the pasted payload. Boundary sequences themselves are forwarded unchanged.
    pub fn process_terminal_input(&mut self, input: &[u8]) -> Vec<u8> {
        const BEGIN: &[u8] = b"\x1b[200~";
        const END: &[u8] = b"\x1b[201~";
        let mut output = Vec::with_capacity(input.len());
        for &byte in input {
            self.paste_marker.push(byte);
            loop {
                let marker = if self.paste { END } else { BEGIN };
                if marker.starts_with(&self.paste_marker) {
                    if self.paste_marker == marker {
                        let complete = std::mem::take(&mut self.paste_marker);
                        output.extend(self.process(&complete, self.paste));
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
                self.command_pending = false;
                if byte == b'd' {
                    output.extend_from_slice(b"\x1e.");
                } else if byte == b's' && self.workspace_picker_enabled {
                    self.workspace_picker = true;
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
            }
        }
        output
    }

    pub fn flush(&mut self) -> Vec<u8> {
        let mut output = Vec::new();
        let marker = std::mem::take(&mut self.paste_marker);
        output.extend(self.process(&marker, self.paste));
        self.flush_pending_into(&mut output);
        output
    }

    pub fn take_workspace_picker(&mut self) -> bool {
        std::mem::take(&mut self.workspace_picker)
    }

    pub fn set_workspace_picker_enabled(&mut self, enabled: bool) {
        self.workspace_picker_enabled = enabled;
    }

    fn flush_pending_into(&mut self, output: &mut Vec<u8>) {
        if self.command_pending {
            output.extend_from_slice(&self.prefix);
            self.command_pending = false;
        }
        output.append(&mut self.matched);
    }
}
