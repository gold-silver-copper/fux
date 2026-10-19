//! The checks of fux-vt's invariant checker (`Parser::check`,
//! `Decoder::check`) that fail at BASE on what the tests and fuzz targets
//! feed, each with the bound whose fix makes it pass, which takes it off
//! this list (`threats.md` section 6).

/// Each check known to fail at BASE, and the fix that takes it off: none
/// now. B17's `history.capacities` (SCR-6), B24's `keys.held` (KEY-3,
/// FA-KEY-1) and B23's `keys.paste` (KEY-1, FA-CODEX-4) were taken off as
/// their fixes landed (FA-SCR-1).
pub const KNOWN_AT_BASE: &[(&str, &str)] = &[];

/// The faults the checker found whose checks are not known to fail at
/// BASE.
pub fn unknown(report: &fux_vt::Checked) -> Vec<&fux_vt::Fault> {
    (report.faults().iter())
        .filter(|f| !KNOWN_AT_BASE.iter().any(|(check, _)| *check == f.check))
        .collect()
}
