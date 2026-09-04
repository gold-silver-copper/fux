use fux::state::{Cell, PaneId, PaneView, WorkspaceState};

pub fn conservative_minimum(state: &WorkspaceState) -> usize {
    let panes = state.panes().values().fold(0usize, |total, pane| {
        total
            .saturating_add(std::mem::size_of::<PaneId>())
            .saturating_add(std::mem::size_of::<PaneView>())
            .saturating_add(pane.cells.len().saturating_mul(std::mem::size_of::<Cell>()))
            .saturating_add(pane.cells.iter().map(|cell| cell.text.len()).sum::<usize>())
            .saturating_add(pane.title.len())
            .saturating_add(pane.agent.id.as_ref().map_or(0, String::len))
            .saturating_add(pane.agent.message.as_ref().map_or(0, String::len))
            .saturating_add(
                pane.wrapped_rows
                    .len()
                    .saturating_mul(std::mem::size_of::<bool>()),
            )
    });
    std::mem::size_of::<WorkspaceState>().saturating_add(panes)
}
