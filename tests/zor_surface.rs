#[test]
fn standalone_build_parses_optional_observer_metadata() {
    // Fux owns only the consumer adapter; this build has no zor dependency.
    let report = fux::parse_agent_report(b"7877;v=1;state=none;seq=1");
    assert!(report.is_ok());
}
