//! Text layout shared by the bar, the panes and the command column: display widths, grapheme-
//! aware truncation and bounds-checked buffer writes (ratatui's `set_stringn` indexes its start
//! cell unconditionally, so every write goes through [`put`]).

use ratatui_core::buffer::Buffer;
use ratatui_core::style::Style;
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

/// Display width in cells, saturating.
#[must_use]
pub fn width(text: &str) -> u16 {
    u16::try_from(text.width()).unwrap_or(u16::MAX)
}

/// The head of `text` within `width` cells, ending with `…` when something was cut.
#[must_use]
pub fn head(text: &str, width_cells: u16) -> String {
    if width(text) <= width_cells {
        return text.to_owned();
    }
    let Some(keep) = width_cells.checked_sub(1) else {
        return String::new();
    };
    let mut out = String::new();
    let mut used = 0_u16;
    for grapheme in text.graphemes(true) {
        let w = width(grapheme);
        if used.saturating_add(w) > keep {
            break;
        }
        out.push_str(grapheme);
        used = used.saturating_add(w);
    }
    out.push('…');
    out
}

/// The tail of `text` within `width` cells, starting with `…` when something was cut.
#[must_use]
pub fn tail(text: &str, width_cells: u16) -> String {
    if width(text) <= width_cells {
        return text.to_owned();
    }
    let Some(keep) = width_cells.checked_sub(1) else {
        return String::new();
    };
    let mut out = String::from("…");
    out.push_str(&tail_raw(text, keep));
    out
}

/// The tail of `text` within `width` cells with no marker: keeps a text input's insertion point
/// visible.
#[must_use]
pub fn tail_raw(text: &str, width_cells: u16) -> String {
    let mut used = 0_u16;
    let mut cut = text.len();
    for (offset, grapheme) in text.grapheme_indices(true).rev() {
        used = used.saturating_add(width(grapheme));
        if used > width_cells {
            break;
        }
        cut = offset;
    }
    text.get(cut..).unwrap_or_default().to_owned()
}

/// Writes `text` at (`x`, `y`) clipped to `max_width` and to the buffer; a start outside the
/// buffer paints nothing.
pub fn put(buffer: &mut Buffer, x: u16, y: u16, text: &str, max_width: u16, style: Style) {
    if max_width == 0 || x >= buffer.area.right() || y >= buffer.area.bottom() {
        return;
    }
    buffer.set_stringn(x, y, text, usize::from(max_width), style);
}

/// Paints blank styled cells over `[from, to)` on `row`.
pub fn fill(buffer: &mut Buffer, from: u16, to: u16, row: u16, style: Style) {
    for x in from..to {
        if let Some(cell) = buffer.cell_mut((x, row)) {
            cell.set_symbol(" ").set_style(style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_is_grapheme_aware_and_never_splits_wide_cells() {
        assert_eq!(head("abcdef", 4), "abc…");
        assert_eq!(head("abc", 3), "abc");
        assert_eq!(head("abc", 0), "");
        assert_eq!(tail("abcdef", 4), "…def");
        assert_eq!(tail_raw("abcdef", 2), "ef");
        assert_eq!(head("日本語", 3), "日…", "a wide cell never splits");
        assert_eq!(width("日本"), 4);
    }

    #[test]
    fn writes_outside_the_buffer_are_ignored() {
        let mut buffer = Buffer::empty(ratatui_core::layout::Rect::new(0, 0, 3, 1));
        put(&mut buffer, 5, 0, "x", 1, Style::default());
        put(&mut buffer, 0, 9, "x", 1, Style::default());
        put(&mut buffer, 1, 0, "yz", 5, Style::default());
        let row: String = (0..3)
            .map(|x| buffer.cell((x, 0)).map_or(" ", |cell| cell.symbol()))
            .collect();
        assert_eq!(row, " yz");
    }
}
