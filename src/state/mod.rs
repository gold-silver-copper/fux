mod diff;
mod layout;
mod types;
mod workspace;

pub use diff::{CellRun, PaneDelta, WorkspaceDiff};
pub use layout::{
    Axis, Direction, LayoutError, LayoutTree, MAX_RATIO, MIN_RATIO, Node, NodeId, RATIO_SCALE,
    Rect, Tab,
};
pub use types::{
    AgentFlags, AgentState, AgentStatus, Cell, CellKind, CellStyle, Color, CopyState, Cursor,
    MAX_CELL_TEXT_BYTES, MAX_CLIPBOARD_BYTES, MAX_DIM, MAX_NAME_BYTES, MAX_PANES, MAX_POPUPS,
    MAX_STATUS_SEGMENTS, MAX_TABS, MAX_TITLE_BYTES, MAX_TOTAL_CELLS, MouseEncoding, MouseMode,
    PaneId, PaneModes, PaneView, PaneViewError, Popup, TabId, WorkspaceMetadata,
};
pub use workspace::{RECEIVE_BUDGET_UNITS, RECV_DECODE_LIMIT, StateError, WorkspaceState};

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::any;
    use std::num::NonZeroU16;

    fn pane(rows: u16, columns: u16, marker: &str) -> PaneView {
        let mut cells = vec![Cell::default(); usize::from(rows) * usize::from(columns)];
        if let Some(cell) = cells.first_mut() {
            cell.text = marker.to_owned();
            cell.kind = CellKind::Text;
        }
        PaneView {
            rows,
            columns,
            cells,
            ..PaneView::default()
        }
    }

    fn workspace() -> WorkspaceState {
        let mut state = WorkspaceState::default();
        assert!(state.insert_pane(PaneId(1), pane(2, 3, "a")).is_ok());
        let tab = Tab {
            id: TabId(1),
            name: "main".into(),
            layout: LayoutTree::new(PaneId(1)),
            focused: PaneId(1),
            zoomed: None,
        };
        assert!(state.replace_tabs(vec![tab], Some(TabId(1))).is_ok());
        state
    }

    #[test]
    fn diff_round_trip_and_cached_budget() {
        let base = workspace();
        let mut target = base.clone();
        assert!(
            target
                .update_pane(PaneId(1), |pane| {
                    if let Some(cell) = pane.cells.get_mut(2) {
                        cell.text = "x".into();
                        cell.kind = CellKind::Text;
                    }
                    pane.modes.bracketed_paste = true;
                    pane.agent.flags.blocker = true;
                })
                .is_ok()
        );
        assert!(
            target
                .update_metadata(|metadata| {
                    metadata.window_title = "fux".into();
                    metadata.generation = 4;
                })
                .is_ok()
        );
        let diff = target.diff_from(&base);
        assert_eq!(
            diff.panes
                .get(&PaneId(1))
                .map(|delta| delta.cell_runs.len()),
            Some(1)
        );
        assert_eq!(
            diff.panes
                .get(&PaneId(1))
                .and_then(|delta| delta.agent_flags),
            Some(AgentFlags {
                idle: false,
                blocker: true,
                working: false
            })
        );
        assert!(diff.panes.get(&PaneId(1)).is_some_and(|delta| {
            delta.agent_id.is_none()
                && delta.agent_state.is_none()
                && delta.agent_sequence.is_none()
                && delta.agent_exited.is_none()
                && delta.agent_message.is_none()
        }));
        let mut replica = base;
        replica.apply(&diff);
        assert_eq!(replica, target);
        assert_eq!(replica.resource_units(), replica.recompute_resource_units());
    }

    #[test]
    fn metadata_and_tab_switch_do_not_emit_cells() {
        let base = workspace();
        let mut target = base.clone();
        assert!(
            target
                .update_metadata(|metadata| metadata.bell_count = 1)
                .is_ok()
        );
        let diff = target.diff_from(&base);
        assert!(diff.panes.is_empty());

        assert!(target.insert_pane(PaneId(2), pane(2, 3, "b")).is_ok());
        let first = target.tabs().first().cloned();
        assert!(first.is_some());
        if let Some(first) = first {
            let second = Tab {
                id: TabId(2),
                name: "other".into(),
                layout: LayoutTree::new(PaneId(2)),
                focused: PaneId(2),
                zoomed: None,
            };
            assert!(
                target
                    .replace_tabs(vec![first, second], Some(TabId(1)))
                    .is_ok()
            );
            let tab_base = target.clone();
            assert!(
                target
                    .replace_tabs(target.tabs().to_vec(), Some(TabId(2)))
                    .is_ok()
            );
            let switch = target.diff_from(&tab_base);
            assert!(switch.panes.is_empty());
            assert_eq!(switch.active_tab, Some(Some(TabId(2))));
        }
    }

    #[test]
    fn pane_removal_round_trips_without_cell_runs() {
        let base = workspace();
        let target = WorkspaceState::default();
        let diff = target.diff_from(&base);
        assert_eq!(diff.removed_panes, vec![PaneId(1)]);
        assert!(diff.panes.is_empty());
        let mut replica = base;
        replica.apply(&diff);
        assert_eq!(replica, target);
    }

    #[test]
    fn serialization_rebuilds_cached_resource_units() {
        let state = workspace();
        let encoded = serde_json::to_vec(&state);
        assert!(encoded.is_ok());
        if let Ok(encoded) = encoded {
            let decoded = serde_json::from_slice::<WorkspaceState>(&encoded);
            assert!(decoded.is_ok());
            if let Ok(decoded) = decoded {
                assert_eq!(decoded.resource_units(), decoded.recompute_resource_units());
                assert_eq!(decoded, state);
            }
        }
    }

    #[test]
    fn resource_units_charge_allocated_blank_cells_and_dynamic_capacity() {
        let mut state = WorkspaceState::default();
        let mut view = pane(64, 64, "");
        if let Some(cell) = view.cells.first_mut() {
            cell.text = String::with_capacity(4096);
            cell.text.push('x');
            cell.kind = CellKind::Text;
        }
        assert!(state.insert_pane(PaneId(1), view).is_ok());
        let cell_storage = 64_usize * 64 * std::mem::size_of::<Cell>();
        assert!(state.resource_units() >= cell_storage.saturating_add(4096));

        let before = state.resource_units();
        assert!(state.insert_pane(PaneId(2), pane(64, 64, "x")).is_ok());
        assert!(state.resource_units() >= before.saturating_add(cell_storage));
        assert_eq!(state.resource_units(), state.recompute_resource_units());
    }

    #[test]
    fn vt100_conversion_keeps_combining_text_in_one_cell() {
        let mut parser = vt100::Parser::new(2, 4, 0);
        parser.process("e\u{301}".as_bytes());
        let converted =
            PaneView::from_vt100(parser.screen(), String::new(), AgentStatus::default(), 0);
        assert!(converted.is_ok());
        if let Ok(converted) = converted {
            assert_eq!(
                converted.cell(0, 0).map(|cell| cell.text.as_str()),
                Some("e\u{301}")
            );
        }
    }

    #[test]
    fn cells_reject_terminal_controls_and_multiple_display_clusters() {
        for text in ["\u{1b}[2J", "\n", "ab", "a\u{85}"] {
            let cell = Cell {
                text: text.to_owned(),
                kind: CellKind::Text,
                style: CellStyle::default(),
            };
            assert!(!cell.valid(), "accepted unsafe cell {text:?}");
        }
        let combining = Cell {
            text: "e\u{301}".to_owned(),
            kind: CellKind::Text,
            style: CellStyle::default(),
        };
        assert!(combining.valid());
        for text in ["🇰🇷", "한"] {
            assert!(
                Cell {
                    text: text.to_owned(),
                    kind: CellKind::WideLeading,
                    style: CellStyle::default(),
                }
                .valid()
            );
        }
        assert!(
            Cell {
                text: "क़".to_owned(),
                kind: CellKind::Text,
                style: CellStyle::default(),
            }
            .valid()
        );
        assert!(
            !Cell {
                text: "界".to_owned(),
                kind: CellKind::Text,
                style: CellStyle::default(),
            }
            .valid()
        );
    }

    #[test]
    fn synchronized_topology_rejects_duplicate_or_unreferenced_panes_and_popup_z() {
        let mut state = workspace();
        assert!(state.validate_complete_topology().is_ok());
        assert!(
            state
                .replace_popups(vec![Popup {
                    pane: PaneId(1),
                    width: 10,
                    height: 5,
                    z_index: 1,
                }])
                .is_ok()
        );
        assert_eq!(
            state.validate_complete_topology(),
            Err(StateError::InvalidLayout)
        );

        let mut state = workspace();
        assert!(state.insert_pane(PaneId(2), pane(1, 1, "x")).is_ok());
        assert_eq!(
            state.validate_complete_topology(),
            Err(StateError::InvalidLayout)
        );
    }

    #[test]
    fn split_geometry_and_direction_are_stable() {
        let half = NonZeroU16::new(RATIO_SCALE / 2);
        assert!(half.is_some());
        let mut tree = LayoutTree::new(PaneId(1));
        if let Some(ratio) = half {
            assert!(
                tree.split(PaneId(1), PaneId(2), Axis::Horizontal, ratio)
                    .is_ok()
            );
        }
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        assert_eq!(
            tree.geometry(area),
            Ok(vec![
                (
                    PaneId(1),
                    Rect {
                        x: 0,
                        y: 0,
                        width: 40,
                        height: 24
                    }
                ),
                (
                    PaneId(2),
                    Rect {
                        x: 40,
                        y: 0,
                        width: 40,
                        height: 24
                    }
                )
            ])
        );
        assert_eq!(
            tree.neighbour(PaneId(1), Direction::Right, area),
            Some(PaneId(2))
        );
        assert_eq!(
            tree.neighbour(PaneId(2), Direction::Left, area),
            Some(PaneId(1))
        );
        assert_eq!(tree.close(PaneId(1)), Ok(Some(PaneId(2))));
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn malformed_cell_runs_are_ignored_without_panicking() {
        let mut state = workspace();
        let mut diff = WorkspaceDiff::default();
        diff.panes.insert(
            PaneId(1),
            PaneDelta {
                cell_runs: vec![CellRun {
                    start: usize::MAX,
                    cells: vec![Cell::default()],
                }],
                ..PaneDelta::default()
            },
        );
        let old = state.clone();
        state.apply(&diff);
        assert_eq!(state, old);
    }

    #[test]
    fn workspace_repaint_converges_after_skipped_snapshots() {
        let mut replica = workspace();
        let mut target = workspace();
        assert!(
            target
                .update_pane(PaneId(1), |pane| {
                    *pane = pane_with_pattern(48, 96);
                    pane.modes.application_cursor = true;
                    pane.agent.flags.working = true;
                })
                .is_ok()
        );
        assert!(
            target
                .update_metadata(|metadata| {
                    metadata.window_title = "lossy workspace".into();
                    metadata.generation = 9;
                })
                .is_ok()
        );
        replica.apply(&target.diff_from(&replica));
        assert_eq!(replica, target);
    }

    fn pane_with_pattern(rows: u16, columns: u16) -> PaneView {
        let cells = (0..usize::from(rows) * usize::from(columns))
            .map(|index| Cell {
                text: char::from(b'a' + u8::try_from(index % 26).unwrap_or_default()).to_string(),
                kind: CellKind::Text,
                style: CellStyle {
                    foreground: Color::Indexed(u8::try_from(index % 255).unwrap_or_default()),
                    bold: index.is_multiple_of(3),
                    ..CellStyle::default()
                },
            })
            .collect();
        PaneView {
            rows,
            columns,
            cells,
            ..PaneView::default()
        }
    }

    #[test]
    fn nested_geometry_matches_the_reference_oracle_cases() {
        let mut tree = LayoutTree::new(PaneId(1));
        let thirty = NonZeroU16::new(3_000);
        let sixty = NonZeroU16::new(6_000);
        let forty = NonZeroU16::new(4_000);
        assert!(thirty.is_some() && sixty.is_some() && forty.is_some());
        if let (Some(thirty), Some(sixty), Some(forty)) = (thirty, sixty, forty) {
            assert!(
                tree.split(PaneId(1), PaneId(2), Axis::Horizontal, thirty)
                    .is_ok()
            );
            assert!(
                tree.split(PaneId(2), PaneId(3), Axis::Vertical, sixty)
                    .is_ok()
            );
            assert!(
                tree.split(PaneId(3), PaneId(4), Axis::Horizontal, forty)
                    .is_ok()
            );
        }
        assert_eq!(
            tree.geometry(Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 40
            }),
            Ok(vec![
                (
                    PaneId(1),
                    Rect {
                        x: 0,
                        y: 0,
                        width: 30,
                        height: 40
                    }
                ),
                (
                    PaneId(2),
                    Rect {
                        x: 30,
                        y: 0,
                        width: 70,
                        height: 24
                    }
                ),
                (
                    PaneId(3),
                    Rect {
                        x: 30,
                        y: 24,
                        width: 28,
                        height: 16
                    }
                ),
                (
                    PaneId(4),
                    Rect {
                        x: 58,
                        y: 24,
                        width: 42,
                        height: 16
                    }
                ),
            ])
        );
        let area = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        };
        assert_eq!(
            tree.neighbour(PaneId(1), Direction::Right, area),
            Some(PaneId(2))
        );
        assert_eq!(
            tree.neighbour(PaneId(3), Direction::Right, area),
            Some(PaneId(4))
        );
        assert_eq!(
            tree.neighbour(PaneId(4), Direction::Up, area),
            Some(PaneId(2))
        );
    }

    #[test]
    fn close_focus_prefers_larger_directional_overlap() {
        let half = NonZeroU16::new(5_000);
        let small = NonZeroU16::new(2_000);
        assert!(half.is_some() && small.is_some());
        let mut tree = LayoutTree::new(PaneId(1));
        if let (Some(half), Some(small)) = (half, small) {
            assert!(
                tree.split(PaneId(1), PaneId(2), Axis::Horizontal, half)
                    .is_ok()
            );
            assert!(
                tree.split(PaneId(2), PaneId(3), Axis::Vertical, small)
                    .is_ok()
            );
        }
        assert_eq!(tree.close(PaneId(1)), Ok(Some(PaneId(3))));
    }

    proptest::proptest! {
        #[test]
        fn random_layout_edits_preserve_invariants(operations in proptest::collection::vec(0_u8..4, 0..80)) {
            let ratio = NonZeroU16::new(RATIO_SCALE / 2);
            proptest::prop_assert!(ratio.is_some());
            let mut tree = LayoutTree::new(PaneId(0));
            let mut next = 1_u32;
            for operation in operations {
                let leaves = tree.leaves();
                let target = leaves.get(usize::from(operation) % leaves.len()).copied();
                if let (Some(target), Some(ratio)) = (target, ratio) {
                    match operation % 4 {
                        0 if leaves.len() < 20 => { let _ = tree.split(target, PaneId(next), Axis::Horizontal, ratio); next = next.saturating_add(1); }
                        1 if leaves.len() < 20 => { let _ = tree.split(target, PaneId(next), Axis::Vertical, ratio); next = next.saturating_add(1); }
                        2 if leaves.len() > 1 => { let _ = tree.close(target); }
                        _ => { let _ = tree.resize(target, i16::from(operation).saturating_sub(2)); }
                    }
                }
                proptest::prop_assert!(tree.validate().is_ok());
            }
        }


        #[test]
        fn arbitrary_malformed_topology_operations_never_panic(
            raw in proptest::collection::vec((0_u8..3, any::<u32>(), any::<u32>(), any::<u16>()), 0..80),
            root in proptest::option::of(any::<u32>()),
            free in proptest::collection::vec(any::<u32>(), 0..80),
        ) {
            let nodes = raw.into_iter().enumerate().map(|(index, (tag, first, second, ratio))| {
                match tag {
                    0 => None,
                    1 => Some(Node::Leaf(PaneId(u32::try_from(index).unwrap_or_default()))),
                    _ => NonZeroU16::new(ratio).map(|ratio| Node::Split { axis: Axis::Horizontal, ratio, first: NodeId(first), second: NodeId(second) }),
                }
            }).collect();
            let tree = LayoutTree::from_raw(nodes, root.map(NodeId), free.into_iter().map(NodeId).collect());
            let area = Rect { x: 0, y: 0, width: 100, height: 40 };
            let _ = tree.validate();
            let _ = tree.geometry(area);
            let _ = tree.neighbour(PaneId(0), Direction::Right, area);
            let _ = tree.leaves();
            if let Some(ratio) = NonZeroU16::new(RATIO_SCALE / 2) {
                let mut edited = tree.clone();
                let _ = edited.split(PaneId(0), PaneId(u32::MAX), Axis::Horizontal, ratio);
                let mut edited = tree.clone();
                let _ = edited.close(PaneId(0));
                let mut edited = tree.clone();
                let _ = edited.swap(PaneId(0), PaneId(1));
                let mut edited = tree;
                let _ = edited.resize(PaneId(0), 100);
            }
        }


        #[test]
        fn arbitrary_state_edits_obey_round_trip_and_resource_law(
            edits in proptest::collection::vec((0_usize..6, any::<u8>(), any::<bool>()), 0..100),
        ) {
            let base = workspace();
            let mut target = base.clone();
            for (index, byte, flag) in edits {
                let _ = target.update_pane(PaneId(1), |pane| {
                    if let Some(cell) = pane.cells.get_mut(index % 6) {
                        cell.text = char::from(b'a' + (byte % 26)).to_string();
                        cell.kind = CellKind::Text;
                        cell.style.bold = flag;
                    }
                    pane.modes.bracketed_paste = flag;
                    pane.agent.flags.idle = flag;
                });
                let _ = target.update_metadata(|metadata| {
                    metadata.generation = metadata.generation.saturating_add(1);
                    metadata.window_title = format!("title-{byte}");
                });
                proptest::prop_assert_eq!(target.resource_units(), target.recompute_resource_units());
            }
            let mut replica = base;
            replica.apply(&target.diff_from(&replica));
            proptest::prop_assert_eq!(replica, target);
        }


        #[test]
        fn arbitrary_malformed_diffs_never_panic(
            runs in proptest::collection::vec((any::<usize>(), proptest::collection::vec(any::<u8>(), 0..40)), 0..80),
        ) {
            let mut state = workspace();
            let cell_runs = runs.into_iter().map(|(start, bytes)| CellRun {
                start,
                cells: bytes.into_iter().map(|byte| Cell {
                    text: char::from(b'a' + (byte % 26)).to_string(),
                    kind: CellKind::Text,
                    style: CellStyle::default(),
                }).collect(),
            }).collect();
            let mut diff = WorkspaceDiff::default();
            diff.panes.insert(PaneId(1), PaneDelta { cell_runs, ..PaneDelta::default() });
            state.apply(&diff);
            proptest::prop_assert!(state.validate().is_ok());
            proptest::prop_assert_eq!(state.resource_units(), state.recompute_resource_units());
        }
    }
}
