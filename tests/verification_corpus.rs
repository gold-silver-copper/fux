#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "verify/mod.rs"]
mod verify;

use verify::interpreters::{InProcessInterpreter, Interpreter, ModelInterpreter};
use verify::schema::Scenario;
use verify::transcript::{assert_fixture_safe, encode_jsonl};

const PREFIX_LITERAL: &str = include_str!("verify/corpus/input/prefix_literal.json");
const PREFIX_LITERAL_GOLDEN: &str = include_str!("verify/fixtures/prefix_literal.jsonl");

#[test]
fn prefix_literal_agrees_across_independent_interpreters_and_the_golden() {
    let scenario: Scenario = serde_json::from_str(PREFIX_LITERAL).expect("strict scenario");
    scenario.validate().expect("bounded scenario");

    let model = ModelInterpreter.run(&scenario).expect("model transcript");
    let production = InProcessInterpreter
        .run(&scenario)
        .expect("production transcript");
    assert_eq!(
        production, model,
        "production diverged from independent model"
    );

    let encoded = encode_jsonl(&production).expect("canonical JSONL");
    assert_fixture_safe(&encoded).expect("fixture secret audit");
    if std::env::var_os("FUX_RECORD_FIXTURES").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/verify/fixtures/prefix_literal.jsonl");
        std::fs::write(path, &encoded).expect("explicit fixture record");
    } else {
        assert_eq!(encoded, PREFIX_LITERAL_GOLDEN);
    }
}

#[test]
fn scenario_decoder_rejects_unknown_and_unbounded_input() {
    let unknown = PREFIX_LITERAL.replace(
        "\"schema_version\": 1",
        "\"schema_version\": 1, \"surprise\": true",
    );
    assert!(serde_json::from_str::<Scenario>(&unknown).is_err());

    let mut scenario: Scenario = serde_json::from_str(PREFIX_LITERAL).expect("scenario");
    scenario.steps.extend(std::iter::repeat_n(
        verify::schema::Step::AdvanceClock { milliseconds: 1 },
        verify::schema::MAX_STEPS,
    ));
    assert!(scenario.validate().is_err());
}
