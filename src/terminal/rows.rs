//! Bounded row reuse shared by viewers; no revision/window snapshot key.
#[cfg(test)]
mod tests;
use super::Style;
use fux_vt::{RowId, Screen};
use std::{collections::HashMap, sync::Arc};

const MAX_ROWS: usize = 4096;
const MAX_BYTES: usize = 4 * 1024 * 1024;
struct Entry {
    version: u64,
    used: u64,
    text: Arc<str>,
}
#[derive(Default)]
pub(super) struct Rows {
    entries: HashMap<(RowId, u16), Entry>,
    bytes: usize,
    clock: u64,
    lines: Vec<Arc<str>>,
    #[cfg(test)]
    pub extractions: usize,
}
impl Rows {
    pub fn snapshot(
        &mut self,
        screen: &Screen,
        offset: usize,
        height: u16,
        width: u16,
    ) -> &[Arc<str>] {
        let window = screen.window(offset, height, width);
        self.lines.clear();
        if self.clock == u64::MAX {
            self.entries.clear();
            self.bytes = 0;
            self.clock = 0;
        }
        self.clock += 1;
        for y in 0..window.rows {
            let Some(row) = window.row(y) else { continue };
            let key = (row.id, window.cols);
            if let Some(entry) = self.entries.get_mut(&key)
                && entry.version == row.version
            {
                entry.used = self.clock;
                self.lines.push(entry.text.clone());
                continue;
            }
            if let Some(old) = self.entries.remove(&key) {
                self.bytes -= old.text.len();
            }
            #[cfg(test)]
            {
                self.extractions += 1;
            }
            let mut line = String::with_capacity(usize::from(window.cols) + 16);
            line.push_str("\x1b[0m");
            let mut previous = None;
            for x in 0..window.cols {
                let Some(cell) = window.cell(y, x) else {
                    // A wide leader clipped at the right edge occupies a blank
                    // cell, rather than shortening this relocatable row.
                    line.push_str("\x1b[0m ");
                    previous = None;
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let style = Style::of(cell);
                if previous != Some(style) {
                    style.write(&mut line);
                    previous = Some(style);
                }
                line.push_str(if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                });
            }
            line.push_str("\x1b[0m");
            let text: Arc<str> = line.into();
            if text.len() <= MAX_BYTES {
                while self.entries.len() >= MAX_ROWS || self.bytes + text.len() > MAX_BYTES {
                    let oldest = self
                        .entries
                        .iter()
                        .min_by_key(|(_, entry)| entry.used)
                        .map(|(key, _)| *key);
                    let Some(oldest) = oldest else { break };
                    if let Some(entry) = self.entries.remove(&oldest) {
                        self.bytes -= entry.text.len();
                    }
                }
                self.bytes += text.len();
                self.entries.insert(
                    key,
                    Entry {
                        version: row.version,
                        used: self.clock,
                        text: text.clone(),
                    },
                );
            }
            self.lines.push(text);
        }
        &self.lines
    }
}
