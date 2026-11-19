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
const MAX_OUTPUT: u64 = 1 << 20;

/// A viewer action a binding asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    NextPane,
    NextTab,
    Fit,
    Detach,
}

pub enum ToViewer {
    Bind(String),
    Unbind(String),
    Verb(Verb),
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
    /// The pane focused when the key was pressed, as `name@id`.
    pub focus: Option<String>,
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
    cancel: Arc<AtomicBool>,
    fux2: PathBuf,
}

impl Host {
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

fn string_arg(g: &mut HostCall<'_, Host>, value: Value) -> Result<String, Error> {
    match g.as_str(&value)? {
        Some(text) => Ok(text.to_owned()),
        None => Err(g.type_error(value)),
    }
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

/// Runs `fux2` with `args`, `input` on its stdin: its output. The viewer's
/// stop ends it.
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
    if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = Vec::new();
            if let Some(pipe) = pipe {
                let _ = pipe.take(MAX_OUTPUT).read_to_end(&mut text);
            }
            String::from_utf8_lossy(&text).into_owned()
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
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
    let out = out.join().unwrap_or_default();
    match status.success() {
        true => Ok(out),
        false => Err(err.join().unwrap_or_default().trim().to_owned()),
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
        let text = string_arg(g, key)?;
        let name = chord(&text).map_err(|why| failed(g, &why))?;
        if host.kept.len() >= MAX_BINDINGS && !host.kept.contains_key(&name) {
            return Err(failed(g, &format!("more than {MAX_BINDINGS} bindings")));
        }
        let kept = g.keep(function)?;
        if let Some(old) = host.kept.insert(name.clone(), kept) {
            let _ = g.release(old);
        }
        host.tell(ToViewer::Bind(name));
        inert()
    }),
    ("unbind-key", |g, host, args| {
        let [key] = *g.exactly(args)?;
        let text = string_arg(g, key)?;
        let name = chord(&text).map_err(|why| failed(g, &why))?;
        if let Some(old) = host.kept.remove(&name) {
            let _ = g.release(old);
        }
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
    ("next-tab", |g, host, args| {
        let [] = *g.exactly(args)?;
        verb(host, Verb::NextTab)
    }),
    ("fit", |g, host, args| {
        let [] = *g.exactly(args)?;
        verb(host, Verb::Fit)
    }),
    ("detach", |g, host, args| {
        let [] = *g.exactly(args)?;
        verb(host, Verb::Detach)
    }),
    ("send", |g, host, args| {
        let [pane, text] = *g.exactly(args)?;
        let pane = string_arg(g, pane)?;
        let text = string_arg(g, text)?;
        let args = vec!["send".to_owned(), pane, "--stdin".to_owned()];
        run_word(g, host, args, Some(text.into_bytes()))?;
        inert()
    }),
    ("send-key", |g, host, args| {
        let [pane, key] = *g.exactly(args)?;
        let args = vec![
            "send".to_owned(),
            string_arg(g, pane)?,
            "--key".to_owned(),
            string_arg(g, key)?,
        ];
        run_word(g, host, args, None)?;
        inert()
    }),
    ("read", |g, host, args| {
        let [pane] = *g.exactly(args)?;
        let args = vec!["read".to_owned(), string_arg(g, pane)?];
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
            string_arg(g, *name)?,
        ];
        if !program.is_empty() {
            words.push("--".to_owned());
        }
        for word in program {
            words.push(string_arg(g, *word)?);
        }
        let out = run_word(g, host, words, None)?;
        text_answer(g, &out)
    }),
    ("resize", |g, host, args| {
        let [pane, cols, rows] = *g.exactly(args)?;
        let args = vec![
            "resize".to_owned(),
            string_arg(g, pane)?,
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
            string_arg(g, pane)?,
            string_arg(g, name)?,
        ];
        run_word(g, host, args, None)?;
        inert()
    }),
    ("wait", |g, host, args| {
        let [pane, token, ms] = *g.exactly(args)?;
        let args = vec![
            "wait".to_owned(),
            string_arg(g, pane)?,
            "--after".to_owned(),
            string_arg(g, token)?,
            "--ms".to_owned(),
            number_arg(g, ms)?,
        ];
        let out = run_word(g, host, args, None)?;
        text_answer(g, &out)
    }),
    ("kill", |g, host, args| {
        let [pane] = *g.exactly(args)?;
        let pane = string_arg(g, pane)?;
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
    if let Some(text) = config {
        host.tell(ToViewer::Started {
            stop: g.interrupter(),
            limit: CONFIG_TIME,
        });
        let outcome = g.evaluate(&mut host, &text).err();
        let why = outcome.map(|e| format!("view.grift: {}", describe(&g, &e)));
        host.tell(ToViewer::Done(why));
    }
    while let Ok(run) = runs.recv() {
        host.cancel.store(false, Ordering::Relaxed);
        host.focus = run.focus;
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
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut text = String::new();
    file.take(MAX_CONFIG.saturating_add(1))
        .read_to_string(&mut text)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_CONFIG {
        return Err(format!("{}: more than {MAX_CONFIG} bytes", path.display()));
    }
    Ok(Some(text))
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
