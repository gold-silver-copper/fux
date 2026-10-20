//! The checks of fux-vt's invariant checker (`Parser::check`,
//! `Decoder::check`) known to fail on what the tests and fuzz targets
//! feed, each with the bound whose fix makes it pass, which takes it off
//! this list (`THREATS.md` section 6).

/// Each check known to fail, and the fix that takes it off: none now.
pub const KNOWN_AT_BASE: &[(&str, &str)] = &[];

/// The faults the checker found whose checks are not known to fail.
pub fn unknown(report: &fux_vt::Checked) -> Vec<&fux_vt::Fault> {
    (report.faults().iter())
        .filter(|f| !KNOWN_AT_BASE.iter().any(|(check, _)| *check == f.check))
        .collect()
}
