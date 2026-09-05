#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use fux::host::{Action, InputRouter};
use fux::state::{
    Axis, Cell, CellKind, LayoutTree, PaneId, PaneView, RATIO_SCALE, Rect, Tab, TabId,
    WorkspaceState,
};
use std::num::NonZeroU16;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick(&mut self, limit: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(limit.max(1)).unwrap_or(1)).unwrap_or(0)
    }
}

fn pane(marker: u8) -> PaneView {
    let mut cells = vec![Cell::default(); 24];
    cells[0].text = char::from(b'a' + marker % 26).to_string();
    cells[0].kind = CellKind::Text;
    PaneView {
        rows: 4,
        columns: 6,
        cells,
        ..PaneView::default()
    }
}

fn coalesce_forward(actions: Vec<Action>) -> Vec<Action> {
    let mut result = Vec::new();
    for action in actions {
        match action {
            Action::Forward(bytes) => match result.last_mut() {
                Some(Action::Forward(previous)) => previous.extend(bytes),
                _ => result.push(Action::Forward(bytes)),
            },
            other => result.push(other),
        }
    }
    result
}

#[test]
fn deterministic_full_workspace_mutations_preserve_diff_and_resource_laws() {
    // Phase I chaos: randomized layout/state edits preserve validation, diff/apply and O(1) budget laws.
    let mut rng = Rng(0x00F0_7877_CAFE_BABE);
    let mut next_pane = 2u32;
    let mut state = WorkspaceState::default();
    state.insert_pane(PaneId(1), pane(1)).expect("initial pane");
    let mut tab = Tab {
        id: TabId(1),
        name: "main".into(),
        layout: LayoutTree::new(PaneId(1)),
        focused: PaneId(1),
        zoomed: None,
    };
    state
        .replace_tabs(vec![tab.clone()], Some(TabId(1)))
        .expect("initial tab");
    for step in 0..2_000 {
        let before = state.clone();
        let mut removed_pane = None;
        match rng.pick(5) {
            0 if tab.layout.leaves().len() < 24 => {
                let leaves = tab.layout.leaves();
                let target = leaves[rng.pick(leaves.len())];
                let id = PaneId(next_pane);
                next_pane += 1;
                let ratio = NonZeroU16::new(
                    (RATIO_SCALE / 4) + u16::try_from(rng.pick(5_000)).unwrap_or(0),
                )
                .expect("ratio");
                if tab
                    .layout
                    .split(
                        target,
                        id,
                        if rng.pick(2) == 0 {
                            Axis::Horizontal
                        } else {
                            Axis::Vertical
                        },
                        ratio,
                    )
                    .is_ok()
                {
                    state
                        .insert_pane(id, pane(u8::try_from(step % 26).unwrap_or(0)))
                        .expect("insert");
                }
            }
            1 if tab.layout.leaves().len() > 1 => {
                let leaves = tab.layout.leaves();
                let removed = leaves[rng.pick(leaves.len())];
                if let Ok(Some(focus)) = tab.layout.close(removed) {
                    tab.focused = focus;
                    removed_pane = Some(removed);
                }
            }
            2 => {
                let leaves = tab.layout.leaves();
                if leaves.len() > 1 {
                    let first = leaves[rng.pick(leaves.len())];
                    let second = leaves[rng.pick(leaves.len())];
                    let _ = tab.layout.swap(first, second);
                }
            }
            3 => {
                let leaves = tab.layout.leaves();
                let id = leaves[rng.pick(leaves.len())];
                let delta = i16::try_from(rng.pick(801)).unwrap_or(0) - 400;
                let _ = tab.layout.resize(id, delta);
            }
            _ => {
                let leaves = tab.layout.leaves();
                let id = leaves[rng.pick(leaves.len())];
                state
                    .update_pane(id, |pane| {
                        pane.agent.sequence = pane.agent.sequence.saturating_add(1);
                        pane.agent.flags.working = !pane.agent.flags.working;
                    })
                    .expect("update pane");
            }
        }
        state
            .replace_tabs(vec![tab.clone()], Some(TabId(1)))
            .expect("replace tab");
        if let Some(removed) = removed_pane {
            state.remove_pane(removed).expect("remove closed pane");
        }
        assert!(tab.layout.validate().is_ok(), "step {step}");
        assert!(
            tab.layout
                .geometry(Rect {
                    x: 0,
                    y: 0,
                    width: 160,
                    height: 60
                })
                .is_ok()
        );
        assert!(state.validate().is_ok(), "step {step}");
        assert_eq!(state.resource_units(), state.recompute_resource_units());
        let mut replica = before;
        let diff = state.diff_from(&replica);
        replica.apply(&diff);
        assert_eq!(replica, state, "step {step}");
    }
}

#[test]
fn deterministic_router_chunk_chaos_matches_unsplit_input() {
    // Phase I chaos: arbitrary chunking cannot transform or reorder router actions.
    let cases: &[&[u8]] = &[
        b"text\x01|more",
        b"\x1b[200~paste\x01x\x1b[201~",
        b"\x1b[<0;12;8Mtail",
        b"\x1b[unfinished",
    ];
    let mut rng = Rng(0x0005_17EA_11CE);
    for _ in 0..1_000 {
        let input = cases[rng.pick(cases.len())];
        let mut whole = InputRouter::new(1, 25);
        let mut expected = whole.feed(input, 0);
        expected.extend(whole.flush_timeout(30));
        let mut split = InputRouter::new(1, 25);
        let mut actual: Vec<Action> = Vec::new();
        let mut offset = 0;
        while offset < input.len() {
            let end = (offset + 1 + rng.pick(4)).min(input.len());
            actual.extend(split.feed(input.get(offset..end).unwrap_or_default(), 0));
            offset = end;
        }
        actual.extend(split.flush_timeout(30));
        assert_eq!(coalesce_forward(actual), coalesce_forward(expected));
    }
}
