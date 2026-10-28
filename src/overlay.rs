//! Native panel scrolling geometry.
/// Rows a scrolled panel has for its entries on a screen of `rows`: the
/// rows above the bar, less `fixed` lines of its own (a title, a help line)
/// and the two lines that say how many entries lie above and below; at
/// least one.
pub fn panel_room(rows: u16, fixed: usize) -> usize {
    usize::from(rows.saturating_sub(1))
        .saturating_sub(fixed.saturating_add(2))
        .max(1)
}

/// The first of `len` entries a panel with `room` rows for them shows, so
/// that `selected` is in view: the window ends at the selection, or at the
/// last entry.
pub fn window_start(len: usize, selected: usize, room: usize) -> usize {
    selected
        .saturating_add(1)
        .saturating_sub(room)
        .min(len.saturating_sub(room))
}
