//! Independent expectations retained from the passing differential phase.
#[path = "corpus/fixtures.rs"]
mod fixtures;
#[path = "corpus/pieces.rs"]
mod pieces;
#[path = "corpus/snapshot.rs"]
mod snapshot;
use fux_vt::Parser;
use std::fmt::Write;

type Result = std::result::Result<(), Box<dyn std::error::Error>>;
#[test]
fn every_permanent_fixture_pins_each_operation_under_every_chunking() -> Result {
    for &(name, operations) in fixtures::CASES {
        let expected = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/golden")
                .join(format!("{name}.snap")),
        )?;
        for chunk in [1, 2, 3, 7, usize::MAX] {
            let mut parser = Parser::new(4, 12, 3)?;
            let mut replies = Vec::new();
            let mut actual = String::new();
            for (index, operation) in operations.iter().enumerate() {
                for bytes in pieces::pieces(operation, chunk) {
                    parser.process_with_replies(bytes, |r| replies.push(r.to_vec()))?;
                }
                let _ = writeln!(actual, "operation={index} replies={replies:?}");
                actual.push_str(&snapshot::screen(parser.screen()));
            }
            assert_eq!(actual, expected, "fixture={name} chunk={chunk}");
        }
    }
    Ok(())
}
