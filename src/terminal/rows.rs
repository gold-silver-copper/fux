//! Bounded row reuse shared by viewers; no revision/window snapshot key.
#[cfg(test)]
mod tests;
use super::Style;
use fux_vt::{RowId, Screen};
use std::{
    collections::HashMap,
    hash::{BuildHasherDefault, Hasher},
    sync::Arc,
};

/// Keys are a monotonically allocated row ID and a width; a multiplicative
/// mix spreads them well without SipHash's per-lookup cost. The table is
/// process-private and bounded, so hash flooding is not a concern.
#[derive(Default)]
struct KeyHasher(u64);
impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u64(u64::from(byte));
        }
    }
    fn write_u64(&mut self, value: u64) {
        self.0 = (self.0 ^ value).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.0 ^= self.0 >> 29;
    }
    fn write_u16(&mut self, value: u16) {
        self.write_u64(u64::from(value));
    }
}
type Entries = HashMap<(RowId, u16), Entry, BuildHasherDefault<KeyHasher>>;

const MAX_ROWS: usize = 4096;
const MAX_BYTES: usize = 4 * 1024 * 1024;
struct Entry {
    version: u64,
    used: u64,
    text: Arc<str>,
}
#[derive(Default)]
pub(super) struct Rows {
    entries: Entries,
    bytes: usize,
    clock: u64,
    /// Recently served windows, most recent first. A window is reusable only
    /// while the emulator's non-destructive mark is unchanged; it shares the
    /// row cache's `Arc`s, so it retains at most `MAX_WINDOWS` × height rows.
    windows: Vec<WindowEntry>,
    #[cfg(test)]
    pub extractions: usize,
    #[cfg(test)]
    pub row_lookups: usize,
}
/// Enough for every viewer of a shared pane at distinct offsets/widths.
const MAX_WINDOWS: usize = 8;
struct WindowEntry {
    mark: fux_vt::Mark,
    offset: usize,
    height: u16,
    width: u16,
    lines: Vec<Arc<str>>,
}
impl Rows {
    pub fn snapshot(
        &mut self,
        screen: &Screen,
        offset: usize,
        height: u16,
        width: u16,
    ) -> &[Arc<str>] {
        // Same window, unchanged emulator: the previous rows are still exact,
        // without per-row work. Unlike a single revision-keyed snapshot, each
        // viewer's window survives the others' requests.
        let mark = screen.mark();
        if let Some(index) = self.windows.iter().position(|w| {
            w.mark == mark && w.offset == offset && w.height == height && w.width == width
        }) {
            let entry = self.windows.remove(index);
            self.windows.insert(0, entry);
            return self.windows.first().map_or(&[], |w| w.lines.as_slice());
        }
        let window = screen.window(offset, height, width);
        let mut lines = Vec::with_capacity(usize::from(window.rows));
        if self.clock == u64::MAX {
            self.entries.clear();
            self.bytes = 0;
            self.clock = 0;
        }
        self.clock += 1;
        for y in 0..window.rows {
            let Some(row) = window.row(y) else { continue };
            #[cfg(test)]
            {
                self.row_lookups += 1;
            }
            let key = (row.id, window.cols);
            if let Some(entry) = self.entries.get_mut(&key)
                && entry.version == row.version
            {
                entry.used = self.clock;
                lines.push(entry.text.clone());
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
            lines.push(text);
        }
        self.windows.truncate(MAX_WINDOWS - 1);
        self.windows.insert(
            0,
            WindowEntry {
                mark,
                offset,
                height,
                width,
                lines,
            },
        );
        self.windows.first().map_or(&[], |w| w.lines.as_slice())
    }
}
