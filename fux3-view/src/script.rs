//! The configuration language's thread. Grift runs here, never on the
//! viewer's own: a binding that loops or waits holds only this thread, the
//! viewer stops it when its time is up, and it can detach at any moment.
//! What a binding does to a pane it does through the pane's files; to the
//! layout, through the one layout command grammar `.view/ctl` takes.
//!
//! The words here are the primitives. The commands (split, move, zoom,
//! pickers, prompts) are grift in `library.grift`, and the keys are grift in
//! `defaults.grift`; the user's `view.grift`, loaded last, extends or
//! replaces any of them.
use crate::layout::{Dir, Layout, Rect};
use fux3::session::Session;
use fuxix::poll::Poke;
use grift::{Answer, Error, Grift, HostCall, HostFunction, Interrupter, Kept, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::time::Duration;

pub const MAX_CONFIG: usize = 64 << 10;
pub const MAX_BINDINGS: usize = 256;
const MAX_HEAP: usize = 16 << 20;
/// How long each part of the configuration may take to load.
pub const CONFIG_TIME: Duration = Duration::from_secs(2);
/// How long a binding may run.
pub const BINDING_TIME: Duration = Duration::from_secs(5);
/// The most text a binding run copies out of the language, over all its words.
const MAX_COPIED: usize = 16 << 20;
/// The most keys in one chord.
const MAX_CHORD_KEYS: usize = 4;

/// A viewer action, done on the viewer's own thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verb {
    NextPane,
    PreviousPane,
    LastPane,
    Focus(Dir),
    NextTab,
    PreviousTab,
    LastTab,
    /// The tab numbered from 1.
    GoToTab(usize),
    NextWorkspace,
    PreviousWorkspace,
    LastWorkspace,
    /// Show the tab or workspace with this id.
    ShowTab(u32),
    ShowWorkspace(u32),
    Fit,
    Detach,
    /// Types the binding's first key itself: a prefix sent through.
    Literal,
    /// Does what this chord is bound to.
    Chord(String),
}

/// The commands every viewer starts with, then its keys. Both are ordinary
/// grift: a configuration may redefine, unbind or replace any of them.
const LIBRARY: &str = include_str!("library.grift");
const DEFAULTS: &str = include_str!("defaults.grift");

pub enum ToViewer {
    /// The chord runs its function on this thread, under this number.
    Bind {
        chord: String,
        caption: String,
        heading: String,
        generation: u64,
    },
    /// The chord does this at once on the viewer's own thread, so it works
    /// while a binding runs.
    BindVerb {
        chord: String,
        caption: String,
        heading: String,
        generation: u64,
        verb: Verb,
    },
    /// The chord holds other keys.
    Group {
        chord: String,
        caption: String,
        heading: String,
        repeat: bool,
        generation: u64,
    },
    Unbind(String),
    Verb(Verb),
    /// Focus this pane once it is there, and come back when it goes.
    Focus(String),
    /// Show a list: rows of value, label and whether it is where you are
    /// now (`*`) or only selected at first, and keys besides choosing with
    /// their captions. What is chosen goes to callback `id`.
    Pick {
        id: u64,
        title: String,
        rows: Vec<(String, String, Start)>,
        actions: Vec<(String, String)>,
    },
    /// Ask for a line; it goes to callback `id`.
    Ask {
        id: u64,
        prompt: String,
        initial: String,
    },
    /// The layout changed: read it now.
    Reread,
    /// Resize these panes to their tiles once the layout is at `version`.
    Fit {
        version: u64,
        panes: Vec<String>,
    },
    /// A line for the bar's corner.
    Say(String),
    /// Bytes for a pane's program, after any keys typed before them.
    Type {
        pane: String,
        bytes: Vec<u8>,
    },
    /// A key for a pane's program, encoded as the program asked.
    Key {
        pane: String,
        key: fux_vt::keys::KeyPress,
    },
    /// A run begins; `stop` ends it, `limit` is its time.
    Started {
        stop: Interrupter,
        limit: Duration,
    },
    /// The run ended: what went wrong, if anything.
    Done(Option<String>),
}

/// How a row of a list starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Plain,
    /// Marked `*` and selected: where you are now.
    Current,
    /// Selected, not marked.
    Selected,
}

/// What the viewer shows, as a run sees it.
#[derive(Clone, Default)]
pub struct Context {
    /// The pane focused, as `NAME@ID`.
    pub focus: Option<String>,
    pub tab: Option<u32>,
    pub workspace: Option<u32>,
    /// The room tiles share, as `COLSxROWS`.
    pub area: Rect,
    pub layout: Arc<Layout>,
    /// Every binding: chord, caption, heading.
    pub keys: Arc<Vec<(String, String, String)>>,
}

/// What to run.
pub enum Job {
    /// The binding of `chord`, if it is still number `generation`.
    Binding { chord: String, generation: u64 },
    /// Callback `id` with what a panel gave: chosen, or one of its keys
    /// pressed on it, by number.
    Answer {
        id: u64,
        value: String,
        action: Option<usize>,
    },
    /// Callback `id` is no longer wanted.
    Drop(u64),
    /// The function the configuration names `name`, with string arguments.
    Call { name: String, args: Vec<String> },
}

pub struct Run {
    /// What its errors are said to come from; empty for none.
    pub label: String,
    pub job: Job,
    pub context: Context,
}

/// What the viewer holds of this thread.
pub struct Script {
    pub runs: Sender<Run>,
    pub cancel: Arc<AtomicBool>,
}

struct Host {
    out: SyncSender<ToViewer>,
    poke: Poke,
    kept: HashMap<String, Kept>,
    context: Context,
    cancel: Arc<AtomicBool>,
    fux3: PathBuf,
    session: Session,
    /// Bytes copied out of the language in this run.
    copied: usize,
    /// Chords bound to viewer actions or groups, which count too.
    others: std::collections::HashSet<String>,
    /// Each chord's binding, numbered: a run queued for an older one is
    /// refused out loud, never run with whatever replaced it.
    generations: HashMap<String, u64>,
    next_generation: u64,
    /// The open panel's callbacks, by number: what choosing calls, then
    /// what each of its keys calls.
    callback: Option<(u64, Vec<Kept>)>,
    next_callback: u64,
}

impl Host {
    /// Room for a binding on `chord`, and the number its new binding gets.
    fn rebind(&mut self, chord: &str) -> Result<u64, String> {
        let known = self.kept.contains_key(chord) || self.others.contains(chord);
        if !known && self.kept.len().saturating_add(self.others.len()) >= MAX_BINDINGS {
            return Err(format!("more than {MAX_BINDINGS} bindings"));
        }
        self.next_generation = self.next_generation.saturating_add(1);
        self.generations
            .insert(chord.to_owned(), self.next_generation);
        Ok(self.next_generation)
    }

    fn tell(&self, message: ToViewer) {
        let _ = self.out.send(message);
        self.poke.poke();
    }
}

type Word = HostFunction<Host>;

/// A key, or keys after a prefix, in their one spelling, or why it is none.
pub fn chord(text: &str) -> Result<String, String> {
    let keys: Vec<&str> = text.split_whitespace().collect();
    if keys.is_empty() || keys.len() > MAX_CHORD_KEYS {
        return Err(format!("{text:?}: a chord is 1 to {MAX_CHORD_KEYS} keys"));
    }
    let names: Result<Vec<String>, String> = keys
        .iter()
        .map(|key| {
            key.parse::<fux_vt::keys::KeyPress>()
                .map(|press| press.to_string())
                .map_err(|e| e.to_string())
        })
        .collect();
    Ok(names?.join(" "))
}

/// A string argument, copied out within the run's one byte budget.
fn string_arg(g: &mut HostCall<'_, Host>, host: &mut Host, value: Value) -> Result<String, Error> {
    let Some(len) = g.as_str(&value)?.map(str::len) else {
        return Err(g.type_error(value));
    };
    host.copied = host.copied.saturating_add(len);
    if host.copied > MAX_COPIED {
        return Err(g.limit("too-much-text", &[]));
    }
    Ok(g.as_str(&value)?.unwrap_or_default().to_owned())
}

fn failed(g: &mut HostCall<'_, Host>, why: &str) -> Error {
    match g.string(why) {
        Ok(text) => g.error("fux3-error", &[text]),
        Err(error) => error,
    }
}

/// What a word that failed says, unless the run was stopped.
fn refused(g: &mut HostCall<'_, Host>, host: &Host, failure: &fux3::Failure) -> Error {
    match host.cancel.load(Ordering::Relaxed) {
        true => g.limit("interrupted", &[]),
        false => failed(g, &failure.0),
    }
}

/// The directory of the pane `NAME` or `NAME@ID` names, refused if that
/// life of it is over.
fn pane_dir(g: &mut HostCall<'_, Host>, host: &Host, pane: &str) -> Result<PathBuf, Error> {
    let target = crate::layout::Target::parse(pane).map_err(|f| failed(g, &f.0))?;
    let dir = host.session.pane_dir(&target.name);
    if let Some(id) = &target.id {
        let status = crate::panes::read_status(&dir).map_err(|f| failed(g, &f.0))?;
        if fux3::files::status_value(&status, "id") != Some(id.as_str()) {
            return Err(failed(g, &format!("pane {pane} is gone")));
        }
    }
    Ok(dir)
}

/// The rest of the arguments, as a program and its words.
fn program(
    g: &mut HostCall<'_, Host>,
    host: &mut Host,
    words: &[Value],
) -> Result<Vec<String>, Error> {
    words
        .iter()
        .map(|word| string_arg(g, host, *word))
        .collect()
}

fn positive(g: &mut HostCall<'_, Host>, value: Value) -> Result<usize, Error> {
    if let Value::Integer(n) = value
        && let Ok(n) = usize::try_from(n)
        && n > 0
    {
        return Ok(n);
    }
    Err(g.type_error(value))
}

fn named_verb(word: &str) -> Option<Verb> {
    Some(match word {
        "next-pane" => Verb::NextPane,
        "previous-pane" => Verb::PreviousPane,
        "last-pane" => Verb::LastPane,
        "focus-left" => Verb::Focus(Dir::Left),
        "focus-right" => Verb::Focus(Dir::Right),
        "focus-up" => Verb::Focus(Dir::Up),
        "focus-down" => Verb::Focus(Dir::Down),
        "next-tab" => Verb::NextTab,
        "previous-tab" => Verb::PreviousTab,
        "last-tab" => Verb::LastTab,
        "next-workspace" => Verb::NextWorkspace,
        "previous-workspace" => Verb::PreviousWorkspace,
        "last-workspace" => Verb::LastWorkspace,
        "fit" => Verb::Fit,
        "detach" => Verb::Detach,
        "literal" => Verb::Literal,
        _ => return None,
    })
}

/// A verb from its word and its argument, if it takes one.
fn verb_of(g: &mut HostCall<'_, Host>, host: &mut Host, args: &[Value]) -> Result<Verb, Error> {
    let (word, number) = match args {
        [word] => (*word, None),
        [word, number] => (*word, Some(*number)),
        _ => return Err(g.arity_error(args)),
    };
    let word = string_arg(g, host, word)?;
    let id = |g: &mut HostCall<'_, Host>, host: &mut Host, value: Value, prefix: char| {
        let text = string_arg(g, host, value)?;
        match text
            .strip_prefix(prefix)
            .and_then(|d| d.parse::<u32>().ok())
        {
            Some(id) => Ok(id),
            None => Err(failed(g, &format!("{text:?} is not an id {prefix}N"))),
        }
    };
    match (word.as_str(), number) {
        ("go-to-tab", Some(number)) => Ok(Verb::GoToTab(positive(g, number)?)),
        ("show-tab", Some(tab)) => Ok(Verb::ShowTab(id(g, host, tab, 't')?)),
        ("show-workspace", Some(space)) => Ok(Verb::ShowWorkspace(id(g, host, space, 'w')?)),
        ("chord", Some(text)) => {
            let text = string_arg(g, host, text)?;
            Ok(Verb::Chord(chord(&text).map_err(|why| failed(g, &why))?))
        }
        (word, None) => named_verb(word).ok_or_else(|| failed(g, &format!("no verb {word}"))),
        (_, Some(_)) => Err(g.arity_error(args)),
    }
}

fn inert() -> Result<Answer, Error> {
    Ok(Answer::Now(Value::Inert))
}

fn text_answer(g: &mut HostCall<'_, Host>, text: &str) -> Result<Answer, Error> {
    g.string(text.trim_end()).map(Answer::Now)
}

/// A list of strings.
fn strings(g: &mut HostCall<'_, Host>, items: &[String]) -> Result<Value, Error> {
    let mut values = Vec::with_capacity(items.len());
    for item in items {
        values.push(g.string(item)?);
    }
    g.list(&values)
}

/// A list of lists of strings.
fn rows(g: &mut HostCall<'_, Host>, items: &[Vec<String>]) -> Result<Answer, Error> {
    let mut values = Vec::with_capacity(items.len());
    for item in items {
        values.push(strings(g, item)?);
    }
    g.list(&values).map(Answer::Now)
}

/// Caption and heading, each a non-empty string.
fn captions(
    g: &mut HostCall<'_, Host>,
    host: &mut Host,
    caption: Value,
    heading: Value,
) -> Result<(String, String), Error> {
    let caption = string_arg(g, host, caption)?;
    let heading = string_arg(g, host, heading)?;
    if caption.trim().is_empty() || heading.trim().is_empty() {
        return Err(failed(g, "a binding needs a caption and a heading"));
    }
    Ok((caption, heading))
}

/// Keeps `functions` as the open panel's callbacks, letting go of any before.
fn keep_callbacks(
    g: &mut HostCall<'_, Host>,
    host: &mut Host,
    functions: &[Value],
) -> Result<u64, Error> {
    let mut kept = Vec::with_capacity(functions.len());
    for function in functions {
        match g.keep(*function) {
            Ok(one) => kept.push(one),
            Err(error) => {
                for one in kept {
                    let _ = g.release(one);
                }
                return Err(error);
            }
        }
    }
    release_callbacks(g, host);
    host.next_callback = host.next_callback.saturating_add(1);
    host.callback = Some((host.next_callback, kept));
    Ok(host.next_callback)
}

fn release_callbacks(g: &mut HostCall<'_, Host>, host: &mut Host) {
    if let Some((_, old)) = host.callback.take() {
        for one in old {
            let _ = g.release(one);
        }
    }
}

const WORDS: &[(&str, Word)] = &[
    ("bind-key", |g, host, args| {
        let [key, caption, heading, function] = *g.exactly(args)?;
        let text = string_arg(g, host, key)?;
        let chord = chord(&text).map_err(|why| failed(g, &why))?;
        let (caption, heading) = captions(g, host, caption, heading)?;
        // Kept first: a binding that can't be made leaves the old one whole.
        let kept = g.keep(function)?;
        let generation = match host.rebind(&chord) {
            Ok(generation) => generation,
            Err(why) => {
                let _ = g.release(kept);
                return Err(failed(g, &why));
            }
        };
        host.others.remove(&chord);
        if let Some(old) = host.kept.insert(chord.clone(), kept) {
            let _ = g.release(old);
        }
        host.tell(ToViewer::Bind {
            chord,
            caption,
            heading,
            generation,
        });
        inert()
    }),
    ("bind-verb", |g, host, args| {
        let Some(([key, caption, heading], rest)) = args.split_first_chunk::<3>() else {
            return Err(g.arity_error(args));
        };
        let text = string_arg(g, host, *key)?;
        let chord = chord(&text).map_err(|why| failed(g, &why))?;
        let (caption, heading) = captions(g, host, *caption, *heading)?;
        let verb = verb_of(g, host, rest)?;
        let generation = host.rebind(&chord).map_err(|why| failed(g, &why))?;
        if let Some(old) = host.kept.remove(&chord) {
            let _ = g.release(old);
        }
        host.others.insert(chord.clone());
        host.tell(ToViewer::BindVerb {
            chord,
            caption,
            heading,
            generation,
            verb,
        });
        inert()
    }),
    ("bind-group", |g, host, args| {
        let (key, caption, heading, repeat) = match args {
            [key, caption, heading] => (*key, *caption, *heading, false),
            [key, caption, heading, repeat] => (
                *key,
                *caption,
                *heading,
                matches!(repeat, Value::Boolean(true)),
            ),
            _ => return Err(g.arity_error(args)),
        };
        let text = string_arg(g, host, key)?;
        let chord = chord(&text).map_err(|why| failed(g, &why))?;
        let (caption, heading) = captions(g, host, caption, heading)?;
        let generation = host.rebind(&chord).map_err(|why| failed(g, &why))?;
        if let Some(old) = host.kept.remove(&chord) {
            let _ = g.release(old);
        }
        host.others.insert(chord.clone());
        host.tell(ToViewer::Group {
            chord,
            caption,
            heading,
            repeat,
            generation,
        });
        inert()
    }),
    ("unbind-key", |g, host, args| {
        let [key] = *g.exactly(args)?;
        let text = string_arg(g, host, key)?;
        let chord = chord(&text).map_err(|why| failed(g, &why))?;
        if let Some(old) = host.kept.remove(&chord) {
            let _ = g.release(old);
        }
        host.others.remove(&chord);
        host.generations.remove(&chord);
        host.tell(ToViewer::Unbind(chord));
        inert()
    }),
    ("act", |g, host, args| {
        let verb = verb_of(g, host, args)?;
        host.tell(ToViewer::Verb(verb));
        inert()
    }),
    ("focused", |g, host, args| {
        let [] = *g.exactly(args)?;
        match host.context.focus.clone() {
            Some(pane) => g.string(&pane).map(Answer::Now),
            None => Err(failed(g, "no pane is focused")),
        }
    }),
    ("shown-tab", |g, host, args| {
        let [] = *g.exactly(args)?;
        match host.context.tab {
            Some(id) => g.string(&format!("t{id}")).map(Answer::Now),
            None => Err(failed(g, "no tab is shown")),
        }
    }),
    ("shown-workspace", |g, host, args| {
        let [] = *g.exactly(args)?;
        match host.context.workspace {
            Some(id) => g.string(&format!("w{id}")).map(Answer::Now),
            None => Err(failed(g, "no workspace is shown")),
        }
    }),
    ("area", |g, host, args| {
        let [] = *g.exactly(args)?;
        let area = host.context.area;
        g.string(&format!("{}x{}", area.w.max(1), area.h.max(1)))
            .map(Answer::Now)
    }),
    ("workspaces", |g, host, args| {
        let [] = *g.exactly(args)?;
        let layout = Arc::clone(&host.context.layout);
        let items: Vec<Vec<String>> = (layout.workspaces.iter())
            .map(|space| vec![format!("w{}", space.id), space.title.clone()])
            .collect();
        rows(g, &items)
    }),
    ("tabs", |g, host, args| {
        let [] = *g.exactly(args)?;
        let layout = Arc::clone(&host.context.layout);
        let items: Vec<Vec<String>> = (layout.workspaces.iter())
            .flat_map(|space| {
                space.tabs.iter().map(|tab| {
                    vec![
                        format!("t{}", tab.id),
                        tab.title.clone(),
                        format!("w{}", space.id),
                    ]
                })
            })
            .collect();
        rows(g, &items)
    }),
    ("panes", |g, host, args| {
        let [] = *g.exactly(args)?;
        let layout = Arc::clone(&host.context.layout);
        let items: Vec<Vec<String>> = layout
            .tabs()
            .flat_map(|tab| {
                (tab.leaves().into_iter())
                    .map(|pane| vec![pane.to_string(), format!("t{}", tab.id)])
            })
            .collect();
        rows(g, &items)
    }),
    ("key-bindings", |g, host, args| {
        let [] = *g.exactly(args)?;
        let keys = Arc::clone(&host.context.keys);
        let items: Vec<Vec<String>> = (keys.iter())
            .map(|(chord, caption, heading)| vec![chord.clone(), caption.clone(), heading.clone()])
            .collect();
        rows(g, &items)
    }),
    ("pick", |g, host, args| {
        let (title, items, function, actions) = match args {
            [title, items, function] => (*title, *items, *function, None),
            [title, items, function, actions] => (*title, *items, *function, Some(*actions)),
            _ => return Err(g.arity_error(args)),
        };
        let title = string_arg(g, host, title)?;
        let mut rows = Vec::new();
        for item in g.list_items(&items)? {
            let parts = g.list_items(&item)?;
            let (Some(value), Some(label)) = (parts.first(), parts.get(1)) else {
                return Err(failed(
                    g,
                    "each row is (value label), then #true for where you are, or \"selected\"",
                ));
            };
            let start = match parts.get(2) {
                Some(Value::Boolean(true)) => Start::Current,
                Some(other) if g.as_str(other)? == Some("selected") => Start::Selected,
                _ => Start::Plain,
            };
            rows.push((
                string_arg(g, host, *value)?,
                string_arg(g, host, *label)?,
                start,
            ));
            if rows.len() > crate::panel::MAX_ITEMS {
                return Err(failed(
                    g,
                    &format!("more than {} rows", crate::panel::MAX_ITEMS),
                ));
            }
        }
        let mut keys = Vec::new();
        let mut functions = vec![function];
        if let Some(actions) = actions {
            for action in g.list_items(&actions)? {
                let parts = g.list_items(&action)?;
                let (Some(key), Some(caption), Some(then)) =
                    (parts.first(), parts.get(1), parts.get(2))
                else {
                    return Err(failed(g, "each key of a list is (key caption function)"));
                };
                keys.push((string_arg(g, host, *key)?, string_arg(g, host, *caption)?));
                functions.push(*then);
            }
        }
        let id = keep_callbacks(g, host, &functions)?;
        host.tell(ToViewer::Pick {
            id,
            title,
            rows,
            actions: keys,
        });
        inert()
    }),
    ("read-line", |g, host, args| {
        let [prompt, initial, function] = *g.exactly(args)?;
        let prompt = string_arg(g, host, prompt)?;
        let initial = string_arg(g, host, initial)?;
        let id = keep_callbacks(g, host, &[function])?;
        host.tell(ToViewer::Ask {
            id,
            prompt,
            initial,
        });
        inert()
    }),
    ("arrange", |g, host, args| {
        let [line] = *g.exactly(args)?;
        let line = string_arg(g, host, line)?;
        let area = host.context.area;
        let outcome = crate::command::run_line(&host.session, &line, area, &host.cancel);
        // Whatever happened, the layout is read again now.
        host.tell(ToViewer::Reread);
        match outcome {
            Ok(edited) => text_answer(g, &edited.to_string()),
            Err(failure) => Err(refused(g, host, &failure)),
        }
    }),
    ("fit-after", |g, host, args| {
        let [text] = *g.exactly(args)?;
        let text = string_arg(g, host, text)?;
        let version = (text.lines())
            .find_map(|line| line.strip_prefix("version "))
            .and_then(|v| v.parse::<u64>().ok());
        let Some(version) = version else {
            return Err(failed(g, "not what arrange gives"));
        };
        let panes: Vec<String> = (text.lines())
            .filter_map(|line| line.strip_prefix("changed "))
            .map(str::to_owned)
            .collect();
        host.tell(ToViewer::Fit { version, panes });
        g.string(&text).map(Answer::Now)
    }),
    ("line-of", |g, host, args| {
        let [key, text] = *g.exactly(args)?;
        let key = format!("{} ", string_arg(g, host, key)?);
        let text = string_arg(g, host, text)?;
        match text.lines().find_map(|line| line.strip_prefix(&key)) {
            Some(value) => g.string(value).map(Answer::Now),
            None => Err(failed(g, &format!("no {}line in what arrange gave", key))),
        }
    }),
    ("quote", |g, host, args| {
        let [text] = *g.exactly(args)?;
        let text = string_arg(g, host, text)?;
        let mut out = String::from('"');
        for c in text.chars() {
            if c == '"' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        g.string(&out).map(Answer::Now)
    }),
    ("say", |g, host, args| {
        let [text] = *g.exactly(args)?;
        let text = string_arg(g, host, text)?;
        host.tell(ToViewer::Say(text));
        inert()
    }),
    ("focus", |g, host, args| {
        let [pane] = *g.exactly(args)?;
        let pane = string_arg(g, host, pane)?;
        host.tell(ToViewer::Focus(pane));
        inert()
    }),
    ("fux3-program", |g, host, args| {
        let [] = *g.exactly(args)?;
        let path = host.fux3.to_string_lossy().into_owned();
        g.string(&path).map(Answer::Now)
    }),
    ("session-dir", |g, host, args| {
        let [] = *g.exactly(args)?;
        let path = host.session.root.to_string_lossy().into_owned();
        g.string(&path).map(Answer::Now)
    }),
    ("start", |g, host, args| {
        let Some((name, rest)) = args.split_first() else {
            return Err(g.arity_error(args));
        };
        let name = string_arg(g, host, *name)?;
        let words = program(g, host, rest)?;
        match crate::command::start(&host.fux3, &host.session, &name, &words, &host.cancel) {
            Ok(pane) => g.string(&pane.to_string()).map(Answer::Now),
            Err(failure) => Err(refused(g, host, &failure)),
        }
    }),
    ("start-new", |g, host, args| {
        let Some((prefix, rest)) = args.split_first() else {
            return Err(g.arity_error(args));
        };
        let prefix = string_arg(g, host, *prefix)?;
        let words = program(g, host, rest)?;
        match crate::command::start_new(&host.fux3, &host.session, &prefix, &words, &host.cancel) {
            Ok(pane) => g.string(&pane.to_string()).map(Answer::Now),
            Err(failure) => Err(refused(g, host, &failure)),
        }
    }),
    ("send", |g, host, args| {
        let [pane, text] = *g.exactly(args)?;
        let pane = string_arg(g, host, pane)?;
        pane_dir(g, host, &pane)?;
        let bytes = string_arg(g, host, text)?.into_bytes();
        host.tell(ToViewer::Type { pane, bytes });
        inert()
    }),
    ("send-key", |g, host, args| {
        let [pane, key] = *g.exactly(args)?;
        let pane = string_arg(g, host, pane)?;
        pane_dir(g, host, &pane)?;
        let key = string_arg(g, host, key)?;
        let key = key
            .parse::<fux_vt::keys::KeyPress>()
            .map_err(|e| failed(g, &e.to_string()))?;
        host.tell(ToViewer::Key { pane, key });
        inert()
    }),
    ("ctl", |g, host, args| {
        let [pane, line] = *g.exactly(args)?;
        let pane = string_arg(g, host, pane)?;
        let line = string_arg(g, host, line)?;
        let dir = pane_dir(g, host, &pane)?;
        crate::panes::ctl_line(&dir, &line).map_err(|f| failed(g, &f.0))?;
        inert()
    }),
    ("read", |g, host, args| {
        let [pane] = *g.exactly(args)?;
        let pane = string_arg(g, host, pane)?;
        let dir = pane_dir(g, host, &pane)?;
        let path = dir.join(fux3::files::SCREEN);
        let bytes = fux3::files::read_small(&path, fux3::files::MAX_SCREEN_FILE)
            .map_err(|f| failed(g, &f.0))?;
        text_answer(g, &String::from_utf8_lossy(&bytes))
    }),
    ("ls", |g, host, args| {
        let [] = *g.exactly(args)?;
        let names = host.session.names().map_err(|f| failed(g, &f.0))?;
        strings(g, &names).map(Answer::Now)
    }),
];

fn describe(g: &Grift<Host>, error: &Error) -> String {
    let mut text = grift::TextBuffer::new();
    match g.describe(error, &mut text, 300) {
        Ok(_) => text.as_str().replace('\n', " "),
        Err(_) => "the program failed".to_owned(),
    }
}

fn prepare() -> Result<Grift<Host>, String> {
    let mut g: Grift<Host> = Grift::new().map_err(|_| "no memory for grift".to_owned())?;
    g.set_memory_limit(Some(MAX_HEAP));
    for (name, word) in WORDS {
        let made = g.host_function(name, *word).and_then(|f| g.define(name, f));
        if let Err(error) = made {
            return Err(describe(&g, &error));
        }
    }
    Ok(g)
}

/// Loads the library, the default keys and then the user's configuration,
/// each in its own time; the first that fails stops the rest, and says so.
fn load(g: &mut Grift<Host>, host: &mut Host, config: Option<String>) -> Option<String> {
    let parts = [
        ("the command library", Some(LIBRARY.to_owned())),
        ("the default keys", Some(DEFAULTS.to_owned())),
        ("view.grift", config),
    ];
    for (what, text) in parts {
        let Some(text) = text else {
            continue;
        };
        host.tell(ToViewer::Started {
            stop: g.interrupter(),
            limit: CONFIG_TIME,
        });
        if let Err(error) = g.evaluate(host, &text) {
            return Some(format!("{what}: {}", describe(g, &error)));
        }
    }
    None
}

/// The function a run calls, and its arguments.
fn function_of(
    g: &mut Grift<Host>,
    host: &mut Host,
    job: Job,
) -> Result<Option<(Value, Vec<Value>)>, String> {
    let kept = |g: &Grift<Host>, kept: &Kept| g.kept(kept).map_err(|e| describe(g, &e));
    match job {
        Job::Binding { chord, generation } => {
            if host.generations.get(&chord) != Some(&generation) {
                return Err(format!(
                    "{chord} was rebound before it ran, and did not run"
                ));
            }
            match host.kept.get(&chord) {
                Some(function) => Ok(Some((kept(g, function)?, Vec::new()))),
                None => Ok(None),
            }
        }
        Job::Answer { id, value, action } => {
            let Some((own, functions)) = host.callback.take() else {
                return Ok(None);
            };
            if own != id {
                host.callback = Some((own, functions));
                return Ok(None);
            }
            let wanted = action.map_or(0, |n| n.saturating_add(1));
            let function = functions.get(wanted).map(|f| kept(g, f));
            for one in functions {
                let _ = g.release(one);
            }
            let value = g.string(&value).map_err(|e| describe(g, &e))?;
            match function {
                Some(function) => Ok(Some((function?, vec![value]))),
                None => Ok(None),
            }
        }
        Job::Drop(id) => {
            if let Some((own, functions)) = host.callback.take() {
                match own == id {
                    true => functions.into_iter().for_each(|f| drop(g.release(f))),
                    false => host.callback = Some((own, functions)),
                }
            }
            Ok(None)
        }
        Job::Call { name, args } => {
            let symbol = g.symbol(&name).map_err(|e| describe(g, &e))?;
            let top = g.top_level();
            let function = g.lookup(&top, &symbol).map_err(|e| describe(g, &e))?;
            let function = function.ok_or_else(|| format!("nothing is called {name}"))?;
            let mut values = Vec::with_capacity(args.len());
            for arg in args {
                values.push(g.string(&arg).map_err(|e| describe(g, &e))?);
            }
            Ok(Some((function, values)))
        }
    }
}

fn thread(mut host: Host, config: Option<String>, runs: Receiver<Run>) {
    let mut g = match prepare() {
        Ok(g) => g,
        Err(why) => return host.tell(ToViewer::Done(Some(why))),
    };
    let why = load(&mut g, &mut host, config);
    host.tell(ToViewer::Done(why));
    while let Ok(run) = runs.recv() {
        host.cancel.store(false, Ordering::Relaxed);
        host.context = run.context;
        host.copied = 0;
        let function = match function_of(&mut g, &mut host, run.job) {
            Ok(Some(function)) => function,
            Ok(None) => {
                host.tell(ToViewer::Done(None));
                continue;
            }
            Err(why) => {
                host.tell(ToViewer::Done(Some(why)));
                continue;
            }
        };
        host.tell(ToViewer::Started {
            stop: g.interrupter(),
            limit: BINDING_TIME,
        });
        let (function, args) = function;
        let outcome = g.call(&mut host, &function, &args).err();
        // A run with no label (an answer from a list or prompt) says its
        // error alone, as fux does.
        let why = outcome.map(|e| match run.label.as_str() {
            "" => describe(&g, &e),
            label => format!("{label}: {}", describe(&g, &e)),
        });
        host.tell(ToViewer::Done(why));
    }
}

/// Reads the user's configuration, if there is one to run: `Err` says why
/// it is not used.
pub fn load_config() -> Result<Option<String>, String> {
    let Some(dir) = fuxix::file::config_dir() else {
        return Ok(None);
    };
    let path = dir.join("fux3").join("view.grift");
    match fux3::files::read_small(&path, MAX_CONFIG) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| format!("{} is not UTF-8", path.display())),
        Err(_) if matches!(std::fs::symlink_metadata(&path), Err(e) if e.kind() == std::io::ErrorKind::NotFound) => {
            Ok(None)
        }
        Err(failure) => Err(failure.0),
    }
}

/// Starts the thread.
pub fn start(
    config: Option<String>,
    fux3: PathBuf,
    session: Session,
    out: SyncSender<ToViewer>,
    poke: Poke,
) -> std::io::Result<Script> {
    let (sender, runs) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let host = Host {
        out,
        poke,
        kept: HashMap::new(),
        context: Context::default(),
        copied: 0,
        others: std::collections::HashSet::new(),
        generations: HashMap::new(),
        next_generation: 0,
        callback: None,
        next_callback: 0,
        cancel: Arc::clone(&cancel),
        fux3,
        session,
    };
    std::thread::Builder::new()
        .name("fux3-grift".to_owned())
        .stack_size(16 << 20)
        .spawn(move || thread(host, config, runs))?;
    Ok(Script {
        runs: sender,
        cancel,
    })
}
