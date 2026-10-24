//! A client's own screens: choosers, the command prompt, rename
//! prompts, confirmations and copy mode. They belong to the client, which
//! reads the keys: every change of mode happens here, at the key that
//! causes it, so no key is ever read in another mode's table. The server
//! draws what the client describes (`Act::Show`), and keeps copy mode's
//! cursor in the scrollback, which it alone holds (`Act::Copy`).
//!
//! What a screen's entry does is fuxth, run as the client runs a binding.
use crate::act::{Act, CopyAct, LineAct, List, ListAct, Panel, Seek};
use crate::command::{AnyRef, Choices, ClientAction, Command};
use crate::id::PaneId;
use crate::keymap::{Held, Keymap, confirm_act, copy_act, letter, line_act, list_act};
use crate::keys::{KeyPress, Keystroke};
use crate::protocol::{Reply, Scalar, ScreenKind};
use crate::script::{Failed, succeeded};
use crate::view::Line;

/// A screen a word asks to open: built with what the server knows (names,
/// a chooser's rows) by `Open::build`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Open {
    /// The tabs of the client's workspace: to show one, or to move a pane to.
    ChooseTab(Option<PaneId>),
    ChooseWorkspace(Option<PaneId>),
    /// The other panes of a pane's tab, to swap it with.
    ChoosePane(PaneId),
    Rename(AnyRef),
    Confirm(AnyRef),
    Prompt,
    Copy,
}

/// One of a client's screens, or none: its keys then go through its
/// bindings, in the middle of what `Held` says. So nothing is held for the
/// bindings while a screen is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    Keys(Held),
    List(ListScreen),
    Prompt(Prompt),
    Confirm {
        question: String,
        about: AnyRef,
    },
    /// Copy mode, and the search line being typed in it, forward or back.
    Copy(Option<(Seek, Line<1024>)>),
}

/// A chooser: its rows, each running fuxth when chosen and about a tab,
/// workspace or pane, which `r` renames and `x` closes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListScreen {
    pub title: String,
    pub items: Vec<Item>,
    pub current: Option<usize>,
    pub selected: usize,
    /// What it was opened for: the server closes it if that goes.
    pub about: Option<AnyRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    /// The fuxth it runs.
    pub code: String,
    /// What it is about.
    pub about: Option<AnyRef>,
}

/// A line being typed, and what Enter does with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub title: String,
    pub line: Line,
    /// None: the line is fuxth to run. Some: it is the new name of this.
    pub rename: Option<AnyRef>,
}

/// What a key leads to, in a screen or through the bindings: an action for
/// the server, or fuxth to run as the client.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Send(Act),
    Run(String),
}

/// The effect that hides the screen showing.
pub const HIDE: Effect = Effect::Send(Act::Hide);

/// The words that push a handle to `target`: `str %3 pane`.
pub fn naming(target: AnyRef) -> String {
    format!("str {} {}", target, target.kind().name())
}

/// What `target` is, as a title says it: `pane %3`.
fn about(target: AnyRef) -> String {
    format!("{} {}", target.kind().name(), target)
}

impl Open {
    /// The fuxth that opens it on client `me`: what is relayed to a client
    /// asked from elsewhere (`str c1 client choose-tab`).
    pub fn code(&self) -> String {
        match self {
            Open::ChooseTab(None) => "me choose-tab".to_owned(),
            Open::ChooseTab(Some(p)) => format!("me str {p} pane choose-tab-for"),
            Open::ChooseWorkspace(None) => "me choose-workspace".to_owned(),
            Open::ChooseWorkspace(Some(p)) => format!("me str {p} pane choose-workspace-for"),
            Open::ChoosePane(p) => format!("me str {p} pane choose-pane"),
            Open::Rename(t) => format!("me {} rename-prompt", naming(*t)),
            Open::Confirm(t) => format!("me {} confirm-close", naming(*t)),
            Open::Prompt => "me command-prompt".to_owned(),
            Open::Copy => "me copy-mode".to_owned(),
        }
    }

    /// The screen, built with what `ask` answers: names, a chooser's rows.
    pub fn build(
        &self,
        ask: &mut dyn FnMut(Command) -> Result<Reply, Failed>,
    ) -> Result<Screen, Failed> {
        let mut name = |target: AnyRef| -> Result<String, Failed> {
            let get = Command::Ls {
                target: Some(target),
                get: Some("name".to_owned()),
            };
            let reply = succeeded(ask(get)?)?;
            Ok(reply.text.trim_end_matches('\n').to_owned())
        };
        Ok(match *self {
            Open::Rename(target) => Screen::Prompt(Prompt {
                title: format!("rename {}", about(target)),
                line: Line::new(name(target)?),
                rename: Some(target),
            }),
            Open::Confirm(target) => Screen::Confirm {
                question: format!("close {} {}?", about(target), name(target)?),
                about: target,
            },
            Open::Prompt => Screen::Prompt(Prompt {
                title: ":".to_owned(),
                line: Line::new(String::new()),
                rename: None,
            }),
            Open::Copy => Screen::Copy(None),
            Open::ChooseTab(moving) => chooser(ask, Choices::Tabs, moving, "tab")?,
            Open::ChooseWorkspace(moving) => {
                chooser(ask, Choices::Workspaces, moving, "workspace")?
            }
            Open::ChoosePane(source) => chooser(ask, Choices::Panes(source), Some(source), "pane")?,
        })
    }
}

/// A chooser of tabs, workspaces or panes: to show one, or to move or swap
/// the pane `moving` with it.
fn chooser(
    ask: &mut dyn FnMut(Command) -> Result<Reply, Failed>,
    choices: Choices,
    moving: Option<PaneId>,
    what: &str,
) -> Result<Screen, Failed> {
    let reply = succeeded(ask(Command::Client {
        client: None,
        action: ClientAction::Choices(choices),
    })?)?;
    let current = match reply.value {
        Some(Scalar::Int(n)) => usize::try_from(n).ok(),
        Some(Scalar::Bool(_) | Scalar::Str(_)) | None => None,
    };
    let items = (reply.ids.iter())
        .zip(reply.text.lines())
        .map(|(id, label)| {
            let it = naming(*id);
            let code = match (choices, moving) {
                (Choices::Panes(source), _) => format!("str {source} pane {it} swap-panes"),
                (Choices::Tabs, None) => format!("me {it} show-tab"),
                (Choices::Workspaces, None) => format!("me {it} show-workspace"),
                (Choices::Tabs | Choices::Workspaces, Some(p)) => {
                    format!("str {p} pane {it} move-to")
                }
            };
            Item {
                label: label.to_owned(),
                code,
                about: Some(*id),
            }
        })
        .collect();
    let title = match (choices, moving) {
        (Choices::Panes(source), _) => format!("swap {source} with"),
        (Choices::Tabs | Choices::Workspaces, Some(p)) => format!("move {p} to {what}"),
        (Choices::Tabs | Choices::Workspaces, None) => format!("{what}s"),
    };
    Ok(Screen::List(ListScreen {
        title,
        items,
        selected: current.unwrap_or(0),
        current,
        about: moving.map(AnyRef::Pane),
    }))
}

impl Default for Screen {
    fn default() -> Self {
        Self::Keys(Held::Normal)
    }
}

impl Screen {
    /// What the server draws it as, if it is one of the client's screens;
    /// nothing for its bindings.
    pub fn kind(&self) -> Option<ScreenKind> {
        match self {
            Self::Keys(_) => None,
            Self::Copy(_) => Some(ScreenKind::Copy),
            Self::List(_) | Self::Prompt(_) | Self::Confirm { .. } => Some(ScreenKind::Panel),
        }
    }

    /// The server ended its screen of `kind`: closed, if it is still that
    /// kind (the client may have opened another since).
    pub fn ended(&mut self, kind: ScreenKind) {
        if self.kind() == Some(kind) {
            *self = Self::default();
        }
    }

    /// What opening it sends the server: the panel to draw, or copy mode's
    /// start.
    pub fn opened(&self) -> Option<Act> {
        match self.kind()? {
            ScreenKind::Copy => Some(Act::Copy(CopyAct::Enter)),
            ScreenKind::Panel => self.panel().map(Act::Show),
        }
    }

    /// What the server draws of it.
    pub fn panel(&self) -> Option<Panel> {
        match self {
            Self::Keys(_) | Self::Copy(_) => None,
            Self::List(list) => Some(Panel::List(List {
                title: list.title.clone(),
                items: list.items.iter().map(|i| i.label.clone()).collect(),
                current: list.current,
                selected: list.selected,
                about: list.about,
            })),
            Self::Prompt(prompt) => Some(Panel::Prompt {
                title: prompt.title.clone(),
                before: prompt.line.before().to_owned(),
                after: prompt.line.after().to_owned(),
                about: prompt.rename,
            }),
            Self::Confirm { question, about } => Some(Panel::Confirm {
                question: question.clone(),
                about: *about,
            }),
        }
    }

    /// A key typed: what it leads to, through the bindings of `keymap` if
    /// no screen is open (`read_only`: the client only watches). A screen
    /// closes itself (`Screen::default`) as the key says.
    pub fn key(&mut self, keymap: &Keymap, stroke: Keystroke, read_only: bool) -> Vec<Effect> {
        if let Self::Keys(held) = self {
            return held.press(keymap, stroke, read_only);
        }
        let press = stroke.press;
        match std::mem::take(self) {
            Self::Keys(_) => Vec::new(),
            Self::List(list) => self.list(list, press),
            Self::Prompt(prompt) => match line_act(press) {
                Some(act) => self.prompt(prompt, act),
                None => {
                    *self = Self::Prompt(prompt);
                    Vec::new()
                }
            },
            Self::Confirm { question, about } => match confirm_act(press) {
                Some(true) => vec![HIDE, Effect::Run(format!("{} kill", naming(about)))],
                Some(false) => vec![HIDE],
                None => {
                    *self = Self::Confirm { question, about };
                    Vec::new()
                }
            },
            Self::Copy(search) => self.copy(search, press),
        }
    }

    /// Text pasted: a line being typed takes it; with no screen open, the
    /// pane does, and the column or a repeat mode ends.
    pub fn paste(&mut self, text: &str) -> Vec<Effect> {
        let insert = LineAct::Edit(crate::act::Edit::Insert(text.to_owned()));
        match std::mem::take(self) {
            Self::Keys(Held::Normal) => vec![Effect::Send(Act::Paste(text.to_owned()))],
            Self::Keys(_) => [Act::Hide, Act::Paste(text.to_owned())]
                .map(Effect::Send)
                .into(),
            Self::Prompt(prompt) => self.prompt(prompt, insert),
            Self::Copy(Some(search)) => self.copy_search(search, insert),
            other @ (Self::List(_) | Self::Confirm { .. } | Self::Copy(_)) => {
                *self = other;
                Vec::new()
            }
        }
    }

    fn list(&mut self, mut list: ListScreen, press: KeyPress) -> Vec<Effect> {
        let (at, last) = (list.selected, list.items.len().saturating_sub(1));
        let chosen = list.items.get(at);
        let about = chosen.and_then(|item| item.about);
        let (moved, code) = match list_act(press) {
            None => (at, None),
            Some(ListAct::Move(moved)) => (moved.from(at, last), None),
            Some(ListAct::Cancel) => return vec![HIDE],
            Some(ListAct::Choose) => (at, chosen.map(|item| item.code.clone())),
            Some(ListAct::Rename) => (at, about.map(|it| Open::Rename(it).code())),
            Some(ListAct::Close) => (at, about.map(|it| Open::Confirm(it).code())),
        };
        if let Some(code) = code {
            return vec![HIDE, Effect::Run(code)];
        }
        list.selected = moved;
        *self = Self::List(list);
        let shown = self.panel().filter(|_| moved != at);
        Vec::from_iter(shown.map(|p| Effect::Send(Act::Show(p))))
    }

    fn prompt(&mut self, mut prompt: Prompt, act: LineAct) -> Vec<Effect> {
        match act {
            LineAct::Cancel => vec![HIDE],
            LineAct::Submit => {
                let text = prompt.line.text();
                let code = match prompt.rename {
                    Some(it) => Some(format!(
                        "{} str {} rename",
                        naming(it),
                        crate::words::quote(&text)
                    )),
                    None => Some(text).filter(|t| !t.trim().is_empty()),
                };
                std::iter::once(HIDE).chain(code.map(Effect::Run)).collect()
            }
            LineAct::Edit(edit) => {
                prompt.line.edit(&edit);
                *self = Self::Prompt(prompt);
                self.panel()
                    .map(|p| Effect::Send(Act::Show(p)))
                    .into_iter()
                    .collect()
            }
        }
    }

    fn copy(&mut self, search: Option<(Seek, Line<1024>)>, press: KeyPress) -> Vec<Effect> {
        if let Some(search) = search {
            return match line_act(press) {
                Some(act) => self.copy_search(search, act),
                None => {
                    *self = Self::Copy(Some(search));
                    Vec::new()
                }
            };
        }
        // `f` and `r` open the search line, forward or back.
        if let Some(seek) = letter(press).and_then(|c| match c {
            'f' => Some(Seek::Forward),
            'r' => Some(Seek::Backward),
            _ => None,
        }) {
            *self = Self::Copy(Some((seek, Line::new(String::new()))));
            let typing = CopyAct::Typing(Some((seek, String::new(), String::new())));
            return vec![Effect::Send(Act::Copy(typing))];
        }
        let act = copy_act(press);
        // Leaving and copying end it, as `copy_act`'s bool says, here as on
        // the server (copy.rs tests that the two agree).
        if act.as_ref().is_none_or(|(_, ends)| !ends) {
            *self = Self::Copy(None);
        }
        act.map(|(act, _)| Effect::Send(Act::Copy(act)))
            .into_iter()
            .collect()
    }

    fn copy_search(&mut self, (seek, mut line): (Seek, Line<1024>), act: LineAct) -> Vec<Effect> {
        let sent = match act {
            LineAct::Cancel => CopyAct::Typing(None),
            LineAct::Submit => CopyAct::Find {
                query: line.text(),
                seek,
            },
            LineAct::Edit(edit) => {
                line.edit(&edit);
                let typing = (seek, line.before().to_owned(), line.after().to_owned());
                *self = Self::Copy(Some((seek, line)));
                return vec![Effect::Send(Act::Copy(CopyAct::Typing(Some(typing))))];
            }
        };
        *self = Self::Copy(None);
        vec![Effect::Send(Act::Copy(sent))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Key `name` typed at `screen`, with the default bindings.
    fn key(screen: &mut Screen, name: &str) -> Result<Vec<Effect>, String> {
        let stroke = crate::keymap::key(name)?.into();
        let keymap = crate::script::default_keys().map_err(|e| e.to_string())?;
        Ok(screen.key(&keymap, stroke, false))
    }

    fn list() -> Screen {
        let item = |n: u32| Item {
            label: format!("@{n}"),
            code: format!("me str @{n} tab show-tab"),
            about: format!("@{n}").parse().ok().map(AnyRef::Tab),
        };
        Screen::List(ListScreen {
            title: "tabs".to_owned(),
            items: vec![item(1), item(2)],
            current: Some(0),
            selected: 0,
            about: None,
        })
    }

    /// What the server ended closes only a screen of its kind: copy mode
    /// ending as a list opens leaves the list.
    #[test]
    fn a_screen_closes_only_for_its_own_kind_ending() {
        let mut screen = list();
        screen.ended(ScreenKind::Copy);
        assert_eq!(screen, list());
        screen.ended(ScreenKind::Panel);
        assert_eq!(screen, Screen::default());
        let mut copy = Screen::Copy(None);
        copy.ended(ScreenKind::Panel);
        assert_eq!(copy, Screen::Copy(None));
        copy.ended(ScreenKind::Copy);
        assert_eq!(copy, Screen::default());
    }

    /// A list moves, chooses, renames and closes on the client: each a
    /// change of mode made at the key itself.
    #[test]
    fn a_list_is_the_clients() -> Result<(), String> {
        let mut screen = list();
        let shown = key(&mut screen, "j")?;
        assert!(
            matches!(shown.as_slice(), [Effect::Send(Act::Show(Panel::List(l)))] if l.selected == 1)
        );
        assert_eq!(
            key(&mut screen, "Enter")?,
            [
                Effect::Send(Act::Hide),
                Effect::Run("me str @2 tab show-tab".into())
            ]
        );
        assert_eq!(screen, Screen::default());
        let mut screen = list();
        assert_eq!(
            key(&mut screen, "x")?,
            [
                Effect::Send(Act::Hide),
                Effect::Run("me str @1 tab confirm-close".into())
            ]
        );
        let mut screen = list();
        assert_eq!(key(&mut screen, "Escape")?, [Effect::Send(Act::Hide)]);
        Ok(())
    }

    /// A rename takes any name, quoted as fuxth reads it back.
    #[test]
    fn a_rename_prompt_runs_its_name_quoted() -> Result<(), String> {
        let mut screen = Screen::Prompt(Prompt {
            title: "rename".into(),
            line: Line::new(String::new()),
            rename: Some(AnyRef::Pane(PaneId::of(1))),
        });
        screen.paste("it's -x");
        assert_eq!(
            key(&mut screen, "Enter")?,
            [
                Effect::Send(Act::Hide),
                Effect::Run("str %1 pane str 'it'\\''s -x' rename".into())
            ]
        );
        Ok(())
    }

    /// Copy mode's search line is typed on the client; the search it asks
    /// for is the server's to make.
    #[test]
    fn copy_mode_searches_from_the_clients_line() -> Result<(), String> {
        let mut screen = Screen::Copy(None);
        key(&mut screen, "f")?;
        key(&mut screen, "a")?;
        let found = key(&mut screen, "Enter")?;
        assert_eq!(
            found,
            [Effect::Send(Act::Copy(CopyAct::Find {
                query: "a".into(),
                seek: Seek::Forward
            }))]
        );
        assert_eq!(screen, Screen::Copy(None));
        assert_eq!(
            key(&mut screen, "q")?,
            [Effect::Send(Act::Copy(CopyAct::Leave))]
        );
        assert_eq!(screen, Screen::default());
        Ok(())
    }
}
