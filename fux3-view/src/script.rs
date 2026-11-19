//! The configuration language's thread. Grift runs here, never on the
//! viewer's own: a binding that loops or waits holds only this thread, the
//! viewer stops it when its time is up, and it can detach at any moment.
//! What a binding does to a pane it does through the pane's files; to the
//! layout, through the layout command grammar `--do` takes.
//!
//! The words here are the primitives. The commands (split, move, zoom,
//! pickers, prompts) are grift in `library.grift`, and the keys are grift in
//! `defaults.grift`; the user's `view.grift`, loaded last, extends or
//! replaces any of them.
use crate::keys::{Action, Binding, Group};
use crate::layout::{Dir, Layout, Rect};
use crate::panel::{Ask, List, Panel, Row};
use fux3::session::Workspace;
use fuxix::poll::Poke;
use grift::{Answer, Error, Grift, HostCall, HostFunction, Interrupter, Kept, Value};
use std::collections::HashMap;
use std::fs::File;
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
/// The most panes whose going a callback waits for, and the most
/// workspaces held at once.
const MAX_WATCHED: usize = 64;
/// The most keys in one chord.
const MAX_CHORD_KEYS: usize = 4;

/// A viewer action, done on the viewer's own thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verb {
    NextPane,
    LastPane,
    Focus(Dir),
    NextTab,
    PreviousTab,
    NextWorkspace,
    PreviousWorkspace,
    /// Show the tab with this id.
    ShowTab(u32),
    /// Show the workspace of this name.
    ShowWorkspace(String),
    Fit,
    Detach,
    /// Does what this chord is bound to.
    Chord(String),
}

/// The commands every viewer starts with, then its keys. Both are ordinary
/// grift: a configuration may redefine, unbind or replace any of them.
const LIBRARY: &str = include_str!("library.grift");
const DEFAULTS: &str = include_str!("defaults.grift");

pub enum ToViewer {
    /// A key's binding: its function runs on this thread, or its verb at
    /// once on the viewer's own (so it works while a binding runs).
    Bind(String, Binding),
    /// A key that holds other keys.
    Group(String, Group),
    Unbind(String),
    Verb(Verb),
    /// Focus this pane, showing its tab.
    Focus(String),
    /// Open a list, or a line to type: what it gives goes to its callback.
    Panel(Panel),
    /// The layout changed: read it now.
    Reread,
    /// This viewer's close of the workspace whose directory has this id
    /// (`Workspace::id`) has begun, or (true) has removed it. One that
    /// failed says `Reread` instead.
    Closing(String, bool),
    /// Run `Job::Gone` for this pane (`NAME`, or one life `NAME@ID`) once
    /// it is gone from the workspace shown.
    Watch(String),
    /// Resize these panes to their tiles (the layout was just read again).
    Fit(Vec<String>),
    /// A line for the bar's corner.
    Say(String),
    /// Bytes for a pane's program, after any keys typed before them.
    Type(String, Vec<u8>),
    /// A run begins: what stops it, and its time.
    Started(Interrupter, Duration),
    /// The run ended: what went wrong, if anything.
    Done(Option<String>),
    /// The configuration loaded (it is no run): what went wrong, if
    /// anything.
    Loaded(Option<String>),
}

/// What the viewer shows, as a run sees it.
#[derive(Clone)]
pub struct Context {
    /// The pane focused, as `NAME@ID` (`NAME@`: a bare tile with no pane).
    pub focus: Option<String>,
    pub tab: Option<u32>,
    /// The workspace shown, its directory held: every word acts on that
    /// directory, whatever has its name later.
    pub workspace: Workspace,
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
    /// The pane `when-gone` named is gone: its callback, once.
    Gone(String),
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
    /// The workspaces `hold-workspace` was asked for, newest last.
    held: Vec<Workspace>,
    /// What `when-gone` calls, by pane.
    gone: HashMap<String, Kept>,
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

/// A pane that is gone: its directory went, or another life has its name.
/// Its own kind, `pane-gone`, so a command can ignore that and nothing else.
fn gone(g: &mut HostCall<'_, Host>, pane: &str) -> Error {
    match g.string(&format!("pane {pane} is gone")) {
        Ok(text) => g.error("pane-gone", &[text]),
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

/// The directory, held open, of the pane `NAME` or `NAME@ID` names in the
/// workspace shown, refused (`pane-gone`) if that life of it is over.
fn pane_dir(g: &mut HostCall<'_, Host>, host: &Host, pane: &str) -> Result<File, Error> {
    let target = crate::layout::Target::parse(pane).map_err(|f| failed(g, &f.0))?;
    let Ok(dir) = host.context.workspace.pane(&target.name) else {
        return Err(gone(g, pane));
    };
    if let Some(id) = &target.id {
        let status = crate::panes::read_status(&dir).unwrap_or_default();
        if fux3::files::status_value(&status, "id") != Some(id.as_str()) {
            return Err(gone(g, pane));
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

fn named_verb(word: &str) -> Option<Verb> {
    Some(match word {
        "next-pane" => Verb::NextPane,
        "last-pane" => Verb::LastPane,
        "focus-left" => Verb::Focus(Dir::Left),
        "focus-right" => Verb::Focus(Dir::Right),
        "focus-up" => Verb::Focus(Dir::Up),
        "focus-down" => Verb::Focus(Dir::Down),
        "next-tab" => Verb::NextTab,
        "previous-tab" => Verb::PreviousTab,
        "next-workspace" => Verb::NextWorkspace,
        "previous-workspace" => Verb::PreviousWorkspace,
        "fit" => Verb::Fit,
        "detach" => Verb::Detach,
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
    match (word.as_str(), number) {
        ("show-tab", Some(tab)) => {
            let text = string_arg(g, host, tab)?;
            match text.strip_prefix('t').and_then(|d| d.parse::<u32>().ok()) {
                Some(id) => Ok(Verb::ShowTab(id)),
                None => Err(failed(g, &format!("{text:?} is not an id tN"))),
            }
        }
        ("show-workspace", Some(space)) => {
            let name = string_arg(g, host, space)?;
            fux3::session::check_name(&name).map_err(|f| failed(g, &f.0))?;
            Ok(Verb::ShowWorkspace(name))
        }
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

/// A binding's keys, caption and heading: the keys in their one spelling,
/// the caption and heading each a non-empty string.
fn head(
    g: &mut HostCall<'_, Host>,
    host: &mut Host,
    words: [Value; 3],
) -> Result<(String, String, String), Error> {
    let [key, caption, heading] = words;
    let text = string_arg(g, host, key)?;
    let chord = chord(&text).map_err(|why| failed(g, &why))?;
    let (caption, heading) = (string_arg(g, host, caption)?, string_arg(g, host, heading)?);
    if caption.trim().is_empty() || heading.trim().is_empty() {
        return Err(failed(g, "a binding needs a caption and a heading"));
    }
    Ok((chord, caption, heading))
}

/// Numbers `chord`'s binding to something the viewer does itself (a verb,
/// a group's menu), letting go of any function it ran before.
fn bind_other(g: &mut HostCall<'_, Host>, host: &mut Host, chord: &str) -> Result<u64, Error> {
    let generation = host.rebind(chord).map_err(|why| failed(g, &why))?;
    if let Some(old) = host.kept.remove(chord) {
        let _ = g.release(old);
    }
    host.others.insert(chord.to_owned());
    Ok(generation)
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
    for old in (host.callback.take().into_iter()).flat_map(|(_, old)| old) {
        let _ = g.release(old);
    }
    host.next_callback = host.next_callback.saturating_add(1);
    host.callback = Some((host.next_callback, kept));
    Ok(host.next_callback)
}

/// `start-new PREFIX [PROGRAM…]`: a pane named `PREFIX-N`, the first N
/// free; `start-once` the same with no recipe written, for a pane nobody
/// starts again.
fn start_new(
    g: &mut HostCall<'_, Host>,
    host: &mut Host,
    args: &[Value],
    recipe: bool,
) -> Result<Answer, Error> {
    let Some((prefix, rest)) = args.split_first() else {
        return Err(g.arity_error(args));
    };
    let prefix = string_arg(g, host, *prefix)?;
    let words = program(g, host, rest)?;
    // `WORKSPACE/PREFIX` starts it in that workspace, made if missing.
    let (workspace, prefix) = match prefix.split_once('/') {
        Some((space, prefix)) => (
            host.context.workspace.session.workspace(space, true),
            prefix,
        ),
        None => (Ok(host.context.workspace.clone()), &*prefix),
    };
    let workspace = workspace.map_err(|f| failed(g, &f.0))?;
    let program = (&*words, recipe);
    match crate::command::start_new(&host.fux3, &workspace, prefix, program, &host.cancel) {
        Ok(pane) => g.string(&pane.to_string()).map(Answer::Now),
        Err(failure) => Err(refused(g, host, &failure)),
    }
}

const WORDS: &[(&str, Word)] = &[
    ("bind-key", |g, host, args| {
        let [key, caption, heading, function] = *g.exactly(args)?;
        let (chord, caption, heading) = head(g, host, [key, caption, heading])?;
        // Kept first: a binding that can't be made leaves the old one whole.
        let kept = g.keep(function)?;
        let order = match host.rebind(&chord) {
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
        let action = Action::Script(order);
        host.tell(ToViewer::Bind(
            chord,
            Binding {
                action,
                caption,
                heading,
                order,
            },
        ));
        inert()
    }),
    ("bind-verb", |g, host, args| {
        let Some((words, rest)) = args.split_first_chunk::<3>() else {
            return Err(g.arity_error(args));
        };
        let (chord, caption, heading) = head(g, host, *words)?;
        let action = Action::Verb(verb_of(g, host, rest)?);
        let order = bind_other(g, host, &chord)?;
        host.tell(ToViewer::Bind(
            chord,
            Binding {
                action,
                caption,
                heading,
                order,
            },
        ));
        inert()
    }),
    ("bind-group", |g, host, args| {
        let [key, caption, heading] = *g.exactly(args)?;
        let (chord, caption, heading) = head(g, host, [key, caption, heading])?;
        let order = bind_other(g, host, &chord)?;
        host.tell(ToViewer::Group(
            chord,
            Group {
                caption,
                heading,
                order,
            },
        ));
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
        let name = host.context.workspace.name.clone();
        g.string(&name).map(Answer::Now)
    }),
    ("workspaces", |g, host, args| {
        let [] = *g.exactly(args)?;
        let session = &host.context.workspace.session;
        let names = session.workspaces().map_err(|f| failed(g, &f.0))?;
        strings(g, &names).map(Answer::Now)
    }),
    ("tabs", |g, host, args| {
        let [] = *g.exactly(args)?;
        let layout = Arc::clone(&host.context.layout);
        let items: Vec<Vec<String>> = (layout.tabs.iter())
            .map(|tab| vec![format!("t{}", tab.id), tab.title.clone()])
            .collect();
        rows(g, &items)
    }),
    ("panes", |g, host, args| {
        let [] = *g.exactly(args)?;
        let layout = Arc::clone(&host.context.layout);
        let items: Vec<Vec<String>> = (layout.tabs.iter())
            .flat_map(|tab| {
                (tab.leaves().into_iter())
                    .map(|p| vec![format!("{}@{}", p.name, p.id), format!("t{}", tab.id)])
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
        let (mut rows, mut selected) = (Vec::new(), None);
        for item in g.list_items(&items)? {
            let parts = g.list_items(&item)?;
            let (Some(value), Some(label)) = (parts.first(), parts.get(1)) else {
                return Err(failed(
                    g,
                    "each row is (value label), then #true for where you are, or \"selected\"",
                ));
            };
            let current = matches!(parts.get(2), Some(Value::Boolean(true)));
            let marked = match parts.get(2) {
                Some(other) => current || g.as_str(other)? == Some("selected"),
                None => false,
            };
            if marked && selected.is_none() {
                selected = Some(rows.len());
            }
            let (value, label) = (string_arg(g, host, *value)?, string_arg(g, host, *label)?);
            rows.push(Row {
                value,
                label,
                current,
            });
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
        let selected = selected.unwrap_or(0);
        let actions = keys;
        host.tell(ToViewer::Panel(Panel::List(List {
            id,
            title,
            rows,
            actions,
            selected,
        })));
        inert()
    }),
    ("read-line", |g, host, args| {
        let [prompt, initial, function] = *g.exactly(args)?;
        let prompt = string_arg(g, host, prompt)?;
        let initial = string_arg(g, host, initial)?;
        let id = keep_callbacks(g, host, &[function])?;
        let text: Vec<char> = initial.chars().collect();
        let cursor = text.len();
        host.tell(ToViewer::Panel(Panel::Ask(Ask {
            id,
            prompt,
            text,
            cursor,
        })));
        inert()
    }),
    ("arrange", |g, host, args| {
        let [line] = *g.exactly(args)?;
        let line = string_arg(g, host, line)?;
        let area = host.context.area;
        let outcome = crate::command::run_line(&host.context.workspace, &line, area, &host.cancel);
        // Whatever happened, the layout is read again now.
        host.tell(ToViewer::Reread);
        match outcome {
            Ok(edited) => g.string(edited.to_string().trim_end()).map(Answer::Now),
            Err(failure) => Err(refused(g, host, &failure)),
        }
    }),
    ("fit-after", |g, host, args| {
        let [text] = *g.exactly(args)?;
        let text = string_arg(g, host, text)?;
        let panes = (text.lines()).filter_map(|line| line.strip_prefix("changed "));
        host.tell(ToViewer::Fit(panes.map(str::to_owned).collect()));
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
    ("when-gone", |g, host, args| {
        let [pane, function] = *g.exactly(args)?;
        let pane = string_arg(g, host, pane)?;
        let pane = crate::layout::Target::parse(&pane).map_err(|f| failed(g, &f.0))?;
        let pane = pane.to_string();
        if host.gone.len() >= MAX_WATCHED && !host.gone.contains_key(&pane) {
            return Err(failed(g, &format!("more than {MAX_WATCHED} panes watched")));
        }
        let kept = g.keep(function)?;
        if let Some(old) = host.gone.insert(pane.clone(), kept) {
            let _ = g.release(old);
        }
        host.tell(ToViewer::Watch(pane));
        inert()
    }),
    ("workspace-dir", |g, host, args| {
        let [] = *g.exactly(args)?;
        let now = crate::command::named(&host.context.workspace).map_err(|f| failed(g, &f.0))?;
        let path = now.dir.to_string_lossy().into_owned();
        g.string(&path).map(Answer::Now)
    }),
    ("start-new", |g, host, args| start_new(g, host, args, true)),
    ("start-once", |g, host, args| {
        start_new(g, host, args, false)
    }),
    ("hold-workspace", |g, host, args| {
        let [name] = *g.exactly(args)?;
        let name = string_arg(g, host, name)?;
        // The one shown is held already; another is opened now, once.
        let shown = &host.context.workspace;
        let workspace = match shown.name == name {
            true => shown.clone(),
            false => (shown.session.workspace(&name, false)).map_err(|f| failed(g, &f.0))?,
        };
        let id = workspace.id();
        // The newest last; past the bound the oldest is let go.
        host.held.retain(|w| w.id() != id);
        host.held.push(workspace);
        let extra = host.held.len().saturating_sub(MAX_WATCHED);
        host.held = std::mem::take(&mut host.held)
            .into_iter()
            .skip(extra)
            .collect();
        g.string(&id).map(Answer::Now)
    }),
    ("close-workspace", |g, host, args| {
        let [held] = *g.exactly(args)?;
        let held = string_arg(g, host, held)?;
        // Only a directory held is closed: a name is never opened here, as
        // another workspace may have it by now.
        let Some(workspace) = host.held.iter().find(|w| w.id() == held).cloned() else {
            let why = format!("{held:?} is no workspace held: (hold-workspace NAME) gives one");
            return Err(failed(g, &why));
        };
        host.tell(ToViewer::Closing(held.clone(), false));
        let outcome = crate::command::close_workspace(&workspace, &host.cancel);
        host.tell(match outcome {
            Ok(()) => ToViewer::Closing(held, true),
            Err(_) => ToViewer::Reread,
        });
        match outcome {
            Ok(()) => inert(),
            Err(failure) => Err(refused(g, host, &failure)),
        }
    }),
    ("send", |g, host, args| {
        let [pane, text] = *g.exactly(args)?;
        let pane = string_arg(g, host, pane)?;
        pane_dir(g, host, &pane)?;
        let bytes = string_arg(g, host, text)?.into_bytes();
        host.tell(ToViewer::Type(pane, bytes));
        inert()
    }),
    ("ctl", |g, host, args| {
        let [pane, line] = *g.exactly(args)?;
        let pane = string_arg(g, host, pane)?;
        let line = string_arg(g, host, line)?;
        let dir = pane_dir(g, host, &pane)?;
        match crate::panes::ctl_line(&dir, &line) {
            Ok(()) => inert(),
            Err(_) if fux3::session::place(&dir).is_none() => Err(gone(g, &pane)),
            Err(failure) => Err(failed(g, &failure.0)),
        }
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
        host.tell(ToViewer::Started(g.interrupter(), CONFIG_TIME));
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
        Job::Gone(pane) => {
            let Some(function) = host.gone.remove(&pane) else {
                return Ok(None);
            };
            let called = kept(g, &function);
            let _ = g.release(function);
            Ok(Some((called?, Vec::new())))
        }
    }
}

fn thread(mut host: Host, config: Option<String>, runs: Receiver<Run>) {
    let mut g = match prepare() {
        Ok(g) => g,
        Err(why) => return host.tell(ToViewer::Loaded(Some(why))),
    };
    let why = load(&mut g, &mut host, config);
    host.tell(ToViewer::Loaded(why));
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
        host.tell(ToViewer::Started(g.interrupter(), BINDING_TIME));
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
    workspace: Workspace,
    out: SyncSender<ToViewer>,
    poke: Poke,
) -> std::io::Result<Script> {
    let (sender, runs) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let host = Host {
        out,
        poke,
        kept: HashMap::new(),
        context: Context {
            focus: None,
            tab: None,
            workspace: workspace.clone(),
            area: Rect::default(),
            layout: Arc::default(),
            keys: Arc::default(),
        },
        copied: 0,
        others: std::collections::HashSet::new(),
        generations: HashMap::new(),
        next_generation: 0,
        callback: None,
        next_callback: 0,
        held: Vec::new(),
        gone: HashMap::new(),
        cancel: Arc::clone(&cancel),
        fux3,
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
