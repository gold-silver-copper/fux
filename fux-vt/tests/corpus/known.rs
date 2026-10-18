//! The checks of fux-vt's invariant checker (`Parser::check`,
//! `Decoder::check`) that fail at BASE on what the tests and fuzz targets
//! feed, each with the bound whose fix makes it pass, which takes it off
//! this list (`threats.md` section 6).

/// Each check known to fail at BASE, and the fix that takes it off.
pub const KNOWN_AT_BASE: &[(&str, &str)] = &[
    (
        "history.capacities",
        "B17: trim shrinks the ring and the maps (SCR-6)",
    ),
    (
        "grid.glyphs",
        "B32: every glyph is capped at 2 columns (TXT-1)",
    ),
    (
        "parser.osc",
        "B20: the OSC buffer's room is reserved exactly (PAR-6)",
    ),
    (
        "keys.held",
        "B24: the key decoder holds at most 258 bytes (KEY-3)",
    ),
    (
        "keys.paste",
        "B23: a paste keeps at most its limit + 5 (KEY-1)",
    ),
];

/// The faults the checker found whose checks are not known to fail at
/// BASE.
pub fn unknown(report: &fux_vt::Report) -> Vec<&fux_vt::Fault> {
    (report.faults().iter())
        .filter(|f| !KNOWN_AT_BASE.iter().any(|(check, _)| *check == f.check))
        .collect()
}
