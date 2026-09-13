//! Keyboard interaction state and its frame-driven ownership decisions.
//! This layer has no sockets, global focus mutations or UI notification side effects.
use super::copy::{CopyKey, CopySession};
use crate::proto::control::{LayoutAction, PaneDestination, Request, TabAction, WorkspaceAction};
use crate::{
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

pub(super) enum Mode {
    RenameWorkspace {
        instance: String,
        workspace: String,
        stream: u64,
        viewer: crate::ids::ViewerId,
        text: String,
    },
    CloseWorkspace {
        instance: String,
        workspace: String,
        stream: u64,
        viewer: crate::ids::ViewerId,
    },
    SwapPicker(Box<super::context::SwapPicker>),
    Menu(Box<super::context::Menu>),
    Destination {
        source_workspace: String,
        transfer: Box<crate::proto::control::WorkspaceTransfer>,
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
    RenamePane {
        pane: PaneId,
        instance: String,
        workspace: String,
        text: String,
    },
    Rename {
        tab: TabId,
        text: String,
    },
    NewWorkspace {
        text: String,
        transfer: Option<Box<crate::proto::control::WorkspaceTransfer>>,
    },
    ClosePane {
        pane: PaneId,
    },
    CloseTab {
        tab: TabId,
        label: String,
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

impl Mode {
    pub fn text_entry(&self) -> bool {
        matches!(
            self,
            Self::RenameWorkspace { .. }
                | Self::RenamePane { .. }
                | Self::Rename { .. }
                | Self::NewWorkspace { .. }
        )
    }
    pub fn loading(&self) -> bool {
        matches!(
            self,
            Self::LoadingWorkspaces { .. }
                | Self::WaitingCommand
                | Self::Destination { loading: true, .. }
        )
    }
    pub fn reconcile(&mut self, frame: &Frame) -> FrameTransition {
        let stale = match self {
            Mode::RenameWorkspace {
                instance,
                workspace,
                stream,
                viewer,
                ..
            }
            | Mode::CloseWorkspace {
                instance,
                workspace,
                stream,
                viewer,
            } => {
                frame.server_instance != *instance
                    || frame.workspace != *workspace
                    || frame.workspace_stream != *stream
                    || frame.viewer != *viewer
            }
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
            Mode::RenamePane {
                pane,
                instance,
                workspace,
                ..
            } => {
                frame.server_instance != *instance
                    || frame.workspace != *workspace
                    || frame.pane(*pane).is_none_or(|pane| pane.exit.is_some())
            }
            Mode::ClosePane { pane } => frame.pane(*pane).is_none(),
            Mode::Layout { pane, tab, .. } => {
                frame.active_tab != Some(*tab) || frame.pane(*pane).is_none()
            }
            Mode::CloseTab { tab, .. } | Mode::Rename { tab, .. } => {
                !frame.tabs.iter().any(|entry| entry.id == *tab)
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
            Mode::Destination { transfer, .. } => {
                frame.server_instance != transfer.instance
                    || frame.active_tab != Some(transfer.source)
                    || frame.layout_generation != transfer.generation
                    || frame.pane(transfer.pane).is_none()
            }
            Mode::NewWorkspace { transfer, .. } => transfer.as_ref().is_some_and(|transfer| {
                frame.server_instance != transfer.instance
                    || frame.active_tab != Some(transfer.source)
                    || frame.layout_generation != transfer.generation
                    || frame.pane(transfer.pane).is_none()
            }),
            Mode::Pane | Mode::WaitingCommand => false,
        };
        if stale {
            FrameTransition::Dismiss("The target of that command changed or closed.")
        } else {
            FrameTransition::Keep
        }
    }
}

/// Every decoded key is locally consumed while an interaction owns input.
/// Completion and the single requested external effect are independent: layout
/// edits retain their owner, whereas submitted fields/choosers finish first.
#[derive(Default)]
pub(super) struct KeyTransition {
    pub completion: Completion,
    pub effect: Option<KeyEffect>,
    pub notice: Option<String>,
}
#[derive(Default)]
pub(super) enum Completion {
    #[default]
    Keep,
    Finish,
}
pub(super) enum KeyEffect {
    Control(Request),
    Manager(crate::daemon::ManagerRequest),
    Action(crate::commands::Action, Frame),
    Copy(CopyKey),
    ScrollCopy(i64),
}

pub(super) const MAX_TEXT_BYTES: usize = 128;
pub const RESIZE_STEP: i16 = 250;

impl Mode {
    pub fn key(&mut self, key: char, paste: bool, frame: &Frame) -> KeyTransition {
        let mut transition = KeyTransition::default();
        let request = (|| -> Option<Request> {
            match self {
                Mode::SwapPicker(picker) => {
                    if !picker.valid(frame) {
                        transition.completion = Completion::Finish;
                        transition.notice = Some("Swap target changed".to_owned());
                        return None;
                    }
                    if paste
                        || step(&mut picker.selected, picker.choices.len(), key)
                        || !is_enter(key)
                    {
                        return None;
                    }
                    let request = picker.request();
                    transition.completion = Completion::Finish;
                    request
                }
                Mode::Menu(menu) => {
                    if !menu.valid(frame) {
                        transition.completion = Completion::Finish;
                        transition.notice = Some("Menu target changed".to_owned());
                        return None;
                    }
                    if paste || step(&mut menu.selected, menu.actions.len(), key) || !is_enter(key)
                    {
                        return None;
                    }
                    if let Some(reason) = menu.disabled.get(&menu.selected).copied() {
                        transition.notice = Some(reason.to_owned());
                        return None;
                    }
                    if let Some(action) = menu.actions.get(menu.selected).copied() {
                        let target = menu.project(frame);
                        transition.effect = Some(KeyEffect::Action(action, target));
                        transition.completion = Completion::Finish;
                    }
                    None
                }
                Mode::Copy(_) => {
                    let mapped = match key {
                        'h' => CopyKey::Left,
                        'l' => CopyKey::Right,
                        'k' => CopyKey::Up,
                        'j' => CopyKey::Down,
                        'u' => {
                            transition.effect = Some(KeyEffect::ScrollCopy(3));
                            return None;
                        }
                        'd' => {
                            transition.effect = Some(KeyEffect::ScrollCopy(-3));
                            return None;
                        }
                        ' ' => CopyKey::Anchor,
                        'c' => CopyKey::Clear,
                        'y' | '\r' | '\n' => CopyKey::Copy,
                        'g' => CopyKey::Live,
                        'q' => CopyKey::Quit,
                        _ => return None,
                    };
                    {
                        transition.effect = Some(KeyEffect::Copy(mapped));
                        None
                    }
                }
                Mode::Destination {
                    entries,
                    selected,
                    transfer,
                    loading,
                    ..
                } => {
                    if *loading || paste || step(selected, entries.len(), key) || !is_enter(key) {
                        return None;
                    }
                    let entry = entries.get(*selected)?;
                    let mut request = (**transfer).clone();
                    request.workspace = crate::proto::control::WorkspaceDestination::Existing {
                        name: entry.name.clone(),
                        stream: entry.stream,
                    };
                    transition.effect = Some(KeyEffect::Manager(
                        crate::daemon::ManagerRequest::Transfer { transfer: request },
                    ));
                    transition.completion = Completion::Finish;
                    None
                }
                Mode::Pane | Mode::LoadingWorkspaces { .. } | Mode::WaitingCommand => None,
                Mode::Workspaces {
                    names,
                    selected,
                    reorder,
                } => {
                    if paste || step(selected, names.len(), key) {
                        return None;
                    }
                    let last = key == '$' && reorder.is_some();
                    if !is_enter(key) && !last {
                        return None;
                    }
                    let name = names.get(*selected)?.name.clone();
                    if let Some(source) = reorder {
                        transition.effect =
                            Some(KeyEffect::Manager(crate::daemon::ManagerRequest::Reorder {
                                name: source.clone(),
                                before: (!last).then_some(name),
                            }));
                        transition.completion = Completion::Finish;
                        return None;
                    }
                    transition.completion = Completion::Finish;
                    if name == frame.workspace {
                        return None;
                    }
                    Some(Request::Workspace {
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
                        return None;
                    }
                    let last = key == '$' && matches!(purpose, TabChoice::Reorder { .. });
                    if !is_enter(key) && !last {
                        return None;
                    }
                    let target = choices.get(*selected)?;
                    if !last && !frame.tabs.iter().any(|entry| entry.id == target.id) {
                        transition.notice =
                            Some("That tab no longer exists; Esc dismisses.".into());
                        return None;
                    }
                    if matches!(purpose, TabChoice::Transfer { .. }) && target.first_pane.is_none()
                    {
                        transition.notice =
                            Some("That tab has no insertion target; Esc dismisses.".into());
                        return None;
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
                        } => Request::Layout {
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
                                    target: target.first_pane?,
                                },
                                side: crate::layout::Direction::Right,
                            },
                        },
                    };
                    transition.completion = Completion::Finish;
                    Some(request)
                }
                Mode::RenamePane {
                    pane,
                    instance,
                    text,
                    ..
                } => match key {
                    '\r' | '\n' if !paste => {
                        let request = Request::RenamePane {
                            id: 0,
                            instance: Some(instance.clone()),
                            pane: *pane,
                            name: text.clone(),
                        };
                        transition.completion = Completion::Finish;
                        Some(request)
                    }
                    _ => {
                        edit_text(text, key);
                        None
                    }
                },
                Mode::RenameWorkspace {
                    instance,
                    workspace,
                    stream,
                    viewer,
                    text,
                } => {
                    if frame.server_instance != *instance
                        || frame.workspace != *workspace
                        || frame.workspace_stream != *stream
                        || frame.viewer != *viewer
                    {
                        transition.completion = Completion::Finish;
                        transition.notice = Some("Workspace changed; rename cancelled.".to_owned());
                        return None;
                    }
                    match key {
                        '\r' | '\n' if !paste => {
                            let request = Request::Workspace {
                                id: 0,
                                instance: Some(instance.clone()),
                                stream: Some(*stream),
                                action: WorkspaceAction::Rename {
                                    label: text.clone(),
                                },
                            };
                            transition.completion = Completion::Finish;
                            Some(request)
                        }
                        _ => {
                            edit_text(text, key);
                            None
                        }
                    }
                }
                Mode::Rename { tab, text } => match key {
                    '\r' | '\n' => {
                        let action = TabAction::Rename {
                            tab: *tab,
                            name: text.clone(),
                        };
                        transition.completion = Completion::Finish;
                        Some(Request::Tab {
                            instance: None,
                            id: 0,
                            action,
                        })
                    }
                    _ => {
                        edit_text(text, key);
                        None
                    }
                },
                Mode::NewWorkspace { text, transfer } => match key {
                    '\r' | '\n' => {
                        if paste {
                            return None;
                        }
                        let name = text.trim().to_owned();
                        if transfer.is_some() && name.is_empty() {
                            transition.notice =
                                Some("Enter a name for the destination workspace".into());
                            return None;
                        }
                        if !name.is_empty() && crate::ids::validate_workspace_name(&name).is_err() {
                            transition.notice = Some(crate::ids::InvalidName.to_string());
                            return None;
                        }
                        if let Some(mut transfer) = transfer.take() {
                            transfer.workspace =
                                crate::proto::control::WorkspaceDestination::New { name };
                            transition.effect = Some(KeyEffect::Manager(
                                crate::daemon::ManagerRequest::Transfer {
                                    transfer: *transfer,
                                },
                            ));
                            transition.completion = Completion::Finish;
                            return None;
                        }
                        transition.completion = Completion::Finish;
                        Some(Request::Workspace {
                            stream: None,
                            instance: None,
                            id: 0,
                            action: WorkspaceAction::New {
                                name: (!name.is_empty()).then_some(name),
                            },
                        })
                    }
                    _ => {
                        edit_text(text, key);
                        None
                    }
                },
                Mode::CloseWorkspace { .. } | Mode::ClosePane { .. } | Mode::CloseTab { .. } => {
                    if paste {
                        return None;
                    }
                    match key {
                        'y' | 'Y' => {
                            let request = match &*self {
                                Mode::CloseWorkspace {
                                    instance,
                                    workspace,
                                    stream,
                                    viewer,
                                } => {
                                    if frame.server_instance != *instance
                                        || frame.workspace != *workspace
                                        || frame.workspace_stream != *stream
                                        || frame.viewer != *viewer
                                    {
                                        transition.completion = Completion::Finish;
                                        return None;
                                    }
                                    Request::Workspace {
                                        id: 0,
                                        instance: Some(instance.clone()),
                                        stream: Some(*stream),
                                        action: WorkspaceAction::Kill {
                                            name: workspace.clone(),
                                        },
                                    }
                                }
                                Mode::ClosePane { pane } => Request::Kill {
                                    instance: None,
                                    id: 0,
                                    pane: *pane,
                                },
                                Mode::CloseTab { tab, .. } => Request::Tab {
                                    instance: None,
                                    id: 0,
                                    action: TabAction::Close { tab: *tab },
                                },
                                _ => return None,
                            };
                            transition.completion = Completion::Finish;
                            Some(request)
                        }
                        'n' | 'N' => {
                            transition.completion = Completion::Finish;
                            None
                        }
                        _ => None,
                    }
                }
                Mode::Layout { pane, tab, kind } => {
                    if paste {
                        return None;
                    }
                    if is_enter(key) {
                        transition.completion = Completion::Finish;
                        return None;
                    }
                    let direction = match key.to_ascii_lowercase() {
                        'h' => crate::layout::Direction::Left,
                        'j' => crate::layout::Direction::Down,
                        'k' => crate::layout::Direction::Up,
                        'l' => crate::layout::Direction::Right,
                        _ => return None,
                    };
                    if frame.active_tab != Some(*tab) || frame.pane(*pane).is_none() {
                        return None;
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
                    Some(Request::Layout {
                        instance: None,
                        id: 0,
                        tab: *tab,
                        generation: Some(frame.layout_generation),
                        action,
                    })
                }
            }
        })();
        if let Some(request) = request {
            transition.effect = Some(KeyEffect::Control(request));
        }
        transition
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
