//! Viewer-local interaction modes: history/copy, tab and workspace choosers, rename, confirmed
//! closes, repeated resize and workspace naming. Transient state never leaves this process.

use super::copy::{CopyKey, CopySession};
use super::effects::Identity;
use super::hints::HintPanel;
use super::input::{Nav, PASTE_BEGIN, PASTE_END, navigation, sequence_complete};
use super::interaction::{
    CloseKind, LayoutMode, MAX_TEXT_BYTES, Mode, Step, TabChoice, TextKind, step,
};
use crate::commands::{Action, Target};
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

/// How long a bar notice stays without a key press.
pub const NOTICE_TTL: std::time::Duration = std::time::Duration::from_secs(2);

pub struct Controller {
    pending_action: Option<(Action, Target)>,
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

    pub fn take_action(&mut self) -> Option<(Action, Target)> {
        self.pending_action.take()
    }

    pub(super) fn selection_dragging(&self) -> bool {
        matches!(&self.mode, Mode::Copy(copy) if copy.dragging())
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

    #[cfg(test)]
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

    /// Enters a mode for a modal action at the viewer's own focus and tab.
    #[cfg(test)]
    pub fn enter(&mut self, action: Action, frame: &Frame) -> bool {
        self.enter_at(action, frame, Target::of(frame))
    }

    /// Enters a mode for a modal action; returns false for actions that need no mode.
    pub fn enter_at(&mut self, action: Action, frame: &Frame, target: Target) -> bool {
        tracing::debug!(target: "fux::diagnostics", pid = std::process::id(), event = "interaction_enter", ?action,
            epoch = self.interaction_epoch, instance = %frame.server_instance,
            viewer = frame.viewer.0, pane = ?target.focused.map(|pane| pane.0), stream = frame.workspace_stream);
        self.interaction_epoch = self.interaction_epoch.wrapping_add(1);
        self.reset_input();
        let tab = target.tab;
        let pane = target.focused;
        let identity = Identity::of(frame);
        let has_identity = !frame.server_instance.is_empty();
        self.entry_regions.clear();
        self.panel_bounds = None;
        let text = |kind: TextKind, text: String| Mode::Text { kind, text };
        self.mode = match action {
            Action::CloseWorkspace if frame.workspace_stream != 0 && has_identity => {
                Mode::Confirm(CloseKind::Workspace(identity))
            }
            Action::SwapPane => match super::context::SwapPicker::new(frame) {
                Some(picker) => Mode::SwapPicker(Box::new(picker)),
                None => return false,
            },
            Action::PaneMenu | Action::TabMenu | Action::WorkspaceMenu => {
                let subject = match action {
                    Action::PaneMenu => match pane {
                        Some(pane) => super::context::Subject::Pane(pane),
                        None => return false,
                    },
                    Action::TabMenu => match tab {
                        Some(tab) => super::context::Subject::Tab(tab),
                        None => return false,
                    },
                    _ => super::context::Subject::Workspace,
                };
                match super::context::Menu::new(subject, frame, self.workspaces_enabled) {
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
                            generation: target.generation,
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
                    Some((pane, view)) if has_identity => text(
                        TextKind::RenamePane { pane, identity },
                        view.label.clone().unwrap_or_default(),
                    ),
                    _ => return false,
                }
            }
            Action::RenameWorkspace if has_identity && frame.workspace_stream != 0 => text(
                TextKind::RenameWorkspace { identity },
                frame.workspace_label.clone().unwrap_or_default(),
            ),
            Action::RenameTab => {
                match tab.and_then(|tab| frame.tabs.iter().find(|entry| entry.id == tab)) {
                    Some(entry) => text(TextKind::RenameTab { tab: entry.id }, entry.label.clone()),
                    None => return false,
                }
            }
            Action::ClosePane => match pane {
                Some(pane) => Mode::Confirm(CloseKind::Pane(pane)),
                None => return false,
            },
            Action::CloseTab => {
                match tab.and_then(|tab| frame.tabs.iter().find(|entry| entry.id == tab)) {
                    Some(entry) => Mode::Confirm(CloseKind::Tab {
                        tab: entry.id,
                        label: entry.label.clone(),
                    }),
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
            Action::NewWorkspace => text(TextKind::NewWorkspace { transfer: None }, String::new()),
            Action::MoveToNewWorkspace | Action::MoveToWorkspace => match pane.zip(tab) {
                Some((pane, tab)) if has_identity => {
                    let transfer = Box::new(crate::proto::control::WorkspaceTransfer {
                        focus: false,
                        follow: Some(frame.viewer),
                        instance: frame.server_instance.clone(),
                        source: tab,
                        generation: target.generation,
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
                        text(
                            TextKind::NewWorkspace {
                                transfer: Some(transfer),
                            },
                            String::new(),
                        )
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
        let confirm = matches!(self.mode, Mode::Confirm(_));
        if confirm || self.mode.selection_mut().is_some() {
            // Close dialogs answer only a left press; lists also scroll with the wheel.
            if confirm && !left_press {
                return MouseDisposition::Ignore;
            }
            self.reconcile(frame);
            if !(confirm || self.mode.selection_mut().is_some()) {
                if left_press {
                    self.capture.adopt(0);
                }
                return MouseDisposition::Ignore;
            }
            if super::drag::plain_wheel(mouse) {
                if let Some((selected, len)) = self.mode.selection_mut() {
                    step(selected, len, if mouse.button() == 1 { 'j' } else { 'k' });
                    return MouseDisposition::Local;
                }
                return MouseDisposition::Ignore;
            }
            if !left_press {
                return MouseDisposition::Ignore;
            }
            let hit = self.hit(mouse);
            let request = if confirm {
                match hit {
                    Some(1) => self.key('y', frame),
                    Some(0) => None, // The explanatory row is not an action.
                    _ => self.key('n', frame),
                }
            } else if let Some((selected, len)) = self.mode.selection_mut()
                && let Some(index) = hit.filter(|index| *index < len)
            {
                *selected = index;
                self.key('\r', frame)
            } else {
                self.end_interaction();
                None
            };
            self.capture.adopt(0);
            return request.map_or(MouseDisposition::Local, MouseDisposition::Request);
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
            && let Some(subject) = super::context::mouse_subject(mouse, frame, &self.tab_regions)
            && let Some(menu) = super::context::Menu::new(subject, frame, self.workspaces_enabled)
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
            if !sequence_complete(&self.escape) {
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
            if sequence == PASTE_BEGIN {
                self.paste = true;
                return None;
            }
            if sequence == PASTE_END || self.paste {
                self.paste = false;
                return None;
            }
            // Arrows step lists (either axis), move within copy/layout modes, and are dropped
            // by text fields; PageUp/PageDown page the copy view; keypad Enter submits.
            let directional = matches!(self.mode, Mode::Copy(_) | Mode::Layout { .. });
            let key = match navigation(&sequence) {
                Some(Nav::Enter) => '\r',
                Some(Nav::PageUp) if self.in_copy() => return self.copy_key(CopyKey::PageUp),
                Some(Nav::PageDown) if self.in_copy() => return self.copy_key(CopyKey::PageDown),
                Some(Nav::Left) if directional => 'h',
                Some(Nav::Right) if directional => 'l',
                Some(Nav::Up) if directional => 'k',
                Some(Nav::Down) if directional => 'j',
                Some(Nav::Up | Nav::Left) if !self.text_entry() => 'k',
                Some(Nav::Down | Nav::Right) if !self.text_entry() => 'j',
                _ => return None,
            };
            return self.key(key, frame);
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

    /// The chooser entry under the pointer, if any.
    fn hit(&self, mouse: MouseEvent) -> Option<usize> {
        let point = (mouse.column.saturating_sub(1), mouse.row.saturating_sub(1)).into();
        self.entry_regions
            .iter()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, index)| *index)
    }

    fn key(&mut self, key: char, frame: &Frame) -> Option<Request> {
        if let super::interaction::FrameTransition::Dismiss(message) = self.mode.reconcile(frame) {
            self.end_interaction();
            self.report_error(message);
            return None;
        }
        let dragging = self.selection_dragging();
        let step = self.mode.key(key, self.paste, frame);
        self.apply(step, dragging)
    }

    fn copy_key(&mut self, key: CopyKey) -> Option<Request> {
        let dragging = self.selection_dragging();
        let step = self.mode.copy(key);
        self.apply(step, dragging)
    }

    /// Applies what a consumed key decided; the mode's own state has already changed.
    fn apply(&mut self, step: Step, was_dragging: bool) -> Option<Request> {
        let request = match step {
            Step::Keep => None,
            Step::Finish => {
                self.end_interaction();
                None
            }
            Step::Send(request) => Some(request),
            Step::Submit(request) => {
                self.end_interaction();
                Some(request)
            }
            Step::Manager(request) => {
                self.end_interaction();
                self.manager_request = Some(request);
                None
            }
            Step::Action(action, target) => {
                self.end_interaction();
                self.pending_action = Some((action, target));
                None
            }
            Step::Copied(text) => {
                self.end_interaction();
                self.copied = Some(text);
                None
            }
            Step::Notice(notice) => {
                self.report_error(notice);
                None
            }
        };
        self.capture
            .cancel_left_if(was_dragging && !self.selection_dragging());
        request
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
            Mode::Text { kind, text } => {
                return Some(HintPanel::text_input(kind.title(), text, kind.footer()));
            }
            Mode::Menu(_) | Mode::SwapPicker(_) => return None,
            Mode::Confirm(kind) => {
                let (title, explanation) = kind.describe();
                (
                    title,
                    vec![explanation.into(), "Confirm close".into(), "Cancel".into()],
                    "y or click confirm · n/Esc or outside click cancel",
                    None,
                )
            }
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
}

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "control_traces.rs"]
mod control_traces;
