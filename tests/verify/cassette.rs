use serde::Deserialize;

use super::schema::{MAX_DIMENSION, MAX_NAME_BYTES, MAX_PAYLOAD_BYTES, Signal};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cassette {
    pub schema_version: u16,
    pub name: String,
    pub rows: u16,
    pub columns: u16,
    pub actions: Vec<Action>,
    pub osc_payloads: Vec<String>,
    pub expected: Expected,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    ChildOutput { bytes_hex: String },
    HostInput { bytes_hex: String },
    Resize { rows: u16, columns: u16 },
    Signal { signal: Signal },
    Exit { status: i32 },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    pub visible_cells: Vec<String>,
    pub title: String,
    pub agent_state: String,
    pub final_rows: u16,
    pub final_columns: u16,
    pub pty_writes_hex: Vec<String>,
    pub signals: Vec<Signal>,
    pub exit_status: i32,
}

impl Cassette {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err("unsupported cassette schema version".into());
        }
        bounded_text("cassette name", &self.name, false)?;
        dimensions(self.rows, self.columns)?;
        if self.actions.len() > 512
            || self.osc_payloads.len() > 512
            || self.expected.visible_cells.len() > 512
            || self.expected.pty_writes_hex.len() > 512
            || self.expected.signals.len() > 512
        {
            return Err("cassette collection exceeds 512 entries".into());
        }
        let mut bytes = 0usize;
        let action_hex = self.actions.iter().filter_map(|action| match action {
            Action::ChildOutput { bytes_hex } | Action::HostInput { bytes_hex } => Some(bytes_hex),
            _ => None,
        });
        for value in action_hex.chain(&self.expected.pty_writes_hex) {
            if value.len() > MAX_PAYLOAD_BYTES.saturating_mul(2) {
                return Err("cassette encoded payload exceeds bound".into());
            }
            bytes = bytes
                .checked_add(value.len().div_ceil(2))
                .ok_or("cassette byte count overflow")?;
            if bytes > MAX_PAYLOAD_BYTES {
                return Err("cassette payload exceeds bound".into());
            }
            decode_hex(value)?;
        }
        let mut exits = 0;
        for action in &self.actions {
            if let Action::Resize { rows, columns } = action {
                dimensions(*rows, *columns)?;
            }
            if matches!(action, Action::Exit { .. }) {
                exits += 1;
            }
        }
        if exits != 1
            || !self
                .actions
                .last()
                .is_some_and(|action| matches!(action, Action::Exit { .. }))
        {
            return Err("cassette requires exactly one final exit action".into());
        }
        for payload in &self.osc_payloads {
            bounded_text("OSC payload", payload, false)?;
        }
        for cell in &self.expected.visible_cells {
            bounded_text("expected cell", cell, true)?;
        }
        dimensions(self.expected.final_rows, self.expected.final_columns)?;
        bounded_text("expected title", &self.expected.title, true)?;
        bounded_text("expected agent state", &self.expected.agent_state, false)
    }
}

pub fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    if !value.len().is_multiple_of(2) {
        return Err("hex payload has odd length".into());
    }
    value
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).map_err(|error| error.to_string())?;
            u8::from_str_radix(text, 16).map_err(|error| error.to_string())
        })
        .collect()
}

fn dimensions(rows: u16, columns: u16) -> Result<(), String> {
    if rows == 0 || columns == 0 || rows > MAX_DIMENSION || columns > MAX_DIMENSION {
        Err("cassette dimensions outside bounded range".into())
    } else {
        Ok(())
    }
}

fn bounded_text(kind: &str, value: &str, empty: bool) -> Result<(), String> {
    if (!empty && value.is_empty()) || value.len() > MAX_NAME_BYTES || value.contains('\0') {
        Err(format!("{kind} is outside its text bound"))
    } else {
        Ok(())
    }
}
