//! A client's own screens: choosers, the command prompt and rename
//! prompts. They belong to the client, which reads the keys: every change
//! of mode happens here, at the key that causes it, so no key is ever read
//! in another mode's table. The server draws what the client describes
//! (`ScreenAct::Show`).
//!
//! What a screen's entry does is fuxth, run as the client runs a binding.
use crate::keymap::{Held, Keymap, Modes};
use crate::script;
use fux_vt::keys::decode::Input;
use fux_vt::keys::{KeyPress, Keystroke};
use fux_wire::act::{Act, LineAct, List, ListAct, Panel, ScreenAct};
use fux_wire::callback::{Callback, Code, Data, ListScreen, Open, Prompt};

/// One of a client's screens, or none: its keys then go through its
/// bindings, in the middle of what `Held` says. So nothing is held for the
/// bindings while a screen is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    Keys(Held),
    List(ListScreen),
    Prompt(Prompt),
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

    /// Open a new instance.
    pub fn open(&mut self, screen: Screen, modes: &Modes) -> Vec<Act> {
        let opening = match self.next() {
            Ok(opening) => opening,
            Err(text) => return vec![Act::Notice { text, error: true }],
        };
        let Some(act) = screen.panel(modes).map(ScreenAct::Show) else {
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
                    if !matches!(act, ScreenAct::Show(_)) {
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
                if matches!(&act, ScreenAct::Hide) {
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

/// What a job runs: a line typed at the command prompt, read as a
/// program is; or a code value, compiled in the client's dictionary and
/// run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Program {
    Line(String),
    Code(Code),
}

/// An owned worker operation: a program with inert typed arguments,
/// or a relayed screen awaiting the receiving dictionary's admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub program: Program,
    pub given: Option<Vec<Data>>,
    pub(crate) screen: Option<Open>,
}

impl From<String> for Job {
    fn from(line: String) -> Self {
        Job {
            program: Program::Line(line),
            given: None,
            screen: None,
        }
    }
}

impl From<&str> for Job {
    fn from(line: &str) -> Self {
        line.to_owned().into()
    }
}

impl From<Code> for Job {
    fn from(code: Code) -> Self {
        Job {
            program: Program::Code(code),
            given: None,
            screen: None,
        }
    }
}

impl Job {
    /// A callback run on its captured arguments, then `args`, each a
    /// value, never source.
    pub fn call(callback: &Callback, args: impl IntoIterator<Item = Data>) -> Self {
        Self {
            program: Program::Code(callback.code.clone()),
            given: Some(callback.args.iter().cloned().chain(args).collect()),
            screen: None,
        }
    }

    pub fn open(screen: Open) -> Self {
        Self {
            program: Program::Line(String::new()),
            given: None,
            screen: Some(screen),
        }
    }

    /// Its machine, ready to run: compiling its code values stops at
    /// `stop`.
    pub fn machine(
        &self,
        config: Option<&script::Config>,
        stop: Option<&script::Stop>,
    ) -> Result<fuxth::Machine<script::Fux>, script::Failed> {
        script::callback_machine(config, &self.program, self.given.as_deref(), stop)
    }
}

/// The effect that hides the screen showing.
pub const HIDE: Effect = Effect::Screen(ScreenAct::Hide);

impl From<Open> for Screen {
    fn from(open: Open) -> Self {
        match open {
            Open::List(list) => Screen::List(list),
            Open::Prompt(prompt) => Screen::Prompt(prompt),
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

    /// What the server draws of it, with the hints of `modes`.
    pub fn panel(&self, modes: &Modes) -> Option<Panel> {
        match self {
            Self::Keys(_) => None,
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
            Input::Paste(_) if read_only => send(crate::keymap::watching(keymap)),
            Input::FocusIn | Input::FocusOut | Input::Mouse(_) if read_only => Vec::new(),
            Input::Paste(text) => self.paste(&keymap.modes, &text),
            // Never: the client's decoder keeps a paste of any length.
            Input::PasteTooLong => Vec::new(),
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
        }
    }

    /// Text pasted: a line being typed takes it; with no screen open, the
    /// pane does, and the column or a repeat mode ends; a list takes none,
    /// and says so.
    pub fn paste(&mut self, modes: &Modes, text: &str) -> Vec<Effect> {
        let insert = LineAct::Edit(fux_wire::act::Edit::Insert(text.to_owned()));
        match std::mem::take(self) {
            Self::Keys(Held::Normal) => vec![Effect::Send(Act::Paste(text.to_owned()))],
            Self::Keys(_) => vec![HIDE, Effect::Send(Act::Paste(text.to_owned()))],
            Self::Prompt(prompt) => self.prompt(modes, prompt, insert),
            other @ Self::List(_) => {
                *self = other;
                let why = "a list is open: nothing was pasted";
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
                Effect::Run(Job::call(
                    action,
                    [list.client.clone(), item.payload.clone()],
                )),
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
                    Some(action) => Some(Job::call(&action, [Data::Text(text)])),
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
}

/// A line's refusal of an edit, told as an error's notice.
fn refusal(text: String) -> Effect {
    Effect::Send(Act::Notice { text, error: true })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fux_wire::act::Line;
    use fux_wire::callback::Item;

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
            action: crate::testing::code("2drop").into(),
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
        for replacement in [list(), prompt] {
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
        let acts = display.open(list(), &Modes::default());
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
                Effect::Run(Job::call(
                    &crate::testing::code("2drop").into(),
                    [Data::Int(0), Data::Int(2)]
                ))
            ]
        );
        assert_eq!(screen, Screen::default());
        let mut screen = list();
        assert_eq!(
            key(&mut screen, "x")?,
            [
                Effect::Screen(ScreenAct::Hide),
                Effect::Run(Job::call(
                    &crate::testing::code("confirm-close").into(),
                    [Data::Int(0), Data::Int(1)]
                ))
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
        let told = vec![refusal("a list is open: nothing was pasted".into())];
        let mut screen = list();
        let was = screen.clone();
        assert_eq!(screen.paste(&Modes::default(), "x"), told);
        assert_eq!(screen, was);
    }

    /// A prompt's action is given the line typed as a string, whatever it
    /// looks like: never read as code.
    #[test]
    fn a_prompt_gives_its_line_to_its_action() -> Result<(), String> {
        let mut screen = Screen::Prompt(Prompt {
            title: "rename".into(),
            line: Line::new(String::new()),
            action: Some(crate::testing::code("%1 swap rename").into()),
        });
        let modes = crate::script::default_state()
            .map(|state| state.keys)
            .map_err(|e| e.to_string())?
            .modes;
        screen.paste(&modes, "it's -x' drop");
        let ran = key(&mut screen, "Enter")?;
        let job = Job {
            program: Program::Code(crate::testing::code("%1 swap rename")),
            given: Some(vec![Data::Text("it's -x' drop".into())]),
            screen: None,
        };
        assert_eq!(ran, [Effect::Screen(ScreenAct::Hide), Effect::Run(job)]);
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
}
