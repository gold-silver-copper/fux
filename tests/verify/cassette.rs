use serde::Deserialize;

use super::schema::{MAX_DIMENSION, MAX_NAME_BYTES, MAX_PAYLOAD_BYTES, Signal};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cassette {
    pub schema_version: u16,
    pub name: String,
    pub rows: u16,
    pub columns: u16,
    pub child_chunks_hex: Vec<String>,
    pub host_input_hex: Vec<String>,
    pub resizes: Vec<Resize>,
    pub signals: Vec<Signal>,
    pub osc_payloads: Vec<String>,
    pub exit_status: i32,
    pub expected: Expected,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resize {
    pub rows: u16,
    pub columns: u16,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    pub visible_cells: Vec<String>,
    pub title: String,
    pub agent_state: String,
}

impl Cassette {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err("unsupported cassette schema version".into());
        }
        bounded_text("cassette name", &self.name, false)?;
        dimensions(self.rows, self.columns)?;
        if self.child_chunks_hex.len() > 512
            || self.host_input_hex.len() > 512
            || self.resizes.len() > 512
            || self.signals.len() > 512
            || self.osc_payloads.len() > 512
        {
            return Err("cassette collection exceeds 512 entries".into());
        }
        let mut bytes = 0usize;
        for value in self.child_chunks_hex.iter().chain(&self.host_input_hex) {
            bytes = bytes
                .checked_add(value.len() / 2)
                .ok_or("cassette byte count overflow")?;
            decode_hex(value)?;
        }
        if bytes > MAX_PAYLOAD_BYTES {
            return Err("cassette payload exceeds bound".into());
        }
        for resize in &self.resizes {
            dimensions(resize.rows, resize.columns)?;
        }
        for payload in &self.osc_payloads {
            bounded_text("OSC payload", payload, false)?;
        }
        for cell in &self.expected.visible_cells {
            bounded_text("expected cell", cell, true)?;
        }
        bounded_text("expected title", &self.expected.title, true)?;
        bounded_text("expected agent state", &self.expected.agent_state, false)
    }

    pub fn child_bytes(&self) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        for chunk in &self.child_chunks_hex {
            bytes.extend(decode_hex(chunk)?);
        }
        Ok(bytes)
    }
}

pub fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    if !value.len().is_multiple_of(2) {
        return Err("hex payload has odd length".into());
    }
    value
        .as_bytes()
        .chunks_exact(2)
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
