//! Built-in template scenes (prompt 3.7: the default layouts a user selects by name). Each is a
//! RON `DynamicWorld` whose leaves carry `PaneTemplate` with an empty `argv`, meaning the
//! configured default command; `scene.restore` falls back to these when no user file of that
//! name exists, and a user file of the same name shadows the built-in.

/// Names in the order `scene.list` reports them.
pub const NAMES: &[&str] = &["two_column", "two_row", "three_column", "main_side"];

/// Two panes side by side, equal width.
pub const TWO_COLUMN: &str = include_str!("builtin/two_column.scn.ron");
/// Two panes stacked, equal height.
pub const TWO_ROW: &str = include_str!("builtin/two_row.scn.ron");
/// Three panes side by side, equal width.
pub const THREE_COLUMN: &str = include_str!("builtin/three_column.scn.ron");
/// A main pane taking two thirds and a side pane taking one third.
pub const MAIN_SIDE: &str = include_str!("builtin/main_side.scn.ron");

/// The built-in document of that name.
pub fn document(name: &str) -> Option<&'static str> {
    match name {
        "two_column" => Some(TWO_COLUMN),
        "two_row" => Some(TWO_ROW),
        "three_column" => Some(THREE_COLUMN),
        "main_side" => Some(MAIN_SIDE),
        _ => None,
    }
}
