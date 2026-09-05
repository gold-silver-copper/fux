#[test]
fn downstream_build_exposes_only_the_osc_parser_contract() {
    // Phase 0 seam: fux can consume zor's no-default-features OSC API.
    let report = fux::parse_agent_report(b"7877;v=1;state=none;seq=1");
    assert!(report.is_ok());
}
