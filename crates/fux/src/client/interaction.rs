//! Keyboard interaction state and its frame-driven ownership decisions.
//! This layer has no sockets, global focus mutations or UI notification side effects.
use super::copy::{CopyKey, CopyOutcome, CopySession};
use super::effects::Identity;
use crate::commands::{Action, Target};
use crate::proto::control::{
    LayoutAction, PaneDestination, Request, TabAction, WorkspaceAction, WorkspaceDestination,
    WorkspaceTransfer,
};
use crate::{
    daemon::ManagerRequest,
    ids::{PaneId, TabId},
    view::{Frame, TabEntry},
};
use unicode_segmentation::UnicodeSegmentation as _;

#[derive(Clone, Copy)]
pub(super) enum LayoutMode {
    Resize,
    Swap,
    Move,
}

pub(super) enum TabChoice {
    Select,
    Transfer {
        pane: PaneId,
        tab: TabId,
        generation: u64,
    },
    Reorder {
        tab: TabId,
    },
}

/// A text field: what it renames or creates, and what submitting it sends.
pub(super) enum TextKind {
    RenameWorkspace {
        identity: Identity,
    },
    RenamePane {
        pane: PaneId,
        identity: Identity,
    },
    RenameTab {
        tab: TabId,
    },
    NewWorkspace {
        transfer: Option<Box<WorkspaceTransfer>>,
    },
}

/// A confirmed close: what it closes and the request `y` sends.
pub(super) enum CloseKind {
    Workspace(Identity),
    Pane(PaneId),
    Tab { tab: TabId, label: String },
}

pub(super) enum Mode {
    Text {
        kind: TextKind,
        text: String,
    },
    Confirm(CloseKind),
    SwapPicker(Box<super::context::SwapPicker>),
    Menu(Box<super::context::Menu>),
    Destination {
        source_workspace: String,
        transfer: Box<WorkspaceTransfer>,
        entries: Vec<crate::proto::control::WorkspaceRoute>,
        selected: usize,
        loading: bool,
    },
    Pane,
    Copy(Box<CopySession>),
    LoadingWorkspaces {
        reorder: Option<String>,
    },
    WaitingCommand,
    Workspaces {
        reorder: Option<String>,
        names: Vec<crate::proto::control::WorkspaceRoute>,
        selected: usize,
    },
    Tabs {
        choices: Vec<TabEntry>,
        selected: usize,
        purpose: TabChoice,
    },
    Layout {
        pane: PaneId,
        tab: TabId,
        kind: LayoutMode,
    },
}

pub(super) enum FrameTransition {
    Keep,
    Dismiss(&'static str),
}

/// What one consumed key decided. A mode either keeps owning input or finishes; layout edits
/// are sent while their owner stays, submitted fields and choosers finish first.
pub(super) enum Step {
    Keep,
    Finish,
    Send(Request),
    Submit(Request),
    Manager(ManagerRequest),
    Action(Action, Target),
    Copied(String),
    Notice(String),
}

pub(super) const MAX_TEXT_BYTES: usize = 128;
pub(super) const RESIZE_STEP: i16 = 250;

fn transfer_stale(transfer: &WorkspaceTransfer, frame: &Frame) -> bool {
    frame.server_instance != transfer.instance
        || frame.active_tab != Some(transfer.source)
        || frame.layout_generation != transfer.generation
        || frame.pane(transfer.pane).is_none()
}

impl TextKind {
    pub fn title(&self) -> &'static str {
        match self {
            Self::RenameWorkspace { .. } => "Rename workspace (empty = routing name)",
            Self::RenamePane { .. } => "Rename pane (empty = application title)",
            Self::RenameTab { .. } => "Rename tab",
            Self::NewWorkspace { transfer: Some(_) } => "Move pane to new workspace",
            Self::NewWorkspace { transfer: None } => "New workspace (empty = automatic name)",
        }
    }
    pub fn footer(&self) -> &'static str {
        match self {
            Self::NewWorkspace { .. } => "Enter create · Esc dismiss · Ctrl-U clear",
            _ => "Enter save · Esc dismiss · Ctrl-U clear · Backspace delete",
        }
    }
    fn stale(&self, frame: &Frame) -> bool {
        match self {
            Self::RenameWorkspace { identity } => !identity.matches(frame),
            Self::RenamePane { pane, identity } => {
                frame.server_instance != identity.instance
                    || frame.workspace != identity.workspace
                    || frame.pane(*pane).is_none_or(|pane| pane.exit.is_some())
            }
            Self::RenameTab { tab } => !frame.tabs.iter().any(|entry| entry.id == *tab),
            Self::NewWorkspace { transfer } => transfer
                .as_ref()
                .is_some_and(|transfer| transfer_stale(transfer, frame)),
        }
    }
    /// Enter submits the field. A pasted line break is text, never a submission.
    fn submit(&mut self, text: &str) -> Step {
        match self {
            Self::RenameWorkspace { identity } => Step::Submit(Request::Workspace {
                id: 0,
                instance: Some(identity.instance.clone()),
                stream: Some(identity.stream),
                action: WorkspaceAction::Rename {
                    label: text.to_owned(),
                },
            }),
            Self::RenamePane { pane, identity } => Step::Submit(Request::RenamePane {
                id: 0,
                instance: Some(identity.instance.clone()),
                pane: *pane,
                name: text.to_owned(),
            }),
            Self::RenameTab { tab } => Step::Submit(Request::Tab {
                instance: None,
                id: 0,
                action: TabAction::Rename {
                    tab: *tab,
                    name: text.to_owned(),
                },
            }),
            Self::NewWorkspace { transfer } => {
                let name = text.trim().to_owned();
                if transfer.is_some() && name.is_empty() {
                    return Step::Notice("Enter a name for the destination workspace".into());
                }
                if !name.is_empty() && crate::ids::validate_workspace_name(&name).is_err() {
                    return Step::Notice(crate::ids::InvalidName.to_string());
                }
                if let Some(mut transfer) = transfer.take() {
                    transfer.workspace = WorkspaceDestination::New { name };
                    return Step::Manager(ManagerRequest::Transfer {
                        transfer: *transfer,
                    });
                }
                Step::Submit(Request::Workspace {
                    stream: None,
                    instance: None,
                    id: 0,
                    action: WorkspaceAction::New {
                        name: (!name.is_empty()).then_some(name),
                    },
                })
            }
        }
    }
}

impl CloseKind {
    /// The dialog title and its explanatory row.
    pub fn describe(&self) -> (String, &'static str) {
        match self {
            Self::Workspace(identity) => (
                format!("Close workspace {}?", identity.workspace),
                "All its panes and processes will be terminated; its viewers will detach.",
            ),
            Self::Pane(pane) => (
                format!("Close pane {pane}?"),
                "Its process and unsaved work will be terminated.",
            ),
            Self::Tab { tab, label } => (
                format!("Close tab {label} ({tab})?"),
                "All its panes and their processes will be terminated.",
            ),
        }
    }
    fn stale(&self, frame: &Frame) -> bool {
        match self {
            Self::Workspace(identity) => !identity.matches(frame),
            Self::Pane(pane) => frame.pane(*pane).is_none(),
            Self::Tab { tab, .. } => !frame.tabs.iter().any(|entry| entry.id == *tab),
        }
    }
    fn request(&self) -> Request {
        match self {
            Self::Workspace(identity) => Request::Workspace {
                id: 0,
                instance: Some(identity.instance.clone()),
                stream: Some(identity.stream),
                action: WorkspaceAction::Kill {
                    name: identity.workspace.clone(),
                },
            },
            Self::Pane(pane) => Request::Kill {
                instance: None,
                id: 0,
                pane: *pane,
            },
            Self::Tab { tab, .. } => Request::Tab {
                instance: None,
                id: 0,
                action: TabAction::Close { tab: *tab },
            },
        }
    }
}

impl Mode {
    pub fn text_entry(&self) -> bool {
        matches!(self, Self::Text { .. })
    }
    pub fn loading(&self) -> bool {
        matches!(
            self,
            Self::LoadingWorkspaces { .. }
                | Self::WaitingCommand
                | Self::Destination { loading: true, .. }
        )
    }
    /// The chooser selection and its length, for the modes that pick from a list.
    pub fn selection_mut(&mut self) -> Option<(&mut usize, usize)> {
        Some(match self {
            Self::SwapPicker(picker) => (&mut picker.selected, picker.choices.len()),
            Self::Menu(menu) => (&mut menu.selected, menu.actions.len()),
            Self::Destination {
                entries,
                selected,
                loading: false,
                ..
            } => (selected, entries.len()),
            Self::Tabs {
                choices, selected, ..
            } => (selected, choices.len()),
            Self::Workspaces {
                names, selected, ..
            } => (selected, names.len()),
            _ => return None,
        })
    }
    pub fn reconcile(&mut self, frame: &Frame) -> FrameTransition {
        let stale = match self {
            Mode::Text { kind, .. } => kind.stale(frame),
            Mode::Confirm(kind) => kind.stale(frame),
            Mode::SwapPicker(picker) => !picker.valid(frame),
            Mode::Menu(menu) => !menu.valid(frame),
            Mode::Copy(copy) => match frame.pane(copy.pane()) {
                Some(live) => {
                    if !copy.same_buffer(live)
                        || !frame.layout.iter().any(|entry| entry.pane == copy.pane())
                    {
                        true
                    } else {
                        if let Some(entry) =
                            frame.layout.iter().find(|entry| entry.pane == copy.pane())
                        {
                            copy.set_viewport(entry.rect.height, entry.rect.width);
                        }
                        copy.refresh_live(live);
                        false
                    }
                }
                None => true,
            },
            Mode::Layout { pane, tab, .. } => {
                frame.active_tab != Some(*tab) || frame.pane(*pane).is_none()
            }
            Mode::Tabs { purpose, .. } => match purpose {
                TabChoice::Transfer {
                    pane,
                    tab,
                    generation,
                } => {
                    frame.active_tab != Some(*tab)
                        || frame.layout_generation != *generation
                        || frame.pane(*pane).is_none()
                }
                TabChoice::Reorder { tab } => !frame.tabs.iter().any(|entry| entry.id == *tab),
                TabChoice::Select => false,
            },
            Mode::LoadingWorkspaces { reorder } | Mode::Workspaces { reorder, .. } => reorder
                .as_ref()
                .is_some_and(|name| *name != frame.workspace),
            Mode::Destination { transfer, .. } => transfer_stale(transfer, frame),
            Mode::Pane | Mode::WaitingCommand => false,
        };
        if stale {
            FrameTransition::Dismiss("The target of that command changed or closed.")
        } else {
            FrameTransition::Keep
        }
    }

    /// A copy-mode key; the session's outcome decides whether the mode survives.
    pub fn copy(&mut self, key: CopyKey) -> Step {
        let Mode::Copy(copy) = self else {
            return Step::Keep;
        };
        match copy.key(key) {
            CopyOutcome::Continue => Step::Keep,
            CopyOutcome::Copied(text) => Step::Copied(text),
            CopyOutcome::Finished => Step::Finish,
        }
    }

    /// Every decoded key is locally consumed while an interaction owns input. The frame has
    /// already been reconciled against this mode; stale targets never reach here.
    pub fn key(&mut self, key: char, paste: bool, frame: &Frame) -> Step {
        match self {
            Mode::SwapPicker(picker) => {
                if paste || step(&mut picker.selected, picker.choices.len(), key) || !is_enter(key)
                {
                    return Step::Keep;
                }
                picker.request().map_or(Step::Finish, Step::Submit)
            }
            Mode::Menu(menu) => {
                if paste || step(&mut menu.selected, menu.actions.len(), key) || !is_enter(key) {
                    return Step::Keep;
                }
                if let Some(reason) = menu.disabled.get(&menu.selected).copied() {
                    return Step::Notice(reason.to_owned());
                }
                match menu.actions.get(menu.selected).copied() {
                    Some(action) => Step::Action(action, menu.target()),
                    None => Step::Keep,
                }
            }
            Mode::Copy(copy) => {
                let mapped = match key {
                    'h' => CopyKey::Left,
                    'l' => CopyKey::Right,
                    'k' => CopyKey::Up,
                    'j' => CopyKey::Down,
                    'u' | 'd' => {
                        copy.scroll(if key == 'u' { 3 } else { -3 });
                        return Step::Keep;
                    }
                    ' ' => CopyKey::Anchor,
                    'c' => CopyKey::Clear,
                    'y' | '\r' | '\n' => CopyKey::Copy,
                    'g' => CopyKey::Live,
                    'q' => CopyKey::Quit,
                    _ => return Step::Keep,
                };
                self.copy(mapped)
            }
            Mode::Destination {
                entries,
                selected,
                transfer,
                loading,
                ..
            } => {
                if *loading || paste || step(selected, entries.len(), key) || !is_enter(key) {
                    return Step::Keep;
                }
                let Some(entry) = entries.get(*selected) else {
                    return Step::Keep;
                };
                let mut request = (**transfer).clone();
                request.workspace = WorkspaceDestination::Existing {
                    name: entry.name.clone(),
                    stream: entry.stream,
                };
                Step::Manager(ManagerRequest::Transfer { transfer: request })
            }
            Mode::Pane | Mode::LoadingWorkspaces { .. } | Mode::WaitingCommand => Step::Keep,
            Mode::Workspaces {
                names,
                selected,
                reorder,
            } => {
                if paste || step(selected, names.len(), key) {
                    return Step::Keep;
                }
                let last = key == '$' && reorder.is_some();
                if !is_enter(key) && !last {
                    return Step::Keep;
                }
                let Some(name) = names.get(*selected).map(|entry| entry.name.clone()) else {
                    return Step::Keep;
                };
                if let Some(source) = reorder {
                    return Step::Manager(ManagerRequest::Reorder {
                        name: source.clone(),
                        before: (!last).then_some(name),
                    });
                }
                if name == frame.workspace {
                    return Step::Finish;
                }
                Step::Submit(Request::Workspace {
                    stream: None,
                    instance: None,
                    id: 0,
                    action: WorkspaceAction::Select { name },
                })
            }
            Mode::Tabs {
                choices,
                selected,
                purpose,
            } => {
                if paste || step(selected, choices.len(), key) {
                    return Step::Keep;
                }
                let last = key == '$' && matches!(purpose, TabChoice::Reorder { .. });
                if !is_enter(key) && !last {
                    return Step::Keep;
                }
                let Some(target) = choices.get(*selected) else {
                    return Step::Keep;
                };
                if !last && !frame.tabs.iter().any(|entry| entry.id == target.id) {
                    return Step::Notice("That tab no longer exists; Esc dismisses.".into());
                }
                let request = match purpose {
                    TabChoice::Select => Request::Tab {
                        instance: None,
                        id: 0,
                        action: TabAction::Select {
                            target: crate::proto::control::TabTarget::Id(target.id),
                        },
                    },
                    TabChoice::Reorder { tab } => Request::Tab {
                        instance: None,
                        id: 0,
                        action: TabAction::Reorder {
                            tab: *tab,
                            before: (!last).then_some(target.id),
                        },
                    },
                    TabChoice::Transfer {
                        pane,
                        tab,
                        generation,
                    } => {
                        let Some(first) = target.first_pane else {
                            return Step::Notice(
                                "That tab has no insertion target; Esc dismisses.".into(),
                            );
                        };
                        Request::Layout {
                            instance: None,
                            id: 0,
                            tab: *tab,
                            generation: Some(*generation),
                            action: LayoutAction::Transfer {
                                focus: false,
                                pane: *pane,
                                destination: PaneDestination::Tab {
                                    ratio: 5000,
                                    tab: target.id,
                                    generation: target.layout_generation,
                                    target: first,
                                },
                                side: crate::layout::Direction::Right,
                            },
                        }
                    }
                };
                Step::Submit(request)
            }
            Mode::Text { kind, text } => {
                if is_enter(key) && !paste {
                    return kind.submit(text);
                }
                edit_text(text, key);
                Step::Keep
            }
            Mode::Confirm(kind) => {
                if paste {
                    return Step::Keep;
                }
                match key {
                    'y' | 'Y' => Step::Submit(kind.request()),
                    'n' | 'N' => Step::Finish,
                    _ => Step::Keep,
                }
            }
            Mode::Layout { pane, tab, kind } => {
                if paste {
                    return Step::Keep;
                }
                if is_enter(key) {
                    return Step::Finish;
                }
                let direction = match key.to_ascii_lowercase() {
                    'h' => crate::layout::Direction::Left,
                    'j' => crate::layout::Direction::Down,
                    'k' => crate::layout::Direction::Up,
                    'l' => crate::layout::Direction::Right,
                    _ => return Step::Keep,
                };
                if frame.active_tab != Some(*tab) || frame.pane(*pane).is_none() {
                    return Step::Keep;
                }
                let action = match kind {
                    LayoutMode::Resize => LayoutAction::ResizeToward {
                        pane: *pane,
                        direction,
                        delta: if key.is_ascii_uppercase() {
                            -RESIZE_STEP
                        } else {
                            RESIZE_STEP
                        },
                    },
                    LayoutMode::Swap => LayoutAction::SwapDirection {
                        pane: *pane,
                        direction,
                    },
                    LayoutMode::Move => LayoutAction::MoveDirection {
                        pane: *pane,
                        direction,
                    },
                };
                Step::Send(Request::Layout {
                    instance: None,
                    id: 0,
                    tab: *tab,
                    generation: Some(frame.layout_generation),
                    action,
                })
            }
        }
    }
}

fn is_enter(key: char) -> bool {
    matches!(key, '\r' | '\n')
}

/// Moves a chooser's selection with j/k; true when `key` was consumed as movement.
pub(super) fn step(selected: &mut usize, len: usize, key: char) -> bool {
    match key {
        'j' => *selected = selected.saturating_add(1).min(len.saturating_sub(1)),
        'k' => *selected = selected.saturating_sub(1),
        _ => return false,
    }
    true
}

fn edit_text(text: &mut String, key: char) {
    match key {
        '\u{7f}' | '\u{8}' => {
            if let Some((index, _)) = text.grapheme_indices(true).next_back() {
                text.truncate(index);
            }
        }
        '\u{15}' => text.clear(),
        character
            if !character.is_control() && text.len() + character.len_utf8() <= MAX_TEXT_BYTES =>
        {
            text.push(character);
        }
        _ => {}
    }
}
