//! The configuration language's thread. Grift runs here, never on the
//! viewer's own: a binding that loops or waits holds only this thread, the
//! viewer stops it when its time is up, and it can detach at any moment.
//! What a binding does to a pane it does through the `fux2` command, so
//! there is one way to reach a pane and it can be cancelled.
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
/// How long the configuration may take to load.
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

/// A viewer action a binding asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    NextPane,
    PreviousPane,
    NextTab,
    PreviousTab,
    LastTab,
    /// The tab numbered from 1.
    GoToTab(usize),
    NextWorkspace,
    PreviousWorkspace,
    LastWorkspace,
    Fit,
    Detach,
    /// Types the binding's first key itself: a prefix sent through.
    Literal,
}

/// The keys every viewer starts with. They are ordinary bindings: a
/// configuration may unbind or replace any of them.
const DEFAULTS: &str = include_str!("defaults.grift");

pub enum ToViewer {
    /// The chord runs its function on this thread, under this number.
    Bind(String, u64),
    /// The chord does this at once on the viewer's own thread, so it works
    /// while a binding runs.
    BindVerb(String, Verb),
    Unbind(String),
    Verb(Verb),
    /// Focus this pane once it is there, and come back when it goes.
    Focus(String),
    /// A run begins; `stop` ends it, `limit` is its time.
    Started {
        stop: Interrupter,
        limit: Duration,
    },
    /// The run ended: what went wrong, if anything.
    Done(Option<String>),
}

pub struct Run {
    pub chord: String,
    /// The binding's number when its key was pressed.
    pub generation: u64,
    /// The pane focused when the key was pressed, as `name@id`.
    pub focus: Option<String>,
    /// The labels a new pane needs to appear in the shown tab, or why no
    /// labels can put it there.
    pub labels: Result<Vec<(String, String)>, String>,
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
    focus: Option<String>,
    labels: Result<Vec<(String, String)>, String>,
    cancel: Arc<AtomicBool>,
    fux2: PathBuf,
    /// Bytes copied out of the language in this run.
    copied: usize,
    /// Chords bound to viewer actions, which count toward the limit too.
    verbs: std::collections::HashSet<String>,
    /// Each chord's binding, numbered: a run queued for an older one is
    /// refused out loud, never run with whatever replaced it.
    generations: HashMap<String, u64>,
    next_generation: u64,
}

impl Host {
    /// Room for a binding on `chord`, and the number its new binding gets.
    fn rebind(&mut self, chord: &str) -> Result<u64, String> {
        let known = self.kept.contains_key(chord) || self.verbs.contains(chord);
        if !known && self.kept.len().saturating_add(self.verbs.len()) >= MAX_BINDINGS {
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

/// A key or `prefix key` in its one spelling, or why it is none.
pub fn chord(text: &str) -> Result<String, String> {
    let keys: Vec<&str> = text.split_whitespace().collect();
    if keys.is_empty() || keys.len() > 2 {
        return Err(format!(
            "{text:?}: a binding is one key, or a prefix and a key"
        ));
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

fn tab_number(g: &mut HostCall<'_, Host>, value: Value) -> Result<usize, Error> {
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

fn inert() -> Result<Answer, Error> {
    Ok(Answer::Now(Value::Inert))
}

fn text_answer(g: &mut HostCall<'_, Host>, text: &str) -> Result<Answer, Error> {
    g.string(text.trim_end()).map(Answer::Now)
}

fn verb(host: &Host, verb: Verb) -> Result<Answer, Error> {
    host.tell(ToViewer::Verb(verb));
    inert()
}

const WORDS: &[(&str, Word)] = &[
    ("bind-key", |g, host, args| {
        let [key, function] = *g.exactly(args)?;
        let text = string_arg(g, host, key)?;
        let name = chord(&text).map_err(|why| failed(g, &why))?;
        let generation = host.rebind(&name).map_err(|why| failed(g, &why))?;
        let kept = g.keep(function)?;
        host.verbs.remove(&name);
        if let Some(old) = host.kept.insert(name.clone(), kept) {
            let _ = g.release(old);
        }
        host.tell(ToViewer::Bind(name, generation));
        inert()
    }),
    ("unbind-key", |g, host, args| {
        let [key] = *g.exactly(args)?;
        let text = string_arg(g, host, key)?;
        let name = chord(&text).map_err(|why| failed(g, &why))?;
        if let Some(old) = host.kept.remove(&name) {
            let _ = g.release(old);
        }
        host.verbs.remove(&name);
        host.generations.remove(&name);
        host.tell(ToViewer::Unbind(name));
        inert()
    }),
    ("focused", |g, host, args| {
        let [] = *g.exactly(args)?;
        match host.focus.clone() {
            Some(pane) => g.string(&pane).map(Answer::Now),
            None => Err(failed(g, "no pane is focused")),
        }
    }),
    ("next-pane", |g, host, args| {
        let [] = *g.exactly(args)?;
        verb(host, Verb::NextPane)
    }),
    ("previous-pane", |g, host, args| {
        let [] = *g.exactly(args)?;
        verb(host, Verb::PreviousPane)
    }),
    ("next-tab", |g, host, args| {
        let [] = *g.exactly(args)?;
        verb(host, Verb::NextTab)
    }),
    ("previous-tab", |g, host, args| {
        let [] = *g.exactly(args)?;
        verb(host, Verb::PreviousTab)
    }),
    ("last-tab", |g, host, args| {
        let [] = *g.exactly(args)?;
        verb(host, Verb::LastTab)
    }),
    ("next-workspace", |g, host, args| {
        let [] = *g.exactly(args)?;
        verb(host, Verb::NextWorkspace)
    }),
    ("previous-workspace", |g, host, args| {
        let [] = *g.exactly(args)?;
        verb(host, Verb::PreviousWorkspace)
    }),
    ("last-workspace", |g, host, args| {
        let [] = *g.exactly(args)?;
        verb(host, Verb::LastWorkspace)
    }),
    ("fit", |g, host, args| {
        let [] = *g.exactly(args)?;
        verb(host, Verb::Fit)
    }),
    ("detach", |g, host, args| {
        let [] = *g.exactly(args)?;
        verb(host, Verb::Detach)
    }),
    ("go-to-tab", |g, host, args| {
        let [number] = *g.exactly(args)?;
        let number = tab_number(g, number)?;
        verb(host, Verb::GoToTab(number))
    }),
    ("bind-verb", |g, host, args| {
        let (key, word, number) = match args {
            [key, word] => (*key, *word, None),
            [key, word, number] => (*key, *word, Some(*number)),
            _ => return Err(g.arity_error(args)),
        };
        let chord_text = string_arg(g, host, key)?;
        let name = chord(&chord_text).map_err(|why| failed(g, &why))?;
        let word = string_arg(g, host, word)?;
        let verb = match (word.as_str(), number) {
            ("go-to-tab", Some(number)) => Verb::GoToTab(tab_number(g, number)?),
            (word, None) => match named_verb(word) {
                Some(verb) => verb,
                None => return Err(failed(g, &format!("no verb {word}"))),
            },
            (_, Some(_)) => return Err(g.arity_error(args)),
        };
        host.rebind(&name).map_err(|why| failed(g, &why))?;
        if let Some(old) = host.kept.remove(&name) {
            let _ = g.release(old);
        }
        host.verbs.insert(name.clone());
        host.tell(ToViewer::BindVerb(name, verb));
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
    ("start-here", |g, host, args| {
        let Some((prefix, program)) = args.split_first() else {
            return Err(g.arity_error(args));
        };
        let prefix = string_arg(g, host, *prefix)?;
        let labels = host.labels.clone().map_err(|why| failed(g, &why))?;
        let mut tail = Vec::new();
        for (key, value) in labels {
            tail.push("--label".to_owned());
            tail.push(format!("{key}={value}"));
        }
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
        let mut names = Vec::new();
        for line in out.lines() {
            names.push(g.string(line)?);
        }
        g.list(&names).map(Answer::Now)
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

fn prepare(host: &mut Host) -> Result<Grift<Host>, String> {
    let mut g: Grift<Host> = Grift::new().map_err(|_| "no memory for grift".to_owned())?;
    g.set_memory_limit(Some(MAX_HEAP));
    for (name, word) in WORDS {
        let made = g.host_function(name, *word).and_then(|f| g.define(name, f));
        if let Err(error) = made {
            return Err(describe(&g, &error));
        }
    }
    let _ = host;
    Ok(g)
}

fn thread(mut host: Host, config: Option<String>, runs: Receiver<Run>) {
    let mut g = match prepare(&mut host) {
        Ok(g) => g,
        Err(why) => return host.tell(ToViewer::Done(Some(why))),
    };
    host.tell(ToViewer::Started {
        stop: g.interrupter(),
        limit: CONFIG_TIME,
    });
    let mut why = g
        .evaluate(&mut host, DEFAULTS)
        .err()
        .map(|e| format!("the default keys: {}", describe(&g, &e)));
    if let Some(text) = config.filter(|_| why.is_none()) {
        host.tell(ToViewer::Started {
            stop: g.interrupter(),
            limit: CONFIG_TIME,
        });
        why = g
            .evaluate(&mut host, &text)
            .err()
            .map(|e| format!("view.grift: {}", describe(&g, &e)));
    }
    host.tell(ToViewer::Done(why));
    while let Ok(run) = runs.recv() {
        host.cancel.store(false, Ordering::Relaxed);
        host.focus = run.focus;
        host.labels = run.labels;
        host.copied = 0;
        let current = host.generations.get(&run.chord) == Some(&run.generation);
        if !current {
            let why = format!("{} was rebound before it ran, and did not run", run.chord);
            host.tell(ToViewer::Done(Some(why)));
            continue;
        }
        let function = host.kept.get(&run.chord).map(|kept| g.kept(kept));
        host.tell(ToViewer::Started {
            stop: g.interrupter(),
            limit: BINDING_TIME,
        });
        let outcome = match function {
            Some(Ok(function)) => g.call(&mut host, &function, &[]).err(),
            Some(Err(error)) => Some(error),
            None => None,
        };
        let why = outcome.map(|e| format!("{}: {}", run.chord, describe(&g, &e)));
        host.tell(ToViewer::Done(why));
    }
}

/// Reads `path` if it is a regular file of at most `most` bytes. It is
/// opened without waiting, so a FIFO put in its place cannot hold the
/// viewer, and refused unless it is a file.
pub fn read_small_file(path: &std::path::Path, most: u64) -> std::io::Result<String> {
    let mut options = std::fs::File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(fuxix::file::NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let mut text = String::new();
    file.take(most.saturating_add(1))
        .read_to_string(&mut text)?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > most {
        return Err(std::io::Error::other(format!("more than {most} bytes")));
    }
    Ok(text)
}

/// Reads the user's configuration, if there is one to run: `Err` says why
/// it is not used.
pub fn load_config() -> Result<Option<String>, String> {
    #[cfg(windows)]
    {
        // A normal process can write the user's files; an elevated viewer
        // must not run what they say.
        let user = fuxix::socket::User::current().map_err(|e| e.to_string())?;
        if user.elevated() {
            return Err("elevated: view.grift is not run, built-in keys only".to_owned());
        }
    }
    let Some(dir) = fuxix::file::config_dir() else {
        return Ok(None);
    };
    let path = dir.join("fux2").join("view.grift");
    match read_small_file(&path, MAX_CONFIG) {
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
        focus: None,
        labels: Ok(Vec::new()),
        copied: 0,
        verbs: std::collections::HashSet::new(),
        generations: HashMap::new(),
        next_generation: 0,
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
