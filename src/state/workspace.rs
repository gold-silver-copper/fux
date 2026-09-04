use super::{
    Cell, LayoutError, MAX_CLIPBOARD_BYTES, MAX_NAME_BYTES, MAX_PANES, MAX_POPUPS,
    MAX_STATUS_SEGMENTS, MAX_TABS, MAX_TITLE_BYTES, MAX_TOTAL_CELLS, PaneId, PaneView, Popup, Tab,
    TabId, WorkspaceDiff, WorkspaceMetadata,
};
use koh::ssp::SyncState;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// One full maximum-size pane with bounded cell text and metadata fits under koh's global 16 MiB
/// inflated-message ceiling; using that ceiling avoids rejecting a legitimate full repaint.
pub const RECV_DECODE_LIMIT: usize = 16 << 20;
pub const RECEIVE_BUDGET_UNITS: usize = 4 * RECV_DECODE_LIMIT;

#[derive(Clone, Debug, Serialize)]
pub struct WorkspaceState {
    tabs: Vec<Tab>,
    active_tab: Option<TabId>,
    panes: BTreeMap<PaneId, PaneView>,
    popups: Vec<Popup>,
    metadata: WorkspaceMetadata,
    #[serde(skip)]
    resource_units: usize,
}

#[derive(Deserialize)]
struct WorkspaceWire {
    tabs: Vec<Tab>,
    active_tab: Option<TabId>,
    panes: BTreeMap<PaneId, PaneView>,
    popups: Vec<Popup>,
    metadata: WorkspaceMetadata,
}

impl<'de> Deserialize<'de> for WorkspaceState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = WorkspaceWire::deserialize(deserializer)?;
        let value = Self::from_parts(
            wire.tabs,
            wire.active_tab,
            wire.panes,
            wire.popups,
            wire.metadata,
        );
        value
            .validate()
            .and_then(|()| value.validate_complete_topology())
            .map_err(|_| serde::de::Error::custom("invalid workspace state"))?;
        Ok(value)
    }
}

impl PartialEq for WorkspaceState {
    fn eq(&self, other: &Self) -> bool {
        self.tabs == other.tabs
            && self.active_tab == other.active_tab
            && self.panes == other.panes
            && self.popups == other.popups
            && self.metadata == other.metadata
    }
}
impl Eq for WorkspaceState {}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self::from_parts(
            Vec::new(),
            None,
            BTreeMap::new(),
            Vec::new(),
            WorkspaceMetadata::default(),
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateError {
    Bounds,
    InvalidLayout,
    MissingTab,
    MissingPane,
    DuplicateId,
}
impl From<LayoutError> for StateError {
    fn from(_: LayoutError) -> Self {
        Self::InvalidLayout
    }
}

impl WorkspaceState {
    pub(crate) fn from_parts(
        tabs: Vec<Tab>,
        active_tab: Option<TabId>,
        panes: BTreeMap<PaneId, PaneView>,
        popups: Vec<Popup>,
        metadata: WorkspaceMetadata,
    ) -> Self {
        let mut value = Self {
            tabs,
            active_tab,
            panes,
            popups,
            metadata,
            resource_units: 0,
        };
        value.refresh_resource_units();
        value
    }
    #[must_use]
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }
    #[must_use]
    pub const fn active_tab(&self) -> Option<TabId> {
        self.active_tab
    }
    #[must_use]
    pub fn panes(&self) -> &BTreeMap<PaneId, PaneView> {
        &self.panes
    }
    #[must_use]
    pub fn pane(&self, id: PaneId) -> Option<&PaneView> {
        self.panes.get(&id)
    }
    #[must_use]
    pub fn popups(&self) -> &[Popup] {
        &self.popups
    }
    #[must_use]
    pub const fn metadata(&self) -> &WorkspaceMetadata {
        &self.metadata
    }

    pub fn replace_tabs(
        &mut self,
        tabs: Vec<Tab>,
        active_tab: Option<TabId>,
    ) -> Result<(), StateError> {
        let old_tabs = std::mem::replace(&mut self.tabs, tabs);
        let old_active = self.active_tab;
        self.active_tab = active_tab;
        if let Err(error) = self.validate() {
            self.tabs = old_tabs;
            self.active_tab = old_active;
            return Err(error);
        }
        self.refresh_resource_units();
        Ok(())
    }
    pub fn insert_pane(&mut self, id: PaneId, pane: PaneView) -> Result<(), StateError> {
        let total_cells = self
            .panes
            .values()
            .map(|value| value.cells.len())
            .sum::<usize>();
        if self.panes.len() >= MAX_PANES
            || total_cells.saturating_add(pane.cells.len()) > MAX_TOTAL_CELLS
            || !pane.valid()
            || self.panes.contains_key(&id)
        {
            return Err(StateError::Bounds);
        }
        self.panes.insert(id, pane);
        self.refresh_resource_units();
        Ok(())
    }
    pub fn update_pane(
        &mut self,
        id: PaneId,
        update: impl FnOnce(&mut PaneView),
    ) -> Result<(), StateError> {
        let current_total = self
            .panes
            .values()
            .map(|value| value.cells.len())
            .sum::<usize>();
        let pane = self.panes.get_mut(&id).ok_or(StateError::MissingPane)?;
        let old = pane.clone();
        update(pane);
        if !pane.valid() {
            *pane = old;
            return Err(StateError::Bounds);
        }
        if current_total
            .saturating_sub(old.cells.len())
            .saturating_add(pane.cells.len())
            > MAX_TOTAL_CELLS
        {
            *pane = old;
            return Err(StateError::Bounds);
        }
        self.refresh_resource_units();
        Ok(())
    }
    pub fn remove_pane(&mut self, id: PaneId) -> Result<PaneView, StateError> {
        if self.tabs.iter().any(|tab| tab.layout.contains(id))
            || self.popups.iter().any(|popup| popup.pane == id)
        {
            return Err(StateError::InvalidLayout);
        }
        let pane = self.panes.remove(&id).ok_or(StateError::MissingPane)?;
        self.refresh_resource_units();
        Ok(pane)
    }
    pub fn replace_popups(&mut self, popups: Vec<Popup>) -> Result<(), StateError> {
        let old = std::mem::replace(&mut self.popups, popups);
        if let Err(error) = self.validate() {
            self.popups = old;
            return Err(error);
        }
        self.refresh_resource_units();
        Ok(())
    }
    pub fn update_metadata(
        &mut self,
        update: impl FnOnce(&mut WorkspaceMetadata),
    ) -> Result<(), StateError> {
        let old = self.metadata.clone();
        update(&mut self.metadata);
        if let Err(error) = self.validate_metadata() {
            self.metadata = old;
            return Err(error);
        }
        self.refresh_resource_units();
        Ok(())
    }

    pub fn validate(&self) -> Result<(), StateError> {
        if self.tabs.len() > MAX_TABS
            || self.panes.len() > MAX_PANES
            || self.popups.len() > MAX_POPUPS
        {
            return Err(StateError::Bounds);
        }
        let mut tab_ids = BTreeSet::new();
        let mut layout_panes = BTreeSet::new();
        for tab in &self.tabs {
            tab.validate()?;
            if !tab_ids.insert(tab.id) {
                return Err(StateError::DuplicateId);
            }
            for pane in tab.layout.leaves() {
                if !layout_panes.insert(pane) || !self.panes.contains_key(&pane) {
                    return Err(StateError::InvalidLayout);
                }
            }
        }
        if self.active_tab.is_some_and(|id| !tab_ids.contains(&id))
            || (self.tabs.is_empty() != self.active_tab.is_none())
        {
            return Err(StateError::MissingTab);
        }
        if self.panes.values().any(|pane| !pane.valid()) {
            return Err(StateError::Bounds);
        }
        if self
            .panes
            .values()
            .map(|pane| pane.cells.len())
            .sum::<usize>()
            > MAX_TOTAL_CELLS
        {
            return Err(StateError::Bounds);
        }
        for popup in &self.popups {
            if popup.width == 0
                || popup.height == 0
                || popup.width > super::MAX_DIM
                || popup.height > super::MAX_DIM
                || !self.panes.contains_key(&popup.pane)
            {
                return Err(StateError::Bounds);
            }
        }
        self.validate_metadata()
    }

    pub(crate) fn validate_complete_topology(&self) -> Result<(), StateError> {
        let mut referenced = BTreeSet::new();
        for pane in self.tabs.iter().flat_map(|tab| tab.layout.leaves()) {
            if !referenced.insert(pane) {
                return Err(StateError::InvalidLayout);
            }
        }
        let mut z_indexes = BTreeSet::new();
        for popup in &self.popups {
            if !referenced.insert(popup.pane) || !z_indexes.insert(popup.z_index) {
                return Err(StateError::InvalidLayout);
            }
        }
        if self
            .panes
            .iter()
            .any(|(id, pane)| !referenced.contains(id) && pane.exit_status.is_none())
        {
            return Err(StateError::InvalidLayout);
        }
        Ok(())
    }
    pub fn recompute_resource_units(&self) -> usize {
        let pane_units = self.panes.values().fold(0usize, |total, pane| {
            total
                .saturating_add(std::mem::size_of::<PaneId>())
                .saturating_add(std::mem::size_of::<PaneView>())
                .saturating_add(
                    pane.cells
                        .capacity()
                        .saturating_mul(std::mem::size_of::<Cell>()),
                )
                .saturating_add(
                    pane.cells
                        .iter()
                        .map(|cell| cell.text.capacity())
                        .sum::<usize>(),
                )
                .saturating_add(pane.title.capacity())
                .saturating_add(pane.agent.id.as_ref().map_or(0, String::capacity))
                .saturating_add(pane.agent.message.as_ref().map_or(0, String::capacity))
                .saturating_add(
                    pane.wrapped_rows
                        .capacity()
                        .saturating_mul(std::mem::size_of::<bool>()),
                )
                .saturating_add(3 * std::mem::size_of::<usize>())
        });
        let tab_units = self
            .tabs
            .iter()
            .map(|tab| {
                tab.name
                    .capacity()
                    .saturating_add(tab.layout.allocation_units())
            })
            .sum::<usize>();
        let metadata_units = self
            .metadata
            .window_title
            .capacity()
            .saturating_add(self.metadata.clipboard_base64.capacity())
            .saturating_add(
                self.metadata
                    .status
                    .iter()
                    .map(|(key, value)| {
                        std::mem::size_of::<(String, String)>()
                            .saturating_add(key.capacity())
                            .saturating_add(value.capacity())
                            .saturating_add(3 * std::mem::size_of::<usize>())
                    })
                    .sum::<usize>(),
            );
        std::mem::size_of::<Self>()
            .saturating_add(pane_units)
            .saturating_add(tab_units)
            .saturating_add(
                self.tabs
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Tab>()),
            )
            .saturating_add(metadata_units)
            .saturating_add(self.popups.capacity() * std::mem::size_of::<Popup>())
    }
    fn refresh_resource_units(&mut self) {
        self.resource_units = self.recompute_resource_units();
    }
    fn validate_metadata(&self) -> Result<(), StateError> {
        if self.metadata.status.len() > MAX_STATUS_SEGMENTS
            || self.metadata.window_title.len() > MAX_TITLE_BYTES
            || self.metadata.clipboard_base64.len() > MAX_CLIPBOARD_BYTES
            || !self
                .metadata
                .clipboard_base64
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
            || self
                .metadata
                .status
                .iter()
                .any(|(key, value)| key.len() > MAX_NAME_BYTES || value.len() > MAX_TITLE_BYTES)
        {
            Err(StateError::Bounds)
        } else {
            Ok(())
        }
    }
}

impl SyncState for WorkspaceState {
    type Diff = WorkspaceDiff;
    const RECV_DECODE_LIMIT: usize = RECV_DECODE_LIMIT;
    const RECEIVE_BUDGET_UNITS: usize = RECEIVE_BUDGET_UNITS;
    fn resource_units(&self) -> usize {
        self.resource_units
    }
    fn diff_from(&self, base: &Self) -> Self::Diff {
        WorkspaceDiff::between(base, self)
    }
    fn apply(&mut self, diff: &Self::Diff) {
        diff.apply_to(self);
    }
}
