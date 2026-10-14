//! fux's command line, config, bindings and prompt: fuxth programs, run in
//! a client. Words build typed requests (`opts … run`, `keys … send-keys`)
//! and execution words submit them: the program suspends while the server
//! does what one asks, and goes on with what it answers, as typed values
//! (a pane's handle, a capture), never text to be read back.
//!
//! Every run is fresh: its own stack and dictionary, the bundled library's
//! words first (`library.fux`).
mod words;

use crate::command::{Command, Extent, Input, Kind, Mouse, Until, WsRef};
use crate::id::{AnyId, ClientId, PaneId, TabId, WsId};
use crate::keymap::Keymap;
use crate::layout::Axis;
use crate::protocol::{Context, Reply, Request, Scalar};
use fuxth::{AppValue, Compiler, Limits, Machine, Outcome, Token, Value};
use std::io::Write;
use std::time::Duration;

pub use words::{WORDS, Word, usage};

/// The bundled words, written in fuxth.
pub const LIBRARY: &str = include_str!("library.fux");

/// A value of fux's own on the stack.
#[derive(Clone, Debug, PartialEq)]
pub enum Fux {
    Pane(PaneId),
    Tab(TabId),
    Workspace(WsId),
    Client(ClientId),
    /// Options being built, for `run`, `split`, `wait`, `capture` and the
    /// rest.
    Opts(Box<Opts>),
    /// Input being built, for `send-keys`.
    Keys(Box<Keys>),
    /// A mouse event being built, for `send-mouse`.
    Mouse(Box<MouseEvent>),
    /// A width or height: cells, or a share of the tab.
    Extent(Extent),
    /// What the server said: a capture, a listing, a wait's outcome.
    Answer(Box<Answer>),
}

impl AppValue for Fux {
    fn kind(&self) -> &'static str {
        match self {
            Fux::Pane(_) => "a pane",
            Fux::Tab(_) => "a tab",
            Fux::Workspace(_) => "a workspace",
            Fux::Client(_) => "a client",
            Fux::Opts(_) => "options (from opts)",
            Fux::Keys(_) => "keys (from keys)",
            Fux::Mouse(_) => "a mouse event (from mouse)",
            Fux::Extent(_) => "a size (from percent)",
            Fux::Answer(answer) => answer.kind,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Fux::Pane(_) => "pane",
            Fux::Tab(_) => "tab",
            Fux::Workspace(_) => "workspace",
            Fux::Client(_) => "client",
            Fux::Opts(_) => "opts",
            Fux::Keys(_) => "keys",
            Fux::Mouse(_) => "mouse",
            Fux::Extent(_) => "size",
            Fux::Answer(_) => "answer",
        }
    }

    fn text(&self) -> String {
        match self {
            Fux::Pane(p) => p.to_string(),
            Fux::Tab(t) => t.to_string(),
            Fux::Workspace(w) => w.to_string(),
            Fux::Client(c) => c.to_string(),
            Fux::Answer(answer) => answer.text.clone(),
            Fux::Extent(Extent::Cells(n)) => n.to_string(),
            Fux::Extent(Extent::Percent(n)) => format!("{n}%"),
            Fux::Opts(_) | Fux::Keys(_) | Fux::Mouse(_) => format!("({})", self.kind()),
        }
    }
}

/// What a wait waits for, as words give it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Waits {
    pub until: Vec<Until>,
    pub timeout: Option<Duration>,
    pub print: bool,
    pub marks: bool,
}

/// Options, as words give them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Opts {
    pub name: Option<String>,
    pub size: Option<fux_vt::Size>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub argv: Vec<String>,
    pub typed: Option<String>,
    pub axis: Option<Axis>,
    pub place: Option<WsRef>,
    pub ids: Vec<AnyId>,
    pub log: Option<String>,
    pub kill: bool,
    pub waits: Waits,
    pub view: crate::command::CaptureView,
    pub history: Option<usize>,
    /// `json` among the options: the capture is given as JSON.
    pub json: bool,
}

/// Input, as words give it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keys {
    pub inputs: Vec<Input>,
    pub waits: Waits,
}

/// A mouse event, as words give it.
#[derive(Clone, Debug, PartialEq)]
pub struct MouseEvent {
    pub mouse: Mouse,
    pub row: u16,
    pub col: u16,
    pub waits: Waits,
}

/// What the server answered, to print as text, or as JSON: the JSON the
/// server gave with it, read at the same moment.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub text: String,
    pub json: Option<String>,
    /// What it is, said where it is refused: `a capture`, `a listing`.
    pub kind: &'static str,
    /// The pane an action's answer is of: a word that wants a pane takes it.
    pub pane: Option<crate::id::PaneId>,
}

/// What an application word asks while the program waits.
#[derive(Clone, Debug)]
pub enum Ask {
    /// A request, as the client `client` (or the caller), and what to make
    /// of its reply.
    Server {
        command: Command,
        client: Option<ClientId>,
        then: Then,
    },
    /// Something the client itself does that takes over (attaching), or
    /// talks to no server (`status`).
    Local(Local),
}

/// What a reply becomes on the stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Then {
    Nothing,
    /// The pane, tab or workspace it made or found, of `Kind`.
    Handle(Kind),
    /// Its text, as an answer `json` can show as JSON: what it is, as
    /// [`Answer::kind`].
    Answer(&'static str),
    /// Its text, as a string.
    Text,
    /// A field's value.
    Value,
    /// A wait's outcome: its text, and JSON; nothing for no wait.
    Outcome {
        waited: bool,
    },
}

/// What the client does itself.
#[derive(Clone, Debug, PartialEq)]
pub enum Local {
    Attach {
        workspace: Option<String>,
        read_only: bool,
    },
    Headless {
        cols: u16,
        rows: u16,
    },
    /// The server for what follows: its socket, by name or path.
    Server(Chosen),
    Start,
    Status,
    Servers,
    /// Stop the server, whatever its version.
    ForceKill,
    /// Open one of the client's own screens: a client running its keys'
    /// programs does (`client::Keyboard`); nothing else has one.
    Open(crate::screens::Open),
}

/// What runs a program's words: where its requests come from, the client's
/// keymap if it runs as a config, and what its output goes to.
pub struct Host<'a> {
    pub context: Context,
    /// The keymap a config's `bind` and `prefix` change.
    pub keymap: Option<&'a mut Keymap>,
    /// The binding `label`, `group` and `repeat` change: its keys, and
    /// whether it is bound without the prefix.
    pub last_bound: Option<(Vec<crate::keys::KeyPress>, bool)>,
    pub out: &'a mut dyn Write,
    /// A command line an agent or a person runs: an action waits for its
    /// pane to settle unless it names what to wait for. Bindings, configs
    /// and restores act at once.
    pub settle: bool,
}

/// How a run ended: its status, and why it failed, if it did.
pub struct Ended {
    pub status: u8,
    pub stack: Vec<Value<Fux>>,
}

/// A run's failure: its status (a wait's: 3, 4, 5, 6, 124; else 1), its
/// stable code and its message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed {
    pub status: u8,
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for Failed {
    /// One line, as `fux: CODE: MESSAGE` is read: a message of several (a
    /// program's last lines, a path holding a line end) has its lines
    /// trimmed and joined with `\n`, any other control character or line
    /// separator escaped.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let escaped = |c: char| match c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
            true => c.escape_default().to_string(),
            false => c.to_string(),
        };
        let lines: Vec<String> = (self.message.lines().map(str::trim))
            .filter(|l| !l.is_empty())
            .map(|line| line.chars().map(escaped).collect())
            .collect();
        write!(f, "{}: {}", self.code, lines.join("\\n"))
    }
}

impl From<fuxth::Error> for Failed {
    fn from(mut error: fuxth::Error) -> Failed {
        // A bundled word that fails whatever way it takes is told by its
        // own name and use, not by the word inside it that would fail.
        let bundled = |w: &str| crate::help::library().iter().any(|(name, ..)| *name == w);
        if error.word.as_deref().is_some_and(bundled)
            && let Some((whole, _)) = error.message.split_once(": ")
            && whole.starts_with("every way through it fails")
        {
            error.message = whole.to_owned();
        }
        // A word given too few values, or the wrong kind, says how it is
        // used.
        if matches!(error.code, fuxth::Code::StackUnderflow | fuxth::Code::Type)
            && !error.message.contains("(use: ")
            && let Some(usage) = error.word.as_deref().and_then(words::usage)
        {
            error.message.push_str(&format!(" ({usage})"));
        }
        if error.code == fuxth::Code::UnknownWord {
            error.message.push_str(" (`fux help` lists every word)");
        }
        let word = error.word.as_deref().filter(|w| !w.is_empty());
        let message = match (&error.place, word) {
            (Some(place), Some(word)) => format!("{place}: {word}: {}", error.message),
            (Some(place), None) => format!("{place}: {}", error.message),
            (None, Some(word)) => format!("{word}: {}", error.message),
            (None, None) => error.message,
        };
        Failed {
            status: 1,
            code: error.code.name().to_owned(),
            message,
        }
    }
}

/// The vocabulary fuxth compiles fux's programs with.
pub fn vocabulary() -> fuxth::Vocabulary {
    let mut vocabulary = fuxth::Vocabulary::default();
    for word in WORDS {
        vocabulary.add(word.name, word.takes, word.gives());
    }
    vocabulary
}

/// Compiles `tokens` after the library: a program ready to run.
pub fn compile(tokens: &[Token]) -> Result<Machine<Fux>, Failed> {
    let mut compiler = READY.with(Clone::clone)?;
    compiler.add(&handles(tokens)?)?;
    Ok(Machine::new(compiler.finish()?, Limits::default()))
}

thread_local! {
    /// The standard library and fux's, compiled and checked once a thread,
    /// for every program after.
    static READY: Result<Compiler<Fux>, Failed> = libraries();
}

fn libraries() -> Result<Compiler<Fux>, Failed> {
    let mut compiler = Compiler::new(vocabulary());
    compiler.add(&fuxth::token::source("stdlib.fux", fuxth::STDLIB)?)?;
    compiler.add(&fuxth::token::source("library.fux", LIBRARY)?)?;
    compiler.prepare();
    Ok(compiler)
}

/// A bare id is the handle it names: `%3` is `str %3 pane`, `@2` is
/// `str @2 tab`, `+1` is `str +1 workspace`; but not where it is the text
/// `str` takes. Nor may `define` or `let` give one as a name: it
/// would always be the handle.
fn handles(tokens: &[Token]) -> Result<Vec<Token>, Failed> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut after: Option<&str> = None;
    for token in tokens {
        let word = match token.text.chars().next() {
            Some('%') => "pane",
            Some('@') => "tab",
            Some('+') => "workspace",
            Some(_) | None => "",
        };
        let id = token.text.len() > 1 && token.text.chars().skip(1).all(|c| c.is_ascii_digit());
        let handle = id && !word.is_empty();
        match after {
            Some("define" | "let") if handle => {
                return Err(Failed {
                    status: 1,
                    code: "bad_name".to_owned(),
                    message: format!(
                        "{}: {}: cannot be defined: it names a {word}",
                        token.place, token.text
                    ),
                });
            }
            None if handle => {
                let at = |text: &str| Token {
                    text: text.into(),
                    place: token.place.clone(),
                };
                out.extend([at("str"), token.clone(), at(word)]);
            }
            Some(_) | None => out.push(token.clone()),
        }
        after = match (&*token.text, after) {
            (text @ ("str" | "define" | "let"), None) => Some(text),
            (_, Some(_) | None) => None,
        };
    }
    Ok(out)
}

/// The server a program names: the default (`FUX_SOCKET`, else
/// `server.sock`), one by name, or a socket's path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Chosen {
    #[default]
    Default,
    Name(String),
    Path(String),
}

/// Where a program's requests go: a connection made when the first is.
#[derive(Default)]
pub struct Server {
    chosen: Chosen,
    requests: Option<crate::client::Requests>,
    /// What stops a run from another thread, if one may.
    stop: Option<Stop>,
}

/// What stops a run from another thread (the prefix then Esc, for a key's
/// program): a flag it looks at between steps, and what ends its
/// connection's reads under a request that waits.
#[derive(Clone, Default)]
pub struct Stop {
    flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stream: std::sync::Arc<std::sync::Mutex<Option<std::io::PipeWriter>>>,
}

impl Stop {
    /// Stops the run under way.
    pub fn stop(&self) {
        self.flag.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut stream) = self.stream.lock() {
            drop(stream.take());
        }
    }

    /// Ready for the next run.
    pub fn reset(&self) {
        self.flag.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether the run is to stop.
    pub fn stopped(&self) -> bool {
        self.flag.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Server {
    pub fn new(chosen: Chosen) -> Server {
        Server {
            chosen,
            requests: None,
            stop: None,
        }
    }

    /// A server whose runs `stop` can stop.
    pub fn stoppable(chosen: Chosen, stop: Stop) -> Server {
        Server {
            stop: Some(stop),
            ..Server::new(chosen)
        }
    }

    /// The server for what follows.
    pub fn choose(&mut self, chosen: Chosen) {
        self.chosen = chosen;
        self.requests = None;
    }

    /// Its socket.
    pub fn socket(&self) -> Result<crate::socket::SocketPath, Failed> {
        let choice = match &self.chosen {
            Chosen::Default => crate::socket::Choice::Default,
            Chosen::Name(name) => crate::socket::Choice::Name(name),
            Chosen::Path(path) => crate::socket::Choice::Path(path, "socket"),
        };
        crate::socket::socket_path(choice).map_err(|e| client_failed(e.into()))
    }

    /// Asks the server `request`, what it streams before its reply going to
    /// `out`.
    pub fn ask(&mut self, request: &Request, out: &mut dyn Write) -> Result<Reply, Failed> {
        let requests = match &mut self.requests {
            Some(requests) => requests,
            None => {
                let socket = self.socket()?;
                let mut opened = crate::client::Requests::open(&socket).map_err(client_failed)?;
                // A connection a stop can end.
                if let Some(stop) = &self.stop
                    && let Ok(mut held) = stop.stream.lock()
                {
                    *held = opened.stopper();
                }
                self.requests.insert(opened)
            }
        };
        let asked = requests.ask(request, out).map_err(client_failed);
        if asked.is_err() {
            self.requests = None;
        }
        asked
    }
}

fn client_failed(error: crate::client::Error) -> Failed {
    let code = match &error {
        crate::client::Error::Socket(crate::socket::Error::NoServer(_))
        | crate::client::Error::Connect { .. } => "no_server",
        crate::client::Error::Mismatch(_) => "mismatch",
        crate::client::Error::Socket(_)
        | crate::client::Error::Refused(_)
        | crate::client::Error::NoAnswer
        | crate::client::Error::Timeout
        | crate::client::Error::Closed
        | crate::client::Error::Protocol(_)
        | crate::client::Error::Write(_)
        | crate::client::Error::Read(_)
        | crate::client::Error::Exe(_)
        | crate::client::Error::Log(_)
        | crate::client::Error::Spawn(_)
        | crate::client::Error::Exited { .. }
        | crate::client::Error::NotStarted { .. }
        | crate::client::Error::NotATerminal
        | crate::client::Error::Modes(_)
        | crate::client::Error::RawMode(_)
        | crate::client::Error::Signals(_)
        | crate::client::Error::Poll(_)
        | crate::client::Error::ReadTerminal(_)
        | crate::client::Error::WriteTerminal(_) => "client",
    };
    Failed {
        status: 1,
        code: code.to_owned(),
        message: error.to_string(),
    }
}

/// What a program's requests go to, and what does what the client does
/// itself: a server (`Server`), the command line's (`Cli`), or in tests a
/// session itself.
pub trait Asks {
    /// Asks `request`, what is streamed before its reply going to `out`.
    fn ask(&mut self, request: &Request, out: &mut dyn Write) -> Result<Reply, Failed>;
    /// Does what the client does itself: by default, nothing, refused.
    fn local(&mut self, what: &Local, _out: &mut dyn Write) -> Result<Vec<Value<Fux>>, Failed> {
        refuse_local(what)
    }
}

impl Asks for Server {
    fn ask(&mut self, request: &Request, out: &mut dyn Write) -> Result<Reply, Failed> {
        Server::ask(self, request, out)
    }
}

/// The command line's: a server, and the client's own words.
#[derive(Default)]
pub struct Cli(pub Server);

impl Asks for Cli {
    fn ask(&mut self, request: &Request, out: &mut dyn Write) -> Result<Reply, Failed> {
        self.0.ask(request, out)
    }
    fn local(&mut self, what: &Local, out: &mut dyn Write) -> Result<Vec<Value<Fux>>, Failed> {
        local(what, &mut self.0, out)
    }
}

/// What a client's own program asks goes to: its keys', its prompt's, one
/// relayed to it. It may open one of the client's screens, built with what
/// `asks` answers; the last it opened is `opened`.
pub struct AsClient<'a> {
    pub asks: &'a mut dyn Asks,
    pub context: Context,
    pub opened: Option<crate::screens::Screen>,
}

impl Asks for AsClient<'_> {
    fn ask(&mut self, request: &Request, out: &mut dyn Write) -> Result<Reply, Failed> {
        self.asks.ask(request, out)
    }
    fn local(&mut self, what: &Local, out: &mut dyn Write) -> Result<Vec<Value<Fux>>, Failed> {
        let Local::Open(open) = what else {
            return self.asks.local(what, out);
        };
        let (asks, context) = (&mut *self.asks, &self.context);
        let mut sink = std::io::sink();
        let screen = open.build(&mut |command| {
            asks.ask(
                &Request {
                    command,
                    context: context.clone(),
                },
                &mut sink,
            )
        })?;
        self.opened = Some(screen);
        Ok(Vec::new())
    }
}

/// Runs `machine` to its end, asking `asks` what its words ask. Its stack
/// at the end.
pub fn run(
    machine: &mut Machine<Fux>,
    host: &mut Host<'_>,
    asks: &mut dyn Asks,
) -> Result<Ended, Failed> {
    run_capped(machine, host, asks, None, None)
}

/// The steps a run takes between looks at its budget.
const SLICE: u64 = 100_000;

/// `run`, failing once it has taken more than `steps` steps, if given.
pub fn run_capped(
    machine: &mut Machine<Fux>,
    host: &mut Host<'_>,
    asks: &mut dyn Asks,
    steps: Option<u64>,
    stop: Option<&Stop>,
) -> Result<Ended, Failed> {
    // A wait met by its program's exit: the run ends with that status.
    let mut status = 0;
    let mut taken: u64 = 0;
    loop {
        if stop.is_some_and(Stop::stopped) {
            return Err(Failed {
                status: 1,
                code: "stopped".to_owned(),
                message: "stopped (the prefix, then Esc)".to_owned(),
            });
        }
        if let Some(steps) = steps.filter(|steps| taken > *steps) {
            let failed = Failed {
                status: 1,
                code: "steps".to_owned(),
                message: format!("ran more than {steps} steps"),
            };
            return Err(placed(machine, failed));
        }
        taken = taken.saturating_add(SLICE);
        match machine
            .run(host, SLICE)
            .map_err(|e| Failed::from(ours(machine, e)))?
        {
            Outcome::Finished => {
                return Ok(Ended {
                    status,
                    stack: machine.stack().to_vec(),
                });
            }
            Outcome::Paused => {}
            Outcome::Suspended(Ask::Server {
                command,
                client,
                then,
            }) => {
                let mut context = host.context.clone();
                if client.is_some() {
                    context.client = client;
                }
                let reply = asks
                    .ask(&Request { command, context }, host.out)
                    .map_err(|f| placed(machine, f))?;
                if met(&reply) && reply.status != 0 {
                    status = reply.status;
                }
                // A failure that answered too (`run`'s pane): its answer is
                // left, printed before why it failed.
                let answer = !reply.message.trim().is_empty() && !reply.text.trim().is_empty();
                if reply.status != 0 && answer {
                    machine.push(Value::str(reply.text.trim_end()))?;
                }
                for value in answered(reply, then).map_err(|f| placed(machine, f))? {
                    machine.push(value)?;
                }
            }
            Outcome::Suspended(Ask::Local(what)) => {
                for value in asks
                    .local(&what, host.out)
                    .map_err(|f| placed(machine, f))?
                {
                    machine.push(value)?;
                }
            }
        }
    }
}

/// A failure in doing what the word the program stopped at asks, named
/// with that word and where it is, as the language's own errors are.
fn placed(machine: &Machine<Fux>, failed: Failed) -> Failed {
    let Some(at) = machine.at() else {
        return failed;
    };
    let error = ours(
        machine,
        fuxth::Error {
            code: fuxth::Code::Host("placed"),
            message: failed.message,
            place: Some(at.place),
            word: Some(at.text),
        },
    );
    let message = match (&error.place, &error.word) {
        (Some(place), Some(word)) => format!("{place}: {word}: {}", error.message),
        (Some(_) | None, Some(_) | None) => error.message,
    };
    Failed { message, ..failed }
}

/// An error placed where the program is: a failure inside a word of the
/// prelude's or library's is named by the program's step that called it,
/// with the bundled word's own step in the message, and how the program's
/// word is used.
fn ours(machine: &Machine<Fux>, mut error: fuxth::Error) -> fuxth::Error {
    let bundled = |place: Option<&fuxth::Place>| {
        matches!(place, Some(fuxth::Place::Line { source, .. })
            if matches!(&**source, "stdlib.fux" | "library.fux"))
    };
    if !bundled(error.place.as_ref()) {
        return error;
    }
    let Some(step) = machine
        .calls()
        .into_iter()
        .rev()
        .find(|t| !bundled(Some(&t.place)))
    else {
        return error;
    };
    let inner = error.word.as_deref().unwrap_or_default();
    // How the program's word is used, in place of the bundled step's.
    let message = error.message.split(" (use: ").next().unwrap_or_default();
    let usage = words::usage(&step.text).map_or_else(String::new, |u| format!(" ({u})"));
    error.message = format!("inside {}, {inner}: {message}{usage}", step.text);
    error.place = Some(step.place);
    error.word = Some(step.text);
    error
}

/// Whether a reply with a status other than 0 is a wait met by its
/// program's exit, which is no failure: that status is the run's (`run`).
/// Every failure says why.
fn met(reply: &Reply) -> bool {
    reply.message.is_empty() && reply.code.is_none()
}

/// What a reply puts on the stack, `then` saying what it is; a failure,
/// with its status, if it failed: why, else what it answered.
pub fn answered(reply: Reply, then: Then) -> Result<Vec<Value<Fux>>, Failed> {
    if reply.status != 0 && !met(&reply) {
        let message = match (reply.message.trim_end(), reply.text.trim_end()) {
            ("", text) => text,
            (message, _) => message,
        }
        .to_owned();
        let code = reply.code.unwrap_or_else(|| {
            match reply.status {
                3 => "exited",
                4 => "closed",
                7 => "stopped",
                124 => "timeout",
                _ => "failed",
            }
            .to_owned()
        });
        return Err(Failed {
            status: reply.status,
            code,
            message,
        });
    }
    let text = reply.text.trim_end_matches('\n').to_owned();
    let json = Some(reply.json.trim_end().to_owned()).filter(|j| !j.is_empty());
    // An action's answer is its pane's, when the reply names one pane alone
    // (a wait on a tab's panes answers each): never read from what the
    // program wrote.
    let mut panes = reply.ids.iter().filter_map(|id| match id {
        crate::command::AnyRef::Pane(p) => Some(*p),
        crate::command::AnyRef::Tab(_) | crate::command::AnyRef::Workspace(_) => None,
    });
    let pane = match (panes.next(), panes.next()) {
        (Some(pane), None) => Some(pane),
        (Some(_), Some(_)) | (None, _) => None,
    };
    let action = |text: String, json: Option<String>| {
        Value::App(Fux::Answer(Box::new(Answer {
            text,
            json,
            kind: match pane {
                Some(_) => "an action's answer",
                None => "an answer of several panes",
            },
            pane,
        })))
    };
    Ok(match then {
        Then::Nothing => Vec::new(),
        Then::Handle(kind) => match reply.ids.iter().rev().find(|id| id.kind() == kind) {
            // A pane an action settled: its answer, which is its pane too.
            Some(crate::command::AnyRef::Pane(p)) if json.is_some() && pane == Some(*p) => {
                vec![action(text, json)]
            }
            Some(id) => vec![Value::App(handle(*id))],
            None => {
                return Err(Failed {
                    status: 1,
                    code: "no_target".to_owned(),
                    message: format!("the server named no {}", kind.name()),
                });
            }
        },
        Then::Answer(kind) => vec![Value::App(Fux::Answer(Box::new(Answer {
            text,
            json,
            kind,
            pane: None,
        })))],
        Then::Text => vec![Value::str(text)],
        Then::Value => vec![match reply.value {
            Some(Scalar::Int(n)) => Value::Int(n),
            Some(Scalar::Bool(b)) => Value::Bool(b),
            Some(Scalar::Str(s)) => Value::str(s),
            // JSON that is no one value of fuxth's: `null`, a list, an
            // object, as text that `json` gives as it is.
            Some(Scalar::Null) | None => Value::App(Fux::Answer(Box::new(Answer {
                json: Some(text.clone()),
                text,
                kind: "a JSON value",
                pane: None,
            }))),
        }],
        Then::Outcome { waited: false } => Vec::new(),
        Then::Outcome { waited: true } => vec![action(text, json)],
    })
}

/// A handle for `id`.
pub fn handle(id: crate::command::AnyRef<WsId>) -> Fux {
    match id {
        crate::command::AnyRef::Pane(p) => Fux::Pane(p),
        crate::command::AnyRef::Tab(t) => Fux::Tab(t),
        crate::command::AnyRef::Workspace(w) => Fux::Workspace(w),
    }
}

/// A value as a program's end prints it.
pub fn display(value: &Value<Fux>) -> String {
    value.text()
}

/// What a program does that the client does itself, from the command line:
/// attaching, choosing and starting a server, and asking after servers.
fn local(
    what: &Local,
    server: &mut Server,
    _out: &mut dyn Write,
) -> Result<Vec<Value<Fux>>, Failed> {
    let failed = |code: &str, message: String| Failed {
        status: 1,
        code: code.to_owned(),
        message,
    };
    match what {
        Local::Server(chosen) => server.choose(chosen.clone()),
        Local::Attach {
            workspace,
            read_only,
        } => {
            if std::env::var_os("FUX_PANE").is_some_and(|p| !p.is_empty()) {
                let why = "this is a fux pane: attaching here would show fux inside itself \
                           (unset FUX_PANE to do it anyway)";
                return Err(failed("nested", why.to_owned()));
            }
            // Before a server is started for it, which would be left behind.
            if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                return Err(client_failed(crate::client::Error::NotATerminal));
            }
            let socket = started(server)?;
            let (keymap, problem) = configured(server);
            let attach = crate::client::Attaching {
                workspace: workspace.clone(),
                read_only: *read_only,
                keymap,
                problem,
            };
            crate::client::attach(&socket, attach).map_err(client_failed)?;
        }
        Local::Headless { cols, rows } => {
            let socket = started(server)?;
            let (keymap, problem) = configured(server);
            let size = fux_vt::Size::new(*rows, *cols)
                .map_err(|_| failed("range", "headless: a size from 1 by 1".to_owned()))?;
            let attach = crate::client::Attaching {
                workspace: None,
                read_only: false,
                keymap,
                problem,
            };
            crate::client::headless(&socket, attach, size).map_err(client_failed)?;
        }
        Local::Start => {
            started(server)?;
        }
        Local::ForceKill => {
            let socket = server.socket()?;
            crate::client::kill_server(&socket).map_err(client_failed)?;
        }
        Local::Open(_) => return refuse_local(what),
        Local::Status => {
            let socket = server.socket()?;
            let probe = crate::client::probe(&socket).map_err(client_failed)?;
            let (text, _) = crate::status(&socket, &probe, false);
            let (json, _) = crate::status(&socket, &probe, true);
            return Ok(vec![answer(text, json, "a status")]);
        }
        Local::Servers => {
            let fail = |e: crate::socket::Error| client_failed(e.into());
            let text = crate::servers(false).map_err(fail)?;
            let json = crate::servers(true).map_err(fail)?;
            return Ok(vec![answer(text, json, "a listing of servers")]);
        }
    }
    Ok(Vec::new())
}

/// An answer the client made itself, as text and JSON.
fn answer(text: String, json: String, kind: &'static str) -> Value<Fux> {
    Value::App(Fux::Answer(Box::new(Answer {
        text: text.trim_end().to_owned(),
        json: Some(json.trim_end().to_owned()),
        kind,
        pane: None,
    })))
}

/// The server, started in the background if none answers: its socket. One
/// started has the config's settings (`set`) at once.
fn started(server: &mut Server) -> Result<crate::socket::SocketPath, Failed> {
    let socket = server.socket()?;
    if !crate::client::answers(&socket) {
        crate::client::start_server(&socket).map_err(client_failed)?;
        if let (_, Some(problem)) = configured(server) {
            let _ = writeln!(std::io::stderr(), "fux: {problem}");
        }
    }
    Ok(socket)
}

/// The client's keymap, its config run on the defaults; and what was wrong
/// with the config, if anything was.
pub fn configured(server: &mut Server) -> (Keymap, Option<String>) {
    let mut keymap = Keymap::default();
    let Some(path) = crate::config::default_path().filter(|p| p.exists()) else {
        return (keymap, None);
    };
    match run_file(&path, &mut keymap, Some(server)) {
        Ok(()) => (keymap, None),
        // Half a config is no config: the defaults, and why.
        Err(failed) => (Keymap::default(), Some(format!("config: {failed}"))),
    }
}

/// The keymap the config file at `path` makes, run dry: its bindings made,
/// its settings checked and sent nowhere; or why it would fail.
pub fn dry_config(path: &std::path::Path) -> Result<Keymap, Failed> {
    let mut keymap = Keymap::default();
    run_file(path, &mut keymap, None)?;
    Ok(keymap)
}

/// Runs the config file at `path` against the server on `socket`, as a
/// client does as it attaches: the keymap it makes, or its failure.
pub fn configure(
    path: &std::path::Path,
    socket: &crate::socket::SocketPath,
) -> Result<Keymap, Failed> {
    let mut keymap = Keymap::default();
    let mut server = Server::new(Chosen::Path(socket.path().to_string_lossy().into_owned()));
    run_file(path, &mut keymap, Some(&mut server))?;
    Ok(keymap)
}

/// Runs a config file, its words changing `keymap`, its settings sent to
/// `server`. A config attaches nothing, and chooses no other server.
fn run_file(
    path: &std::path::Path,
    keymap: &mut Keymap,
    server: Option<&mut Server>,
) -> Result<(), Failed> {
    let shown = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|e| Failed {
        status: 1,
        code: "read".to_owned(),
        message: format!("{shown}: {e}"),
    })?;
    let mut machine = compile(&fuxth::token::source(&shown, &text)?)?;
    let mut out = std::io::sink();
    let mut host = Host {
        context: crate::client::context(),
        keymap: Some(keymap),
        last_bound: None,
        out: &mut out,
        settle: false,
    };
    run_capped(
        &mut machine,
        &mut host,
        &mut Settings(server),
        Some(PROGRAM_STEPS),
        None,
    )
    .map(drop)
}

/// What a config's requests go to: settings alone, as a config runs each
/// time a client attaches, and must make nothing. With no server, a dry
/// run: each setting checked, none sent.
struct Settings<'a>(Option<&'a mut Server>);

impl Asks for Settings<'_> {
    fn ask(&mut self, request: &Request, out: &mut dyn Write) -> Result<Reply, Failed> {
        if let Command::Set { option, value } = &request.command {
            return match &mut self.0 {
                Some(server) => server.ask(request, out),
                None => match crate::config::Config::default().set(option, value) {
                    Ok(()) => Ok(Reply::default()),
                    Err(error) => Err(Failed {
                        status: 1,
                        code: "config".to_owned(),
                        message: error.to_string(),
                    }),
                },
            };
        }
        Err(Failed {
            status: 1,
            code: "not_config".to_owned(),
            message: "a config sets and binds: it runs each time a client attaches, so it makes and changes nothing else".to_owned(),
        })
    }
}

/// A program that may not do what the client does itself: a config's, a
/// key's, a saved session's.
pub fn refuse_local(what: &Local) -> Result<Vec<Value<Fux>>, Failed> {
    let word = match what {
        Local::Attach { .. } => "attach",
        Local::Headless { .. } => "headless",
        Local::Server(_) => "server",
        Local::Start => "start",
        Local::Status => "status",
        Local::Servers => "servers",
        Local::ForceKill => "force-kill-server",
        Local::Open(_) => "a client's screen",
    };
    Err(Failed {
        status: 1,
        code: "not_here".to_owned(),
        message: format!("{word} runs from the command line only"),
    })
}

/// Brings back a saved session: its program run against the server on
/// `socket`, ids asked for as saved, programs left for `respawn`.
pub fn restore(
    socket: &crate::socket::SocketPath,
    path: &std::path::Path,
    text: &str,
) -> Result<(), Failed> {
    let mut machine = compile(&fuxth::token::source(&path.display().to_string(), text)?)?;
    let mut out = std::io::sink();
    let mut context = crate::client::context();
    context.restoring = true;
    // Its panes are its own, not a pane's it runs in.
    (context.pane, context.server) = (None, None);
    let mut host = Host {
        context,
        keymap: None,
        last_bound: None,
        out: &mut out,
        settle: false,
    };
    let mut server = Server::new(Chosen::Path(socket.path().to_string_lossy().into_owned()));
    run_capped(
        &mut machine,
        &mut host,
        &mut Layout(&mut server),
        Some(PROGRAM_STEPS),
        None,
    )
    .map(drop)
}

/// The most steps a program the client runs for itself takes (a config, a
/// saved session): one that loops without end fails.
pub const PROGRAM_STEPS: u64 = 10_000_000;

/// What a saved session's requests go to: those that make and shape panes,
/// tabs and workspaces, and find them; never what stops the server, grants,
/// sets, or reaches a client, whatever the file says.
struct Layout<'a>(&'a mut Server);

impl Asks for Layout<'_> {
    fn ask(&mut self, request: &Request, out: &mut dyn Write) -> Result<Reply, Failed> {
        use Command as C;
        let refused = match &request.command {
            C::KillServer { .. } => Some("kill-server"),
            C::Grant { .. } => Some("grant"),
            C::Set { .. } => Some("set"),
            C::Client { .. } => Some("a client's words"),
            C::Ls { .. }
            | C::NewWorkspace { .. }
            | C::NewTab { .. }
            | C::Split { .. }
            | C::Run { .. }
            | C::Respawn { .. }
            | C::Notify { .. }
            | C::Resize { .. }
            | C::Wait { .. }
            | C::SendMouse { .. }
            | C::KillPane { .. }
            | C::Signal { .. }
            | C::KillTab { .. }
            | C::KillWorkspace { .. }
            | C::Rename { .. }
            | C::MovePane { .. }
            | C::SwapPane { .. }
            | C::ResizePane { .. }
            | C::SendKeys { .. }
            | C::Events { .. }
            | C::DumpLayout { .. }
            | C::Log { .. }
            | C::ClearHistory { .. }
            | C::CapturePane { .. }
            | C::Reorder { .. }
            | C::Resolve { .. }
            | C::ListBuffers
            | C::ShowBuffer { .. }
            | C::PasteBuffer { .. } => None,
        };
        match refused {
            Some(word) => Err(Failed {
                status: 1,
                code: "not_layout".to_owned(),
                message: format!(
                    "{word} in a saved session: it makes and shapes, and does nothing else"
                ),
            }),
            None => self.0.ask(request, out),
        }
    }
}

/// The `fux` binary's words: attaches with none, runs `-` (stdin) or a file,
/// or runs the words given. What is left on the stack is printed, a value a
/// line. Its status, or why it failed.
pub fn main(args: &[String], out: &mut dyn Write) -> Result<u8, Failed> {
    let tokens = match args {
        [] => fuxth::token::arguments(&["attach"]),
        // A program read whole, at most `SOURCE_MAX` bytes of it.
        [dash] if dash == "-" => {
            fuxth::token::source("stdin", &source("stdin", std::io::stdin())?)?
        }
        [file] if file.contains('/') || file.ends_with(".fux") => {
            let opened =
                std::fs::File::open(file).map_err(|e| read_failed(file, &e.to_string()))?;
            fuxth::token::source(file, &source(file, opened)?)?
        }
        // A whole program in one word, as a shell passes `fux 'ls json'`.
        [line] if line.contains(char::is_whitespace) => fuxth::token::source("command line", line)?,
        // Help as other programs give it, and older notes say: `help agents`,
        // `help json`, `help WORD`.
        [help, topic] if help == "help" => match topic.as_str() {
            "agents" => fuxth::token::arguments(&["help-agents"]),
            "json" => fuxth::token::arguments(&["help-json"]),
            word if crate::help::explain(word).is_some() => {
                fuxth::token::arguments(&["str", word, "explain"])
            }
            word => {
                return Err(Failed {
                    status: 1,
                    code: "unknown_word".to_owned(),
                    message: format!(
                        "argument 2: help: no word {word:?} (`fux help` lists every word)"
                    ),
                });
            }
        },
        // What other programs take for help and their version.
        [flag] if matches!(flag.as_str(), "--help" | "-h") => fuxth::token::arguments(&["help"]),
        [flag] if matches!(flag.as_str(), "--version" | "-V") => {
            fuxth::token::arguments(&["version"])
        }
        words => fuxth::token::arguments(words),
    };
    let mut machine = compile(&tokens)?;
    let mut out = Checked { out, failed: None };
    let mut host = Host {
        context: crate::client::context(),
        keymap: None,
        last_bound: None,
        out: &mut out,
        settle: true,
    };
    let ran = run(&mut machine, &mut host, &mut Cli::default());
    // What the words before a failure left is printed too: their answers.
    let stack = match &ran {
        Ok(ended) => ended.stack.as_slice(),
        Err(_) => machine.stack(),
    };
    for value in stack {
        // An answer with nothing to say (a wait for quiet, an empty
        // listing) prints no blank line.
        if matches!(value, Value::App(Fux::Answer(a)) if a.text.is_empty()) {
            continue;
        }
        let _ = writeln!(host.out, "{}", display(value));
    }
    let _ = host.out.flush();
    let status = ran?.status;
    match out.failed {
        Some(error) => Err(Failed {
            status: 1,
            code: "output".to_owned(),
            message: format!("writing the output: {error}"),
        }),
        None => Ok(status),
    }
}

/// The most a program read from a file or stdin may be.
const SOURCE_MAX: u64 = 16 << 20;

fn read_failed(name: &str, why: &str) -> Failed {
    Failed {
        status: 1,
        code: "read".to_owned(),
        message: format!("{name}: {why}"),
    }
}

/// A program's text from `from`, which may be anything (`/dev/zero`): at
/// most `SOURCE_MAX` bytes of UTF-8.
fn source(name: &str, from: impl std::io::Read) -> Result<String, Failed> {
    let mut bytes = Vec::new();
    use std::io::Read;
    from.take(SOURCE_MAX.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| read_failed(name, &e.to_string()))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > SOURCE_MAX {
        return Err(read_failed(
            name,
            "more than 16 MiB: no program is that long",
        ));
    }
    String::from_utf8(bytes).map_err(|_| read_failed(name, "not UTF-8 text: no fuxth program"))
}

/// Output that keeps its first failure, so that a run whose output cannot
/// be written fails, however its words wrote it.
struct Checked<'a> {
    out: &'a mut dyn Write,
    failed: Option<std::io::Error>,
}

impl Write for Checked<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.out.write(bytes);
        if let Err(error) = &written
            && self.failed.is_none()
        {
            self.failed = Some(std::io::Error::new(error.kind(), error.to_string()));
        }
        written
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let flushed = self.out.flush();
        if let Err(error) = &flushed
            && self.failed.is_none()
        {
            self.failed = Some(std::io::Error::new(error.kind(), error.to_string()));
        }
        flushed
    }
}
