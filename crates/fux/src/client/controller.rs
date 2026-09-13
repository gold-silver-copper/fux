//! Viewer-local interaction modes: history/copy, tab and workspace choosers, rename, confirmed
//! closes, repeated resize and workspace naming. Transient state never leaves this process.

use super::copy::{CopyKey, CopyOutcome, CopySession};
use super::hints::HintPanel;
use super::interaction::{LayoutMode, MAX_TEXT_BYTES, Mode, TabChoice, step};
use crate::commands::Action;
use crate::ids::{PaneId, TabId};
use crate::proto::attach::{MouseEvent, ViewReply};
use crate::proto::control::{PaneDestination, Request};
use crate::view::{Frame, MouseMode};

fn workspace_choice_label(entry: &crate::proto::control::WorkspaceRoute) -> String {
    entry.label.as_ref().map_or_else(
        || entry.name.clone(),
        |label| format!("{label} ({})", entry.name),
    )
}

const MAX_HISTORY_BYTES: usize = 64 * 1024 * 1024;
pub use super::interaction::RESIZE_STEP;

/// How long a bar notice stays without a key press.
pub const NOTICE_TTL: std::time::Duration = std::time::Duration::from_secs(2);

pub struct Controller {
    pending_action: Option<(Action, Frame)>,
    waiting_hint: Option<HintPanel>,
    forwarded_mouse: Option<MouseEvent>,
    capture: super::capture::Capture,
    entry_regions: Vec<(ratatui_core::layout::Rect, usize)>,
    panel_bounds: Option<ratatui_core::layout::Rect>,
    tab_regions: Vec<(ratatui_core::layout::Rect, TabId)>,
    drag: Option<super::drag::Drag>,
    mode: Mode,
    escape: Vec<u8>,
    utf8: Vec<u8>,
    paste: bool,
    histories: super::history::Histories,
    interaction_epoch: u64,
    error: Option<String>,
    info: Option<String>,
    notice_since: Option<std::time::Instant>,
    copied: Option<String>,
    workspaces_enabled: bool,
    loading_input: Vec<u8>,
    manager_request: Option<crate::daemon::ManagerRequest>,
}

/// What a mouse report should do.
pub enum MouseDisposition {
    Request(Request),
    /// Handled locally (history, selection); nothing goes to the server.
    Local,
    /// Forward to the server, which focuses or re-encodes for the application.
    Forward,
    /// Consumed and dropped (a mode that ignores the mouse).
    Ignore,
}

impl Controller {
    #[must_use]
    pub fn new(workspaces_enabled: bool) -> Self {
        Self {
            pending_action: None,
            waiting_hint: None,
            forwarded_mouse: None,
            capture: super::capture::Capture::default(),
            entry_regions: Vec::new(),
            panel_bounds: None,
            tab_regions: Vec::new(),
            drag: None,
            mode: Mode::Pane,
            escape: Vec::new(),
            utf8: Vec::new(),
            paste: false,
            histories: super::history::Histories::default(),
            interaction_epoch: 0,
            error: None,
            info: None,
            notice_since: None,
            copied: None,
            workspaces_enabled,
            loading_input: Vec::new(),
            manager_request: None,
        }
    }

    pub fn interaction_epoch(&self) -> u64 {
        self.interaction_epoch
    }

    pub fn workspaces_loaded_for(
        &mut self,
        epoch: u64,
        result: anyhow::Result<Vec<crate::proto::control::WorkspaceRoute>>,
        current: &str,
    ) -> bool {
        if epoch != self.interaction_epoch {
            return false;
        }
        self.workspaces_loaded(result, current);
        true
    }

    pub fn take_forwarded_mouse(&mut self) -> Option<MouseEvent> {
        self.forwarded_mouse.take()
    }

    pub fn history_pending(&self, request: u64, pane: PaneId) -> bool {
        self.histories.pending(request, pane)
            || matches!(&self.mode, Mode::Copy(copy) if copy.pending_matches(request, pane))
    }

    pub fn take_action(&mut self) -> Option<(Action, Frame)> {
        self.pending_action.take()
    }

    pub fn active(&self) -> bool {
        self.drag.is_some() || !matches!(self.mode, Mode::Pane)
    }

    pub fn set_tab_regions(&mut self, regions: Vec<(ratatui_core::layout::Rect, TabId)>) -> bool {
        let cancelled = self.tab_regions != regions && self.drag.is_some();
        if cancelled {
            self.end_interaction();
            self.report_error("Tab bar changed; drag cancelled.");
        }
        self.tab_regions = regions;
        cancelled
    }

    pub fn set_regions(&mut self, regions: super::render::HitRegions) -> bool {
        self.entry_regions = regions.entries;
        self.panel_bounds = regions.panel;
        self.set_tab_regions(regions.tabs)
    }

    /// A cancelled mode still owns an unfinished paste or sequence; its tail must never be
    /// reinterpreted as commands or forwarded to a pane.
    pub fn owns_input(&self) -> bool {
        self.active() || self.paste || !self.escape.is_empty() || !self.utf8.is_empty()
    }

    pub fn in_copy(&self) -> bool {
        matches!(self.mode, Mode::Copy(_))
    }

    pub fn escape_pending(&self) -> bool {
        !self.escape.is_empty()
            && !self.paste
            && (self.escape.last() == Some(&27)
                || !(self.escape.starts_with(b"\x1b[") || self.escape.starts_with(b"\x1bO")))
    }

    /// Explicit dismissal never requests another input mode.
    pub fn has_history(&self) -> bool {
        !self.histories.is_empty()
    }

    pub fn dismiss_history(&mut self) -> bool {
        self.histories.dismiss()
    }

    pub fn resume_input(&mut self, frame: &Frame) {
        self.histories.resume(frame.focused);
    }

    pub fn prefix_from_copy(&mut self) -> bool {
        if self.in_copy() && !self.paste && self.escape.is_empty() && self.utf8.is_empty() {
            self.end_interaction();
            true
        } else {
            false
        }
    }

    pub fn take_copied(&mut self) -> Option<String> {
        self.copied.take()
    }

    pub fn clear_error(&mut self) {
        self.error = None;
        self.info = None;
        self.notice_since = None;
    }

    /// The bar notice to show at `now`: an error wins over an info line; both live for
    /// [`NOTICE_TTL`] or until the next key.
    pub fn notice(&self, now: std::time::Instant) -> Option<super::render::Notice> {
        let since = self.notice_since?;
        if now.duration_since(since) >= NOTICE_TTL {
            return None;
        }
        if let Some(error) = &self.error {
            return Some(super::render::Notice {
                text: error.clone(),
                error: true,
            });
        }
        self.info.as_ref().map(|info| super::render::Notice {
            text: info.clone(),
            error: false,
        })
    }

    /// When the current notice expires, if one is showing at `now`.
    pub fn notice_deadline(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        let since = self.notice_since?;
        if self.error.is_none() && self.info.is_none() {
            return None;
        }
        let deadline = since + NOTICE_TTL;
        (deadline > now).then_some(deadline)
    }

    /// Drops a notice whose time is up.
    pub fn expire_notice(&mut self, now: std::time::Instant) {
        if self.notice(now).is_none() {
            self.clear_error();
        }
    }

    /// A transient confirmation shown in the bar until the next key or [`NOTICE_TTL`].
    pub fn report_info(&mut self, message: impl Into<String>) {
        self.notice_since = Some(std::time::Instant::now());
        self.info = Some(crate::view::printable(&message.into(), 256));
    }

    pub fn report_error(&mut self, error: impl Into<String>) {
        self.notice_since = Some(std::time::Instant::now());
        self.error = Some(crate::view::printable(&error.into(), 256));
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// All viewer-private history replacements, plus the explicit keyboard selection.
    pub fn local_views(&self) -> Vec<super::render::LocalView<'_>> {
        let mut views: Vec<_> = self
            .histories
            .iter()
            .map(|copy| super::render::LocalView {
                pane: copy.pane(),
                view: copy.view(),
                cursor: copy.cursor(),
                anchor: None,
                keyboard: false,
            })
            .collect();
        if let Mode::Copy(copy) = &self.mode {
            views.push(super::render::LocalView {
                pane: copy.pane(),
                view: copy.view(),
                cursor: copy.cursor(),
                anchor: copy.anchor(),
                keyboard: true,
            });
        }
        views
    }

    pub fn take_read(&mut self) -> Option<(u64, PaneId, u32)> {
        let keyboard = match &mut self.mode {
            Mode::Copy(copy) => Some(copy.as_mut()),
            _ => None,
        };
        self.histories.take_read(keyboard)
    }

    pub fn awaiting_read(&self) -> bool {
        self.histories.awaiting_read()
            || matches!(&self.mode, Mode::Copy(copy) if copy.awaiting_read())
    }

    fn enforce_history_budget(&mut self, limit: usize) {
        let keyboard = match &self.mode {
            Mode::Copy(copy) => copy.retained_bytes(),
            _ => 0,
        };
        if !self.histories.fit_budget(keyboard, limit) {
            self.end_interaction();
            self.report_error("This pane exceeds the private history memory budget");
        }
    }

    pub fn history_failed(&mut self, request: u64, pane: PaneId) {
        let passive = self.histories.fail(request, pane);
        let keyboard =
            matches!(&self.mode, Mode::Copy(copy) if copy.pending_matches(request, pane));
        if keyboard {
            self.end_interaction();
        }
        if keyboard || passive {
            self.report_error("History request timed out; returned to live output.");
        }
    }

    pub fn install_view(&mut self, reply: ViewReply) {
        let dragging = self.selection_dragging();
        match self.histories.install(reply) {
            super::history::Install::Installed => {}
            super::history::Install::Invalidated => self.report_error(
                "History view changed or became unavailable; returned to live output.",
            ),
            super::history::Install::Unowned(reply) => {
                if let Mode::Copy(copy) = &mut self.mode
                    && !copy.install(reply)
                {
                    self.end_interaction();
                    self.report_error(
                        "Copy view changed or became unavailable; returned to live output.",
                    );
                }
            }
        }
        self.enforce_history_budget(MAX_HISTORY_BYTES);
        self.capture
            .cancel_left_if(dragging && !self.selection_dragging());
    }

    /// Reconciles the mode with a new frame: stale targets cancel with feedback, live copy views
    /// follow new output.
    pub fn reconcile(&mut self, frame: &Frame) {
        let dragging = self.selection_dragging();
        if self.histories.reconcile(frame)
            && self.active()
            && !matches!(self.mode, Mode::WaitingCommand)
        {
            self.end_interaction();
        }
        if self.drag.as_ref().is_some_and(|drag| !drag.valid(frame)) {
            self.drag = None;
            self.capture.adopt(0);
            self.report_error("Layout changed; drag cancelled.");
        }
        if let Some(message) = &frame.message {
            self.report_error(message.clone());
        }
        if let super::interaction::FrameTransition::Dismiss(message) = self.mode.reconcile(frame) {
            self.end_interaction();
            self.report_error(message);
        }
        self.enforce_history_budget(MAX_HISTORY_BYTES);
        self.capture
            .cancel_left_if(dragging && !self.selection_dragging());
    }

    pub fn wait_for_command(&mut self) {
        self.end_interaction();
        self.mode = Mode::WaitingCommand;
    }

    pub fn cancel_waiting_command(&mut self) {
        if matches!(self.mode, Mode::WaitingCommand) {
            self.end_interaction();
        }
    }

    pub fn wait_for_layout_reply(&mut self) -> Option<Action> {
        let action = match &self.mode {
            Mode::Layout {
                kind: LayoutMode::Resize,
                ..
            } => Action::ResizeMode,
            Mode::Layout {
                kind: LayoutMode::Swap,
                ..
            } => Action::SwapMode,
            Mode::Layout {
                kind: LayoutMode::Move,
                ..
            } => Action::MoveMode,
            _ => return None,
        };
        let hint = self.panel();
        self.wait_for_command();
        self.waiting_hint = hint;
        Some(action)
    }

    pub fn waiting_command_ready(&self) -> bool {
        matches!(self.mode, Mode::WaitingCommand)
            && !self.paste
            && self.escape.is_empty()
            && self.utf8.is_empty()
    }

    pub fn finish_waiting_command(&mut self) -> Vec<u8> {
        self.mode = Mode::Pane;
        self.take_loading_input()
    }

    pub fn loading_workspaces(&mut self) {
        self.interaction_epoch = self.interaction_epoch.wrapping_add(1);
        self.loading_input.clear();
        self.mode = Mode::LoadingWorkspaces { reorder: None };
        self.reset_input();
    }

    fn reset_input(&mut self) {
        self.escape.clear();
        self.utf8.clear();
        self.error = None;
    }

    pub fn workspaces_loaded(
        &mut self,
        result: anyhow::Result<Vec<crate::proto::control::WorkspaceRoute>>,
        current: &str,
    ) {
        let Mode::LoadingWorkspaces { reorder } = &self.mode else {
            return;
        };
        let reorder = reorder.clone();
        let result = result.map(|names| {
            names
                .into_iter()
                .filter(|entry| reorder.as_ref() != Some(&entry.name))
                .collect::<Vec<_>>()
        });
        match result {
            Ok(names) if !names.is_empty() => {
                self.escape.clear();
                self.utf8.clear();
                self.paste = false;
                let selected = names
                    .iter()
                    .position(|entry| entry.name == current)
                    .unwrap_or(0);
                self.mode = Mode::Workspaces {
                    names,
                    selected,
                    reorder,
                };
            }
            result => {
                self.mode = Mode::Pane;
                self.loading_input.clear();
                self.report_error(result.err().map_or_else(
                    || {
                        if reorder.is_some() {
                            "No other workspaces are available".to_owned()
                        } else {
                            "No workspaces are available".to_owned()
                        }
                    },
                    |error| error.to_string(),
                ));
            }
        }
    }

    pub fn loading_destination(&self) -> bool {
        matches!(self.mode, Mode::Destination { loading: true, .. })
    }

    fn loading_interaction(&self) -> bool {
        self.mode.loading()
    }

    pub fn manager_failed(&mut self, message: impl Into<String>) {
        if self.loading_destination() {
            self.end_interaction();
        }
        self.report_error(message);
    }

    pub fn destinations_loaded(&mut self, catalog: crate::proto::control::WorkspaceCatalog) {
        self.entry_regions.clear();
        self.panel_bounds = None;
        let Mode::Destination {
            source_workspace,
            transfer,
            entries,
            selected,
            loading: true,
        } = &mut self.mode
        else {
            return;
        };
        let mut names = std::collections::BTreeSet::new();
        let mut streams = std::collections::BTreeSet::new();
        if catalog.instance != transfer.instance
            || catalog.entries.len() > crate::config::MAX_WORKSPACES
            || catalog.entries.iter().any(|entry| {
                entry.stream == 0
                    || crate::ids::validate_workspace_name(&entry.name).is_err()
                    || entry.label.as_ref().is_some_and(|label| {
                        label.len() > MAX_TEXT_BYTES || label.chars().any(char::is_control)
                    })
                    || !names.insert(&entry.name)
                    || !streams.insert(entry.stream)
            })
        {
            self.manager_failed("Destination catalog does not match the attached server");
            return;
        }
        *entries = catalog
            .entries
            .into_iter()
            .filter(|entry| entry.name != *source_workspace)
            .collect();
        if entries.is_empty() {
            self.manager_failed("No other workspaces are available");
            return;
        }
        *selected = 0;
        self.escape.clear();
        self.utf8.clear();
        self.paste = false;
        if let Mode::Destination { loading, .. } = &mut self.mode {
            *loading = false;
        }
    }

    /// A confirmed global operation is executed through the manager, never a workspace socket.
    pub fn take_manager_request(&mut self) -> Option<crate::daemon::ManagerRequest> {
        self.manager_request.take()
    }

    /// Bytes typed while the chooser was loading, replayed once it is ready.
    pub fn take_loading_input(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.loading_input)
    }

    /// Enters a mode for a modal action; returns false for actions that need no mode.
    pub fn enter(&mut self, action: Action, frame: &Frame) -> bool {
        tracing::debug!(target: "fux::diagnostics", pid = std::process::id(), event = "interaction_enter", ?action,
            epoch = self.interaction_epoch, instance = %frame.server_instance,
            viewer = frame.viewer.0, pane = ?frame.focused.map(|pane| pane.0), stream = frame.workspace_stream);
        self.interaction_epoch = self.interaction_epoch.wrapping_add(1);
        self.reset_input();
        let tab = frame.active_tab;
        let pane = frame.focused;
        self.entry_regions.clear();
        self.panel_bounds = None;
        self.mode = match action {
            Action::CloseWorkspace
                if frame.workspace_stream != 0 && !frame.server_instance.is_empty() =>
            {
                Mode::CloseWorkspace {
                    instance: frame.server_instance.clone(),
                    workspace: frame.workspace.clone(),
                    stream: frame.workspace_stream,
                    viewer: frame.viewer,
                }
            }
            Action::SwapPane => {
                self.entry_regions.clear();
                self.panel_bounds = None;
                match super::context::SwapPicker::new(frame) {
                    Some(picker) => Mode::SwapPicker(Box::new(picker)),
                    None => return false,
                }
            }
            Action::PaneMenu | Action::TabMenu | Action::WorkspaceMenu => {
                self.entry_regions.clear();
                self.panel_bounds = None;
                let target = match action {
                    Action::PaneMenu => match pane {
                        Some(pane) => super::context::Target::Pane(pane),
                        None => return false,
                    },
                    Action::TabMenu => match tab {
                        Some(tab) => super::context::Target::Tab(tab),
                        None => return false,
                    },
                    _ => super::context::Target::Workspace,
                };
                match super::context::Menu::new(target, frame, self.workspaces_enabled) {
                    Some(menu) => Mode::Menu(Box::new(menu)),
                    None => return false,
                }
            }
            Action::CopyMode => {
                match pane.and_then(|pane| frame.pane(pane).map(|view| (pane, view))) {
                    Some((pane, view)) => {
                        let mut copy = self
                            .histories
                            .take(pane)
                            .unwrap_or_else(|| CopySession::new(pane, view.clone()));
                        if let Some(entry) = frame.layout.iter().find(|entry| entry.pane == pane) {
                            copy.set_viewport(entry.rect.height, entry.rect.width);
                            copy.refresh_live(view);
                        }
                        Mode::Copy(Box::new(copy))
                    }
                    None => return false,
                }
            }
            Action::ChooseTab | Action::MoveToTab | Action::ReorderTab => Mode::Tabs {
                choices: frame
                    .tabs
                    .iter()
                    .filter(|entry| action == Action::ChooseTab || Some(entry.id) != tab)
                    .cloned()
                    .collect(),
                selected: if action == Action::ChooseTab {
                    frame
                        .tabs
                        .iter()
                        .position(|entry| Some(entry.id) == tab)
                        .unwrap_or(0)
                } else {
                    0
                },
                purpose: match action {
                    Action::MoveToTab => match pane.zip(tab) {
                        Some((pane, tab)) => TabChoice::Transfer {
                            pane,
                            tab,
                            generation: frame.layout_generation,
                        },
                        None => return false,
                    },
                    Action::ReorderTab => match tab {
                        Some(tab) => TabChoice::Reorder { tab },
                        None => return false,
                    },
                    _ => TabChoice::Select,
                },
            },
            Action::RenamePane => {
                match pane.and_then(|pane| frame.pane(pane).map(|view| (pane, view))) {
                    Some((pane, view)) if !frame.server_instance.is_empty() => Mode::RenamePane {
                        pane,
                        instance: frame.server_instance.clone(),
                        workspace: frame.workspace.clone(),
                        text: view.label.clone().unwrap_or_default(),
                    },
                    _ => return false,
                }
            }
            Action::RenameWorkspace
                if !frame.server_instance.is_empty() && frame.workspace_stream != 0 =>
            {
                Mode::RenameWorkspace {
                    instance: frame.server_instance.clone(),
                    workspace: frame.workspace.clone(),
                    stream: frame.workspace_stream,
                    viewer: frame.viewer,
                    text: frame.workspace_label.clone().unwrap_or_default(),
                }
            }
            Action::RenameTab => {
                match tab.and_then(|tab| frame.tabs.iter().find(|entry| entry.id == tab)) {
                    Some(entry) => Mode::Rename {
                        tab: entry.id,
                        text: entry.label.clone(),
                    },
                    None => return false,
                }
            }
            Action::ClosePane => match pane {
                Some(pane) => Mode::ClosePane { pane },
                None => return false,
            },
            Action::CloseTab => {
                match tab.and_then(|tab| frame.tabs.iter().find(|entry| entry.id == tab)) {
                    Some(entry) => Mode::CloseTab {
                        tab: entry.id,
                        label: entry.label.clone(),
                    },
                    None => return false,
                }
            }
            Action::ResizeMode | Action::SwapMode | Action::MoveMode => match pane.zip(tab) {
                Some((pane, tab)) => Mode::Layout {
                    pane,
                    tab,
                    kind: match action {
                        Action::SwapMode => LayoutMode::Swap,
                        Action::MoveMode => LayoutMode::Move,
                        _ => LayoutMode::Resize,
                    },
                },
                None => return false,
            },
            Action::NewWorkspace => Mode::NewWorkspace {
                text: String::new(),
                transfer: None,
            },
            Action::MoveToNewWorkspace | Action::MoveToWorkspace => match pane.zip(tab) {
                Some((pane, tab)) if !frame.server_instance.is_empty() => {
                    let transfer = Box::new(crate::proto::control::WorkspaceTransfer {
                        focus: false,
                        follow: Some(frame.viewer),
                        instance: frame.server_instance.clone(),
                        source: tab,
                        generation: frame.layout_generation,
                        pane,
                        workspace: crate::proto::control::WorkspaceDestination::New {
                            name: String::new(),
                        },
                        destination: PaneDestination::NewTab { label: None },
                        side: crate::layout::Direction::Right,
                    });
                    if action == Action::MoveToWorkspace {
                        self.manager_request = Some(crate::daemon::ManagerRequest::Catalog);
                        self.loading_input.clear();
                        Mode::Destination {
                            source_workspace: frame.workspace.clone(),
                            transfer,
                            entries: Vec::new(),
                            selected: 0,
                            loading: true,
                        }
                    } else {
                        Mode::NewWorkspace {
                            text: String::new(),
                            transfer: Some(transfer),
                        }
                    }
                }
                _ => return false,
            },
            Action::ChooseWorkspace | Action::ReorderWorkspace => {
                self.loading_workspaces();
                if action == Action::ReorderWorkspace {
                    self.mode = Mode::LoadingWorkspaces {
                        reorder: Some(frame.workspace.clone()),
                    };
                }
                return true;
            }
            _ => return false,
        };
        self.enforce_history_budget(MAX_HISTORY_BYTES);
        true
    }

    pub fn resolve_escape(&mut self) {
        if !self.escape_pending() {
            return;
        }
        if self.escape == [27] || self.escape.last() == Some(&27) {
            self.end_interaction();
        }
        self.escape.clear();
    }

    fn end_interaction(&mut self) {
        tracing::debug!(target: "fux::diagnostics", pid = std::process::id(), event = "interaction_end", epoch = self.interaction_epoch, active = self.active());
        self.interaction_epoch = self.interaction_epoch.wrapping_add(1);
        self.pending_action = None;
        self.waiting_hint = None;
        self.panel_bounds = None;
        let was_dragging =
            self.drag.take().is_some() || matches!(&self.mode, Mode::Copy(copy) if copy.dragging());
        if was_dragging {
            self.capture.adopt(0);
        }
        self.mode = Mode::Pane;
        self.loading_input.clear();
        self.utf8.clear();
        self.error = None;
    }

    pub fn suppress_gesture_tail(&mut self) {
        self.capture.adopt(0);
    }

    /// A command can transfer keyboard ownership before its popup press releases.
    pub fn adopt_popup_capture(&mut self, button: u16) {
        self.capture.adopt(button);
    }

    /// Routes a mouse report while a mode is active or the pointer is over pane content.
    pub fn mouse(&mut self, mouse: MouseEvent, frame: &Frame) -> MouseDisposition {
        let disposition = self.route_mouse(mouse, frame);
        if !matches!(disposition, MouseDisposition::Forward) {
            self.capture.remember_local_press(mouse);
        }
        disposition
    }

    fn route_mouse(&mut self, mouse: MouseEvent, frame: &Frame) -> MouseDisposition {
        if self.capture.consume(mouse) {
            return MouseDisposition::Ignore;
        }
        let left_press = !mouse.release && !mouse.motion() && super::drag::Drag::accepts(mouse);
        if left_press {
            // A fresh press replaces a gesture whose release was lost. Discard
            // any uncommitted layout preview before normal hit testing.
            self.drag = None;
            if let Mode::Copy(copy) = &mut self.mode
                && copy.dragging()
            {
                copy.clear_selection();
            }
        }
        if let Mode::Copy(copy) = &mut self.mode
            && copy.dragging()
            && !mouse.wheel()
            && mouse.button() == 0
            && let Some(entry) = frame.layout.iter().find(|entry| entry.pane == copy.pane())
        {
            copy.drag(
                mouse.row.saturating_sub(1).saturating_sub(entry.rect.y),
                mouse.column.saturating_sub(1).saturating_sub(entry.rect.x),
                mouse.release,
            );
            return MouseDisposition::Local;
        }
        let point = (mouse.column.saturating_sub(1), mouse.row.saturating_sub(1)).into();
        if self.active()
            && left_press
            && self
                .panel_bounds
                .is_some_and(|bounds| bounds.contains(point))
            && (self.text_entry()
                || !self
                    .entry_regions
                    .iter()
                    .any(|(rect, _)| rect.contains(point)))
        {
            // Headings, padding and text fields are not menu actions.
            self.suppress_gesture_tail();
            return MouseDisposition::Local;
        }
        if self.text_entry() {
            if left_press
                && self
                    .panel_bounds
                    .is_some_and(|bounds| !bounds.contains(point))
            {
                self.end_interaction();
                self.suppress_gesture_tail();
                return MouseDisposition::Local;
            }
            return MouseDisposition::Ignore;
        }
        let left_release = mouse.release && super::drag::Drag::accepts(mouse);
        if matches!(self.mode, Mode::SwapPicker(_)) {
            self.reconcile(frame);
            let Mode::SwapPicker(picker) = &mut self.mode else {
                return MouseDisposition::Ignore;
            };
            if mouse.wheel() && !mouse.release {
                step(
                    &mut picker.selected,
                    picker.choices.len(),
                    if mouse.button() == 1 { 'j' } else { 'k' },
                );
                return MouseDisposition::Local;
            }
            if !mouse.release && !mouse.motion() && super::drag::Drag::accepts(mouse) {
                let hit = self
                    .entry_regions
                    .iter()
                    .find(|(rect, _)| {
                        rect.contains(
                            (mouse.column.saturating_sub(1), mouse.row.saturating_sub(1)).into(),
                        )
                    })
                    .map(|(_, index)| *index);
                let request = if let Some(index) = hit.filter(|index| *index < picker.choices.len())
                {
                    picker.selected = index;
                    self.key('\r', frame)
                } else {
                    self.end_interaction();
                    None
                };
                self.capture.adopt(0);
                return request.map_or(MouseDisposition::Local, MouseDisposition::Request);
            }
            return MouseDisposition::Ignore;
        }
        if matches!(self.mode, Mode::Menu(_)) {
            self.reconcile(frame);
            let Mode::Menu(menu) = &mut self.mode else {
                return MouseDisposition::Ignore;
            };
            if mouse.wheel() && !mouse.release {
                step(
                    &mut menu.selected,
                    menu.actions.len(),
                    if mouse.button() == 1 { 'j' } else { 'k' },
                );
                return MouseDisposition::Local;
            }
            if !mouse.release && !mouse.motion() && super::drag::Drag::accepts(mouse) {
                let hit = self
                    .entry_regions
                    .iter()
                    .find(|(rect, _)| {
                        rect.contains(
                            (mouse.column.saturating_sub(1), mouse.row.saturating_sub(1)).into(),
                        )
                    })
                    .map(|(_, index)| *index);
                if let Some(index) = hit.filter(|index| *index < menu.actions.len()) {
                    menu.selected = index;
                    self.key('\r', frame);
                } else {
                    self.end_interaction();
                }
                self.capture.adopt(0);
                return MouseDisposition::Local;
            }
            return MouseDisposition::Ignore;
        }
        if matches!(
            self.mode,
            Mode::ClosePane { .. } | Mode::CloseTab { .. } | Mode::CloseWorkspace { .. }
        ) {
            if mouse.release || mouse.motion() || !super::drag::Drag::accepts(mouse) {
                return MouseDisposition::Ignore;
            }
            self.reconcile(frame);
            let hit = self
                .entry_regions
                .iter()
                .find(|(rect, _)| {
                    rect.contains(
                        (mouse.column.saturating_sub(1), mouse.row.saturating_sub(1)).into(),
                    )
                })
                .map(|(_, index)| *index);
            let request = if matches!(
                self.mode,
                Mode::ClosePane { .. } | Mode::CloseTab { .. } | Mode::CloseWorkspace { .. }
            ) {
                match hit {
                    Some(1) => self.key('y', frame),
                    Some(0) => None, // The explanatory row is not an action.
                    _ => self.key('n', frame),
                }
            } else {
                None
            };
            self.capture.adopt(0);
            return request.map_or(MouseDisposition::Local, MouseDisposition::Request);
        }
        if matches!(
            self.mode,
            Mode::Destination { loading: false, .. } | Mode::Tabs { .. } | Mode::Workspaces { .. }
        ) {
            let left_press = !mouse.release && !mouse.motion() && super::drag::Drag::accepts(mouse);
            self.reconcile(frame);
            let (selected, len) = match &mut self.mode {
                Mode::Destination {
                    entries,
                    selected,
                    loading: false,
                    ..
                } => (selected, entries.len()),
                Mode::Tabs {
                    choices, selected, ..
                } => (selected, choices.len()),
                Mode::Workspaces {
                    names, selected, ..
                } => (selected, names.len()),
                _ => {
                    if left_press {
                        self.capture.adopt(0);
                    }
                    return MouseDisposition::Ignore;
                }
            };
            if mouse.wheel() && !mouse.release && mouse.code & !(4 | 8 | 16 | 64 | 1) == 0 {
                step(selected, len, if mouse.button() == 1 { 'j' } else { 'k' });
                return MouseDisposition::Local;
            }
            if left_press {
                let hit = self
                    .entry_regions
                    .iter()
                    .find(|(rect, _)| {
                        rect.contains(
                            (mouse.column.saturating_sub(1), mouse.row.saturating_sub(1)).into(),
                        )
                    })
                    .map(|(_, index)| *index)
                    .filter(|index| *index < len);
                let request = if let Some(index) = hit {
                    *selected = index;
                    self.key('\r', frame)
                } else {
                    self.end_interaction();
                    None
                };
                self.capture.adopt(0);
                return request.map_or(MouseDisposition::Local, MouseDisposition::Request);
            }
            return MouseDisposition::Ignore;
        }
        if let Some(mut drag) = self.drag.take() {
            if !drag.valid(frame) {
                self.capture.finish_left(left_release);
                return MouseDisposition::Ignore;
            }
            if !super::drag::Drag::accepts(mouse) {
                let selected = drag.scroll_tabs(mouse);
                self.drag = Some(drag);
                return if selected {
                    MouseDisposition::Local
                } else {
                    MouseDisposition::Ignore
                };
            }
            drag.update(mouse);
            if mouse.release {
                if let Some(pane) = drag.workspace_pane() {
                    self.open_workspace_drop(pane, frame);
                    return MouseDisposition::Local;
                }
                return drag
                    .finish(frame)
                    .map_or(MouseDisposition::Local, MouseDisposition::Request);
            }
            self.drag = Some(drag);
            return MouseDisposition::Local;
        }
        if matches!(self.mode, Mode::Pane)
            && !mouse.release
            && !mouse.motion()
            && mouse.button() == 2
            && mouse.code & !(2 | 8) == 0
            && let Some(target) = super::context::mouse_target(mouse, frame, &self.tab_regions)
            && let Some(menu) = super::context::Menu::new(target, frame, self.workspaces_enabled)
        {
            self.mode = Mode::Menu(Box::new(menu));
            self.entry_regions.clear();
            self.panel_bounds = None;
            return MouseDisposition::Local;
        }
        if matches!(self.mode, Mode::Pane)
            && let Some(drag) = super::drag::Drag::start(mouse, frame, &self.tab_regions)
        {
            self.drag = Some(drag);
            return MouseDisposition::Local;
        }
        if self.loading_interaction() && !mouse.wheel() && !mouse.motion() && !mouse.release {
            self.end_interaction();
            self.suppress_gesture_tail();
            return MouseDisposition::Local;
        }
        if !matches!(self.mode, Mode::Pane | Mode::Copy(_)) && !self.loading_interaction() {
            return MouseDisposition::Ignore;
        }
        let x = mouse.column.saturating_sub(1);
        let y = mouse.row.saturating_sub(1);
        let Some(entry) = frame.pane_at(x, y) else {
            return if self.in_copy() {
                MouseDisposition::Ignore
            } else {
                MouseDisposition::Forward
            };
        };
        let content = entry.rect;
        let Some(view) = frame.pane(entry.pane) else {
            return MouseDisposition::Ignore;
        };
        let app_owns_mouse = view.exit.is_none() && view.modes.mouse_mode != MouseMode::None;
        // Shift is the documented override: fux history/selection even when the app owns the mouse.
        let local = matches!(&self.mode, Mode::Copy(copy) if copy.pane() == entry.pane)
            || mouse.shift()
            || (!app_owns_mouse && (mouse.wheel() || mouse.motion() && mouse.button() == 0));
        if !local {
            if !mouse.release && !mouse.motion() && mouse.button() == 0 && self.in_copy() {
                self.end_interaction();
            }
            return MouseDisposition::Forward;
        }
        if !content.contains(x, y) {
            return MouseDisposition::Ignore;
        }
        let row = y.saturating_sub(content.y);
        let column = x.saturating_sub(content.x);
        if mouse.wheel() {
            if mouse.release || mouse.button() > 1 {
                return MouseDisposition::Ignore;
            }
            if let Mode::Copy(copy) = &mut self.mode
                && copy.pane() == entry.pane
            {
                self.capture.cancel_left_if(copy.dragging());
                copy.scroll(if mouse.code & 1 == 0 { 3 } else { -3 });
                return MouseDisposition::Local;
            }
            let mut copy = self
                .histories
                .take(entry.pane)
                .unwrap_or_else(|| CopySession::new(entry.pane, view.clone()));
            copy.set_viewport(content.height, content.width);
            copy.refresh_live(view);
            copy.scroll(if mouse.code & 1 == 0 { 3 } else { -3 });
            // Bounded most-recent-interaction order, independent of keyboard focus.
            self.histories.remember(copy);
            self.enforce_history_budget(MAX_HISTORY_BYTES);
        } else if mouse.button() == 0 {
            if !matches!(&self.mode, Mode::Copy(copy) if copy.pane() == entry.pane) {
                if mouse.release {
                    return MouseDisposition::Ignore;
                }
                let copy = self
                    .histories
                    .take(entry.pane)
                    .unwrap_or_else(|| CopySession::new(entry.pane, view.clone()));
                let mut copy = copy;
                copy.set_viewport(content.height, content.width);
                copy.refresh_live(view);
                self.mode = Mode::Copy(Box::new(copy));
            }
            if let Mode::Copy(copy) = &mut self.mode {
                copy.drag(row, column, mouse.release);
            }
        }
        MouseDisposition::Local
    }

    fn open_workspace_drop(&mut self, pane: PaneId, frame: &Frame) {
        self.interaction_epoch = self.interaction_epoch.wrapping_add(1);
        let Some(source) = frame.active_tab else {
            return;
        };
        if !self.workspaces_enabled || frame.server_instance.is_empty() {
            self.report_error("Workspace moves require a manager connection");
            return;
        }
        self.entry_regions.clear();
        self.panel_bounds = None;
        self.loading_input.clear();
        self.manager_request = Some(crate::daemon::ManagerRequest::Catalog);
        self.mode = Mode::Destination {
            source_workspace: frame.workspace.clone(),
            entries: Vec::new(),
            selected: 0,
            loading: true,
            transfer: Box::new(crate::proto::control::WorkspaceTransfer {
                focus: false,
                follow: Some(frame.viewer),
                instance: frame.server_instance.clone(),
                source,
                generation: frame.layout_generation,
                pane,
                workspace: crate::proto::control::WorkspaceDestination::New {
                    name: String::new(),
                },
                destination: PaneDestination::NewTab { label: None },
                side: crate::layout::Direction::Right,
            }),
        };
    }

    /// Feeds one input byte to the active mode. Returns a request to send, if any.
    pub fn feed(&mut self, byte: u8, frame: &Frame) -> Option<Request> {
        if self.loading_interaction() {
            if self.loading_input.len() < 4096 {
                self.loading_input.push(byte);
            } else {
                self.end_interaction();
                self.loading_input.clear();
                self.report_error("Buffered dialog input exceeded its limit; dialog dismissed");
            }
        }
        if self.paste {
            let (literal, ended) = super::input::paste_byte(&mut self.escape, byte);
            if self.text_entry() {
                for byte in literal {
                    self.plain_input(byte, frame);
                }
            }
            if ended {
                self.paste = false;
            }
            return None;
        }
        if !self.escape.is_empty() || byte == 27 {
            self.escape.push(byte);
            let complete = self.escape.len() > 1
                && match self.escape.get(1) {
                    Some(b'[' | b'O') => self.escape.len() > 2 && (0x40..=0x7e).contains(&byte),
                    _ => true,
                };
            if !complete && self.escape.len() < 64 {
                return None;
            }
            let sequence = std::mem::take(&mut self.escape);
            if self.active()
                && !self.paste
                && let Some(mouse) = MouseEvent::parse(&sequence)
            {
                if self.loading_interaction() {
                    self.loading_input
                        .truncate(self.loading_input.len().saturating_sub(sequence.len()));
                }
                return match self.mouse(mouse, frame) {
                    MouseDisposition::Request(request) => Some(request),
                    MouseDisposition::Forward => {
                        self.forwarded_mouse = Some(mouse);
                        None
                    }
                    _ => None,
                };
            }
            match sequence.as_slice() {
                b"\x1b[200~" => self.paste = true,
                b"\x1b[201~" => self.paste = false,
                b"\x1bOM" if !self.paste => return self.key('\r', frame),
                b"\x1b[D" | b"\x1bOD" if !self.paste && self.in_copy() => {
                    return self.key('h', frame);
                }
                b"\x1b[C" | b"\x1bOC" if !self.paste && self.in_copy() => {
                    return self.key('l', frame);
                }
                b"\x1b[5~" if !self.paste && self.in_copy() => {
                    return self.copy_key(CopyKey::PageUp);
                }
                b"\x1b[6~" if !self.paste && self.in_copy() => {
                    return self.copy_key(CopyKey::PageDown);
                }
                b"\x1b[A" | b"\x1bOA"
                    if !self.paste && matches!(self.mode, Mode::Layout { .. }) =>
                {
                    return self.key('k', frame);
                }
                b"\x1b[B" | b"\x1bOB"
                    if !self.paste && matches!(self.mode, Mode::Layout { .. }) =>
                {
                    return self.key('j', frame);
                }
                b"\x1b[C" | b"\x1bOC"
                    if !self.paste && matches!(self.mode, Mode::Layout { .. }) =>
                {
                    return self.key('l', frame);
                }
                b"\x1b[D" | b"\x1bOD"
                    if !self.paste && matches!(self.mode, Mode::Layout { .. }) =>
                {
                    return self.key('h', frame);
                }
                b"\x1b[A" | b"\x1b[D" | b"\x1bOA" | b"\x1bOD"
                    if !self.paste && !self.text_entry() =>
                {
                    return self.key('k', frame);
                }
                b"\x1b[B" | b"\x1b[C" | b"\x1bOB" | b"\x1bOC"
                    if !self.paste && !self.text_entry() =>
                {
                    return self.key('j', frame);
                }
                _ => {}
            }
            return None;
        }
        self.plain_input(byte, frame)
    }

    fn plain_input(&mut self, byte: u8, frame: &Frame) -> Option<Request> {
        if self.paste && !self.text_entry() {
            return None;
        }
        if byte.is_ascii() {
            self.utf8.clear();
            if self.paste && byte.is_ascii_control() {
                return None;
            }
            return self.key(char::from(byte), frame);
        }
        self.utf8.push(byte);
        match std::str::from_utf8(&self.utf8) {
            Ok(text) => {
                let character = text.chars().next();
                self.utf8.clear();
                character.and_then(|character| self.key(character, frame))
            }
            Err(error) if error.error_len().is_some() || self.utf8.len() >= 4 => {
                self.utf8.clear();
                None
            }
            Err(_) => None,
        }
    }

    fn text_entry(&self) -> bool {
        self.mode.text_entry()
    }

    fn selection_dragging(&self) -> bool {
        matches!(&self.mode, Mode::Copy(copy) if copy.dragging())
    }

    fn copy_key(&mut self, key: CopyKey) -> Option<Request> {
        let dragging = self.selection_dragging();
        let outcome = match &mut self.mode {
            Mode::Copy(copy) => copy.key(key),
            _ => CopyOutcome::Continue,
        };
        match outcome {
            CopyOutcome::Continue => {}
            CopyOutcome::Copied(text) => {
                self.end_interaction();
                self.copied = Some(text);
            }
            CopyOutcome::Finished => self.end_interaction(),
        }
        self.capture
            .cancel_left_if(dragging && !self.selection_dragging());
        None
    }

    fn key(&mut self, key: char, frame: &Frame) -> Option<Request> {
        use super::interaction::{Completion, KeyEffect};
        let transition = self.mode.key(key, self.paste, frame);
        if matches!(transition.completion, Completion::Finish) {
            self.end_interaction();
        }
        if let Some(notice) = transition.notice {
            self.report_error(notice);
        }
        match transition.effect {
            Some(KeyEffect::Control(request)) => Some(request),
            Some(KeyEffect::Manager(request)) => {
                self.manager_request = Some(request);
                None
            }
            Some(KeyEffect::Action(action, target)) => {
                self.pending_action = Some((action, target));
                None
            }
            Some(KeyEffect::Copy(key)) => self.copy_key(key),
            Some(KeyEffect::ScrollCopy(delta)) => self.scroll_copy(delta),
            None => None,
        }
    }

    fn scroll_copy(&mut self, delta: i64) -> Option<Request> {
        if let Mode::Copy(copy) = &mut self.mode {
            copy.scroll(delta);
        }
        None
    }

    /// The panel this mode wants painted, if any.
    pub fn drag_panel(&self, frame: &Frame) -> Option<HintPanel> {
        self.drag.as_ref().map(|drag| {
            let mut panel = HintPanel::bar(&drag.hint(frame));
            panel.drop_target = drag.preview(frame);
            panel.drop_tab = drag.preview_tab(frame);
            panel
        })
    }

    pub fn panel(&self) -> Option<HintPanel> {
        if let Mode::SwapPicker(picker) = &self.mode {
            return Some(picker.panel());
        }
        if let Mode::Menu(menu) = &self.mode {
            return Some(menu.panel());
        }
        let (title, entries, footer, focus) = match &self.mode {
            Mode::Destination {
                entries,
                selected,
                transfer,
                loading,
                ..
            } => (
                format!("Move pane {} to workspace", transfer.pane),
                if *loading {
                    vec!["Loading destinations…".into()]
                } else {
                    entries.iter().map(workspace_choice_label).collect()
                },
                if *loading {
                    "Esc dismiss"
                } else {
                    "↑/↓ or j/k move · Enter move to new tab · Esc dismiss"
                },
                if *loading { None } else { Some(*selected) },
            ),
            // Pane-mode notices live in the bar, not in a popup.
            Mode::Pane => return self.histories.last().map(|copy| {
                HintPanel::bar(&format!(
                    "History pane {} · Esc live · offset {} · wheel browses · typing follows focus",
                    copy.pane(),
                    copy.offset()
                ))
            }),
            Mode::Copy(copy) => return Some(HintPanel::bar(&copy.hint())),
            Mode::WaitingCommand => {
                return self.waiting_hint.clone().or_else(|| {
                    Some(HintPanel::bar(
                        "Waiting for previous operation · Esc dismiss",
                    ))
                });
            }
            Mode::LoadingWorkspaces { reorder } => (
                if reorder.is_some() {
                    "Reorder workspace".into()
                } else {
                    "Choose workspace".into()
                },
                vec!["Loading workspaces…".into()],
                "Esc dismiss",
                None,
            ),
            Mode::Workspaces {
                names,
                selected,
                reorder,
            } => (
                if reorder.is_some() {
                    "Place workspace before".into()
                } else {
                    "Choose workspace".into()
                },
                names.iter().map(workspace_choice_label).collect(),
                if reorder.is_some() {
                    "↑/↓ or j/k move · Enter place before · $ place last · Esc dismiss"
                } else {
                    "↑/↓ or j/k move · Enter switch · Esc dismiss"
                },
                Some(*selected),
            ),
            Mode::Tabs {
                choices,
                selected,
                purpose,
            } => (
                match purpose {
                    TabChoice::Select => "Choose tab".into(),
                    TabChoice::Transfer { pane, .. } => format!("Move pane {pane} to tab"),
                    TabChoice::Reorder { .. } => "Place current tab before".into(),
                },
                choices.iter().map(|entry| entry.label.clone()).collect(),
                match purpose {
                    TabChoice::Select => "↑/↓ or j/k move · Enter select · Esc dismiss",
                    TabChoice::Transfer { .. } => {
                        "↑/↓ or j/k move · Enter insert right of first pane · Esc dismiss"
                    }
                    TabChoice::Reorder { .. } => {
                        "↑/↓ or j/k move · Enter place before · $ place last · Esc dismiss"
                    }
                },
                Some(*selected),
            ),
            Mode::RenamePane { text, .. } => {
                return Some(HintPanel::text_input(
                    "Rename pane (empty = application title)",
                    text,
                    "Enter save · Esc dismiss · Ctrl-U clear · Backspace delete",
                ));
            }
            Mode::RenameWorkspace { text, .. } => {
                return Some(HintPanel::text_input(
                    "Rename workspace (empty = routing name)",
                    text,
                    "Enter save · Esc dismiss · Ctrl-U clear · Backspace delete",
                ));
            }
            Mode::Rename { text, .. } => {
                return Some(HintPanel::text_input(
                    "Rename tab",
                    text,
                    "Enter save · Esc dismiss · Ctrl-U clear · Backspace delete",
                ));
            }
            Mode::NewWorkspace { text, transfer } => {
                return Some(HintPanel::text_input(
                    if transfer.is_some() {
                        "Move pane to new workspace"
                    } else {
                        "New workspace (empty = automatic name)"
                    },
                    text,
                    "Enter create · Esc dismiss · Ctrl-U clear",
                ));
            }
            Mode::Menu(_) | Mode::SwapPicker(_) => return None,
            Mode::CloseWorkspace { workspace, .. } => (
                format!("Close workspace {workspace}?"),
                vec![
                    "All its panes and processes will be terminated; its viewers will detach."
                        .into(),
                    "Confirm close".into(),
                    "Cancel".into(),
                ],
                "y or click confirm · n/Esc or outside click cancel",
                None,
            ),
            Mode::ClosePane { pane } => (
                format!("Close pane {pane}?"),
                vec![
                    "Its process and unsaved work will be terminated.".into(),
                    "Confirm close".into(),
                    "Cancel".into(),
                ],
                "y or click confirm · n/Esc or outside click cancel",
                None,
            ),
            Mode::CloseTab { tab, label } => (
                format!("Close tab {label} ({tab})?"),
                vec![
                    "All its panes and their processes will be terminated.".into(),
                    "Confirm close".into(),
                    "Cancel".into(),
                ],
                "y or click confirm · n/Esc or outside click cancel",
                None,
            ),
            Mode::Layout { pane, kind, .. } => {
                let verb = match kind {
                    LayoutMode::Resize => "Resize",
                    LayoutMode::Swap => "Swap",
                    LayoutMode::Move => "Move",
                };
                return Some(HintPanel::bar(&format!(
                    "{verb} pane {pane}: Enter/Esc finish · arrows/hjkl · resize HJKL shrinks"
                )));
            }
        };
        let mut entries: Vec<String> = entries;
        if let Some(error) = &self.error {
            entries.push(error.clone());
        }
        Some(HintPanel::context(title, entries, footer, focus))
    }

    pub fn workspaces_enabled(&self) -> bool {
        self.workspaces_enabled
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use crate::proto::control::{LayoutAction, TabAction, WorkspaceAction};
    use crate::view::{PaneRect, PaneView, TabEntry};

    fn frame() -> Frame {
        let mut parser = vt100::Parser::new(4, 10, 0);
        parser.process(b"hello");
        let view = PaneView::from_screen(parser.screen(), "", 0, None).unwrap_or_default();
        let mut frame = Frame {
            workspace: "default".into(),
            ..Frame::default()
        };
        frame.tabs.push(TabEntry {
            id: TabId(1),
            label: "main".into(),
            layout_generation: 0,
            first_pane: Some(PaneId(1)),
        });
        frame.active_tab = Some(TabId(1));
        frame.focused = Some(PaneId(1));
        frame.layout.push(PaneRect {
            pane: PaneId(1),
            rect: crate::layout::Rect {
                x: 0,
                y: 0,
                width: 12,
                height: 6,
            },
        });
        frame.panes.insert(PaneId(1), view);
        frame
    }

    fn feed(controller: &mut Controller, bytes: &[u8], frame: &Frame) -> Vec<Request> {
        bytes
            .iter()
            .filter_map(|byte| controller.feed(*byte, frame))
            .collect()
    }

    pub(super) fn split_frame() -> Frame {
        let mut frame = frame();
        let view = frame.panes.get(&PaneId(1)).cloned().unwrap_or_default();
        frame.panes.insert(PaneId(2), view);
        frame.layout.push(PaneRect {
            pane: PaneId(2),
            rect: crate::layout::Rect {
                x: 13,
                y: 0,
                width: 12,
                height: 6,
            },
        });
        frame.layout_generation = 17;
        frame
    }

    #[test]
    fn successful_field_submission_retires_ownership_without_losing_its_request() {
        let mut frame = frame();
        frame.server_instance = "fixture-owner".into();
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::RenamePane, &frame));
        controller.panel_bounds = Some(ratatui_core::layout::Rect::new(0, 0, 20, 5));
        let epoch = controller.interaction_epoch();
        let requests = feed(&mut controller, b"\x15renamed\r", &frame);
        assert!(
            matches!(requests.as_slice(), [Request::RenamePane { pane, name, .. }]
            if *pane == PaneId(1) && name == "renamed")
        );
        assert!(!controller.owns_input());
        assert!(controller.panel_bounds.is_none());
        assert_ne!(controller.interaction_epoch(), epoch);
        assert!(controller.take_action().is_none());
        assert!(controller.take_manager_request().is_none());
        assert!(feed(&mut controller, b"next", &frame).is_empty());
    }

    #[test]
    fn destination_chooser_keeps_catalog_lifetime_and_discards_cancelled_input() {
        use crate::proto::control::{WorkspaceCatalog, WorkspaceDestination, WorkspaceRoute};
        let mut frame = frame();
        frame.server_instance = "attached".into();
        frame.viewer = crate::ids::ViewerId(7);
        frame.layout_generation = 12;
        let catalog = || WorkspaceCatalog {
            instance: "attached".into(),
            entries: vec![
                WorkspaceRoute {
                    label: None,
                    name: "default".into(),
                    stream: 1,
                },
                WorkspaceRoute {
                    label: None,
                    name: "other".into(),
                    stream: 8,
                },
            ],
        };
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::MoveToWorkspace, &frame));
        assert!(matches!(
            controller.take_manager_request(),
            Some(crate::daemon::ManagerRequest::Catalog)
        ));
        assert!(feed(&mut controller, b"\r", &frame).is_empty());
        controller.destinations_loaded(catalog());
        let buffered = controller.take_loading_input();
        assert!(feed(&mut controller, &buffered, &frame).is_empty());
        assert!(
            matches!(controller.take_manager_request(), Some(crate::daemon::ManagerRequest::Transfer {
            transfer: crate::proto::control::WorkspaceTransfer {
                focus: false,
                generation: 12, pane: PaneId(1), follow: Some(crate::ids::ViewerId(7)),
                workspace: WorkspaceDestination::Existing { name, stream: 8 }, ..
            }
        }) if name == "other")
        );
        assert!(controller.enter(Action::MoveToWorkspace, &frame));
        controller.take_manager_request();
        assert!(feed(&mut controller, b"unconfirmed\x1b", &frame).is_empty());
        controller.resolve_escape();
        controller.destinations_loaded(catalog());
        assert!(controller.take_loading_input().is_empty());
        assert!(controller.take_manager_request().is_none());
        assert!(controller.enter(Action::MoveToWorkspace, &frame));
        controller.take_manager_request();
        controller.destinations_loaded(WorkspaceCatalog {
            instance: "replacement".into(),
            ..catalog()
        });
        assert!(!controller.loading_destination());
        assert!(feed(&mut controller, b"\r", &frame).is_empty());
        assert!(controller.take_manager_request().is_none());
        assert!(controller.enter(Action::MoveToWorkspace, &frame));
        controller.take_manager_request();
        frame.layout_generation += 1;
        controller.reconcile(&frame);
        controller.destinations_loaded(catalog());
        assert!(controller.take_manager_request().is_none());
        assert!(!controller.active());
    }

    #[test]
    fn workspace_order_uses_manager_and_cancels_stale_loading() {
        let mut frame = frame();
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::ReorderWorkspace, &frame));
        controller.workspaces_loaded(
            Ok(vec!["other".into(), "default".into()]
                .into_iter()
                .map(|name| crate::proto::control::WorkspaceRoute {
                    name,
                    label: None,
                    stream: 1,
                })
                .collect()),
            "default",
        );
        assert!(feed(&mut controller, b"\x1b[200~\r\x1b[201~", &frame).is_empty());
        assert!(controller.take_manager_request().is_none());
        assert!(feed(&mut controller, b"\r", &frame).is_empty());
        assert!(
            matches!(controller.take_manager_request(), Some(crate::daemon::ManagerRequest::Reorder {
            name, before: Some(before),
        }) if name == "default" && before == "other")
        );
        assert!(controller.enter(Action::ReorderWorkspace, &frame));
        controller.workspaces_loaded(
            Ok(vec!["other".into()]
                .into_iter()
                .map(|name| crate::proto::control::WorkspaceRoute {
                    name,
                    label: None,
                    stream: 1,
                })
                .collect()),
            "default",
        );
        assert!(feed(&mut controller, b"$", &frame).is_empty());
        assert!(matches!(
            controller.take_manager_request(),
            Some(crate::daemon::ManagerRequest::Reorder { before: None, .. })
        ));
        assert!(controller.enter(Action::ReorderWorkspace, &frame));
        frame.workspace = "changed".into();
        controller.reconcile(&frame);
        controller.workspaces_loaded(
            Ok(vec!["other".into()]
                .into_iter()
                .map(|name| crate::proto::control::WorkspaceRoute {
                    name,
                    label: None,
                    stream: 1,
                })
                .collect()),
            "changed",
        );
        assert!(feed(&mut controller, b"\r", &frame).is_empty());
        assert!(controller.take_manager_request().is_none());
    }

    #[test]
    fn workspace_move_keeps_observed_identity_and_follows_only_its_viewer() {
        let mut frame = frame();
        frame.server_instance = "test-instance".into();
        frame.viewer = crate::ids::ViewerId(7);
        frame.layout_generation = 19;
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::MoveToNewWorkspace, &frame));
        assert!(feed(&mut controller, b"\r", &frame).is_empty());
        assert!(controller.take_manager_request().is_none());
        assert!(feed(&mut controller, b"\x1b[200~destination\r\x1b[201~", &frame).is_empty());
        assert!(controller.take_manager_request().is_none());
        assert!(feed(&mut controller, b"\r", &frame).is_empty());
        assert!(
            matches!(controller.take_manager_request(), Some(crate::daemon::ManagerRequest::Transfer {
            transfer: crate::proto::control::WorkspaceTransfer {
                focus: false,
                instance, source: TabId(1), generation: 19, pane: PaneId(1), follow: Some(crate::ids::ViewerId(7)),
                workspace: crate::proto::control::WorkspaceDestination::New { name }, ..
            }
        }) if instance == "test-instance" && name == "destination")
        );
        assert!(controller.enter(Action::MoveToNewWorkspace, &frame));
        frame.layout_generation += 1;
        controller.reconcile(&frame);
        assert!(feed(&mut controller, b"destination\r", &frame).is_empty());
        assert!(controller.take_manager_request().is_none());
    }

    #[test]
    fn transfer_chooser_pins_both_revisions_and_ignores_paste() {
        let mut frame = split_frame();
        frame.tabs.push(TabEntry {
            id: TabId(2),
            label: "destination".into(),
            layout_generation: 23,
            first_pane: Some(PaneId(3)),
        });
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::MoveToTab, &frame));
        assert!(feed(&mut controller, b"\x1b[200~\r\x1b[201~", &frame).is_empty());
        // A new destination revision must not silently change the pending choice.
        if let Some(destination) = frame.tabs.get_mut(1) {
            destination.layout_generation = 24;
        }
        controller.reconcile(&frame);
        assert!(matches!(
            feed(&mut controller, b"\r", &frame).as_slice(),
            [Request::Layout {
                tab: TabId(1),
                generation: Some(17),
                action: LayoutAction::Transfer {
                    focus: false,
                    pane: PaneId(1),
                    destination: PaneDestination::Tab {
                        ratio: 5000,
                        tab: TabId(2),
                        generation: 23,
                        target: PaneId(3),
                    },
                    side: crate::layout::Direction::Right,
                },
                ..
            }]
        ));
        assert!(controller.enter(Action::MoveToTab, &frame));
        frame.layout_generation += 1;
        controller.reconcile(&frame);
        assert!(feed(&mut controller, b"\r", &frame).is_empty());
        assert!(controller.enter(Action::MoveToTab, &frame));
        frame.tabs.pop();
        assert!(feed(&mut controller, b"\r", &frame).is_empty());
    }

    #[test]
    fn tab_chooser_mouse_selects_reorders_and_cancels_without_forwarding_release() {
        let mut frame = frame();
        for id in 2..=3 {
            frame.tabs.push(TabEntry {
                id: TabId(id),
                label: format!("tab-{id}"),
                layout_generation: 0,
                first_pane: Some(PaneId(id)),
            });
        }
        let mut controller = Controller::new(true);
        let click = MouseEvent {
            code: 0,
            column: 11,
            row: 5,
            release: false,
        };
        assert!(controller.enter(Action::ReorderTab, &frame));
        assert!(matches!(
            controller.mouse(MouseEvent { code: 65, ..click }, &frame),
            MouseDisposition::Local
        ));
        assert!(matches!(controller.mode, Mode::Tabs { selected: 1, .. }));
        controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 0)];
        assert!(matches!(
            controller.mouse(click, &frame),
            MouseDisposition::Request(Request::Tab {
                action: TabAction::Reorder {
                    tab: TabId(1),
                    before: Some(TabId(2))
                },
                ..
            })
        ));
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    release: true,
                    ..click
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(!controller.capture.left_pending());
        assert!(controller.enter(Action::ChooseTab, &frame));
        controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 2)];
        assert!(matches!(
            controller.mouse(click, &frame),
            MouseDisposition::Request(Request::Tab {
                action: TabAction::Select {
                    target: crate::proto::control::TabTarget::Id(TabId(3))
                },
                ..
            })
        ));
        controller.mouse(
            MouseEvent {
                release: true,
                ..click
            },
            &frame,
        );
        assert!(controller.enter(Action::ReorderTab, &frame));
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    column: 1,
                    row: 1,
                    ..click
                },
                &frame
            ),
            MouseDisposition::Local
        ));
        assert!(matches!(controller.mode, Mode::Pane));
        assert!(!controller.active());
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    release: true,
                    ..click
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(controller.enter(Action::ReorderTab, &frame));
        controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 0)];
        frame.tabs.remove(0);
        assert!(matches!(
            controller.mouse(click, &frame),
            MouseDisposition::Ignore
        ));
        assert!(!controller.active());
        assert!(controller.capture.left_pending());
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    release: true,
                    ..click
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(!controller.capture.left_pending());
        assert!(controller.enter(Action::ChooseTab, &frame));
        controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 0)];
        assert!(matches!(
            controller.mouse(click, &frame),
            MouseDisposition::Request(Request::Tab {
                action: TabAction::Select {
                    target: crate::proto::control::TabTarget::Id(TabId(2))
                },
                ..
            })
        ));
    }

    #[test]
    fn workspace_chooser_mouse_reorders_through_manager() {
        let frame = frame();
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::ReorderWorkspace, &frame));
        controller.workspaces_loaded(
            Ok(["default", "other", "third"]
                .into_iter()
                .map(|name| crate::proto::control::WorkspaceRoute {
                    name: name.into(),
                    label: None,
                    stream: 1,
                })
                .collect()),
            "default",
        );
        let click = MouseEvent {
            code: 0,
            column: 11,
            row: 5,
            release: false,
        };
        assert!(matches!(
            controller.mouse(MouseEvent { code: 65, ..click }, &frame),
            MouseDisposition::Local
        ));
        assert!(matches!(
            controller.mode,
            Mode::Workspaces { selected: 1, .. }
        ));
        controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 0)];
        assert!(matches!(
            controller.mouse(click, &frame),
            MouseDisposition::Local
        ));
        assert!(
            matches!(controller.take_manager_request(), Some(crate::daemon::ManagerRequest::Reorder { name, before: Some(before) }) if name == "default" && before == "other")
        );
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    release: true,
                    ..click
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(controller.take_manager_request().is_none());
    }

    #[test]
    fn mouse_close_dialogs_confirm_cancel_and_consume_gesture_tails() {
        let mut frame = frame();
        frame.server_instance = "server".into();
        frame.workspace_stream = 7;
        let click = MouseEvent {
            code: 0,
            column: 11,
            row: 5,
            release: false,
        };
        for action in [Action::ClosePane, Action::CloseTab, Action::CloseWorkspace] {
            let mut controller = Controller::new(true);
            for entry in [0, 2, 99] {
                assert!(controller.enter(action, &frame));
                controller.entry_regions =
                    vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), entry)];
                assert!(matches!(
                    controller.mouse(click, &frame),
                    MouseDisposition::Local
                ));
                assert!(matches!(
                    controller.mouse(
                        MouseEvent {
                            release: true,
                            ..click
                        },
                        &frame
                    ),
                    MouseDisposition::Ignore
                ));
                assert!(!controller.capture.left_pending());
                assert!(controller.take_manager_request().is_none());
                if entry == 0 {
                    assert!(controller.active());
                } else {
                    assert!(!controller.active());
                    assert!(!controller.active());
                }
            }
            assert!(controller.enter(action, &frame));
            assert!(controller.entry_regions.is_empty());
            // A click before the confirmation has painted cannot reuse an old menu row.
            assert!(matches!(
                controller.mouse(click, &frame),
                MouseDisposition::Local
            ));
            assert!(!controller.active());
            controller.mouse(
                MouseEvent {
                    release: true,
                    ..click
                },
                &frame,
            );
            assert!(controller.enter(action, &frame));
            controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 1)];
            let disposition = controller.mouse(click, &frame);
            assert!(match action {
                Action::ClosePane => matches!(
                    disposition,
                    MouseDisposition::Request(Request::Kill {
                        pane: PaneId(1),
                        ..
                    })
                ),
                Action::CloseTab => matches!(
                    disposition,
                    MouseDisposition::Request(Request::Tab {
                        action: TabAction::Close { tab: TabId(1) },
                        ..
                    })
                ),
                Action::CloseWorkspace => matches!(
                    disposition,
                    MouseDisposition::Request(Request::Workspace {
                        stream: Some(7),
                        action: WorkspaceAction::Kill { .. },
                        ..
                    })
                ),
                _ => false,
            });
            assert!(matches!(
                controller.mouse(
                    MouseEvent {
                        release: true,
                        ..click
                    },
                    &frame
                ),
                MouseDisposition::Ignore
            ));
            assert!(!controller.capture.left_pending());
        }
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::ClosePane, &frame));
        controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 1)];
        frame.panes.clear();
        assert!(matches!(
            controller.mouse(click, &frame),
            MouseDisposition::Local
        ));
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    release: true,
                    ..click
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(!controller.active() && !controller.capture.left_pending());
    }

    #[test]
    fn reorder_chooser_places_before_or_last_and_escape_cancels() {
        let mut frame = frame();
        frame.tabs.push(TabEntry {
            id: TabId(2),
            label: "second".into(),
            layout_generation: 0,
            first_pane: Some(PaneId(2)),
        });
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::ReorderTab, &frame));
        assert!(matches!(
            feed(&mut controller, b"\r", &frame).as_slice(),
            [Request::Tab {
                action: TabAction::Reorder {
                    tab: TabId(1),
                    before: Some(TabId(2))
                },
                ..
            }]
        ));
        assert!(controller.enter(Action::ReorderTab, &frame));
        assert!(matches!(
            feed(&mut controller, b"$", &frame).as_slice(),
            [Request::Tab {
                action: TabAction::Reorder {
                    tab: TabId(1),
                    before: None
                },
                ..
            }]
        ));
        assert!(controller.enter(Action::MoveToTab, &frame));
        assert!(feed(&mut controller, b"\x1b", &frame).is_empty());
        controller.resolve_escape();
        assert!(feed(&mut controller, b"\r", &frame).is_empty());
    }

    #[test]
    fn workspace_drop_and_click_keep_dragged_pane_and_consume_release() {
        use crate::proto::control::{WorkspaceCatalog, WorkspaceDestination, WorkspaceRoute};
        let mut frame = split_frame();
        frame.server_instance = "server".into();
        let mut controller = Controller::new(true);
        controller.set_tab_regions(vec![(
            ratatui_core::layout::Rect::new(10, 7, 8, 1),
            TabId(1),
        )]);
        let click = MouseEvent {
            code: 8,
            column: 24,
            row: 2,
            release: false,
        };
        controller.mouse(click, &frame);
        controller.mouse(
            MouseEvent {
                column: 2,
                row: 8,
                release: true,
                ..click
            },
            &frame,
        );
        assert!(matches!(
            controller.take_manager_request(),
            Some(crate::daemon::ManagerRequest::Catalog)
        ));
        controller.destinations_loaded(WorkspaceCatalog {
            instance: "server".into(),
            entries: vec![WorkspaceRoute {
                label: None,
                name: "other".into(),
                stream: 9,
            }],
        });
        controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 0)];
        let confirm = MouseEvent {
            code: 0,
            column: 12,
            row: 5,
            release: false,
        };
        assert!(matches!(
            controller.mouse(confirm, &frame),
            MouseDisposition::Local
        ));
        assert!(
            matches!(controller.take_manager_request(), Some(crate::daemon::ManagerRequest::Transfer {
            transfer: crate::proto::control::WorkspaceTransfer {
                focus: false, pane: PaneId(2), generation: 17,
                workspace: WorkspaceDestination::Existing {name, stream: 9}, .. }
        }) if name == "other")
        );
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    release: true,
                    ..confirm
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(controller.take_manager_request().is_none());
    }

    #[test]
    fn wheel_during_tab_drag_reaches_hidden_destinations_and_wraps() {
        use crate::proto::control::PaneDestination;
        let mut frame = split_frame();
        for id in 2..=4 {
            frame.tabs.push(TabEntry {
                id: TabId(id),
                label: format!("dest-{id}"),
                layout_generation: 20 + u64::from(id),
                first_pane: Some(PaneId(id + 1)),
            });
        }
        let mut controller = Controller::new(true);
        controller.set_tab_regions(vec![(
            ratatui_core::layout::Rect::new(10, 7, 8, 1),
            TabId(2),
        )]);
        let click = MouseEvent {
            code: 8,
            column: 2,
            row: 2,
            release: false,
        };
        let wheel = MouseEvent {
            code: 65,
            column: 12,
            row: 8,
            release: false,
        };
        controller.mouse(click, &frame);
        assert!(matches!(
            controller.mouse(wheel, &frame),
            MouseDisposition::Local
        ));
        assert_eq!(
            controller
                .drag_panel(&frame)
                .and_then(|panel| panel.drop_tab),
            Some(TabId(3))
        );
        controller.mouse(MouseEvent { code: 64, ..wheel }, &frame);
        controller.mouse(MouseEvent { code: 64, ..wheel }, &frame);
        assert_eq!(
            controller
                .drag_panel(&frame)
                .and_then(|panel| panel.drop_tab),
            Some(TabId(4))
        );
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    code: 0,
                    release: true,
                    ..wheel
                },
                &frame
            ),
            MouseDisposition::Request(Request::Layout {
                action: LayoutAction::Transfer {
                    focus: false,
                    destination: PaneDestination::Tab {
                        ratio: 5000,
                        tab: TabId(4),
                        generation: 24,
                        target: PaneId(5)
                    },
                    ..
                },
                ..
            })
        ));
        controller.mouse(click, &frame);
        controller.mouse(wheel, &frame);
        // Moving back into pane content abandons the wheel-selected tab.
        controller.mouse(
            MouseEvent {
                code: 40,
                column: 24,
                row: 3,
                release: false,
            },
            &frame,
        );
        assert_eq!(
            controller
                .drag_panel(&frame)
                .and_then(|panel| panel.drop_tab),
            None
        );
        controller.mouse(wheel, &frame);
        frame.tabs.retain(|tab| tab.id != TabId(3));
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    code: 0,
                    release: true,
                    ..wheel
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
    }

    #[test]
    fn pane_tab_drop_keeps_destination_revision_and_cancels_changed_bar() {
        use crate::proto::control::PaneDestination;
        let mut frame = split_frame();
        frame.tabs.push(TabEntry {
            id: TabId(2),
            label: "dest".into(),
            layout_generation: 23,
            first_pane: Some(PaneId(3)),
        });
        let regions = vec![(ratatui_core::layout::Rect::new(10, 7, 8, 1), TabId(2))];
        let click = MouseEvent {
            code: 8,
            column: 2,
            row: 2,
            release: false,
        };
        let drop = MouseEvent {
            code: 0,
            column: 12,
            row: 8,
            release: true,
        };
        let mut controller = Controller::new(true);
        controller.set_tab_regions(regions.clone());
        controller.mouse(click, &frame);
        controller.mouse(
            MouseEvent {
                code: 40,
                release: false,
                ..drop
            },
            &frame,
        );
        assert_eq!(
            controller
                .drag_panel(&frame)
                .and_then(|panel| panel.drop_tab),
            Some(TabId(2))
        );
        assert!(matches!(
            controller.mouse(drop, &frame),
            MouseDisposition::Request(Request::Layout {
                tab: TabId(1),
                generation: Some(17),
                action: LayoutAction::Transfer {
                    focus: false,
                    pane: PaneId(1),
                    destination: PaneDestination::Tab {
                        ratio: 5000,
                        tab: TabId(2),
                        generation: 23,
                        target: PaneId(3)
                    },
                    ..
                },
                ..
            })
        ));
        controller.mouse(click, &frame);
        for tab in &mut frame.tabs {
            if tab.id == TabId(2) {
                tab.layout_generation += 1;
            }
        }
        assert!(matches!(
            controller.mouse(drop, &frame),
            MouseDisposition::Ignore
        ));
        controller.mouse(click, &frame);
        controller.set_tab_regions(Vec::new());
        assert!(!controller.active());
        assert!(matches!(
            controller.mouse(drop, &frame),
            MouseDisposition::Ignore
        ));
    }

    #[test]
    fn pane_drag_requires_alt_and_commits_only_on_release_with_original_revision() {
        let frame = split_frame();
        let mut controller = Controller::new(true);
        let click = MouseEvent {
            code: 0,
            column: 2,
            row: 2,
            release: false,
        };
        assert!(matches!(
            controller.mouse(click, &frame),
            MouseDisposition::Forward
        ));
        assert!(matches!(
            controller.mouse(MouseEvent { code: 8, ..click }, &frame),
            MouseDisposition::Local
        ));
        assert!(controller.active());
        assert!(feed(&mut controller, b"\x1b[<40;24;3M", &frame).is_empty());
        assert_eq!(
            controller
                .drag_panel(&frame)
                .and_then(|panel| panel.drop_target),
            Some((PaneId(2), crate::layout::Direction::Right))
        );
        let requests = feed(&mut controller, b"\x1b[<8;24;3m", &frame);
        assert!(matches!(
            requests.as_slice(),
            [Request::Layout {
                generation: Some(17),
                action: LayoutAction::Relocate {
                    pane: PaneId(1),
                    target: PaneId(2),
                    side: crate::layout::Direction::Right
                },
                ..
            }]
        ));
        assert!(!controller.active());
    }

    #[test]
    fn a_fresh_press_abandons_an_unreleased_layout_preview() {
        let frame = split_frame();
        let mut controller = Controller::new(true);
        let press = MouseEvent {
            code: 8,
            column: 2,
            row: 2,
            release: false,
        };
        controller.mouse(press, &frame);
        controller.mouse(
            MouseEvent {
                code: 40,
                column: 24,
                ..press
            },
            &frame,
        );
        assert!(controller.drag.is_some());
        // The first gesture lost its release. An ordinary new press must not
        // move the old pane when the second gesture eventually releases.
        assert!(matches!(
            controller.mouse(MouseEvent { code: 0, ..press }, &frame),
            MouseDisposition::Forward
        ));
        assert!(controller.drag.is_none());
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    code: 0,
                    column: 24,
                    release: true,
                    ..press
                },
                &frame
            ),
            MouseDisposition::Forward
        ));
        assert!(!controller.active());
        // A fresh Alt press instead starts its own preview targeting pane 2.
        controller.mouse(press, &frame);
        controller.mouse(
            MouseEvent {
                column: 15,
                ..press
            },
            &frame,
        );
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    code: 0,
                    column: 2,
                    release: true,
                    ..press
                },
                &frame
            ),
            MouseDisposition::Request(Request::Layout {
                action: LayoutAction::Relocate {
                    pane: PaneId(2),
                    target: PaneId(1),
                    ..
                },
                ..
            })
        ));
    }

    #[test]
    fn an_ignored_fresh_right_press_keeps_its_release_owned_across_modes() {
        let mut frame = split_frame();
        frame.server_instance = "attached".into();
        frame
            .panes
            .get_mut(&PaneId(2))
            .unwrap_or_else(|| panic!("pane 2"))
            .modes
            .mouse_mode = MouseMode::AnyMotion;
        let mut controller = Controller::new(true);
        let press = MouseEvent {
            code: 2,
            column: 2,
            row: 2,
            release: false,
        };
        assert!(matches!(
            controller.mouse(press, &frame),
            MouseDisposition::Local
        ));
        assert!(matches!(
            controller.mouse(press, &frame),
            MouseDisposition::Ignore
        ));
        // A keyboard action changes the mode while the ignored second press is
        // still held. Its release must not leak into pane 2's reporting app.
        controller.enter(Action::CopyMode, &frame);
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    column: 15,
                    release: true,
                    ..press
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(controller.capture.auxiliary().is_none());
    }

    #[test]
    fn cancelled_right_menu_accepts_a_new_press_after_a_lost_release() {
        let mut frame = split_frame();
        frame.server_instance = "attached".into();
        let mut controller = Controller::new(true);
        let press = MouseEvent {
            code: 2,
            column: 2,
            row: 2,
            release: false,
        };
        assert!(matches!(
            controller.mouse(press, &frame),
            MouseDisposition::Local
        ));
        feed(&mut controller, b"\x1b", &frame);
        controller.resolve_escape();
        assert!(!controller.active());
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    column: 15,
                    ..press
                },
                &frame
            ),
            MouseDisposition::Local
        ));
        assert!(
            matches!(&controller.mode, Mode::Menu(menu) if menu.project(&frame).focused == Some(PaneId(2)))
        );
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    release: true,
                    column: 15,
                    ..press
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
    }

    #[test]
    fn layout_drag_ignores_other_buttons_and_keeps_cancelled_tail_captured() {
        let frame = split_frame();
        let mut controller = Controller::new(true);
        let click = MouseEvent {
            code: 8,
            column: 2,
            row: 2,
            release: false,
        };
        controller.mouse(click, &frame);
        for code in [1, 2, 3, 64, 65, 128, 129, 256] {
            for release in [false, true] {
                assert!(matches!(
                    controller.mouse(
                        MouseEvent {
                            code,
                            release,
                            column: 24,
                            row: 3,
                        },
                        &frame
                    ),
                    MouseDisposition::Ignore
                ));
                assert!(controller.active());
            }
        }
        // Releasing Alt before the mouse button still completes the captured left drag.
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    code: 0,
                    release: true,
                    column: 24,
                    row: 3,
                },
                &frame
            ),
            MouseDisposition::Request(Request::Layout {
                action: LayoutAction::Relocate {
                    pane: PaneId(1),
                    target: PaneId(2),
                    ..
                },
                ..
            })
        ));
        controller.mouse(click, &frame);
        controller.end_interaction();
        assert!(
            !controller.active(),
            "drag cancellation must not open the command popup"
        );
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    code: 2,
                    release: true,
                    ..click
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    code: 40,
                    column: 24,
                    ..click
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    code: 0,
                    release: true,
                    ..click
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(matches!(
            controller.mouse(MouseEvent { code: 0, ..click }, &frame),
            MouseDisposition::Forward
        ));
    }

    #[test]
    fn layout_drag_rejects_replaced_server_viewer_or_zoom_even_with_same_revision() {
        for changed in 0..3 {
            let mut frame = split_frame();
            let mut controller = Controller::new(true);
            let click = MouseEvent {
                code: 8,
                column: 2,
                row: 2,
                release: false,
            };
            controller.mouse(click, &frame);
            match changed {
                0 => frame.server_instance.push_str("replacement"),
                1 => frame.viewer = crate::ids::ViewerId(frame.viewer.0 + 1),
                _ => frame.zoomed = Some(PaneId(1)),
            }
            assert!(matches!(
                controller.mouse(
                    MouseEvent {
                        release: true,
                        column: 24,
                        row: 3,
                        ..click
                    },
                    &frame
                ),
                MouseDisposition::Ignore
            ));
            assert!(!controller.active());
        }
    }

    #[test]
    fn border_drag_cancel_and_stale_layout_swallow_the_gesture_tail() {
        let mut frame = split_frame();
        let mut controller = Controller::new(true);
        let click = MouseEvent {
            code: 0,
            column: 13,
            row: 2,
            release: false,
        };
        assert!(matches!(
            controller.mouse(click, &frame),
            MouseDisposition::Local
        ));
        feed(&mut controller, b"\x1b", &frame);
        controller.resolve_escape();
        assert!(!controller.active());
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    release: true,
                    column: 18,
                    ..click
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        controller.mouse(click, &frame);
        frame.layout_generation += 1;
        controller.reconcile(&frame);
        assert!(!controller.active());
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    release: true,
                    ..click
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        controller.mouse(click, &frame);
        let requests = feed(&mut controller, b"\x1b[<0;18;2m", &frame);
        assert!(matches!(
            requests.as_slice(),
            [Request::Layout {
                generation: Some(18),
                action: LayoutAction::ResizeBorder {
                    column: 12,
                    row: 1,
                    to_column: 17,
                    ..
                },
                ..
            }]
        ));
    }

    #[test]
    fn move_and_swap_modes_use_latest_frame_but_keep_original_target() {
        let mut frame = split_frame();
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::SwapMode, &frame));
        frame.focused = Some(PaneId(2));
        frame.layout_generation = 19;
        assert!(matches!(
            feed(&mut controller, b"\x1b[C", &frame).as_slice(),
            [Request::Layout {
                generation: Some(19),
                action: LayoutAction::SwapDirection {
                    pane: PaneId(1),
                    direction: crate::layout::Direction::Right
                },
                ..
            }]
        ));
        assert!(controller.enter(Action::MoveMode, &frame));
        assert!(feed(&mut controller, b"\x1b[200~h\x1b[201~", &frame).is_empty());
        assert!(matches!(
            feed(&mut controller, b"h", &frame).as_slice(),
            [Request::Layout {
                action: LayoutAction::MoveDirection {
                    pane: PaneId(2),
                    direction: crate::layout::Direction::Left
                },
                ..
            }]
        ));
    }

    #[test]
    fn right_click_policy_controls_menu_capture_and_preserves_alt_override()
    -> Result<(), &'static str> {
        use crate::view::{MouseMode, RightClickPolicy};
        for policy in [
            RightClickPolicy::Auto,
            RightClickPolicy::Fux,
            RightClickPolicy::Pane,
        ] {
            for reporting in [false, true] {
                for alt in [false, true] {
                    let mut frame = split_frame();
                    frame.server_instance = "attached".into();
                    let pane = frame.panes.get_mut(&PaneId(2)).ok_or("missing pane")?;
                    pane.right_click = policy;
                    pane.modes.mouse_mode = if reporting {
                        MouseMode::PressRelease
                    } else {
                        MouseMode::None
                    };
                    let mut controller = Controller::new(true);
                    let click = MouseEvent {
                        code: if alt { 10 } else { 2 },
                        column: 15,
                        row: 2,
                        release: false,
                    };
                    let menu = alt
                        || policy == RightClickPolicy::Fux
                        || policy == RightClickPolicy::Auto && !reporting;
                    let result = controller.mouse(click, &frame);
                    assert_eq!(matches!(result, MouseDisposition::Local), menu);
                    assert_eq!(matches!(result, MouseDisposition::Forward), !menu);
                    let release = controller.mouse(
                        MouseEvent {
                            release: true,
                            ..click
                        },
                        &frame,
                    );
                    assert_eq!(matches!(release, MouseDisposition::Ignore), menu);
                    assert_eq!(matches!(release, MouseDisposition::Forward), !menu);
                }
            }
        }
        Ok(())
    }

    #[test]
    fn contextual_pane_actions_keep_clicked_target_and_release_ownership()
    -> Result<(), &'static str> {
        let mut frame = split_frame();
        frame.server_instance = "attached".into();
        let mut controller = Controller::new(true);
        let right = MouseEvent {
            code: 2,
            column: 15,
            row: 2,
            release: false,
        };
        assert!(matches!(
            controller.mouse(right, &frame),
            MouseDisposition::Local
        ));
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    release: true,
                    ..right
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(feed(&mut controller, b"\r", &frame).is_empty());
        let (action, target) = controller.take_action().ok_or("menu action")?;
        assert_eq!(action, Action::RenamePane);
        assert_eq!(target.focused, Some(PaneId(2)));
        assert_eq!(
            frame.focused,
            Some(PaneId(1)),
            "menu must not change the real viewer focus"
        );

        assert!(matches!(
            controller.mouse(right, &frame),
            MouseDisposition::Local
        ));
        assert!(feed(&mut controller, b"\x1b", &frame).is_empty());
        controller.resolve_escape();
        assert!(!controller.active());
        assert!(
            !controller.active(),
            "cancelled menu must not hide the captured release behind another popup"
        );
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    release: true,
                    ..right
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(matches!(
            controller.mouse(right, &frame),
            MouseDisposition::Local
        ));
        frame.layout_generation += 1;
        controller.reconcile(&frame);
        assert!(!controller.active());
        assert!(controller.take_action().is_none());
        Ok(())
    }

    #[test]
    fn contextual_menu_clicks_disabled_actions_and_application_mouse_override() {
        let mut frame = split_frame();
        frame.server_instance = "attached".into();
        let mut controller = Controller::new(false);
        if let Some(pane) = frame.panes.get_mut(&PaneId(1)) {
            pane.modes.mouse_mode = MouseMode::PressRelease;
        }
        let right = MouseEvent {
            code: 2,
            column: 2,
            row: 2,
            release: false,
        };
        assert!(matches!(
            controller.mouse(right, &frame),
            MouseDisposition::Forward
        ));
        assert!(matches!(
            controller.mouse(MouseEvent { code: 10, ..right }, &frame),
            MouseDisposition::Local
        ));
        controller.mouse(
            MouseEvent {
                code: 10,
                release: true,
                ..right
            },
            &frame,
        );
        controller.entry_regions = vec![(ratatui_core::layout::Rect::new(8, 3, 16, 1), 0)];
        let left = MouseEvent {
            code: 0,
            column: 9,
            row: 4,
            release: false,
        };
        assert!(matches!(
            controller.mouse(left, &frame),
            MouseDisposition::Local
        ));
        assert_eq!(
            controller.take_action().map(|(action, _)| action),
            Some(Action::RenamePane)
        );
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    release: true,
                    ..left
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(controller.enter(Action::WorkspaceMenu, &frame));
        assert!(feed(&mut controller, b"\r", &frame).is_empty());
        assert!(
            controller.take_action().is_none(),
            "unavailable manager action dispatched"
        );
        assert!(controller.active());
    }

    #[test]
    fn hidden_tab_menu_targets_its_tab_and_cancels_when_catalog_changes() -> Result<(), &'static str>
    {
        let mut frame = frame();
        frame.server_instance = "attached".into();
        frame.tabs.push(TabEntry {
            id: TabId(2),
            label: "other".into(),
            layout_generation: 9,
            first_pane: Some(PaneId(2)),
        });
        let mut controller = Controller::new(true);
        controller.tab_regions = vec![(ratatui_core::layout::Rect::new(10, 6, 8, 1), TabId(2))];
        let right = MouseEvent {
            code: 2,
            column: 12,
            row: 7,
            release: false,
        };
        assert!(matches!(
            controller.mouse(right, &frame),
            MouseDisposition::Local
        ));
        controller.mouse(
            MouseEvent {
                release: true,
                ..right
            },
            &frame,
        );
        feed(&mut controller, b"j\r", &frame);
        let (action, target) = controller.take_action().ok_or("tab action")?;
        assert_eq!(action, Action::RenameTab);
        assert_eq!(target.active_tab, Some(TabId(2)));
        assert_eq!(frame.active_tab, Some(TabId(1)));
        assert!(matches!(
            controller.mouse(right, &frame),
            MouseDisposition::Local
        ));
        frame.tabs.pop();
        controller.reconcile(&frame);
        assert!(!controller.active());
        Ok(())
    }

    #[test]
    fn swap_picker_selects_an_explicit_nonadjacent_target_and_cancels_stale_edits()
    -> Result<(), &'static str> {
        let mut frame = split_frame();
        frame.server_instance = "attached".into();
        frame.panes.insert(
            PaneId(3),
            frame.panes.get(&PaneId(1)).cloned().ok_or("pane")?,
        );
        frame.layout.push(PaneRect {
            pane: PaneId(3),
            rect: crate::layout::Rect {
                x: 26,
                y: 0,
                width: 12,
                height: 6,
            },
        });
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::SwapPane, &frame));
        frame.focused = Some(PaneId(2));
        controller.reconcile(&frame);
        let requests = feed(&mut controller, b"j\r", &frame);
        assert!(matches!(requests.as_slice(), [Request::Layout {
            instance: Some(instance), tab: TabId(1), generation: Some(17),
            action: LayoutAction::Swap { pane: PaneId(1), target: PaneId(3) }, ..
        }] if instance == "attached"));
        frame.focused = Some(PaneId(1));
        assert!(controller.enter(Action::SwapPane, &frame));
        assert!(feed(&mut controller, b"\x1b[200~j\r\x1b[201~", &frame).is_empty());
        assert!(controller.active());
        controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 1)];
        let click = MouseEvent {
            code: 0,
            column: 11,
            row: 5,
            release: false,
        };
        assert!(matches!(
            controller.mouse(click, &frame),
            MouseDisposition::Request(Request::Layout {
                action: LayoutAction::Swap {
                    pane: PaneId(1),
                    target: PaneId(3)
                },
                ..
            })
        ));
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    release: true,
                    ..click
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(controller.enter(Action::SwapPane, &frame));
        frame.layout_generation += 1;
        assert!(feed(&mut controller, b"\r", &frame).is_empty());
        assert!(!controller.active());
        assert!(!controller.active());
        Ok(())
    }

    #[test]
    fn workspace_close_confirms_the_captured_lifetime_and_rejects_replacements() {
        let mut frame = frame();
        frame.server_instance = "attached".into();
        frame.workspace_stream = 42;
        let mut controller = Controller::new(false);
        assert!(controller.enter(Action::CloseWorkspace, &frame));
        assert!(feed(&mut controller, b"\x1b[200~y\x1b[201~", &frame).is_empty());
        assert!(controller.active());
        assert_eq!(
            feed(&mut controller, b"y", &frame),
            vec![Request::Workspace {
                id: 0,
                instance: Some("attached".into()),
                stream: Some(42),
                action: WorkspaceAction::Kill {
                    name: "default".into()
                },
            }]
        );
        for change in 0..4 {
            assert!(controller.enter(Action::CloseWorkspace, &frame));
            let mut changed = frame.clone();
            match change {
                0 => changed.workspace_stream = 43,
                1 => changed.server_instance = "replacement".into(),
                2 => changed.workspace = "other".into(),
                _ => changed.viewer = crate::ids::ViewerId(999),
            }
            assert!(feed(&mut controller, b"y", &changed).is_empty());
            assert!(!controller.active());
        }
        assert!(controller.enter(Action::CloseWorkspace, &frame));
        controller.capture.adopt(2);
        assert!(feed(&mut controller, b"n", &frame).is_empty());
        assert!(
            !controller.active(),
            "cancel must leave a path for the captured release"
        );
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    code: 2,
                    column: 2,
                    row: 2,
                    release: true
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(controller.capture.auxiliary().is_none());
    }

    #[test]
    fn workspace_rename_edits_labels_and_rejects_stale_lifetimes() {
        let mut frame = split_frame();
        frame.server_instance = "attached".into();
        frame.workspace_stream = 7;
        let mut controller = Controller::new(false);
        assert!(controller.enter(Action::RenameWorkspace, &frame));
        assert!(feed(&mut controller, b"\x1b[200~pasted\r\x1b[201~", &frame).is_empty());
        assert_eq!(
            feed(&mut controller, "\u{15}Build 界\r".as_bytes(), &frame),
            vec![Request::Workspace {
                id: 0,
                instance: Some("attached".into()),
                stream: Some(7),
                action: WorkspaceAction::Rename {
                    label: "Build 界".into()
                },
            }]
        );
        assert!(controller.enter(Action::RenameWorkspace, &frame));
        frame.workspace_stream += 1;
        assert!(feed(&mut controller, b"wrong\r", &frame).is_empty());
        assert!(!controller.active());
    }

    #[test]
    fn pane_rename_keeps_its_target_and_cancels_on_identity_changes() {
        let mut frame = split_frame();
        frame.server_instance = "attached".into();
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::RenamePane, &frame));
        frame.focused = Some(PaneId(2));
        controller.reconcile(&frame);
        assert_eq!(
            feed(&mut controller, "manual界\r".as_bytes(), &frame),
            vec![Request::RenamePane {
                id: 0,
                instance: Some("attached".into()),
                pane: PaneId(1),
                name: "manual界".into(),
            }]
        );
        assert!(controller.enter(Action::RenamePane, &frame));
        assert!(feed(&mut controller, b"\x1b[200~pasted\r\x1b[201~", &frame).is_empty());
        assert!(controller.active(), "pasted newlines must not submit");
        assert_eq!(
            feed(&mut controller, b"\x15\r", &frame),
            vec![Request::RenamePane {
                id: 0,
                instance: Some("attached".into()),
                pane: PaneId(2),
                name: String::new(),
            }]
        );
        for change in 0..3 {
            let mut changed = frame.clone();
            assert!(controller.enter(Action::RenamePane, &frame));
            match change {
                0 => changed.server_instance = "replacement".into(),
                1 => changed.workspace = "other".into(),
                _ => {
                    changed.panes.remove(&PaneId(2));
                }
            }
            controller.reconcile(&changed);
            assert!(!controller.active());
            assert!(feed(&mut controller, b"\r", &changed).is_empty());
        }
    }

    #[test]
    fn rename_submits_fragmented_unicode_and_cancels_without_mutation() {
        let frame = frame();
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::RenameTab, &frame));
        let requests = feed(&mut controller, "\u{15}renamed界\r".as_bytes(), &frame);
        assert_eq!(
            requests,
            vec![Request::Tab {
                instance: None,
                id: 0,
                action: TabAction::Rename {
                    tab: TabId(1),
                    name: "renamed界".into()
                }
            }]
        );
        assert!(!controller.active());
        assert!(controller.enter(Action::RenameTab, &frame));
        assert!(feed(&mut controller, b"discard\x1b", &frame).is_empty());
        controller.resolve_escape();
        assert!(!controller.active() && !controller.owns_input());
    }

    #[test]
    fn confirmations_carry_the_original_target_and_ignore_paste() {
        let frame = frame();
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::ClosePane, &frame));
        assert!(feed(&mut controller, b"\x1b[200~y\r\x1b[201~", &frame).is_empty());
        assert!(controller.active(), "pasted confirmation ignored");
        assert_eq!(
            feed(&mut controller, b"y", &frame),
            vec![Request::Kill {
                instance: None,
                id: 0,
                pane: PaneId(1)
            }]
        );
        assert!(controller.enter(Action::CloseTab, &frame));
        assert_eq!(
            feed(&mut controller, b"Y", &frame),
            vec![Request::Tab {
                instance: None,
                id: 0,
                action: TabAction::Close { tab: TabId(1) }
            }]
        );
        // A stale target cancels with feedback when the frame no longer has it.
        assert!(controller.enter(Action::ClosePane, &frame));
        controller.reconcile(&Frame::default());
        assert!(!controller.active());
        assert!(controller.error().is_some());
    }

    #[test]
    fn resize_repeats_with_arrows_and_application_cursor_keys() {
        let frame = frame();
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::ResizeMode, &frame));
        let requests = feed(&mut controller, b"j\x1b[A\x1bOC\x1bOD\r", &frame);
        let directions: Vec<crate::layout::Direction> = requests
            .iter()
            .filter_map(|request| match request {
                Request::Layout {
                    action: LayoutAction::ResizeToward { direction, .. },
                    ..
                } => Some(*direction),
                _ => None,
            })
            .collect();
        use crate::layout::Direction::{Down, Left, Right, Up};
        assert_eq!(directions, vec![Down, Up, Right, Left]);
        assert!(!controller.active());
    }

    #[test]
    fn workspace_chooser_replays_buffered_input_and_switches() {
        let frame = frame();
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::ChooseWorkspace, &frame));
        assert!(feed(&mut controller, b"j\r", &frame).is_empty());
        controller.workspaces_loaded(
            Ok(vec!["default".into(), "other".into()]
                .into_iter()
                .map(|name| crate::proto::control::WorkspaceRoute {
                    name,
                    label: None,
                    stream: 1,
                })
                .collect()),
            "default",
        );
        let replay = controller.take_loading_input();
        assert_eq!(
            feed(&mut controller, &replay, &frame),
            vec![Request::Workspace {
                stream: None,
                instance: None,
                id: 0,
                action: WorkspaceAction::Select {
                    name: "other".into()
                }
            }]
        );
        let mut controller = Controller::new(true);
        controller.enter(Action::ChooseWorkspace, &frame);
        controller.workspaces_loaded(Err(anyhow::anyhow!("lookup failed")), "default");
        assert!(!controller.active() && !controller.owns_input());
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::NewWorkspace, &frame));
        assert_eq!(
            feed(&mut controller, b"proj\r", &frame),
            vec![Request::Workspace {
                stream: None,
                instance: None,
                id: 0,
                action: WorkspaceAction::New {
                    name: Some("proj".into())
                }
            }]
        );
    }

    #[test]
    fn normal_mouse_does_not_belong_to_stale_popup_regions() {
        let frame = frame();
        let mut controller = Controller::new(true);
        controller.set_regions(super::super::render::HitRegions {
            tabs: Vec::new(),
            entries: Vec::new(),
            panel: Some(ratatui_core::layout::Rect::new(2, 1, 8, 3)),
        });
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    code: 0,
                    column: 4,
                    row: 2,
                    release: false
                },
                &frame
            ),
            MouseDisposition::Forward
        ));
    }

    #[test]
    fn text_fields_ignore_inside_clicks_and_dismiss_outside_with_release_owned() {
        let mut frame = frame();
        frame.server_instance = "fixture-instance".into();
        frame.workspace_stream = 7;
        for action in [
            Action::RenamePane,
            Action::RenameTab,
            Action::RenameWorkspace,
            Action::NewWorkspace,
            Action::MoveToNewWorkspace,
        ] {
            let mut controller = Controller::new(true);
            assert!(controller.enter(action, &frame));
            controller.set_regions(super::super::render::HitRegions {
                tabs: Vec::new(),
                entries: Vec::new(),
                panel: Some(ratatui_core::layout::Rect::new(2, 1, 8, 3)),
            });
            let click = MouseEvent {
                code: 0,
                column: 4,
                row: 2,
                release: false,
            };
            assert!(!matches!(
                controller.mouse(click, &frame),
                MouseDisposition::Request(_)
            ));
            assert!(controller.active(), "inside click dismissed {action:?}");
            controller.mouse(
                MouseEvent {
                    release: true,
                    ..click
                },
                &frame,
            );
            assert!(!matches!(
                controller.mouse(
                    MouseEvent {
                        column: 1,
                        row: 1,
                        ..click
                    },
                    &frame
                ),
                MouseDisposition::Request(_)
            ));
            assert!(
                !controller.owns_input(),
                "outside click retained {action:?}"
            );
            assert!(matches!(
                controller.mouse(
                    MouseEvent {
                        column: 1,
                        row: 1,
                        release: true,
                        ..click
                    },
                    &frame
                ),
                MouseDisposition::Ignore
            ));
            assert!(!controller.capture.left_pending());
        }
    }

    #[test]
    fn captured_tab_and_layout_modes_cannot_submit_after_target_loss() {
        for action in [
            Action::RenameTab,
            Action::CloseTab,
            Action::ReorderTab,
            Action::ResizeMode,
            Action::SwapMode,
            Action::MoveMode,
        ] {
            for remove_pane in [false, true] {
                if remove_pane
                    && matches!(
                        action,
                        Action::RenameTab | Action::CloseTab | Action::ReorderTab
                    )
                {
                    continue; // These actions target the tab, not an individual pane.
                }
                let mut original = split_frame();
                original.server_instance = "attached".into();
                let mut controller = Controller::new(true);
                controller.reconcile(&original);
                assert!(controller.enter(action, &original));
                let mut changed = original.clone();
                if remove_pane {
                    changed.panes.remove(&PaneId(1));
                    changed.focused = Some(PaneId(2));
                } else {
                    changed.tabs.clear();
                    changed.active_tab = None;
                }
                controller.reconcile(&changed);
                assert!(
                    !controller.owns_input(),
                    "{action:?} retained a lost target"
                );
                assert!(
                    feed(&mut controller, b"y\r\x1b[C", &changed).is_empty(),
                    "{action:?} mutated a replacement target"
                );
                assert!(controller.take_action().is_none());
                assert!(controller.take_manager_request().is_none());
            }
        }
    }

    #[test]
    fn workspace_creation_and_menu_discard_replaced_attachment_context() {
        for action in [Action::NewWorkspace, Action::WorkspaceMenu] {
            for replacement in 0..4 {
                let mut original = split_frame();
                original.server_instance = "attached".into();
                original.workspace_stream = 7;
                let mut controller = Controller::new(true);
                controller.reconcile(&original);
                assert!(controller.enter(action, &original));
                let mut changed = original.clone();
                match replacement {
                    0 => changed.server_instance = "replacement".into(),
                    1 => changed.workspace = "replacement".into(),
                    2 => changed.workspace_stream += 1,
                    _ => changed.viewer = crate::ids::ViewerId(99),
                }
                controller.reconcile(&changed);
                assert!(
                    !controller.owns_input(),
                    "{action:?} retained context {replacement}"
                );
                assert!(feed(&mut controller, b"unintended\r", &changed).is_empty());
                assert!(controller.take_action().is_none());
                assert!(controller.take_manager_request().is_none());
            }
        }
    }

    #[test]
    fn vanished_tab_choice_reports_failure_and_escape_returns_normal() {
        let mut frame = split_frame();
        frame.tabs.push(TabEntry {
            id: TabId(2),
            label: "other".into(),
            layout_generation: 1,
            first_pane: Some(PaneId(3)),
        });
        let mut controller = Controller::new(true);
        controller.reconcile(&frame);
        assert!(controller.enter(Action::ChooseTab, &frame));
        assert!(feed(&mut controller, b"j", &frame).is_empty());
        frame.tabs.retain(|tab| tab.id != TabId(2));
        controller.reconcile(&frame);
        assert!(feed(&mut controller, b"\r", &frame).is_empty());
        assert!(
            controller.active(),
            "a chooser can still select a remaining tab"
        );
        assert_eq!(
            controller.error(),
            Some("That tab no longer exists; Esc dismisses.")
        );
        assert!(feed(&mut controller, b"\x1b", &frame).is_empty());
        controller.resolve_escape();
        assert!(!controller.owns_input());
        assert!(controller.take_action().is_none());
        assert!(controller.take_manager_request().is_none());
    }

    #[test]
    fn transient_modes_escape_to_normal_without_forwarding_or_mutating() {
        let mut frame = split_frame();
        frame.server_instance = "fixture-instance".into();
        frame.workspace_stream = 9;
        frame.tabs.push(TabEntry {
            id: TabId(2),
            label: "other".into(),
            layout_generation: 1,
            first_pane: None,
        });
        for action in [
            Action::RenameWorkspace,
            Action::CloseWorkspace,
            Action::PaneMenu,
            Action::TabMenu,
            Action::WorkspaceMenu,
            Action::CopyMode,
            Action::ChooseTab,
            Action::MoveToTab,
            Action::ReorderTab,
            Action::RenamePane,
            Action::RenameTab,
            Action::CloseTab,
            Action::ClosePane,
            Action::ResizeMode,
            Action::SwapMode,
            Action::SwapPane,
            Action::MoveMode,
            Action::NewWorkspace,
            Action::MoveToNewWorkspace,
            Action::MoveToWorkspace,
            Action::ChooseWorkspace,
            Action::ReorderWorkspace,
        ] {
            let mut controller = Controller::new(true);
            controller.reconcile(&frame);
            assert!(
                controller.enter(action, &frame),
                "fixture cannot enter {action:?}"
            );
            assert!(controller.owns_input(), "missing owner for {action:?}");
            assert!(
                feed(&mut controller, b"\x1b", &frame).is_empty(),
                "Escape mutated {action:?}"
            );
            controller.resolve_escape();
            assert!(
                !controller.owns_input() && !controller.active(),
                "Escape retained {action:?}"
            );
            assert!(controller.take_action().is_none());
            assert!(controller.take_copied().is_none());
            let mut filter =
                super::super::input::PrefixFilter::new(crate::commands::ClientBindings::new(1, []));
            assert_eq!(
                filter.feed(b"N"),
                vec![super::super::input::InputEvent::Bytes(vec![b'N'])]
            );
        }
    }

    #[test]
    fn workspace_identity_change_cancels_lookup_and_rejects_its_completion() {
        let mut frame = frame();
        let mut controller = Controller::new(true);
        controller.reconcile(&frame);
        controller.enter(Action::ChooseWorkspace, &frame);
        let epoch = controller.interaction_epoch();
        frame.workspace_stream += 1;
        controller.reconcile(&frame);
        assert!(!controller.owns_input());
        assert!(!controller.workspaces_loaded_for(
            epoch,
            Err(anyhow::anyhow!("stale lookup")),
            "default"
        ));
        assert!(controller.error().is_none());
    }

    #[test]
    fn pending_workspace_lookup_browses_without_replaying_mouse() {
        let frame = frame();
        let mut controller = Controller::new(true);
        controller.enter(Action::ChooseWorkspace, &frame);
        feed(&mut controller, b"j\x1b[<64;3;3M", &frame);
        assert_eq!(controller.local_views().len(), 1);
        assert_eq!(controller.loading_input, b"j");
        feed(&mut controller, b"\x1b", &frame);
        controller.resolve_escape();
        assert!(!controller.owns_input());
        assert!(controller.take_loading_input().is_empty());
        assert_eq!(controller.local_views().len(), 1);
    }

    #[test]
    fn pending_destination_browses_and_dismisses_without_replaying_mouse() {
        let mut frame = frame();
        frame.server_instance = "attached".into();
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::MoveToWorkspace, &frame));
        controller.take_manager_request();
        feed(&mut controller, b"j\x1b[<64;3;3M", &frame);
        assert_eq!(controller.local_views().len(), 1);
        assert_eq!(controller.loading_input, b"j");
        feed(&mut controller, b"\x1b[<0;3;3M\x1b[<0;3;3m", &frame);
        assert!(!controller.active());
        assert!(controller.take_loading_input().is_empty());
        assert!(controller.take_manager_request().is_none());
        assert_eq!(controller.local_views().len(), 1);
    }

    #[test]
    fn fresh_selection_press_recovers_a_lost_release() {
        for (column, pane, anchor) in [(5, PaneId(1), (1, 4)), (17, PaneId(2), (1, 3))] {
            let frame = split_frame();
            let mut controller = Controller::new(true);
            controller.enter(Action::CopyMode, &frame);
            feed(&mut controller, b"\x1b[<4;2;2M\x1b[<36;3;3M", &frame);
            assert!(controller.selection_dragging());
            let press = MouseEvent {
                code: 4,
                column,
                row: 2,
                release: false,
            };
            assert!(matches!(
                controller.mouse(press, &frame),
                MouseDisposition::Local
            ));
            assert!(matches!(&controller.mode, Mode::Copy(copy)
                if copy.pane() == pane && copy.anchor() == Some(anchor)));
            controller.mouse(
                MouseEvent {
                    release: true,
                    ..press
                },
                &frame,
            );
            assert!(!controller.selection_dragging());
        }
    }

    #[test]
    fn waiting_command_browses_without_replaying_mouse_and_cancels_buffered_suffix() {
        let frame = frame();
        let mut controller = Controller::new(true);
        controller.wait_for_command();
        let epoch = controller.interaction_epoch();
        feed(&mut controller, b"name\x1b[<64;3;3M", &frame);
        assert_eq!(controller.local_views().len(), 1);
        assert_eq!(controller.loading_input, b"name");
        assert!(controller.waiting_command_ready());
        feed(&mut controller, b"\x1b", &frame);
        controller.resolve_escape();
        assert_ne!(controller.interaction_epoch(), epoch);
        assert!(!controller.owns_input());
        assert!(controller.take_loading_input().is_empty());
        assert_eq!(controller.local_views().len(), 1);
    }

    #[test]
    fn selection_exit_and_clear_own_the_release_then_accept_a_new_drag() {
        for finish in [b"\x1b".as_slice(), b"q", b"c", b"\x1b[<64;3;3M"] {
            let frame = frame();
            let mut controller = Controller::new(true);
            controller.enter(Action::CopyMode, &frame);
            feed(&mut controller, b"\x1b[<0;3;3M", &frame);
            assert!(controller.selection_dragging());
            feed(&mut controller, finish, &frame);
            controller.resolve_escape();
            assert!(!controller.selection_dragging());
            assert!(controller.capture.left_pending());
            let release = MouseEvent {
                code: 0,
                column: 3,
                row: 3,
                release: true,
            };
            assert!(matches!(
                controller.mouse(release, &frame),
                MouseDisposition::Ignore
            ));
            assert!(!controller.capture.left_pending());
            controller.enter(Action::CopyMode, &frame);
            feed(&mut controller, b"\x1b[<0;3;3M", &frame);
            assert!(controller.selection_dragging());
        }
    }

    #[test]
    fn popup_auxiliary_capture_survives_keyboard_command_handoff() {
        for button in [1, 2] {
            let mut frame = split_frame();
            frame
                .panes
                .get_mut(&PaneId(2))
                .unwrap_or_else(|| panic!("pane 2"))
                .modes
                .mouse_mode = MouseMode::AnyMotion;
            let mut popup = super::super::popup::Popup::default();
            let press = MouseEvent {
                code: button,
                column: 15,
                row: 2,
                release: false,
            };
            assert_eq!(popup.mouse(press), super::super::popup::Outcome::Ignore);
            let mut controller = Controller::new(true);
            controller
                .adopt_popup_capture(popup.take_capture().unwrap_or_else(|| panic!("capture")));
            assert!(controller.enter(Action::CopyMode, &frame));
            assert!(matches!(
                controller.mouse(
                    MouseEvent {
                        release: true,
                        ..press
                    },
                    &frame
                ),
                MouseDisposition::Ignore
            ));
            assert!(matches!(
                controller.mouse(press, &frame),
                MouseDisposition::Forward
            ));
        }
    }

    #[test]
    fn popup_capture_survives_parser_handoff_and_recovers_without_release() {
        let frame = frame();
        let mut controller = Controller::new(true);
        controller.suppress_gesture_tail();
        assert!(controller.enter(Action::CopyMode, &frame));
        assert!(feed(&mut controller, b"\x1b[<0;3;3m", &frame).is_empty());
        assert!(matches!(&controller.mode, Mode::Copy(copy) if !copy.selecting()));
        assert!(feed(&mut controller, b"\x1b[<0;3;3M", &frame).is_empty());
        assert!(matches!(&controller.mode, Mode::Copy(copy) if copy.selecting()));
        controller.end_interaction();
        assert!(controller.capture.left_pending());
        assert!(controller.enter(Action::CopyMode, &frame));
        assert!(feed(&mut controller, b"\x1b[<0;3;3M", &frame).is_empty());
        assert!(matches!(&controller.mode, Mode::Copy(copy) if copy.selecting()));
        assert!(!controller.capture.left_pending());
    }

    #[test]
    fn wheel_browsing_is_passive_and_accepts_another_pane() {
        let frame = split_frame();
        let mut controller = Controller::new(true);
        let wheel = MouseEvent {
            code: 64,
            column: 3,
            row: 3,
            release: false,
        };
        assert!(matches!(
            controller.mouse(wheel, &frame),
            MouseDisposition::Local
        ));
        assert!(
            !controller.owns_input(),
            "wheel must not capture application keys"
        );
        let other = MouseEvent {
            column: 16,
            ..wheel
        };
        assert!(matches!(
            controller.mouse(other, &frame),
            MouseDisposition::Local
        ));
        let first = controller
            .take_read()
            .unwrap_or_else(|| panic!("missing fixture value"));
        let second = controller
            .take_read()
            .unwrap_or_else(|| panic!("missing fixture value"));
        assert_ne!(first.1, second.1);
        assert_eq!((first.2, second.2), (3, 3));
    }

    #[test]
    fn histories_are_memory_bounded_and_exited_apps_do_not_own_the_mouse() {
        let mut frame = split_frame();
        let mut controller = Controller::new(true);
        if let Some(view) = frame.panes.get_mut(&PaneId(1)) {
            view.exit = Some(0);
            view.modes.mouse_mode = MouseMode::AnyMotion;
        }
        assert!(Action::CopyMode.unavailable(&frame, true).is_none());
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    code: 64,
                    column: 3,
                    row: 3,
                    release: false
                },
                &frame
            ),
            MouseDisposition::Local
        ));
        assert_eq!(controller.local_views().len(), 1);
        controller.enforce_history_budget(1);
        assert!(controller.local_views().is_empty());
        assert!(!controller.owns_input());
    }

    #[test]
    fn overlapping_paste_delimiters_release_a_cancelled_mode() {
        let frame = frame();
        for split in 0..=4 {
            let mut controller = Controller::new(true);
            controller.enter(Action::CopyMode, &frame);
            feed(&mut controller, b"\x1b[200~", &frame);
            let payload = b"a\x1b\x1b[201~";
            let (first, second) = payload.split_at(split);
            feed(&mut controller, first, &frame);
            controller.reconcile(&Frame::default());
            feed(&mut controller, second, &frame);
            assert!(
                !controller.owns_input(),
                "paste ownership stuck at split {split}"
            );
            assert!(controller.take_action().is_none());
        }
    }

    #[test]
    fn clearing_a_selection_releases_capture_without_leaving_copy() {
        let frame = frame();
        let mut controller = Controller::new(true);
        controller.enter(Action::CopyMode, &frame);
        controller.mouse(
            MouseEvent {
                code: 4,
                column: 3,
                row: 2,
                release: false,
            },
            &frame,
        );
        feed(&mut controller, b"c", &frame);
        assert!(
            matches!(&controller.mode, Mode::Copy(copy) if !copy.dragging() && !copy.selecting())
        );
    }

    #[test]
    fn cancelled_lookup_cannot_populate_a_reopened_chooser() {
        let frame = frame();
        let mut controller = Controller::new(true);
        controller.enter(Action::ChooseWorkspace, &frame);
        let old = controller.interaction_epoch();
        feed(&mut controller, b"j\x1b", &frame);
        controller.resolve_escape();
        controller.enter(Action::ChooseWorkspace, &frame);
        let new = controller.interaction_epoch();
        assert_ne!(new, old);
        assert!(!controller.workspaces_loaded_for(
            old,
            Err(anyhow::anyhow!("old failure")),
            "default"
        ));
        assert!(matches!(controller.mode, Mode::LoadingWorkspaces { .. }));
        assert!(controller.take_loading_input().is_empty());
        assert!(controller.workspaces_loaded_for(
            new,
            Err(anyhow::anyhow!("current failure")),
            "default"
        ));
        assert!(!controller.active());
        assert_eq!(controller.error(), Some("current failure"));
    }

    #[test]
    fn buffer_switch_discards_history_and_ignores_old_buffer_reply() {
        let mut frame = frame();
        let mut controller = Controller::new(true);
        controller.reconcile(&frame);
        let wheel = MouseEvent {
            code: 64,
            column: 3,
            row: 3,
            release: false,
        };
        controller.mouse(wheel, &frame);
        let (old, pane, _) = controller
            .take_read()
            .unwrap_or_else(|| panic!("history read"));
        frame
            .panes
            .get_mut(&pane)
            .unwrap_or_else(|| panic!("pane"))
            .modes
            .alternate_screen = true;
        controller.reconcile(&frame);
        assert!(controller.local_views().is_empty());
        controller.mouse(MouseEvent { code: 68, ..wheel }, &frame);
        let (new, _, _) = controller
            .take_read()
            .unwrap_or_else(|| panic!("alternate history read"));
        assert_ne!(old, new);
        controller.install_view(ViewReply {
            request: old,
            pane,
            view: None,
            history: 0,
        });
        assert_eq!(controller.local_views().len(), 1);
        controller.enter(Action::CopyMode, &frame);
        frame
            .panes
            .get_mut(&pane)
            .unwrap_or_else(|| panic!("pane"))
            .modes
            .alternate_screen = false;
        controller.reconcile(&frame);
        assert!(!controller.owns_input());
        assert!(controller.local_views().is_empty());
    }

    #[test]
    fn passive_histories_resume_only_the_focused_pane_and_expire_by_identity() {
        let mut frame = split_frame();
        let mut controller = Controller::new(true);
        controller.reconcile(&frame);
        for column in [3, 16] {
            controller.mouse(
                MouseEvent {
                    code: 64,
                    column,
                    row: 3,
                    release: false,
                },
                &frame,
            );
        }
        assert_eq!(controller.local_views().len(), 2);
        controller.resume_input(&frame);
        assert_eq!(controller.local_views().len(), 1);
        assert_eq!(
            controller.local_views().first().map(|view| view.pane),
            Some(PaneId(2))
        );
        frame.workspace_stream += 1;
        controller.reconcile(&frame);
        assert!(controller.local_views().is_empty());
    }

    #[test]
    fn copy_parser_forwards_other_application_mouse_and_ends_outside_drag() {
        let mut frame = split_frame();
        frame
            .panes
            .get_mut(&PaneId(2))
            .unwrap_or_else(|| panic!("missing fixture value"))
            .modes
            .mouse_mode = MouseMode::AnyMotion;
        let mut controller = Controller::new(true);
        controller.enter(Action::CopyMode, &frame);
        feed(&mut controller, b"\x1b[<64;16;3M", &frame);
        assert_eq!(
            controller.take_forwarded_mouse().map(|event| event.column),
            Some(16)
        );
        controller.mouse(
            MouseEvent {
                code: 4,
                column: 2,
                row: 2,
                release: false,
            },
            &frame,
        );
        controller.mouse(
            MouseEvent {
                code: 4,
                column: 25,
                row: 20,
                release: true,
            },
            &frame,
        );
        assert!(matches!(&controller.mode, Mode::Copy(copy) if !copy.dragging()));
        controller.mouse(
            MouseEvent {
                code: 4,
                column: 4,
                row: 2,
                release: false,
            },
            &frame,
        );
        assert!(matches!(&controller.mode, Mode::Copy(copy) if copy.anchor() == Some((1, 3))));
    }

    #[test]
    fn copy_escape_dismisses_instead_of_requesting_commands() {
        let frame = frame();
        let mut controller = Controller::new(true);
        controller.enter(Action::CopyMode, &frame);
        feed(&mut controller, b" \x1b", &frame);
        controller.resolve_escape();
        assert!(!controller.owns_input());
        assert!(!controller.active());
    }

    #[test]
    fn copy_mode_selection_and_mouse_routing() {
        let frame = frame();
        let mut controller = Controller::new(true);
        assert!(controller.enter(Action::CopyMode, &frame));
        assert!(feed(&mut controller, b"\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D ", &frame).is_empty());
        feed(&mut controller, b"llll", &frame);
        feed(&mut controller, b"y", &frame);
        assert_eq!(controller.take_copied(), Some("hello".into()));
        assert!(!controller.active());
        // A wheel over a pane whose application does not own the mouse browses locally.
        let wheel = MouseEvent {
            code: 64,
            column: 3,
            row: 3,
            release: false,
        };
        assert!(matches!(
            controller.mouse(wheel, &frame),
            MouseDisposition::Local
        ));
        assert!(!controller.owns_input());
        assert_eq!(controller.take_read().map(|(_, _, offset)| offset), Some(3));
        // A click without shift on the pane is the server's business.
        let mut plain = Controller::new(true);
        let click = MouseEvent {
            code: 0,
            column: 3,
            row: 3,
            release: false,
        };
        assert!(matches!(
            plain.mouse(click, &frame),
            MouseDisposition::Forward
        ));
        // Shift-drag selects locally even when the application owns the mouse.
        let mut owned = frame.clone();
        if let Some(view) = owned.panes.get_mut(&PaneId(1)) {
            view.modes.mouse_mode = MouseMode::AnyMotion;
        }
        let mut shift = Controller::new(true);
        let press = MouseEvent {
            code: 4,
            column: 1,
            row: 1,
            release: false,
        };
        assert!(matches!(
            shift.mouse(press, &owned),
            MouseDisposition::Local
        ));
        let drag = MouseEvent {
            code: 36,
            column: 5,
            row: 1,
            release: false,
        };
        assert!(matches!(shift.mouse(drag, &owned), MouseDisposition::Local));
        let release = MouseEvent {
            code: 4,
            column: 5,
            row: 1,
            release: true,
        };
        assert!(matches!(
            shift.mouse(release, &owned),
            MouseDisposition::Local
        ));
        feed(&mut shift, b"y", &owned);
        assert_eq!(shift.take_copied(), Some("hello".into()));
        assert!(matches!(
            Controller::new(true).mouse(wheel, &owned),
            MouseDisposition::Forward
        ));
    }

    #[test]
    fn canceled_modes_keep_owning_unfinished_pastes() {
        let frame = frame();
        let mut controller = Controller::new(true);
        controller.enter(Action::CopyMode, &frame);
        for byte in b"\x1b[200~ab" {
            assert!(controller.feed(*byte, &frame).is_none());
        }
        controller.reconcile(&Frame::default());
        assert!(!controller.active());
        assert!(
            controller.owns_input(),
            "the paste tail still belongs to the controller"
        );
        for byte in b"t\x01xy\x1b[201~" {
            assert!(controller.feed(*byte, &frame).is_none());
        }
        assert!(!controller.owns_input());
    }
}

#[cfg(test)]
#[path = "control_traces.rs"]
mod control_traces;
