#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "verify/mod.rs"]
mod verify;

use fux::state::{Axis, LayoutTree, PaneId, RATIO_SCALE, Rect};
use proptest::prelude::*;
use std::num::NonZeroU16;
use verify::interpreters::{InProcessInterpreter, Interpreter, ModelInterpreter};
use verify::schema::Scenario;
use verify::transcript::{Entry, Event, assert_fixture_safe, encode_jsonl};

const PREFIX_LITERAL: &str = include_str!("verify/corpus/input/prefix_literal.json");
const PREFIX_LITERAL_GOLDEN: &str = include_str!("verify/fixtures/prefix_literal.jsonl");
const PREFIX_AND_PASTE: &str = include_str!("verify/corpus/input/prefix_and_paste.json");
const PREFIX_AND_PASTE_GOLDEN: &str = include_str!("verify/fixtures/prefix_and_paste.jsonl");
const PREFIX_TIMEOUT: &str = include_str!("verify/corpus/input/prefix_timeout.json");
const PREFIX_TIMEOUT_GOLDEN: &str = include_str!("verify/fixtures/prefix_timeout.jsonl");
const SIGNAL_HUP: &str = include_str!("verify/corpus/input/signal_hup.json");
const SIGNAL_HUP_GOLDEN: &str = include_str!("verify/fixtures/signal_hup.jsonl");
const SIGNAL_INT: &str = include_str!("verify/corpus/input/signal_int.json");
const SIGNAL_INT_GOLDEN: &str = include_str!("verify/fixtures/signal_int.jsonl");
const SIGNAL_TERM: &str = include_str!("verify/corpus/input/signal_term.json");
const SIGNAL_TERM_GOLDEN: &str = include_str!("verify/fixtures/signal_term.jsonl");
const SIGNAL_KILL: &str = include_str!("verify/corpus/input/signal_kill.json");
const SIGNAL_KILL_GOLDEN: &str = include_str!("verify/fixtures/signal_kill.jsonl");
const KILL_PANE: &str = include_str!("verify/corpus/input/kill_pane.json");
const KILL_PANE_GOLDEN: &str = include_str!("verify/fixtures/kill_pane.jsonl");
const WORKSPACE_LIFECYCLE: &str = include_str!("verify/corpus/input/workspace_lifecycle.json");
const WORKSPACE_LIFECYCLE_GOLDEN: &str = include_str!("verify/fixtures/workspace_lifecycle.jsonl");
const WORKSPACE_SHUTDOWN_CLEANUP: &str =
    include_str!("verify/corpus/input/workspace_shutdown_cleanup.json");
const WORKSPACE_SHUTDOWN_CLEANUP_GOLDEN: &str =
    include_str!("verify/fixtures/workspace_shutdown_cleanup.jsonl");
const WORKSPACE_SWITCH: &str = include_str!("verify/corpus/input/workspace_switch.json");
const WORKSPACE_SWITCH_GOLDEN: &str = include_str!("verify/fixtures/workspace_switch.jsonl");
const WIDE_OSC_CASSETTE: &str = include_str!("verify/fixtures/terminal/wide_osc.json");

#[test]
fn prefix_literal_agrees_across_independent_interpreters_and_the_golden() {
    assert_scenario_golden(
        PREFIX_LITERAL,
        PREFIX_LITERAL_GOLDEN,
        "prefix_literal.jsonl",
    );
}

#[test]
fn prefix_and_paste_agrees_across_independent_interpreters_and_the_golden() {
    assert_scenario_golden(
        PREFIX_AND_PASTE,
        PREFIX_AND_PASTE_GOLDEN,
        "prefix_and_paste.jsonl",
    );
}

#[test]
fn prefix_timeout_agrees_across_independent_interpreters_and_the_golden() {
    assert_scenario_golden(
        PREFIX_TIMEOUT,
        PREFIX_TIMEOUT_GOLDEN,
        "prefix_timeout.jsonl",
    );
}

#[test]
fn signal_hup_agrees_across_independent_interpreters_and_the_golden() {
    assert_scenario_golden(SIGNAL_HUP, SIGNAL_HUP_GOLDEN, "signal_hup.jsonl");
}

#[test]
fn signal_int_agrees_across_independent_interpreters_and_the_golden() {
    assert_scenario_golden(SIGNAL_INT, SIGNAL_INT_GOLDEN, "signal_int.jsonl");
}

#[test]
fn signal_term_agrees_across_independent_interpreters_and_the_golden() {
    assert_scenario_golden(SIGNAL_TERM, SIGNAL_TERM_GOLDEN, "signal_term.jsonl");
}

#[test]
fn signal_kill_agrees_across_independent_interpreters_and_the_golden() {
    assert_scenario_golden(SIGNAL_KILL, SIGNAL_KILL_GOLDEN, "signal_kill.jsonl");
}

#[test]
fn kill_pane_agrees_across_independent_interpreters_and_the_golden() {
    assert_scenario_golden(KILL_PANE, KILL_PANE_GOLDEN, "kill_pane.jsonl");
}

#[test]
fn workspace_lifecycle_agrees_across_independent_interpreters_and_the_golden() {
    assert_scenario_golden(
        WORKSPACE_LIFECYCLE,
        WORKSPACE_LIFECYCLE_GOLDEN,
        "workspace_lifecycle.jsonl",
    );
}

#[test]
fn workspace_shutdown_cleanup_agrees_across_all_interpreters() {
    assert_scenario_golden(
        WORKSPACE_SHUTDOWN_CLEANUP,
        WORKSPACE_SHUTDOWN_CLEANUP_GOLDEN,
        "workspace_shutdown_cleanup.jsonl",
    );
}

#[test]
fn workspace_switch_agrees_across_all_interpreters() {
    assert_scenario_golden(
        WORKSPACE_SWITCH,
        WORKSPACE_SWITCH_GOLDEN,
        "workspace_switch.jsonl",
    );
}

fn assert_scenario_golden(source: &str, golden: &str, fixture_name: &str) {
    let scenario: Scenario = serde_json::from_str(source).expect("strict scenario");
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
            .join("tests/verify/fixtures")
            .join(fixture_name);
        std::fs::write(path, &encoded).expect("explicit fixture record");
    } else {
        assert_eq!(encoded, golden);
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

    let mut resize: Scenario = serde_json::from_str(PREFIX_AND_PASTE).expect("resize scenario");
    let step = resize
        .steps
        .iter_mut()
        .find(|step| matches!(step, verify::schema::Step::Resize { .. }))
        .expect("resize step");
    if let verify::schema::Step::Resize { client, .. } = step {
        *client = "x".repeat(verify::schema::MAX_NAME_BYTES + 1);
    }
    assert!(resize.validate().is_err());

    for invalid in ["../x".to_owned(), ".".to_owned(), "x".repeat(65)] {
        let mut workspace: Scenario =
            serde_json::from_str(WORKSPACE_LIFECYCLE).expect("workspace scenario");
        let step = workspace
            .steps
            .iter_mut()
            .find(|step| matches!(step, verify::schema::Step::CreateWorkspace { .. }))
            .expect("workspace creation step");
        if let verify::schema::Step::CreateWorkspace { workspace } = step {
            *workspace = invalid;
        }
        assert!(workspace.validate().is_err());
    }

    let mut child_output: Scenario =
        serde_json::from_str(PREFIX_AND_PASTE).expect("child output scenario");
    let step = child_output
        .steps
        .iter_mut()
        .find(|step| matches!(step, verify::schema::Step::ChildOutput { .. }))
        .expect("child output step");
    if let verify::schema::Step::ChildOutput { bytes, .. } = step {
        *bytes = b"\x1b[31m".to_vec();
    }
    assert!(child_output.validate().is_err());

    let invalid_frames = [
        ("mode", verify::schema::MAX_NAME_BYTES + 1),
        ("status", verify::schema::MAX_NAME_BYTES + 1),
    ];
    for (kind, length) in invalid_frames {
        let mut scenario: Scenario =
            serde_json::from_str(PREFIX_AND_PASTE).expect("terminal frame scenario");
        let frame = scenario
            .steps
            .iter_mut()
            .find_map(|step| match step {
                verify::schema::Step::Expect { expected } => expected.terminal_frames.first_mut(),
                _ => None,
            })
            .expect("expected terminal frame");
        if kind == "mode" {
            frame.modes.mouse_mode = "x".repeat(length);
        } else {
            frame.status.insert("x".repeat(length), "value".into());
        }
        assert!(scenario.validate().is_err());
    }

    let mut scenario: Scenario =
        serde_json::from_str(PREFIX_AND_PASTE).expect("terminal selection scenario");
    let frame = scenario
        .steps
        .iter_mut()
        .find_map(|step| match step {
            verify::schema::Step::Expect { expected } => expected.terminal_frames.first_mut(),
            _ => None,
        })
        .expect("expected terminal frame");
    frame.selection = Some(verify::transcript::TerminalSelection {
        cursor: (frame.rows, 0),
        anchor: None,
    });
    assert!(scenario.validate().is_err());

    let mut invalid_semantics: Scenario =
        serde_json::from_str(PREFIX_AND_PASTE).expect("terminal semantics scenario");
    let frame = invalid_semantics
        .steps
        .iter_mut()
        .find_map(|step| match step {
            verify::schema::Step::Expect { expected } => expected.terminal_frames.first_mut(),
            _ => None,
        })
        .expect("expected terminal frame");
    frame.modes.mouse_mode = "invented".into();
    frame.prediction_target = Some(2);
    assert!(invalid_semantics.validate().is_err());

    let mouse_scenario: Scenario =
        serde_json::from_str(PREFIX_AND_PASTE).expect("mouse lifecycle scenario");
    let mut missing_enable = mouse_scenario.clone();
    missing_enable
        .steps
        .retain(|step| !matches!(step, verify::schema::Step::EnableMouseTracking { .. }));
    assert!(missing_enable.validate().is_err());

    let mut duplicate_enable = mouse_scenario.clone();
    let enable_index = duplicate_enable
        .steps
        .iter()
        .position(|step| matches!(step, verify::schema::Step::EnableMouseTracking { .. }))
        .expect("mouse enable step");
    duplicate_enable.steps.insert(
        enable_index,
        verify::schema::Step::EnableMouseTracking { pane: 1 },
    );
    assert!(duplicate_enable.validate().is_err());

    let mut wrong_pane = mouse_scenario;
    let enable = wrong_pane
        .steps
        .iter_mut()
        .find(|step| matches!(step, verify::schema::Step::EnableMouseTracking { .. }))
        .expect("mouse enable step");
    *enable = verify::schema::Step::EnableMouseTracking { pane: 2 };
    assert!(wrong_pane.validate().is_err());

    for invalid in [Vec::new(), b"trailing ".to_vec(), vec![b'X'; 79]] {
        let mut child_output: Scenario =
            serde_json::from_str(PREFIX_LITERAL).expect("child output bounds scenario");
        child_output.steps.insert(
            1,
            verify::schema::Step::ChildOutput {
                pane: 1,
                bytes: invalid,
            },
        );
        assert!(child_output.validate().is_err());
    }

    let mut terminal_reply: Scenario =
        serde_json::from_str(PREFIX_AND_PASTE).expect("terminal reply scenario");
    let step = terminal_reply
        .steps
        .iter_mut()
        .find(|step| matches!(step, verify::schema::Step::TerminalReply { .. }))
        .expect("terminal reply step");
    if let verify::schema::Step::TerminalReply { query, .. } = step {
        *query = b"\x1b[c".to_vec();
    }
    assert!(terminal_reply.validate().is_err());

    let mut copy_input: Scenario =
        serde_json::from_str(PREFIX_AND_PASTE).expect("copy input scenario");
    let step = copy_input
        .steps
        .iter_mut()
        .find(|step| matches!(step, verify::schema::Step::CopyInput { .. }))
        .expect("copy input step");
    if let verify::schema::Step::CopyInput { client, .. } = step {
        *client = "bob".into();
    }
    assert!(copy_input.validate().is_err());

    let mut post_exit: Scenario =
        serde_json::from_str(PREFIX_AND_PASTE).expect("post-exit scenario");
    let exit = post_exit
        .steps
        .iter()
        .position(|step| matches!(step, verify::schema::Step::ChildExit { .. }))
        .expect("child exit step");
    post_exit.steps.insert(
        exit + 1,
        verify::schema::Step::ChildOutput {
            pane: 1,
            bytes: b"late".to_vec(),
        },
    );
    assert!(post_exit.validate().is_err());

    let mut restarted: Scenario = serde_json::from_str(PREFIX_LITERAL).expect("restart scenario");
    let expect = restarted
        .steps
        .iter()
        .position(|step| matches!(step, verify::schema::Step::Expect { .. }))
        .expect("expect step");
    restarted
        .steps
        .insert(expect, verify::schema::Step::StartDaemon);
    assert!(restarted.validate().is_err());

    let mut after_shutdown: Scenario =
        serde_json::from_str(PREFIX_LITERAL).expect("post-shutdown scenario");
    let expect = after_shutdown
        .steps
        .iter()
        .position(|step| matches!(step, verify::schema::Step::Expect { .. }))
        .expect("expect step");
    after_shutdown.steps.insert(
        expect,
        verify::schema::Step::Input {
            client: "alice".into(),
            bytes: b"late".to_vec(),
        },
    );
    assert!(after_shutdown.validate().is_err());

    let mut no_start: Scenario = serde_json::from_str(PREFIX_LITERAL).expect("no-start scenario");
    no_start.steps.remove(0);
    assert!(no_start.validate().is_err());

    let mut no_shutdown: Scenario =
        serde_json::from_str(PREFIX_LITERAL).expect("no-shutdown scenario");
    no_shutdown
        .steps
        .retain(|step| !matches!(step, verify::schema::Step::Shutdown));
    assert!(no_shutdown.validate().is_err());

    let mut no_expect: Scenario = serde_json::from_str(PREFIX_LITERAL).expect("no-expect scenario");
    no_expect
        .steps
        .retain(|step| !matches!(step, verify::schema::Step::Expect { .. }));
    assert!(no_expect.validate().is_err());
}

#[test]
fn normalizer_preserves_order_and_identity_without_hiding_counts() {
    let mut normalizer = verify::normalize::Normalizer::default();
    assert_eq!(normalizer.pid(9001), "process-1");
    assert_eq!(normalizer.pid(42), "process-2");
    assert_eq!(normalizer.pid(9001), "process-1");
    assert_eq!(normalizer.path("/private/tmp/a"), "private-path-1");
    assert_eq!(normalizer.path("/private/tmp/b"), "private-path-2");
    assert_eq!(normalizer.endpoint("volatile-a"), "endpoint-1");
    assert_eq!(normalizer.endpoint("volatile-a"), "endpoint-1");
}

#[test]
fn selected_terminal_grid_operations_and_resource_floor_match_independent_oracles() {
    use fux::state::{CellKind, PaneView};
    let mut parser = vt100::Parser::new(2, 8, 0);
    parser.process("A界e\u{301}".as_bytes());
    let pane = PaneView::from_vt100(parser.screen(), String::new(), Default::default(), 0)
        .expect("valid terminal grid");
    let observed: Vec<_> = pane
        .cells
        .iter()
        .take(5)
        .map(|cell| (cell.text.as_str(), cell.kind))
        .collect();
    assert_eq!(
        observed,
        vec![
            ("A", CellKind::Text),
            ("界", CellKind::WideLeading),
            ("", CellKind::WideContinuation),
            ("e\u{301}", CellKind::Text),
            ("", CellKind::Blank),
        ]
    );

    let mut state = fux::state::WorkspaceState::default();
    state.insert_pane(PaneId(1), pane).expect("pane");
    let minimum = verify::oracle::resource::conservative_minimum(&state);
    assert!(state.resource_units() >= minimum);
    assert_eq!(state.resource_units(), state.recompute_resource_units());
}

proptest! {
    #[test]
    fn recursive_layout_geometry_matches_the_independent_oracle(
        width in 1_u16..=512,
        height in 1_u16..=512,
        first_ratio in fux::state::MIN_RATIO..=fux::state::MAX_RATIO,
        second_ratio in fux::state::MIN_RATIO..=fux::state::MAX_RATIO,
        horizontal in any::<bool>(),
        second_horizontal in any::<bool>(),
        split_first in any::<bool>(),
    ) {
        let first_axis = if horizontal { Axis::Horizontal } else { Axis::Vertical };
        let second_axis = if second_horizontal { Axis::Horizontal } else { Axis::Vertical };
        let split_target = if split_first { PaneId(1) } else { PaneId(2) };
        let mut production = LayoutTree::new(PaneId(1));
        production.split(PaneId(1), PaneId(2), first_axis, NonZeroU16::new(first_ratio).expect("ratio")).expect("split");
        production.split(split_target, PaneId(3), second_axis, NonZeroU16::new(second_ratio).expect("ratio")).expect("split");

        let mut oracle = verify::oracle::layout::Tree::Leaf(PaneId(1));
        assert!(oracle.split(PaneId(1), PaneId(2), first_axis, first_ratio));
        assert!(oracle.split(split_target, PaneId(3), second_axis, second_ratio));
        let area = Rect { x: 7, y: 11, width, height };
        prop_assert_eq!(production.geometry(area).expect("geometry"), oracle.geometry(area));
        for pane in [PaneId(1), PaneId(2), PaneId(3)] {
            for direction in [
                fux::state::Direction::Left,
                fux::state::Direction::Right,
                fux::state::Direction::Up,
                fux::state::Direction::Down,
            ] {
                prop_assert_eq!(
                    production.neighbour(pane, direction, area),
                    oracle.neighbour(pane, direction, area),
                    "directional focus diverged for pane={:?} direction={:?} area={:?}",
                    pane,
                    direction,
                    area,
                );
            }
        }
        prop_assert_eq!(RATIO_SCALE, 10_000);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn workspace_transitions_match_the_independent_oracle(
        encoded_actions in prop::collection::vec((0_u8..3, 0_usize..4), 0..=24),
    ) {
        use verify::oracle::workspace::{Action, WorkspaceOracle};
        use verify::schema::{Applicability, Expected, Size, Step};

        const NAMES: [&str; 4] = ["binary", "alpha", "beta", "gamma"];
        let mut oracle = WorkspaceOracle::started();
        let mut steps = vec![Step::StartDaemon];
        let mut expected = vec![Entry {
            sequence: 0,
            source: "model".into(),
            event: Event::Lifecycle {
                resource: "daemon".into(),
                state: "running".into(),
            },
        }];
        for (kind, name_index) in encoded_actions {
            let name = *NAMES.get(name_index).expect("generated bounded name index");
            let action = match kind {
                0 => Action::Create(name),
                1 => Action::Select(name),
                _ => Action::Delete(name),
            };
            steps.push(match action {
                Action::Create(workspace) => Step::CreateWorkspace { workspace: workspace.into() },
                Action::Select(workspace) => Step::SelectWorkspace { workspace: workspace.into() },
                Action::Delete(workspace) => Step::DeleteWorkspace { workspace: workspace.into() },
            });
            if let Some(transition) = oracle.apply(action) {
                expected.push(Entry {
                    sequence: u64::try_from(expected.len()).expect("bounded oracle transcript"),
                    source: "model".into(),
                    event: Event::Lifecycle {
                        resource: format!("workspace:{}", transition.workspace),
                        state: transition.state.into(),
                    },
                });
            } else {
                let mut rejected_steps = steps;
                rejected_steps.push(Step::Shutdown);
                rejected_steps.push(Step::Expect {
                    expected: Expected::default(),
                });
                let rejected = Scenario {
                    schema_version: 1,
                    name: "generated_rejected_workspace_transition".into(),
                    applicability: Applicability::Production,
                    initial_size: Size { rows: 24, columns: 80 },
                    steps: rejected_steps,
                };
                prop_assert!(
                    rejected.validate().is_err(),
                    "scenario validation accepted the first transition rejected by the oracle: {:?}",
                    rejected.steps,
                );
                return Ok(());
            }
        }
        steps.push(Step::Shutdown);
        expected.push(Entry {
            sequence: u64::try_from(expected.len()).expect("bounded oracle transcript"),
            source: "model".into(),
            event: Event::Lifecycle {
                resource: "daemon".into(),
                state: "stopped".into(),
            },
        });
        steps.push(Step::Expect { expected: Expected::default() });
        expected.push(Entry {
            sequence: u64::try_from(expected.len()).expect("bounded oracle transcript"),
            source: "model".into(),
            event: Event::Cleanup { owned_resources: 0 },
        });
        let scenario = Scenario {
            schema_version: 1,
            name: "generated_workspace_transitions".into(),
            applicability: Applicability::Production,
            initial_size: Size { rows: 24, columns: 80 },
            steps,
        };
        scenario.validate().expect("oracle accepted scenario was rejected");
        prop_assert_eq!(
            InProcessInterpreter.run(&scenario).expect("production workspace transcript"),
            expected,
        );
    }
}

#[test]
fn copy_and_event_oracles_preserve_wrapping_and_suppress_duplicates() {
    let rows = vec![
        vec![Some("A"), Some("界"), None],
        vec![Some("e\u{301}"), Some(" "), Some("Z")],
    ];
    assert_eq!(
        verify::oracle::copy::extract(&rows, &[true, false], (0, 0), (1, 2)),
        "A界e\u{301} Z"
    );

    use fux::client::CopyMode;
    use fux::state::{Cell, CellKind, PaneView, Tab, TabId, WorkspaceState};
    let cells = vec![
        Cell {
            text: "A".into(),
            kind: CellKind::Text,
            ..Cell::default()
        },
        Cell {
            text: "界".into(),
            kind: CellKind::WideLeading,
            ..Cell::default()
        },
        Cell {
            kind: CellKind::WideContinuation,
            ..Cell::default()
        },
        Cell {
            text: "e\u{301}".into(),
            kind: CellKind::Text,
            ..Cell::default()
        },
        Cell {
            text: " ".into(),
            kind: CellKind::Text,
            ..Cell::default()
        },
        Cell {
            text: "Z".into(),
            kind: CellKind::Text,
            ..Cell::default()
        },
    ];
    let pane = PaneView {
        rows: 2,
        columns: 3,
        cells,
        wrapped_rows: vec![true, false],
        ..PaneView::default()
    };
    let mut state = WorkspaceState::default();
    state.insert_pane(PaneId(1), pane.clone()).expect("pane");
    state
        .replace_tabs(
            vec![Tab {
                id: TabId(1),
                name: "main".into(),
                layout: LayoutTree::new(PaneId(1)),
                focused: PaneId(1),
                zoomed: None,
            }],
            Some(TabId(1)),
        )
        .expect("tab");
    let mut copy = CopyMode::default();
    copy.enter(&pane);
    assert!(copy.key(b" ", &mut state, PaneId(1)));
    assert!(copy.key(b"jll", &mut state, PaneId(1)));
    assert_eq!(
        copy.selected_text(&pane),
        verify::oracle::copy::extract(&rows, &[true, false], (0, 0), (1, 2))
    );

    use verify::oracle::event::{AgentState, apply};
    let mut state = AgentState::None;
    let transition = apply(&mut state, AgentState::Working).expect("state transition");
    let event = fux::control::Event::AgentState {
        id: 7,
        pane: 1,
        agent: Some("fixture".into()),
        old_state: fux::control::AgentStatus::None,
        new_state: fux::control::AgentStatus::Working,
        timestamp_ms: 0,
    };
    let wire = serde_json::to_string(&event).expect("event wire");
    assert!(wire.contains("\"event\":\"agent.state\""));
    assert_eq!(transition.old, AgentState::None);
    assert_eq!(transition.new, AgentState::Working);
    assert!(apply(&mut state, AgentState::Working).is_none());
    assert!(apply(&mut state, AgentState::Blocked).is_some());
    assert!(apply(&mut state, AgentState::Idle).is_some());
}

#[test]
fn every_default_prefix_command_matches_the_oracle_at_every_boundary() {
    let keys = b"|-hjklxctnpz[ds?";
    for key in keys {
        let bytes = [0x02, *key];
        for boundary in 0..=bytes.len() {
            let mut model = verify::oracle::input::PrefixOracle::new(0x02);
            let mut production = fux::host::InputRouter::new(0x02, 25);
            let (head, tail) = bytes.split_at(boundary);
            let expected = [model.feed(head), model.feed(tail)].concat();
            let actual = [production.feed(head, 0), production.feed(tail, 0)].concat();
            assert_eq!(
                actual.len(),
                expected.len(),
                "action count for key {key:?} at boundary {boundary}"
            );
            let actual_name = actual.first().and_then(|action| match action {
                fux::host::Action::Command(command) => Some(production_command_name(command)),
                _ => None,
            });
            let expected_name: Option<&str> = expected.first().and_then(|outcome| match outcome {
                verify::oracle::input::Outcome::Command(command) => Some(*command),
                _ => None,
            });
            assert_eq!(actual_name, expected_name);
        }
    }
}

#[test]
fn terminal_cassette_replays_identically_at_every_byte_boundary() {
    let cassette: verify::cassette::Cassette =
        serde_json::from_str(WIDE_OSC_CASSETTE).expect("strict cassette");
    cassette.validate().expect("bounded cassette");
    let expected = replay_terminal(&cassette, None);
    for (action_index, action) in cassette.actions.iter().enumerate() {
        if let verify::cassette::Action::ChildOutput { bytes_hex } = action {
            let bytes = verify::cassette::decode_hex(bytes_hex).expect("validated child output");
            for boundary in 0..=bytes.len() {
                assert_eq!(
                    replay_terminal(&cassette, Some((action_index, boundary))),
                    expected,
                    "terminal parser diverged at action {action_index} byte boundary {boundary}"
                );
            }
        }
    }
    assert_eq!(expected.cells, cassette.expected.visible_cells);
    assert_eq!(expected.title, cassette.expected.title);
    assert_eq!(expected.rows, cassette.expected.final_rows);
    assert_eq!(expected.columns, cassette.expected.final_columns);
    assert_eq!(expected.pty_writes_hex, cassette.expected.pty_writes_hex);
    assert_eq!(expected.signals, cassette.expected.signals);
    assert_eq!(expected.exit_status, cassette.expected.exit_status);
    let report = fux::parse_agent_report(
        cassette
            .osc_payloads
            .first()
            .expect("OSC payload")
            .as_bytes(),
    )
    .expect("agent OSC");
    assert_eq!(
        format!("{:?}", report.state()).to_ascii_lowercase(),
        cassette.expected.agent_state
    );
}

#[test]
fn terminal_cassette_rejects_unbounded_or_incomplete_actions() {
    use verify::cassette::{Action, Cassette};

    let mut cells: Cassette = serde_json::from_str(WIDE_OSC_CASSETTE).expect("cassette");
    cells.expected.visible_cells = vec!["x".into(); 513];
    assert!(cells.validate().is_err());

    let mut actions: Cassette = serde_json::from_str(WIDE_OSC_CASSETTE).expect("cassette");
    actions.actions.extend((0..512).map(|_| Action::Signal {
        signal: verify::schema::Signal::Term,
    }));
    assert!(actions.validate().is_err());

    let mut no_exit: Cassette = serde_json::from_str(WIDE_OSC_CASSETTE).expect("cassette");
    no_exit
        .actions
        .retain(|action| !matches!(action, Action::Exit { .. }));
    assert!(no_exit.validate().is_err());

    let mut post_exit: Cassette = serde_json::from_str(WIDE_OSC_CASSETTE).expect("cassette");
    post_exit.actions.push(Action::HostInput {
        bytes_hex: "78".into(),
    });
    assert!(post_exit.validate().is_err());

    let mut oversized: Cassette = serde_json::from_str(WIDE_OSC_CASSETTE).expect("cassette");
    oversized.actions.insert(
        0,
        Action::HostInput {
            bytes_hex: "00".repeat(verify::schema::MAX_PAYLOAD_BYTES + 1),
        },
    );
    assert!(oversized.validate().is_err());
}

#[derive(Debug, Eq, PartialEq)]
struct CassetteReplay {
    cells: Vec<String>,
    title: String,
    rows: u16,
    columns: u16,
    pty_writes_hex: Vec<String>,
    signals: Vec<verify::schema::Signal>,
    exit_status: i32,
}

fn replay_terminal(
    cassette: &verify::cassette::Cassette,
    split: Option<(usize, usize)>,
) -> CassetteReplay {
    #[derive(Default)]
    struct Callbacks {
        title: String,
    }
    impl vt100::Callbacks for Callbacks {
        fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
            self.title = String::from_utf8_lossy(title).into_owned();
        }
    }
    let mut parser = vt100::Parser::new_with_callbacks(
        cassette.rows,
        cassette.columns,
        32,
        Callbacks::default(),
    );
    let mut rows = cassette.rows;
    let mut columns = cassette.columns;
    let mut router = fux::host::InputRouter::new(2, 40);
    let mut pty_writes_hex = Vec::new();
    let mut signals = Vec::new();
    let mut exit_status = None;
    for (action_index, action) in cassette.actions.iter().enumerate() {
        match action {
            verify::cassette::Action::ChildOutput { bytes_hex } => {
                let bytes = verify::cassette::decode_hex(bytes_hex)
                    .expect("validated cassette child output");
                if split.is_some_and(|(index, _)| index == action_index) {
                    let boundary = split.map_or(0, |(_, boundary)| boundary);
                    parser.process(bytes.get(..boundary).expect("bounded child head"));
                    parser.process(bytes.get(boundary..).expect("bounded child tail"));
                } else {
                    parser.process(&bytes);
                }
            }
            verify::cassette::Action::HostInput { bytes_hex } => {
                let bytes =
                    verify::cassette::decode_hex(bytes_hex).expect("validated cassette host input");
                let mut forwarded = Vec::new();
                for action in router.feed(&bytes, 0) {
                    assert!(
                        matches!(action, fux::host::Action::Forward(_)),
                        "cassette host input was not ordinary input: {action:?}"
                    );
                    if let fux::host::Action::Forward(bytes) = action {
                        forwarded.extend(bytes);
                    }
                }
                pty_writes_hex.push(verify::transcript::hex(&forwarded));
            }
            verify::cassette::Action::Resize {
                rows: next_rows,
                columns: next_columns,
            } => {
                rows = *next_rows;
                columns = *next_columns;
                parser.screen_mut().set_size(rows, columns);
            }
            verify::cassette::Action::Signal { signal } => signals.push(*signal),
            verify::cassette::Action::Exit { status } => exit_status = Some(*status),
        }
    }
    let pane = fux::state::PaneView::from_vt100(
        parser.screen(),
        parser.callbacks().title.clone(),
        Default::default(),
        0,
    )
    .expect("cassette frame");
    let cells = pane
        .cells
        .into_iter()
        .filter_map(|cell| {
            if cell.text.is_empty() {
                None
            } else {
                Some(cell.text)
            }
        })
        .collect();
    CassetteReplay {
        cells,
        title: pane.title,
        rows,
        columns,
        pty_writes_hex,
        signals,
        exit_status: exit_status.expect("validated cassette exit action"),
    }
}

fn production_command_name(command: &fux::host::Command) -> &'static str {
    use fux::host::Command;
    use fux::state::Direction;
    match command {
        Command::SplitHorizontal => "split_horizontal",
        Command::SplitVertical => "split_vertical",
        Command::Focus(Direction::Left) => "focus_left",
        Command::Focus(Direction::Right) => "focus_right",
        Command::Focus(Direction::Up) => "focus_up",
        Command::Focus(Direction::Down) => "focus_down",
        Command::Close => "close",
        Command::NewPane => "new_pane",
        Command::NewTab => "new_tab",
        Command::NextTab => "next_tab",
        Command::PreviousTab => "previous_tab",
        Command::Zoom => "zoom",
        Command::CopyMode => "copy_mode",
        Command::Detach => "detach",
        Command::WorkspacePicker => "workspace_picker",
        Command::Help => "help",
        Command::TabPicker => "tab-picker",
        Command::RenameTab => "rename-tab",
        Command::ResizeMode => "resize-mode",
        Command::External(_) => "external",
    }
}
