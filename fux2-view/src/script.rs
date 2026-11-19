//! The configuration language's thread. Grift runs here, never on the
//! viewer's own: a binding that loops or waits holds only this thread, the
//! viewer stops it when its time is up, and it can detach at any moment.
//! What a binding does to a pane or the layout it does through the `fux2`
//! command, so there is one way to reach either and it can be cancelled.
//!
//! The words here are the primitives. The commands (split, move, zoom,
//! pickers, prompts) are grift in `library.grift`, and the keys are grift in
//! `defaults.grift`; the user's `view.grift`, loaded last, extends or
//! replaces any of them.
use fux2::layout::{Layout, Rect};
use fuxix::poll::Poke;
use grift::{Answer, Error, Grift, HostCall, HostFunction, Interrupter, Kept, Value};
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::time::{Duration, Instant};

pub const MAX_CONFIG: u64 = 64 << 10;
pub const MAX_BINDINGS: usize = 256;
const MAX_HEAP: usize = 16 << 20;
/// How long each part of the configuration may take to load.
pub const CONFIG_TIME: Duration = Duration::from_secs(2);
/// How long a binding may run.
pub const BINDING_TIME: Duration = Duration::from_secs(5);
/// The longest one `fux2` command of a binding may take.
const COMMAND_TIME: Duration = Duration::from_secs(4);
/// The most output one command gives a binding: a whole screen's text fits.
const MAX_OUTPUT: u64 = 8 << 20;
/// The most text a binding run copies out of the language, over all its words.
const MAX_COPIED: usize = 16 << 20;
/// How many numbers a new pane's name is tried with.
const MAX_NEW_NAMES: u32 = 128;
/// The most keys in one chord.
const MAX_CHORD_KEYS: usize = 4;

/// A viewer action, done on the viewer's own thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verb {
    NextPane,
    PreviousPane,
    LastPane,
    Focus(fux2::layout::Dir),
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
    /// Show a list; the value picked goes to callback `id`.
    Pick {
        id: u64,
        title: String,
        items: Vec<(String, String)>,
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
    /// A line for the status line.
    Say(String),
    /// A run begins; `stop` ends it, `limit` is its time.
    Started {
        stop: Interrupter,
        limit: Duration,
    },
    /// The run ended: what went wrong, if anything.
    Done(Option<String>),
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
    /// Callback `id` with what a panel gave.
    Answer { id: u64, value: String },
    /// Callback `id` is no longer wanted.
    Drop(u64),
    /// The function the configuration names `name`, with string arguments.
    Call { name: String, args: Vec<String> },
}

pub struct Run {
    /// What it is called on the status line.
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
    fux2: PathBuf,
    /// Bytes copied out of the language in this run.
    copied: usize,
    /// Chords bound to viewer actions or groups, which count too.
    others: std::collections::HashSet<String>,
    /// Each chord's binding, numbered: a run queued for an older one is
    /// refused out loud, never run with whatever replaced it.
    generations: HashMap<String, u64>,
    next_generation: u64,
    /// The one panel callback waiting, by number.
    callback: Option<(u64, Kept)>,
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

fn number_arg(g: &mut HostCall<'_, Host>, value: Value) -> Result<String, Error> {
    if let Value::Integer(n) = value {
        return Ok(n.to_string());
    }
    Err(g.type_error(value))
}

fn failed(g: &mut HostCall<'_, Host>, why: &str) -> Error {
    match g.string(why) {
        Ok(text) => g.error("fux2-error", &[text]),
        Err(error) => error,
    }
}

/// Reads all of `pipe`, at most [`MAX_OUTPUT`] bytes: past that, `None`.
fn read_all(pipe: Option<impl Read>) -> Option<String> {
    let mut text = Vec::new();
    if let Some(pipe) = pipe {
        let most = MAX_OUTPUT.saturating_add(1);
        let _ = pipe.take(most).read_to_end(&mut text);
    }
    let fits = u64::try_from(text.len()).is_ok_and(|len| len <= MAX_OUTPUT);
    fits.then(|| String::from_utf8_lossy(&text).into_owned())
}

/// Runs `fux2` with `args`, `input` on its stdin: its output. The viewer's
/// stop ends it, and so does running out of time.
fn command(host: &Host, args: &[OsString], input: Option<Vec<u8>>) -> Result<String, String> {
    let mut child = Command::new(&host.fux2)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", host.fux2.display()))?;
    let stdin = child.stdin.take();
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let helpers = (|| -> std::io::Result<_> {
        let thread = || std::thread::Builder::new().name("fux2-pipe".to_owned());
        if let (Some(mut stdin), Some(input)) = (stdin, input) {
            thread().spawn(move || {
                let _ = stdin.write_all(&input);
            })?;
        }
        let out = thread().spawn(move || read_all(stdout))?;
        let err = thread().spawn(move || read_all(stderr))?;
        Ok((out, err))
    })();
    let (out, err) = match helpers {
        Ok(helpers) => helpers,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("a thread: {e}"));
        }
    };
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
        let late = start.elapsed() >= COMMAND_TIME;
        if late || host.cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(if late { "it took too long" } else { "stopped" }.to_owned());
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let out = out.join().ok().flatten();
    let err = err.join().ok().flatten().unwrap_or_default();
    match (status.success(), out) {
        (true, Some(out)) => Ok(out),
        (true, None) => Err(format!("its output passed {MAX_OUTPUT} bytes")),
        (false, _) => Err(err.trim().to_owned()),
    }
}

fn run_word(
    g: &mut HostCall<'_, Host>,
    host: &Host,
    args: Vec<String>,
    input: Option<Vec<u8>>,
) -> Result<String, Error> {
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    match command(host, &args, input) {
        Ok(out) => Ok(out),
        Err(_) if host.cancel.load(Ordering::Relaxed) => Err(g.limit("interrupted", &[])),
        Err(why) => Err(failed(g, &why)),
    }
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
    use fux2::layout::Dir;
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

/// Keeps `function` as the one panel callback, letting go of any before.
fn keep_callback(
    g: &mut HostCall<'_, Host>,
    host: &mut Host,
    function: Value,
) -> Result<u64, Error> {
    let kept = g.keep(function)?;
    if let Some((_, old)) = host.callback.take() {
        let _ = g.release(old);
    }
    host.next_callback = host.next_callback.saturating_add(1);
    host.callback = Some((host.next_callback, kept));
    Ok(host.next_callback)
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
                tab.leaves().into_iter().map(|leaf| {
                    vec![
                        leaf.pane.to_string(),
                        format!("t{}", tab.id),
                        leaf.title.clone(),
                    ]
                })
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
        let [title, items, function] = *g.exactly(args)?;
        let title = string_arg(g, host, title)?;
        let mut list = Vec::new();
        for item in g.list_items(&items)? {
            let pair = g.list_items(&item)?;
            let (Some(value), Some(label)) = (pair.first(), pair.get(1)) else {
                return Err(failed(g, "each item is (value label)"));
            };
            list.push((string_arg(g, host, *value)?, string_arg(g, host, *label)?));
            if list.len() > crate::panel::MAX_ITEMS {
                return Err(failed(
                    g,
                    &format!("more than {} items", crate::panel::MAX_ITEMS),
                ));
            }
        }
        let id = keep_callback(g, host, function)?;
        host.tell(ToViewer::Pick {
            id,
            title,
            items: list,
        });
        inert()
    }),
    ("read-line", |g, host, args| {
        let [prompt, initial, function] = *g.exactly(args)?;
        let prompt = string_arg(g, host, prompt)?;
        let initial = string_arg(g, host, initial)?;
        let id = keep_callback(g, host, function)?;
        host.tell(ToViewer::Ask {
            id,
            prompt,
            initial,
        });
        inert()
    }),
    ("arrange", |g, host, args| {
        let mut words = vec!["layout".to_owned()];
        for arg in args {
            words.push(string_arg(g, host, *arg)?);
        }
        let area = host.context.area;
        words.push("--area".to_owned());
        words.push(format!("{}x{}", area.w.max(1), area.h.max(1)));
        let out = run_word(g, host, words, None);
        // Whatever happened, the layout is read again now.
        host.tell(ToViewer::Reread);
        text_answer(g, &out?)
    }),
    ("fit-after", |g, host, args| {
        let [text] = *g.exactly(args)?;
        let text = string_arg(g, host, text)?;
        let version = text
            .lines()
            .find_map(|line| line.strip_prefix("version "))
            .and_then(|v| v.parse::<u64>().ok());
        let Some(version) = version else {
            return Err(failed(g, "not what fux2 layout prints"));
        };
        let panes: Vec<String> = (text.lines())
            .filter_map(|line| line.strip_prefix("changed "))
            .map(str::to_owned)
            .collect();
        host.tell(ToViewer::Fit { version, panes });
        g.string(&text).map(Answer::Now)
    }),
    ("made", |g, host, args| {
        let [text] = *g.exactly(args)?;
        let text = string_arg(g, host, text)?;
        match text.lines().find_map(|line| line.strip_prefix("made ")) {
            Some(made) => g.string(made).map(Answer::Now),
            None => Err(failed(g, "the edit made nothing")),
        }
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
    ("fux2-program", |g, host, args| {
        let [] = *g.exactly(args)?;
        let path = host.fux2.to_string_lossy().into_owned();
        g.string(&path).map(Answer::Now)
    }),
    ("start-new", |g, host, args| {
        let Some((prefix, program)) = args.split_first() else {
            return Err(g.arity_error(args));
        };
        let prefix = string_arg(g, host, *prefix)?;
        let mut tail = Vec::new();
        if !program.is_empty() {
            tail.push("--".to_owned());
        }
        for word in program {
            tail.push(string_arg(g, host, *word)?);
        }
        for number in 1..=MAX_NEW_NAMES {
            let name = format!("{prefix}-{number}");
            let mut words = vec!["start".to_owned(), "--name".to_owned(), name.clone()];
            words.extend(tail.iter().cloned());
            let os: Vec<OsString> = words.into_iter().map(Into::into).collect();
            match command(host, &os, None) {
                // `NAME ID@VERSION`: the pane as this one life of it.
                Ok(out) => {
                    let id = out
                        .split_whitespace()
                        .nth(1)
                        .and_then(|t| t.split('@').next());
                    let pane = id.map_or_else(|| name.clone(), |id| format!("{name}@{id}"));
                    return g.string(&pane).map(Answer::Now);
                }
                Err(why) if why.contains("exists:") => {}
                Err(_) if host.cancel.load(Ordering::Relaxed) => {
                    return Err(g.limit("interrupted", &[]));
                }
                Err(why) => return Err(failed(g, &why)),
            }
        }
        Err(failed(g, &format!("no free name {prefix}-N")))
    }),
    ("send", |g, host, args| {
        let [pane, text] = *g.exactly(args)?;
        let pane = string_arg(g, host, pane)?;
        let text = string_arg(g, host, text)?;
        let args = vec!["send".to_owned(), pane, "--stdin".to_owned()];
        run_word(g, host, args, Some(text.into_bytes()))?;
        inert()
    }),
    ("send-key", |g, host, args| {
        let [pane, key] = *g.exactly(args)?;
        let args = vec![
            "send".to_owned(),
            string_arg(g, host, pane)?,
            "--key".to_owned(),
            string_arg(g, host, key)?,
        ];
        run_word(g, host, args, None)?;
        inert()
    }),
    ("read", |g, host, args| {
        let [pane] = *g.exactly(args)?;
        let args = vec!["read".to_owned(), string_arg(g, host, pane)?];
        let out = run_word(g, host, args, None)?;
        text_answer(g, &out)
    }),
    ("ls", |g, host, args| {
        let [] = *g.exactly(args)?;
        let out = run_word(g, host, vec!["ls".to_owned()], None)?;
        let names: Vec<String> = out.lines().map(str::to_owned).collect();
        strings(g, &names).map(Answer::Now)
    }),
    ("start", |g, host, args| {
        let Some((name, program)) = args.split_first() else {
            return Err(g.arity_error(args));
        };
        let mut words = vec![
            "start".to_owned(),
            "--name".to_owned(),
            string_arg(g, host, *name)?,
        ];
        if !program.is_empty() {
            words.push("--".to_owned());
        }
        for word in program {
            words.push(string_arg(g, host, *word)?);
        }
        let out = run_word(g, host, words, None)?;
        text_answer(g, &out)
    }),
    ("resize", |g, host, args| {
        let [pane, cols, rows] = *g.exactly(args)?;
        let args = vec![
            "resize".to_owned(),
            string_arg(g, host, pane)?,
            number_arg(g, cols)?,
            number_arg(g, rows)?,
        ];
        run_word(g, host, args, None)?;
        inert()
    }),
    ("signal", |g, host, args| {
        let [pane, name] = *g.exactly(args)?;
        let args = vec![
            "signal".to_owned(),
            string_arg(g, host, pane)?,
            string_arg(g, host, name)?,
        ];
        run_word(g, host, args, None)?;
        inert()
    }),
    ("wait", |g, host, args| {
        let [pane, token, ms] = *g.exactly(args)?;
        let args = vec![
            "wait".to_owned(),
            string_arg(g, host, pane)?,
            "--after".to_owned(),
            string_arg(g, host, token)?,
            "--ms".to_owned(),
            number_arg(g, ms)?,
        ];
        let out = run_word(g, host, args, None)?;
        text_answer(g, &out)
    }),
    ("kill", |g, host, args| {
        let [pane] = *g.exactly(args)?;
        let pane = string_arg(g, host, pane)?;
        run_word(g, host, vec!["kill".to_owned(), pane], None)?;
        inert()
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
        Job::Answer { id, value } => {
            let Some((own, function)) = host.callback.take() else {
                return Ok(None);
            };
            if own != id {
                host.callback = Some((own, function));
                return Ok(None);
            }
            let function_value = kept(g, &function);
            let _ = g.release(function);
            let value = g.string(&value).map_err(|e| describe(g, &e))?;
            Ok(Some((function_value?, vec![value])))
        }
        Job::Drop(id) => {
            if let Some((own, function)) = host.callback.take() {
                match own == id {
                    true => drop(g.release(function)),
                    false => host.callback = Some((own, function)),
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
        let why = outcome.map(|e| format!("{}: {}", run.label, describe(&g, &e)));
        host.tell(ToViewer::Done(why));
    }
}

/// Reads the user's configuration, if there is one to run: `Err` says why
/// it is not used.
pub fn load_config() -> Result<Option<String>, String> {
    let Some(dir) = fuxix::file::config_dir() else {
        return Ok(None);
    };
    let path = dir.join("fux2").join("view.grift");
    match fux2::layout::read_small_file(&path, MAX_CONFIG) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Starts the thread.
pub fn start(
    config: Option<String>,
    fux2: PathBuf,
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
        fux2,
    };
    std::thread::Builder::new()
        .name("fux2-grift".to_owned())
        .stack_size(16 << 20)
        .spawn(move || thread(host, config, runs))?;
    Ok(Script {
        runs: sender,
        cancel,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{Action, Binding, Group, Keys, Line};

    /// Loads the library, the default keys and `config` as a viewer would,
    /// and gives the key table they made.
    fn loaded(config: &str) -> Result<(Keys, Option<String>), Box<dyn std::error::Error>> {
        let (_wake, poke) = fuxix::poll::waker()?;
        let (out, from) = std::sync::mpsc::sync_channel(1024);
        let _script = start(Some(config.to_owned()), PathBuf::from("fux2"), out, poke)?;
        let mut keys = Keys::default();
        loop {
            match from.recv_timeout(Duration::from_secs(20))? {
                ToViewer::Bind {
                    chord,
                    caption,
                    heading,
                    generation,
                } => {
                    let action = Action::Script(generation);
                    let order = generation;
                    let binding = Binding {
                        action,
                        caption,
                        heading,
                        order,
                    };
                    keys.bindings.insert(chord, binding);
                }
                ToViewer::BindVerb {
                    chord,
                    caption,
                    heading,
                    generation,
                    verb,
                } => {
                    let binding = Binding {
                        action: Action::Verb(verb),
                        caption,
                        heading,
                        order: generation,
                    };
                    keys.bindings.insert(chord, binding);
                }
                ToViewer::Group {
                    chord,
                    caption,
                    heading,
                    repeat,
                    generation,
                } => {
                    let group = Group {
                        caption,
                        heading,
                        repeat,
                        order: generation,
                    };
                    keys.groups.insert(chord, group);
                }
                ToViewer::Done(why) => return Ok((keys, why)),
                ToViewer::Unbind(_)
                | ToViewer::Verb(_)
                | ToViewer::Focus(_)
                | ToViewer::Pick { .. }
                | ToViewer::Ask { .. }
                | ToViewer::Reread
                | ToViewer::Fit { .. }
                | ToViewer::Say(_)
                | ToViewer::Started { .. } => {}
            }
        }
    }

    /// Every default key has a caption and a heading and is in the menu of
    /// the keys before it, and so is one the configuration adds.
    #[test]
    fn every_key_is_in_a_menu_with_its_caption() -> Result<(), Box<dyn std::error::Error>> {
        let config = r#"(bind-key "C-b t y" "my tab thing" "Mine" (lambda! () 1))"#;
        let (keys, why) = loaded(config)?;
        assert_eq!(why, None);
        assert!(keys.bindings.len() > 50, "{}", keys.bindings.len());
        for (chord, binding) in &keys.bindings {
            assert!(!binding.caption.trim().is_empty() && !binding.heading.trim().is_empty());
            let Some((parent, key)) = chord.rsplit_once(' ') else {
                continue;
            };
            let listed = keys
                .menu(parent)
                .into_iter()
                .any(|line| line == Line::Key(key.to_owned(), binding.caption.clone(), false));
            assert!(listed, "{chord} is not in the menu of {parent}");
        }
        let menu = keys.menu("C-b t");
        assert!(menu.contains(&Line::Heading("Mine".to_owned())), "{menu:?}");
        // Without a caption, a binding is refused out loud.
        let (_, why) = loaded(r#"(bind-key "C-b y" "" "Mine" (lambda! () 1))"#)?;
        assert!(why.is_some_and(|why| why.contains("caption")));
        Ok(())
    }
}
