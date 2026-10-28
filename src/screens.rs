//! A client's own screens: choosers, the command prompt, rename
//! prompts, confirmations and copy mode. They belong to the client, which
//! reads the keys: every change of mode happens here, at the key that
//! causes it, so no key is ever read in another mode's table. The server
//! draws what the client describes (`ScreenAct::Show`), and keeps copy mode's
//! cursor in the scrollback, which it alone holds (`ScreenAct::Copy`).
//!
//! What a screen's entry does is fuxth, run as the client runs a binding.
use crate::act::{Act, CopyAct, LineAct, List, ListAct, Panel, ScreenAct, Seek};
use crate::callback::{Callback, Data};
use crate::command::AnyRef;
use crate::copy::SEARCH_MAX;
use crate::decode::Input;
use crate::keymap::{Held, Keymap, Modes};
use crate::keys::{KeyPress, Keystroke};
use crate::script;
use crate::view::Line;
use crate::wire::{Wire, wire};

/// A typed screen request; only Callback.code is executable source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Open {
    List(ListScreen),
    Prompt(Prompt),
    Confirm(Confirm),
    Copy,
}

impl Wire for Open {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        match self {
            Self::List(list) => {
                out.push(0);
                list.put(out);
            }
            Self::Prompt(prompt) => {
                out.push(1);
                prompt.put(out);
            }
            Self::Confirm(confirm) => {
                out.push(2);
                confirm.put(out);
            }
            Self::Copy => out.push(3),
        }
    }
    fn get(r: &mut crate::wire::Reader<'_>) -> Result<Self, crate::wire::Error> {
        r.limited(crate::client::JOB_BYTES, |r| {
            Ok(match u8::get(r)? {
                0 => Self::List(ListScreen::get(r)?),
                1 => Self::Prompt(Prompt::get(r)?),
                2 => Self::Confirm(Confirm::get(r)?),
                3 => Self::Copy,
                tag => return Err(crate::wire::Error::Tag("screen", tag)),
            })
        })
    }
}

/// One of a client's screens, or none: its keys then go through its
/// bindings, in the middle of what `Held` says. So nothing is held for the
/// bindings while a screen is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    Keys(Held),
    List(ListScreen),
    Prompt(Prompt),
    Confirm(Confirm),
    /// Copy mode, and the search line being typed in it, forward or back.
    Copy(Option<(Seek, Line<SEARCH_MAX>)>),
}

wire! {
    /// An admitted chooser with one callback and independent current and selection.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct ListScreen {
        pub title: String,
        pub items: Vec<Item>,
        pub current: Option<usize>,
        pub selected: usize,
        pub about: Option<(AnyRef, Option<crate::command::Run>)>,
        pub action: Callback,
        pub client: Data,
    }
}
wire! {
    /// A row's inert label and actual immutable payload.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Item {
        pub label: String,
        pub payload: Data,
    }
}

/// A line being typed, and what Enter does with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub title: String,
    pub line: Line,
    /// None: the line is fuxth to run. Some: fuxth run with the line on its
    /// stack, a string.
    pub action: Option<Callback>,
}

/// A question, and the fuxth its yes runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Confirm {
    pub question: String,
    pub action: Callback,
}

impl Wire for Prompt {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.title.put(out);
        self.line.clone().text().put(out);
        self.action.put(out);
    }
    fn get(r: &mut crate::wire::Reader<'_>) -> Result<Self, crate::wire::Error> {
        let title = String::get(r)?;
        let text = r.bounded_text(crate::view::LINE_MAX)?;
        let mut line = Line::new(String::new());
        if text.contains(char::is_control) {
            return Err(crate::wire::Error::Bad("prompt text"));
        }
        line.edit(&crate::act::Edit::Insert(text))
            .map_err(|_| crate::wire::Error::Bad("prompt length"))?;
        Ok(Self {
            title,
            line,
            action: Option::get(r)?,
        })
    }
}
impl Wire for Confirm {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.question.put(out);
        self.action.put(out);
    }
    fn get(r: &mut crate::wire::Reader<'_>) -> Result<Self, crate::wire::Error> {
        Ok(Self {
            question: String::get(r)?,
            action: Callback::get(r)?,
        })
    }
}

/// What a key leads to, in a screen or through the bindings: an action for
/// the server, or fuxth to run as the client.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Send(Act),
    Screen(ScreenAct),
    /// A replacement key panel, rather than an edit of the one shown.
    Open(Panel),
    Run(Job),
}

/// Effects ready for the attachment: every screen action names its opening.
#[derive(Clone, Debug, PartialEq)]
pub enum Dispatch {
    Send(Act),
    Run(Job),
    /// Tell the worker the frame count after all preceding sends completed.
    Fence,
}

/// A client's display and its opening counter, kept for the attachment's life.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Display {
    pub screen: Screen,
    opening: u64,
}

impl Display {
    fn next(&self) -> Result<u64, String> {
        self.opening
            .checked_add(1)
            .ok_or_else(|| "screen opening ids exhausted".into())
    }

    /// Open a new instance, admitted with room for its longest edit.
    pub fn open(&mut self, screen: Screen, modes: &Modes) -> Vec<Act> {
        use crate::protocol::{AttachedFrame, Frame, MAX_FRAME};
        let opening = match self.next() {
            Ok(opening) => opening,
            Err(text) => return vec![Act::Notice { text, error: true }],
        };
        if screen.largest(modes).is_some_and(|act| {
            AttachedFrame::Act(Box::new(Act::Screen { opening, act }))
                .encode()
                .is_err()
        }) {
            let text =
                format!("a screen past {MAX_FRAME} bytes, too large to show, was not opened");
            return vec![Act::Notice { text, error: true }];
        }
        let Some(act) = screen.opened(modes) else {
            return Vec::new();
        };
        self.opening = opening;
        self.screen = screen;
        vec![Act::Screen { opening, act }]
    }

    /// A server ending belongs only to the opening it names.
    pub fn ended(&mut self, opening: u64) {
        if self.shown() && self.opening == opening {
            self.screen = Screen::default();
        }
    }

    pub fn close(&mut self) -> Option<Act> {
        let shown = self.shown();
        self.screen = Screen::default();
        shown.then_some(Act::Screen {
            opening: self.opening,
            act: ScreenAct::Hide,
        })
    }

    fn shown(&self) -> bool {
        !matches!(self.screen, Screen::Keys(Held::Normal | Held::Prefixed))
    }

    pub fn input(&mut self, keymap: &Keymap, input: Input, read_only: bool) -> Vec<Dispatch> {
        let mut active = self.shown();
        let effects = self.screen.input(keymap, input, read_only);
        effects
            .into_iter()
            .filter_map(|effect| {
                let (act, fresh) = match effect {
                    Effect::Run(job) => return Some(Dispatch::Run(job)),
                    Effect::Send(act) => return Some(Dispatch::Send(act)),
                    Effect::Open(panel) => (ScreenAct::Show(panel), true),
                    Effect::Screen(act) => (act, false),
                };
                if fresh || !active {
                    if !matches!(
                        act,
                        ScreenAct::Show(_) | ScreenAct::Copy(CopyAct::Enter { .. })
                    ) {
                        return None;
                    }
                    match self.next() {
                        Ok(opening) => self.opening = opening,
                        Err(text) => {
                            self.screen = Screen::default();
                            return Some(Dispatch::Send(Act::Notice { text, error: true }));
                        }
                    }
                    active = true;
                }
                if matches!(&act, ScreenAct::Hide)
                    || matches!(&act, ScreenAct::Copy(copy) if copy.ends())
                {
                    active = false;
                }
                Some(Dispatch::Send(Act::Screen {
                    opening: self.opening,
                    act,
                }))
            })
            .collect()
    }
}

/// An owned worker operation: a program with inert typed arguments,
/// or a relayed screen awaiting the receiving dictionary's admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub code: String,
    pub given: Option<Vec<Data>>,
    pub(crate) screen: Option<Open>,
}

impl From<String> for Job {
    fn from(code: String) -> Self {
        Job {
            code,
            given: None,
            screen: None,
        }
    }
}

impl From<&str> for Job {
    fn from(code: &str) -> Self {
        code.to_owned().into()
    }
}

impl Job {
    /// The code alone; arguments enter the compiler as literal values.
    pub fn tokens(&self) -> Result<Vec<fuxth::token::Token>, fuxth::Error> {
        fuxth::token::source("key", &self.code)
    }

    pub fn open(screen: Open) -> Self {
        Self {
            code: String::new(),
            given: None,
            screen: Some(screen),
        }
    }

    pub fn bytes(&self) -> usize {
        let retained = self.screen.as_ref().map_or(0, Open::bytes);
        self.given
            .iter()
            .flatten()
            .fold(self.code.capacity().saturating_add(retained), |n, v| {
                n.saturating_add(v.bytes())
            })
    }

    pub fn machine(
        &self,
        config: Option<&script::Config>,
    ) -> Result<fuxth::Machine<script::Fux>, script::Failed> {
        script::callback_machine(config, &self.tokens()?, self.given.as_deref())
    }
}

/// The most bytes of fuxth a prompt or a question holds to run: the
/// client keeps it, outside any frame, so it has a bound of its own.
pub const ACTION_MAX: usize = 1 << 16;

/// The effect that hides the screen showing.
pub const HIDE: Effect = Effect::Screen(ScreenAct::Hide);

impl Open {
    pub fn build(self) -> Screen {
        match self {
            Self::List(list) => Screen::List(list),
            Self::Prompt(prompt) => Screen::Prompt(prompt),
            Self::Confirm(confirm) => Screen::Confirm(confirm),
            Self::Copy => Screen::Copy(None),
        }
    }
}

impl Default for Screen {
    fn default() -> Self {
        Self::Keys(Held::Normal)
    }
}

impl Screen {
    /// Whether it owns the keys, beyond the client's bindings.
    pub fn is_open(&self) -> bool {
        !matches!(self, Self::Keys(_))
    }

    /// What opening it sends the server.
    pub fn opened(&self, modes: &Modes) -> Option<ScreenAct> {
        match self {
            Self::Copy(_) => Some(ScreenAct::Copy(CopyAct::Enter {
                hints: modes.copy.hints(),
                typing: modes.line.hints(),
            })),
            Self::Keys(_) | Self::List(_) | Self::Prompt(_) | Self::Confirm(_) => {
                self.panel(modes).map(ScreenAct::Show)
            }
        }
    }

    /// What `opened` sends at its largest, its line, if it has one, as long
    /// as a line may be: if this fits a frame, so does every edit's.
    pub fn largest(&self, modes: &Modes) -> Option<ScreenAct> {
        let mut largest = self.clone();
        if let Self::Prompt(prompt) = &mut largest {
            prompt.line = Line::new(std::iter::repeat_n('x', crate::view::LINE_MAX).collect());
        }
        largest.opened(modes)
    }

    /// What the server draws of it, with the hints of `modes`.
    pub fn panel(&self, modes: &Modes) -> Option<Panel> {
        match self {
            Self::Keys(_) | Self::Copy(_) => None,
            Self::List(list) => Some(Panel::List(List {
                title: list.title.clone(),
                items: list.items.iter().map(|i| i.label.clone()).collect(),
                current: list.current,
                selected: list.selected,
                about: list.about.map(|(id, _)| id),
                hints: modes.list.hints(),
            })),
            Self::Prompt(prompt) => Some(Panel::Prompt {
                title: prompt.title.clone(),
                before: prompt.line.before().to_owned(),
                after: prompt.line.after().to_owned(),
                hints: modes.line.hints(),
            }),
            Self::Confirm(confirm) => Some(Panel::Confirm {
                question: confirm.question.clone(),
                hints: modes.confirm.hints(),
            }),
        }
    }

    /// What a client's terminal sent (`input`), decoded: what it leads to,
    /// as `key` and `paste` say, else what the server is sent. A client
    /// that only watches (`read_only`) is told it of a paste, and sends no
    /// focus change or mouse report; the mouse is a program's only while
    /// no screen of the client's own is open.
    pub fn input(&mut self, keymap: &Keymap, input: Input, read_only: bool) -> Vec<Effect> {
        let send = |act| vec![Effect::Send(act)];
        match input {
            Input::Key(stroke) => self.key(keymap, stroke, read_only),
            Input::Reply(reply) => send(Act::Reply(reply)),
            Input::Paste(_) | Input::PasteTooLong if read_only => {
                send(crate::keymap::watching(keymap))
            }
            Input::FocusIn | Input::FocusOut | Input::Mouse(_) if read_only => Vec::new(),
            Input::Paste(text) => self.paste(&keymap.modes, &text),
            Input::PasteTooLong => {
                let limit = crate::decode::PASTE_LIMIT / 1024;
                let text = format!("paste exceeds {limit} KiB; discarded");
                send(Act::Notice { text, error: true })
            }
            Input::FocusIn => send(Act::Focus(true)),
            Input::FocusOut => send(Act::Focus(false)),
            Input::Mouse(_) if self.is_open() => Vec::new(),
            Input::Mouse(event) => send(Act::Mouse(event)),
        }
    }

    /// A key typed: what it leads to, through the bindings of `keymap` if
    /// no screen is open (`read_only`: the client only watches), else its
    /// screen's keys. A screen closes itself (`Screen::default`) as the key
    /// says.
    pub fn key(&mut self, keymap: &Keymap, stroke: Keystroke, read_only: bool) -> Vec<Effect> {
        if let Self::Keys(held) = self {
            return held.press(keymap, stroke, read_only);
        }
        let (press, modes) = (stroke.press, &keymap.modes);
        match std::mem::take(self) {
            Self::Keys(_) => Vec::new(),
            Self::List(list) => self.list(modes, list, press),
            Self::Prompt(prompt) => match modes.line.typed(press) {
                Some(act) => self.prompt(modes, prompt, act),
                None => {
                    *self = Self::Prompt(prompt);
                    Vec::new()
                }
            },
            Self::Confirm(confirm) => match modes.confirm.get(press) {
                Some(true) => vec![HIDE, Effect::Run(confirm.action.job([]))],
                Some(false) => vec![HIDE],
                None => {
                    *self = Self::Confirm(confirm);
                    Vec::new()
                }
            },
            Self::Copy(search) => self.copy(modes, search, press),
        }
    }

    /// Text pasted: a line being typed takes it; with no screen open, the
    /// pane does, and the column or a repeat mode ends; a list, a question
    /// or copy mode takes none, and says so.
    pub fn paste(&mut self, modes: &Modes, text: &str) -> Vec<Effect> {
        let insert = LineAct::Edit(crate::act::Edit::Insert(text.to_owned()));
        match std::mem::take(self) {
            Self::Keys(Held::Normal) => vec![Effect::Send(Act::Paste(text.to_owned()))],
            Self::Keys(_) => vec![HIDE, Effect::Send(Act::Paste(text.to_owned()))],
            Self::Prompt(prompt) => self.prompt(modes, prompt, insert),
            Self::Copy(Some(search)) => self.copy_search(search, insert),
            other @ (Self::List(_) | Self::Confirm(_) | Self::Copy(_)) => {
                *self = other;
                let why = "a list, a question or copy mode is open: nothing was pasted";
                vec![refusal(why.to_owned())]
            }
        }
    }

    fn list(&mut self, modes: &Modes, mut list: ListScreen, press: KeyPress) -> Vec<Effect> {
        let (at, last) = (list.selected, list.items.len().saturating_sub(1));
        let chosen = list.items.get(at);
        let act = modes.list.get(press);
        let action = match &act {
            Some(ListAct::Cancel) => return vec![HIDE],
            Some(ListAct::Choose) => Some(&list.action),
            Some(ListAct::Action(action)) => Some(action),
            Some(ListAct::Move(_)) | None => None,
        };
        if let (Some(action), Some(item)) = (action, chosen) {
            return vec![
                HIDE,
                Effect::Run(action.job([list.client.clone(), item.payload.clone()])),
            ];
        }
        let moved = match act {
            Some(ListAct::Move(moved)) => moved.from(at, last),
            Some(ListAct::Cancel | ListAct::Choose | ListAct::Action(_)) | None => at,
        };
        list.selected = moved;
        *self = Self::List(list);
        let shown = self.panel(modes).filter(|_| moved != at);
        Vec::from_iter(shown.map(|p| Effect::Screen(ScreenAct::Show(p))))
    }

    fn prompt(&mut self, modes: &Modes, mut prompt: Prompt, act: LineAct) -> Vec<Effect> {
        match act {
            LineAct::Cancel => vec![HIDE],
            LineAct::Submit => {
                let text = prompt.line.text();
                let job = match prompt.action {
                    Some(action) => Some(action.job([Data::Text(text)])),
                    None => Some(text).filter(|t| !t.trim().is_empty()).map(Job::from),
                };
                std::iter::once(HIDE).chain(job.map(Effect::Run)).collect()
            }
            LineAct::Edit(edit) => {
                let refused = prompt.line.edit(&edit).err();
                *self = Self::Prompt(prompt);
                let shown = self
                    .panel(modes)
                    .map(|p| Effect::Screen(ScreenAct::Show(p)));
                shown.into_iter().chain(refused.map(refusal)).collect()
            }
        }
    }

    fn copy(
        &mut self,
        modes: &Modes,
        search: Option<(Seek, Line<SEARCH_MAX>)>,
        press: KeyPress,
    ) -> Vec<Effect> {
        if let Some(search) = search {
            return match modes.line.typed(press) {
                Some(act) => self.copy_search(search, act),
                None => {
                    *self = Self::Copy(Some(search));
                    Vec::new()
                }
            };
        }
        let act = modes.copy.get(press);
        *self = match &act {
            // Leaving and copying end it, here as on the server.
            Some(act) if act.ends() => Self::default(),
            Some(CopyAct::Typing(Some((seek, ..)))) => {
                Self::Copy(Some((*seek, Line::new(String::new()))))
            }
            Some(_) | None => Self::Copy(None),
        };
        Vec::from_iter(act.map(|act| Effect::Screen(ScreenAct::Copy(act))))
    }

    fn copy_search(
        &mut self,
        (seek, mut line): (Seek, Line<SEARCH_MAX>),
        act: LineAct,
    ) -> Vec<Effect> {
        let sent = match act {
            LineAct::Cancel => CopyAct::Typing(None),
            LineAct::Submit => CopyAct::Find {
                query: line.text(),
                seek,
            },
            LineAct::Edit(edit) => {
                let refused = line.edit(&edit).err();
                let typing = (seek, line.before().to_owned(), line.after().to_owned());
                *self = Self::Copy(Some((seek, line)));
                let shown = Effect::Screen(ScreenAct::Copy(CopyAct::Typing(Some(typing))));
                return std::iter::once(shown).chain(refused.map(refusal)).collect();
            }
        };
        *self = Self::Copy(None);
        vec![Effect::Screen(ScreenAct::Copy(sent))]
    }
}

/// A line's refusal of an edit, told as an error's notice.
fn refusal(text: String) -> Effect {
    Effect::Send(Act::Notice { text, error: true })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Key `name` typed at `screen`, with the default bindings.
    fn key(screen: &mut Screen, name: &str) -> Result<Vec<Effect>, String> {
        let stroke = crate::keymap::key(name)?.into();
        let keymap = crate::script::default_state()
            .map(|state| state.keys)
            .map_err(|e| e.to_string())?;
        Ok(screen.key(&keymap, stroke, false))
    }

    fn list() -> Screen {
        let item = |n: u32| Item {
            label: format!("@{n}"),
            payload: Data::Int(i64::from(n)),
        };
        Screen::List(ListScreen {
            title: "tabs".to_owned(),
            items: vec![item(1), item(2)],
            current: Some(0),
            selected: 0,
            about: None,
            action: "2drop".into(),
            client: Data::Int(0),
        })
    }

    /// Delayed endings cannot close another opening, even of the same kind.
    #[test]
    fn an_ending_closes_only_its_own_opening() -> Result<(), String> {
        let keys = crate::script::default_state()
            .map(|state| state.keys)
            .map_err(|e| e.to_string())?;
        let prompt = Screen::Prompt(Prompt {
            title: "prompt".into(),
            line: Line::new("unsent work".into()),
            action: None,
        });
        for replacement in [list(), prompt, Screen::Copy(None)] {
            let mut display = Display::default();
            display.open(replacement.clone(), &keys.modes);
            let old = display.opening;
            display.open(replacement.clone(), &keys.modes);
            display.ended(old);
            assert_eq!(display.screen, replacement);
            let redraw = display.input(&keys, Input::Key(KeyPress::char('j').into()), false);
            assert!(redraw.iter().all(
                |e| !matches!(e, Dispatch::Send(Act::Screen { opening, .. }) if *opening == old)
            ));
            display.ended(display.opening);
            assert_eq!(display.screen, Screen::default());
        }
        Ok(())
    }

    #[test]
    fn opening_ids_never_wrap() {
        let mut display = Display {
            opening: u64::MAX,
            ..Display::default()
        };
        let acts = display.open(Screen::Copy(None), &Modes::default());
        assert!(matches!(acts.as_slice(), [Act::Notice { error: true, .. }]));
        assert_eq!(display.screen, Screen::default());
    }

    /// A list moves, chooses, renames and closes on the client: each a
    /// change of mode made at the key itself.
    #[test]
    fn a_list_is_the_clients() -> Result<(), String> {
        let mut screen = list();
        let shown = key(&mut screen, "j")?;
        assert!(
            matches!(shown.as_slice(), [Effect::Screen(ScreenAct::Show(Panel::List(l)))] if l.selected == 1)
        );
        assert_eq!(
            key(&mut screen, "Enter")?,
            [
                Effect::Screen(ScreenAct::Hide),
                Effect::Run(Callback::from("2drop").job([Data::Int(0), Data::Int(2)]))
            ]
        );
        assert_eq!(screen, Screen::default());
        let mut screen = list();
        assert_eq!(
            key(&mut screen, "x")?,
            [
                Effect::Screen(ScreenAct::Hide),
                Effect::Run(Callback::from("confirm-close").job([Data::Int(0), Data::Int(1)]))
            ]
        );
        let mut screen = list();
        assert_eq!(
            key(&mut screen, "Escape")?,
            [Effect::Screen(ScreenAct::Hide)]
        );
        Ok(())
    }

    /// A paste where no line is typed goes nowhere, and says so.
    #[test]
    fn a_paste_no_screen_takes_is_told() {
        let told = vec![refusal(
            "a list, a question or copy mode is open: nothing was pasted".into(),
        )];
        for mut screen in [list(), Screen::Copy(None)] {
            let was = screen.clone();
            assert_eq!(screen.paste(&Modes::default(), "x"), told);
            assert_eq!(screen, was);
        }
    }

    /// A prompt's action is given the line typed as a string, whatever it
    /// looks like: never read as code.
    #[test]
    fn a_prompt_gives_its_line_to_its_action() -> Result<(), String> {
        let mut screen = Screen::Prompt(Prompt {
            title: "rename".into(),
            line: Line::new(String::new()),
            action: Some("%1 swap rename".into()),
        });
        let modes = crate::script::default_state()
            .map(|state| state.keys)
            .map_err(|e| e.to_string())?
            .modes;
        screen.paste(&modes, "it's -x' drop");
        let ran = key(&mut screen, "Enter")?;
        let job = Job {
            code: "%1 swap rename".into(),
            given: Some(vec![Data::Text("it's -x' drop".into())]),
            screen: None,
        };
        assert_eq!(
            ran,
            [Effect::Screen(ScreenAct::Hide), Effect::Run(job.clone())]
        );
        assert_eq!(job.given, Some(vec![Data::Text("it's -x' drop".into())]));
        Ok(())
    }

    /// A prompt opens only if its line, grown as long as a line may be,
    /// still fits a frame: an edit never makes a display too large to send.
    #[test]
    fn a_prompt_fits_a_frame_at_its_largest() -> Result<(), String> {
        let modes = crate::script::default_state()
            .map(|state| state.keys)
            .map_err(|e| e.to_string())?
            .modes;
        let fits = |act: Option<ScreenAct>| {
            act.is_some_and(|act| {
                use crate::protocol::Frame;
                let frame =
                    crate::protocol::AttachedFrame::Act(Box::new(Act::Screen { opening: 1, act }));
                frame.encode().is_ok()
            })
        };
        let title = std::iter::repeat_n('t', crate::protocol::MAX_FRAME - 2048).collect();
        let screen = Screen::Prompt(Prompt {
            title,
            line: Line::new(String::new()),
            action: None,
        });
        assert!(fits(screen.opened(&modes)));
        assert!(!fits(screen.largest(&modes)));
        Ok(())
    }

    /// A named key with a modifier does what it does alone, unless bound
    /// so: Ctrl-Backspace deletes in a line.
    #[test]
    fn a_named_key_acts_with_its_modifiers() -> Result<(), String> {
        let mut screen = Screen::Prompt(Prompt {
            title: "t".into(),
            line: Line::new("ab".into()),
            action: None,
        });
        key(&mut screen, "C-BSpace")?;
        let Screen::Prompt(prompt) = &screen else {
            return Err(format!("{screen:?}"));
        };
        assert_eq!(prompt.line.before(), "a");
        assert_eq!(
            key(&mut screen, "C-Escape")?,
            [HIDE],
            "Escape leaves, held or not"
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
            [Effect::Screen(ScreenAct::Copy(CopyAct::Find {
                query: "a".into(),
                seek: Seek::Forward
            }))]
        );
        assert_eq!(screen, Screen::Copy(None));
        assert_eq!(
            key(&mut screen, "q")?,
            [Effect::Screen(ScreenAct::Copy(CopyAct::Leave))]
        );
        assert_eq!(screen, Screen::default());
        Ok(())
    }
}
