//! Prompts and confirmations (prompt 3.11). A [`Prompt`] is a one-line text field in the
//! `bevy_ui_widgets::text_input` shape over a plain `String`: keys and pastes are parsed into
//! queued [`Edit`]s and applied by one system per update, which also refreshes the painted
//! text and cursor. Enter commits the value as a BRP call on a worker thread (`fux/root.rename`
//! for `prefix ,`, `fux/workspace.new` for `prefix S`); Escape or focus loss cancels.
//!
//! A [`Confirmation`] is a popover beside the mode indicator (`Popover` placement scoring) that
//! commits its action on `y` and closes on `n`/Escape: `prefix x` closes the target pane,
//! `prefix K` kills the workspace.

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemId;
use bevy_input::keyboard::Key;
use bevy_input_focus::InputFocus;
use bevy_state::prelude::*;
use bevy_ui::prelude::*;

use super::chrome::{
    Align, Chrome, ChromeRoots, PendingNotice, Placement, Popover, Popup, Side, Text, close_popup,
    spawn_popup,
};
use super::focus::Bindings;
use super::keys::KeyChord;
use super::replicate::{Roots, Session, ShowingRoot, TargetPane};
use super::{Brp, BrpReply, BrpTag, Mode, Outbox, Reconnect, Viewport};
use crate::assets::ThemeToken;
use crate::model::{NodeId, ViewerRequest};

/// What a prompt's value becomes when committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    RenameRoot(NodeId),
    NewWorkspace,
}

/// A queued text edit (`text_input`'s `TextEdit`, reduced to one line without selection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Insert(String),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    WordLeft,
    WordRight,
    BackspaceWord,
    DeleteWord,
}

/// The text prompt popup: its value, the cursor as a byte offset, and the edits queued since
/// the last apply. `field` is the child node that paints `label + text`.
#[derive(Component, Debug)]
pub struct Prompt {
    pub kind: PromptKind,
    label: &'static str,
    field: Entity,
    text: String,
    cursor: usize,
    edits: Vec<Edit>,
}

impl Prompt {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The cursor as a character index.
    pub fn cursor(&self) -> usize {
        self.halves().0.chars().count()
    }

    fn queue(&mut self, edit: Edit) {
        self.edits.push(edit);
    }

    /// The text before and after the cursor.
    fn halves(&self) -> (&str, &str) {
        self.text
            .split_at_checked(self.cursor)
            .unwrap_or((self.text.as_str(), ""))
    }

    fn prev_char(&self) -> usize {
        self.halves()
            .0
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next_char(&self) -> usize {
        self.halves()
            .1
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }

    /// Start of the word before the cursor: skip spaces, then non-spaces.
    fn word_start(&self) -> usize {
        let trimmed = self.halves().0.trim_end_matches(' ');
        trimmed.rfind(' ').map_or(0, |i| i + 1)
    }

    /// End of the word after the cursor: skip spaces, then non-spaces.
    fn word_end(&self) -> usize {
        let tail = self.halves().1;
        let rest = tail.trim_start_matches(' ');
        let spaces = tail.len() - rest.len();
        self.cursor + spaces + rest.find(' ').unwrap_or(rest.len())
    }

    /// Applies every queued edit in order.
    fn apply(&mut self) {
        for edit in core::mem::take(&mut self.edits) {
            match edit {
                Edit::Insert(s) => {
                    self.text.insert_str(self.cursor, &s);
                    self.cursor += s.len();
                }
                Edit::Backspace => {
                    let start = self.prev_char();
                    self.text.replace_range(start..self.cursor, "");
                    self.cursor = start;
                }
                Edit::Delete => {
                    let end = self.next_char();
                    self.text.replace_range(self.cursor..end, "");
                }
                Edit::Left => self.cursor = self.prev_char(),
                Edit::Right => self.cursor = self.next_char(),
                Edit::Home => self.cursor = 0,
                Edit::End => self.cursor = self.text.len(),
                Edit::WordLeft => self.cursor = self.word_start(),
                Edit::WordRight => self.cursor = self.word_end(),
                Edit::BackspaceWord => {
                    let start = self.word_start();
                    self.text.replace_range(start..self.cursor, "");
                    self.cursor = start;
                }
                Edit::DeleteWord => {
                    let end = self.word_end();
                    self.text.replace_range(self.cursor..end, "");
                }
            }
        }
    }
}

/// On a chrome text node: the painter puts the terminal cursor at this character column while
/// a prompt is open.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorAt(pub u16);

/// A pending destructive action shown as a `y/n` popover.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub enum Confirmation {
    ClosePane,
    KillWorkspace(String),
}

const PROMPT_PLACEMENTS: &[Placement] = &[Placement::new(Side::Top, Align::Start)];
const CONFIRM_PLACEMENTS: &[Placement] = &[
    Placement::new(Side::Top, Align::End),
    Placement::new(Side::Top, Align::Start),
    Placement::new(Side::Bottom, Align::End),
];

fn open_prompt(
    kind: PromptKind,
    label: &'static str,
    initial: String,
    commands: &mut Commands,
    chrome: &ChromeRoots,
    viewport: &Viewport,
    focus: &mut InputFocus,
    next: &mut NextState<Mode>,
) {
    let field = commands
        .spawn((
            Chrome,
            ThemeToken::BAR,
            CursorAt(0),
            Node {
                height: Val::Px(1.0),
                flex_shrink: 1.0,
                overflow: Overflow::clip(),
                ..Default::default()
            },
            Text::default(),
        ))
        .id();
    let cursor = initial.len();
    let popup = spawn_popup(
        commands,
        chrome,
        focus,
        (
            Prompt {
                kind,
                label,
                field,
                text: initial,
                cursor,
                // A no-op edit makes the first apply paint the initial value.
                edits: vec![Edit::End],
            },
            ThemeToken::BAR_BACKGROUND,
            Popover {
                anchor: chrome.status_bar,
                placements: PROMPT_PLACEMENTS,
            },
            Node {
                position_type: PositionType::Absolute,
                width: Val::Px(f32::from(viewport.cols)),
                height: Val::Px(1.0),
                ..Default::default()
            },
        ),
    );
    commands.entity(popup).add_child(field);
    next.set(Mode::Prompt);
}

/// `prefix ,`: rename the shown root, starting from its current name.
pub fn open_rename_root(
    roots: Res<Roots>,
    showing: Res<ShowingRoot>,
    chrome: Res<ChromeRoots>,
    viewport: Res<Viewport>,
    mut focus: ResMut<InputFocus>,
    mut next: ResMut<NextState<Mode>>,
    mut commands: Commands,
) {
    let Some(root) = showing
        .0
        .and_then(|node| roots.0.iter().find(|r| r.node == node))
    else {
        return;
    };
    open_prompt(
        PromptKind::RenameRoot(root.node),
        "rename: ",
        root.name.clone(),
        &mut commands,
        &chrome,
        &viewport,
        &mut focus,
        &mut next,
    );
}

/// `prefix S`: create a workspace and attach to it.
pub fn open_new_workspace(
    chrome: Res<ChromeRoots>,
    viewport: Res<Viewport>,
    mut focus: ResMut<InputFocus>,
    mut next: ResMut<NextState<Mode>>,
    mut commands: Commands,
) {
    open_prompt(
        PromptKind::NewWorkspace,
        "new workspace: ",
        String::new(),
        &mut commands,
        &chrome,
        &viewport,
        &mut focus,
        &mut next,
    );
}

/// Applies queued edits and refreshes the painted field.
fn apply_edits(mut prompts: Query<&mut Prompt>, mut fields: Query<(&mut Text, &mut CursorAt)>) {
    for mut prompt in &mut prompts {
        if prompt.edits.is_empty() {
            continue;
        }
        prompt.apply();
        let Ok((mut text, mut cursor)) = fields.get_mut(prompt.field) else {
            continue;
        };
        text.0.clear();
        text.0.push_str(prompt.label);
        text.0.push_str(&prompt.text);
        // One blank past the end so the node is wide enough for the cursor there.
        text.0.push(' ');
        let column = (prompt.label.chars().count() + prompt.cursor()).min(usize::from(u16::MAX));
        cursor.set_if_neq(CursorAt(column as u16));
    }
}

/// Commits a prompt's value: the BRP call the value stands for, on a worker thread.
fn commit(prompt: &Prompt, brp: &Brp, notice: &mut PendingNotice) {
    let value = prompt.text.trim();
    if value.is_empty() {
        notice.0 = Some("empty name".into());
        return;
    }
    match &prompt.kind {
        PromptKind::RenameRoot(root) => brp.call(
            BrpTag::RootRename(*root),
            "fux/root.rename",
            serde_json::json!({ "root": root.0, "name": value }),
        ),
        PromptKind::NewWorkspace => brp.call(
            BrpTag::WorkspaceNew(value.to_owned()),
            "fux/workspace.new",
            serde_json::json!({ "name": value }),
        ),
    }
}

/// Keys in `Mode::Prompt`: `text_input`'s keymap reduced to one line.
#[allow(clippy::too_many_arguments)]
pub fn handle_key(
    In(chord): In<KeyChord>,
    bindings: Res<Bindings>,
    brp: Res<Brp>,
    mut prompts: Query<(Entity, &Popup, &mut Prompt)>,
    mut focus: ResMut<InputFocus>,
    mut next: ResMut<NextState<Mode>>,
    mut notice: ResMut<PendingNotice>,
    mut commands: Commands,
) {
    let Ok((entity, shell, mut prompt)) = prompts.single_mut() else {
        next.set(Mode::Normal);
        return;
    };
    let popup = (entity, shell);
    if chord == *bindings.prefix() {
        close_popup(&mut commands, &mut focus, &mut next, popup, Mode::Prefix);
        return;
    }
    let edit = match (&chord.key, chord.ctrl, chord.alt) {
        (Key::Escape, ..) => {
            close_popup(&mut commands, &mut focus, &mut next, popup, Mode::Normal);
            return;
        }
        (Key::Enter, ..) => {
            prompt.apply();
            commit(&prompt, &brp, &mut notice);
            close_popup(&mut commands, &mut focus, &mut next, popup, Mode::Normal);
            return;
        }
        (Key::Backspace, ..) => Edit::Backspace,
        (Key::Delete, ..) => Edit::Delete,
        (Key::ArrowLeft, ..) => Edit::Left,
        (Key::ArrowRight, ..) => Edit::Right,
        (Key::Home, ..) => Edit::Home,
        (Key::End, ..) => Edit::End,
        (Key::Character(c), true, false) => match c.as_str() {
            "a" => Edit::Home,
            "e" => Edit::End,
            "w" => Edit::BackspaceWord,
            "h" => Edit::Backspace,
            "d" => Edit::Delete,
            _ => return,
        },
        (Key::Character(c), false, true) => match c.as_str() {
            "b" => Edit::WordLeft,
            "f" => Edit::WordRight,
            "d" => Edit::DeleteWord,
            _ => return,
        },
        (Key::Character(c), false, false) => Edit::Insert(c.as_str().to_owned()),
        _ => return,
    };
    prompt.queue(edit);
}

/// Bracketed paste into the prompt (`Ime::Commit`, run by `focus::on_paste`): one line of it,
/// as one edit.
pub fn handle_paste(In(value): In<String>, mut prompts: Query<&mut Prompt>) {
    let Ok(mut prompt) = prompts.single_mut() else {
        return;
    };
    let line: String = value.chars().filter(|c| !c.is_control()).collect();
    if !line.is_empty() {
        prompt.queue(Edit::Insert(line));
    }
}

fn open_confirmation(
    action: Confirmation,
    text: String,
    commands: &mut Commands,
    chrome: &ChromeRoots,
    focus: &mut InputFocus,
    next: &mut NextState<Mode>,
) {
    let width = text.chars().count() as f32;
    spawn_popup(
        commands,
        chrome,
        focus,
        (
            action,
            ThemeToken::NOTICE,
            Popover {
                anchor: chrome.mode_indicator,
                placements: CONFIRM_PLACEMENTS,
            },
            Node {
                position_type: PositionType::Absolute,
                width: Val::Px(width),
                height: Val::Px(1.0),
                flex_shrink: 0.0,
                ..Default::default()
            },
            Text(text),
        ),
    );
    next.set(Mode::Confirm);
}

/// `prefix x`.
pub fn confirm_close_pane(
    target: Res<TargetPane>,
    chrome: Res<ChromeRoots>,
    mut focus: ResMut<InputFocus>,
    mut next: ResMut<NextState<Mode>>,
    mut commands: Commands,
) {
    let Some(pane) = target.0 else {
        return;
    };
    open_confirmation(
        Confirmation::ClosePane,
        format!(" close pane {pane}? y/n "),
        &mut commands,
        &chrome,
        &mut focus,
        &mut next,
    );
}

/// `prefix K`.
pub fn confirm_kill_workspace(
    session: Res<Session>,
    chrome: Res<ChromeRoots>,
    mut focus: ResMut<InputFocus>,
    mut next: ResMut<NextState<Mode>>,
    mut commands: Commands,
) {
    let Some(name) = session.0.as_ref().map(|w| w.workspace.clone()) else {
        return;
    };
    let text = format!(" kill workspace {name}? y/n ");
    open_confirmation(
        Confirmation::KillWorkspace(name),
        text,
        &mut commands,
        &chrome,
        &mut focus,
        &mut next,
    );
}

/// Keys in `Mode::Confirm`.
#[allow(clippy::too_many_arguments)]
pub fn handle_confirm_key(
    In(chord): In<KeyChord>,
    bindings: Res<Bindings>,
    brp: Res<Brp>,
    confirmations: Query<(Entity, &Popup, &Confirmation)>,
    mut focus: ResMut<InputFocus>,
    mut next: ResMut<NextState<Mode>>,
    mut outbox: ResMut<Outbox>,
    mut commands: Commands,
) {
    let Ok((entity, shell, action)) = confirmations.single() else {
        next.set(Mode::Normal);
        return;
    };
    let popup = (entity, shell);
    if chord == *bindings.prefix() {
        close_popup(&mut commands, &mut focus, &mut next, popup, Mode::Prefix);
        return;
    }
    let answer = match &chord.key {
        Key::Escape => Some(false),
        Key::Character(c) => match c.as_str() {
            "y" | "Y" => Some(true),
            "n" | "N" | "q" => Some(false),
            _ => None,
        },
        _ => None,
    };
    let Some(yes) = answer else {
        return;
    };
    if yes {
        match action {
            Confirmation::ClosePane => outbox.push(ViewerRequest::ClosePane),
            Confirmation::KillWorkspace(name) => brp.call(
                BrpTag::WorkspaceKill(name.clone()),
                "fux/workspace.kill",
                serde_json::json!({ "name": name }),
            ),
        }
    }
    close_popup(&mut commands, &mut focus, &mut next, popup, Mode::Normal);
}

/// Replies to committed prompts and confirmations: failures become notices, a created
/// workspace is attached to.
fn on_reply(
    mut replies: MessageReader<BrpReply>,
    mut notice: ResMut<PendingNotice>,
    mut commands: Commands,
) {
    for reply in replies.read() {
        match (&reply.tag, &reply.result) {
            (BrpTag::RootRename(_), Err(e)) => notice.0 = Some(format!("rename failed: {e}")),
            (BrpTag::WorkspaceNew(name), Ok(_)) => {
                commands.insert_resource(Reconnect(name.clone()));
            }
            (BrpTag::WorkspaceNew(name), Err(e)) => {
                notice.0 = Some(format!("workspace {name}: {e}"));
            }
            (BrpTag::WorkspaceKill(name), Err(e)) => {
                notice.0 = Some(format!("kill {name}: {e}"));
            }
            _ => {}
        }
    }
}

/// The prompt and confirmation actions by name, for the bindings registry.
pub const ACTIONS: &[(&str, fn(&mut World) -> SystemId)] = &[
    ("rename-root", |w| w.register_system(open_rename_root)),
    ("new-workspace", |w| w.register_system(open_new_workspace)),
    ("close-pane", |w| w.register_system(confirm_close_pane)),
    ("kill-workspace", |w| {
        w.register_system(confirm_kill_workspace)
    }),
];

pub struct PromptsPlugin;

impl Plugin for PromptsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (on_reply, apply_edits)
                .in_set(super::ViewerSystems::Chrome)
                .before(super::chrome::size_text_nodes),
        );
    }
}
