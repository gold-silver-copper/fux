use super::{
    AgentFlags, AgentState, Cell, CopyState, Cursor, PaneId, PaneModes, PaneView, Popup, Tab,
    TabId, WorkspaceMetadata, WorkspaceState,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CellRun {
    pub start: usize,
    pub cells: Vec<Cell>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PaneDelta {
    pub full: Option<PaneView>,
    pub cell_runs: Vec<CellRun>,
    pub cursor: Option<Cursor>,
    pub modes: Option<PaneModes>,
    pub title: Option<String>,
    pub agent_id: Option<Option<String>>,
    pub agent_state: Option<AgentState>,
    pub agent_flags: Option<AgentFlags>,
    pub agent_sequence: Option<u64>,
    pub agent_exited: Option<bool>,
    pub agent_message: Option<Option<String>>,
    pub viewport_offset: Option<u32>,
    pub copy: Option<CopyState>,
    pub wrapped_rows: Option<Vec<bool>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceDiff {
    pub removed_panes: Vec<PaneId>,
    pub panes: BTreeMap<PaneId, PaneDelta>,
    pub tabs: Option<Vec<Tab>>,
    pub active_tab: Option<Option<TabId>>,
    pub popups: Option<Vec<Popup>>,
    pub metadata: Option<WorkspaceMetadata>,
}

impl WorkspaceDiff {
    #[must_use]
    pub fn between(base: &WorkspaceState, target: &WorkspaceState) -> Self {
        let removed_panes = base
            .panes()
            .keys()
            .filter(|id| !target.panes().contains_key(id))
            .copied()
            .collect();
        let mut panes = BTreeMap::new();
        for (id, pane) in target.panes() {
            let delta = match base.pane(*id) {
                None => PaneDelta {
                    full: Some(pane.clone()),
                    ..PaneDelta::default()
                },
                Some(old)
                    if old.rows != pane.rows
                        || old.columns != pane.columns
                        || old.cells.len() != pane.cells.len() =>
                {
                    PaneDelta {
                        full: Some(pane.clone()),
                        ..PaneDelta::default()
                    }
                }
                Some(old) => pane_delta(old, pane),
            };
            if delta != PaneDelta::default() {
                panes.insert(*id, delta);
            }
        }
        Self {
            removed_panes,
            panes,
            tabs: (base.tabs() != target.tabs()).then(|| target.tabs().to_vec()),
            active_tab: (base.active_tab() != target.active_tab()).then(|| target.active_tab()),
            popups: (base.popups() != target.popups()).then(|| target.popups().to_vec()),
            metadata: (base.metadata() != target.metadata()).then(|| target.metadata().clone()),
        }
    }

    pub(crate) fn apply_to(&self, state: &mut WorkspaceState) {
        let mut panes = state.panes().clone();
        let removed: BTreeSet<_> = self.removed_panes.iter().copied().collect();
        for id in removed {
            panes.remove(&id);
        }
        for (id, delta) in &self.panes {
            if let Some(full) = &delta.full {
                if full.valid() {
                    panes.insert(*id, full.clone());
                }
                continue;
            }
            let Some(pane) = panes.get_mut(id) else {
                continue;
            };
            apply_pane_delta(pane, delta);
        }
        let tabs = self.tabs.as_deref().unwrap_or(state.tabs()).to_vec();
        let active = self.active_tab.unwrap_or(state.active_tab());
        let popups = self.popups.as_deref().unwrap_or(state.popups()).to_vec();
        let metadata = self.metadata.as_ref().unwrap_or(state.metadata()).clone();
        let candidate = WorkspaceState::from_parts(tabs, active, panes, popups, metadata);
        if candidate.validate().is_ok() && candidate.validate_complete_topology().is_ok() {
            *state = candidate;
        }
    }
}

fn pane_delta(old: &PaneView, new: &PaneView) -> PaneDelta {
    PaneDelta {
        full: None,
        cell_runs: changed_runs(&old.cells, &new.cells),
        cursor: (old.cursor != new.cursor).then_some(new.cursor),
        modes: (old.modes != new.modes).then_some(new.modes),
        title: (old.title != new.title).then(|| new.title.clone()),
        agent_id: (old.agent.id != new.agent.id).then(|| new.agent.id.clone()),
        agent_state: (old.agent.state != new.agent.state).then_some(new.agent.state),
        agent_flags: (old.agent.flags != new.agent.flags).then_some(new.agent.flags),
        agent_sequence: (old.agent.sequence != new.agent.sequence).then_some(new.agent.sequence),
        agent_exited: (old.agent.exited != new.agent.exited).then_some(new.agent.exited),
        agent_message: (old.agent.message != new.agent.message).then(|| new.agent.message.clone()),
        viewport_offset: (old.viewport_offset != new.viewport_offset)
            .then_some(new.viewport_offset),
        copy: (old.copy != new.copy).then_some(new.copy),
        wrapped_rows: (old.wrapped_rows != new.wrapped_rows).then(|| new.wrapped_rows.clone()),
    }
}

fn changed_runs(old: &[Cell], new: &[Cell]) -> Vec<CellRun> {
    let mut output = Vec::new();
    let mut index = 0;
    while index < new.len() {
        if old.get(index) == new.get(index) {
            index += 1;
            continue;
        }
        let start = index;
        let mut cells = Vec::new();
        while index < new.len() && old.get(index) != new.get(index) {
            if let Some(cell) = new.get(index) {
                cells.push(cell.clone());
            }
            index += 1;
        }
        output.push(CellRun { start, cells });
    }
    output
}

fn apply_pane_delta(pane: &mut PaneView, delta: &PaneDelta) {
    for run in &delta.cell_runs {
        let Some(end) = run.start.checked_add(run.cells.len()) else {
            continue;
        };
        let Some(target) = pane.cells.get_mut(run.start..end) else {
            continue;
        };
        target.clone_from_slice(&run.cells);
    }
    if let Some(value) = delta.cursor {
        pane.cursor = value;
    }
    if let Some(value) = delta.modes {
        pane.modes = value;
    }
    if let Some(value) = &delta.title {
        pane.title.clone_from(value);
    }
    if let Some(value) = &delta.agent_id {
        pane.agent.id.clone_from(value);
    }
    if let Some(value) = delta.agent_state {
        pane.agent.state = value;
    }
    if let Some(value) = delta.agent_flags {
        pane.agent.flags = value;
    }
    if let Some(value) = delta.agent_sequence {
        pane.agent.sequence = value;
    }
    if let Some(value) = delta.agent_exited {
        pane.agent.exited = value;
    }
    if let Some(value) = &delta.agent_message {
        pane.agent.message.clone_from(value);
    }
    if let Some(value) = delta.viewport_offset {
        pane.viewport_offset = value;
    }
    if let Some(value) = delta.copy {
        pane.copy = value;
    }
    if let Some(value) = &delta.wrapped_rows
        && value.len() == usize::from(pane.rows)
    {
        pane.wrapped_rows.clone_from(value);
    }
}
