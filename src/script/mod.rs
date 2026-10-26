//! fux's command line, config, bindings and prompt: fuxth programs, run in
//! a client. Words build typed requests (`opts … run`, `%2 … send-keys`)
//! and execution words submit them: the program suspends while the server
//! does what one asks, and goes on with what it answers, as typed values
//! (a pane's handle, a capture), never text to be read back.
//!
//! Every run is fresh: its own stack and dictionary, the bundled library's
//! words first (`library.fux`).
mod words;

use crate::client;
use crate::code::Code;
use crate::command::{AnyRef, Extent, Input, Kind, Mouse, Request, Version, WsRef};
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::keymap::Keymap;
use crate::layout::Axis;
use crate::protocol::{Asked, Context, End, Reply, Scalar};
use crate::socket::{Choice, SocketPath};
use fuxth::{AppValue, Code as Lang, Compiler, Limits, Machine, Outcome, Token, Value, token};
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub use words::{WORDS, Word, usage};

/// A config, by its name and text, whose definitions its bindings and
/// prompt see.
pub type Config = std::sync::Arc<(String, String)>;

/// The bundled words, written in fuxth.
pub const LIBRARY: &str = include_str!("library.fux");

/// The first line of a fuxth script, which runs it as `fux --file`.
pub const SHEBANG: &str = "#!/usr/bin/env -S fux --file";

/// fux's default keys: a config, run before the user's.
const KEYS: &str = include_str!("keys.fux");

/// A value of fux's own on the stack.
#[derive(Clone, Debug, PartialEq)]
pub enum Fux {
    Pane(PaneId),
    Tab(TabId),
    Workspace(WsId),
    Client(ClientId),
    /// Options for `run`, `new-tab` or `new-workspace` (from `opts`); or a
    /// request: a pane, tab, workspace or client given options, keys, a
    /// wait or a mouse event, for the word that acts on it after
    /// (`%2 str ls text str Enter key send-keys`).
    Opts(Box<Opts>),
    /// A width or height: cells, or a share of the tab.
    Extent(Extent),
    /// What the server said: a capture, a listing, a wait's outcome.
    Answer(Box<Answer>),
}

impl AppValue for Fux {
    fn kind(&self) -> &'static str {
        if let Fux::Answer(answer) = self {
            return answer.said.kind();
        }
        let kind = words::KINDS.iter().find(|(name, ..)| *name == self.name());
        kind.map_or("a value of fux's", |(_, said, _)| said)
    }

    fn name(&self) -> &'static str {
        match self {
            Fux::Pane(_) => "pane",
            Fux::Tab(_) => "tab",
            Fux::Workspace(_) => "workspace",
            Fux::Client(_) => "client",
            Fux::Opts(opts) => match opts.on {
                None => "opts",
                Some(_) => "request",
            },
            Fux::Extent(_) => "size",
            Fux::Answer(answer) => answer.said.name(),
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
            Fux::Extent(Extent::Percent(n)) => format!("{}%", n.get()),
            Fux::Opts(_) => format!("({})", self.kind()),
        }
    }
}

/// Options, keys, waits and a mouse event, as words give them, and what
/// they are on: nothing, for `run`, `new-tab` and `new-workspace`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Opts {
    pub on: Option<On>,
    pub name: Option<String>,
    pub size: crate::command::Size,
    pub cwd: Option<std::path::PathBuf>,
    pub env: crate::command::Env,
    pub argv: Vec<String>,
    pub axis: Option<Axis>,
    /// A split's shares (`shares`).
    pub shares: Option<(std::num::NonZeroU32, std::num::NonZeroU32)>,
    pub place: Option<WsRef>,
    pub kill: bool,
    /// How long a wait waits at most.
    pub timeout: Option<crate::config::Limit>,
    pub view: crate::command::CaptureView,
    pub history: Option<usize>,
    pub inputs: Vec<Input>,
    /// A mouse event: what it does, and its row and column.
    pub mouse: Option<(Mouse, u16, u16)>,
    /// The words given, for the word they go on to refuse any it would not
    /// use.
    pub given: Vec<&'static str>,
}

/// What a request is on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum On {
    Target(AnyRef),
    Client(ClientId),
    /// A version's pane (`changed`, or an action's or wait's answer): a
    /// wait on it waits from that version.
    Version(Version),
}

/// What the server answered, to print as text, or as JSON: the JSON the
/// server gave with it, read at the same moment.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub text: String,
    pub json: Option<String>,
    pub said: Said,
}

/// What an answer is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Said {
    Capture,
    Listing,
    /// An action's or wait's answer of one pane, which holds its
    /// version: a word that wants a pane takes it, and a wait waits from
    /// that version.
    Action(Version),
    /// An answer that holds no one pane's version: a tab's or workspace's
    /// panes', or a closed pane's.
    Panes,
    /// JSON that is no one value of fuxth's.
    Json,
    Status,
}

impl Said {
    /// Its kind, as the vocabulary names it.
    fn name(self) -> &'static str {
        match self {
            Said::Capture => "capture",
            Said::Listing | Said::Status => "listing",
            Said::Action(_) | Said::Panes => "answer",
            Said::Json => "json",
        }
    }

    /// What it is, said where it is refused.
    fn kind(self) -> &'static str {
        match self {
            Said::Capture => "a capture",
            Said::Listing => "a listing",
            Said::Action(_) => "an action's answer",
            Said::Panes => "an answer of a tab's or workspace's panes, or of a closed pane",
            Said::Json => "a JSON value",
            Said::Status => "a status",
        }
    }

    /// The version an action's answer holds: its pane's from before what
    /// it did.
    pub fn version(self) -> Option<crate::command::Version> {
        match self {
            Said::Action(version) => Some(version),
            Said::Capture | Said::Listing | Said::Panes | Said::Json | Said::Status => None,
        }
    }
}

impl Answer {
    /// An answer, as a value on the stack.
    fn value(text: String, json: Option<String>, said: Said) -> Value<Fux> {
        Value::App(Fux::Answer(Box::new(Answer { text, json, said })))
    }
}

/// What an application word asks while the program waits.
#[derive(Clone, Debug)]
pub enum Ask {
    /// A request, as the client `client` (or the caller), and what to make
    /// of its reply.
    Server {
        request: Box<Request>,
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
    /// Its text, as an answer `json` can show as JSON.
    Answer(Said),
    /// Its text, as a string.
    Text,
    /// A field's value.
    Value,
    /// A wait's outcome: its text, and JSON; empty for no wait.
    Outcome,
}

/// The keys of a binding just made.
#[derive(Clone, Debug, PartialEq)]
pub enum Bound {
    /// Keys after the prefix.
    Prefix(Vec<crate::keys::KeyPress>),
    /// One key, bound without the prefix.
    Root(crate::keys::KeyPress),
}

/// What the client does itself.
#[derive(Clone, Debug, PartialEq)]
pub enum Local {
    /// This terminal, or a client of no terminal of `size`.
    Attach {
        workspace: Option<WsRef>,
        read_only: bool,
        size: Option<fux_vt::Size>,
    },
    /// The server for what follows: its socket, by name or path.
    Server(Choice),
    Start,
    Status,
    /// Stop the server, whatever its version.
    ForceKill,
    /// Open one of the client's own screens: a client running its keys'
    /// programs does (`client::Keyboard`); nothing else has one.
    Open(crate::screens::Open),
    /// What the command line's program asks of its process.
    Shell(Shell),
}

/// What a command line's program asks of the process it runs in, as a
/// shell script would: no other program has one.
#[derive(Clone, Debug, PartialEq)]
pub enum Shell {
    /// Its arguments (`args`).
    Args,
    /// The rest of stdin (`input`).
    Input,
    /// A file's text (`read-file`).
    ReadFile(String),
    /// An environment variable's value (`getenv`).
    Getenv(VarName),
    /// A line on stderr (`warn`).
    Warn(String),
    /// The program's end, with this exit status (`exit`).
    Exit(u8),
}

/// What runs a program's words: where its requests come from, the client's
/// keymap if it runs as a config, and what its output goes to.
pub struct Host<'a> {
    pub context: Context,
    /// The keymap a config's `bind` and `prefix` change.
    pub keymap: Option<&'a mut Keymap>,
    /// The binding `label`, `group` and `repeat` change.
    pub last_bound: Option<Bound>,
    pub out: &'a mut dyn Write,
}

impl<'a> Host<'a> {
    pub fn new(
        context: Context,
        keymap: Option<&'a mut Keymap>,
        out: &'a mut dyn Write,
    ) -> Host<'a> {
        Host {
            context,
            keymap,
            last_bound: None,
            out,
        }
    }
}

/// A run's failure: its code, which names its status, and its message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed {
    pub code: Code,
    pub message: String,
}

impl Failed {
    /// A failure of kind `code`, and why.
    pub fn new(code: impl Into<Code>, message: impl Into<String>) -> Failed {
        Failed {
            code: code.into(),
            message: message.into(),
        }
    }
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
    /// A mistake compiling, told as fux tells it.
    fn from(error: fuxth::Error) -> Failed {
        telling(error)
    }
}

/// `error` as fux tells it, with how its word is used where it was given
/// too few values, the wrong kind, or one out of its range.
fn telling(error: fuxth::Error) -> Failed {
    let usage = match error.code {
        Lang::StackUnderflow | Lang::Type | Lang::Range => true,
        Lang::OpenQuote
        | Lang::UnknownWord
        | Lang::MissingToken
        | Lang::BadName
        | Lang::NotTopLevel
        | Lang::Unmatched
        | Lang::Unclosed
        | Lang::StackOverflow
        | Lang::CallDepth
        | Lang::DivByZero
        | Lang::Overflow
        | Lang::Unset
        | Lang::Unused
        | Lang::BadCodes
        | Lang::Fail
        | Lang::TooLong
        | Lang::Contract
        | Lang::Host(_) => false,
    };
    told(error, usage)
}

/// `error` as fux tells it, with how its word is used if `usage`, and
/// for options or a request left over, the words that would take them: a
/// failure of its code, saying where and why it happened. What `check`
/// read says where its mistake is: it is placed nowhere.
fn told(mut error: fuxth::Error, usage: bool) -> Failed {
    let left = words::KINDS
        .iter()
        .any(|(.., why)| *why == Some(&*error.message));
    if error.code == Lang::Unused && left {
        let word = error.word.as_deref().unwrap_or_default();
        error.message += &format!(": end with {}", words::takers(word));
    }
    if usage && let Some(usage) = error.word.as_deref().and_then(words::usage) {
        error.message.push_str(&format!(" ({usage})"));
    }
    if error.code == Lang::UnknownWord {
        error.message.push_str(" (`fux help` lists every word)");
    }
    if Code::from(error.code) == Code::Check {
        (error.place, error.word) = (None, None);
    }
    Failed::new(Code::from(error.code), error.located())
}

/// The vocabulary fuxth compiles fux's programs with: fux's kinds, the
/// names its words' effects use, the words, and the codes they raise.
pub fn vocabulary() -> Result<fuxth::Vocabulary, fuxth::Error> {
    let mut vocabulary = fuxth::Vocabulary::default();
    for (kind, said, unused) in words::KINDS {
        vocabulary.kind(kind, said)?;
        if let Some(why) = unused {
            vocabulary.must_use(kind, why)?;
        }
    }
    let strings = words::STRINGS
        .split_whitespace()
        .map(|n| (n, "string", "a string"));
    let integers = words::INTEGERS
        .split_whitespace()
        .map(|n| (n, "integer", "an integer"));
    for (name, kinds, said) in strings.chain(integers).chain(words::NAMES.iter().copied()) {
        vocabulary.name(name, kinds, said)?;
    }
    for word in WORDS {
        vocabulary.add(word.name, word.effect)?;
    }
    let own = Code::ALL.iter().filter(|c| c.raised());
    for code in own
        .map(|c| fuxth::Code::from(*c))
        .chain(crate::code::RAISED)
    {
        vocabulary.raises(code)?;
    }
    Ok(vocabulary)
}

/// Compiles `tokens` after the library: a program ready to run.
pub fn compile(tokens: &[Token]) -> Result<Machine<Fux>, Failed> {
    compiled(None, tokens).map_err(Failed::from)
}

/// Compiles `tokens` after the library and, for a key's or the prompt's
/// program, its client's config's definitions (`config`); its mistake as
/// fuxth says it.
pub(crate) fn compiled(
    config: Option<&Config>,
    tokens: &[Token],
) -> Result<Machine<Fux>, fuxth::Error> {
    let broken = |f: Failed| fuxth::Error::new(Lang::Contract, f.message, None);
    let mut compiler = READY.with(Clone::clone).map_err(broken)?;
    if let Some(config) = config {
        compiler = configured_compiler(config, compiler)?;
    }
    compiler.add(&handles(tokens)?)?;
    Ok(Machine::new(compiler.finish()?, Limits::default()))
}

/// The libraries and `config`'s definitions, compiled and checked once a
/// thread for each config: a config's bindings, each checked as it binds,
/// and every key's program after.
fn configured_compiler(
    config: &Config,
    mut compiler: Compiler<Fux>,
) -> Result<Compiler<Fux>, fuxth::Error> {
    if let Some(ready) = CONFIGURED.with_borrow(|c| {
        (c.as_ref())
            .filter(|(of, _)| of == config)
            .map(|(_, ready)| ready.clone())
    }) {
        return Ok(ready);
    }
    let (name, text) = &**config;
    compiler.add(&handles(&token::source(name, text)?)?)?;
    let ready = compiler.definitions();
    CONFIGURED.set(Some((Arc::clone(config), ready.clone())));
    Ok(ready)
}

thread_local! {
    /// The standard library and fux's, compiled and checked once a thread,
    /// for every program after.
    static READY: Result<Compiler<Fux>, Failed> = libraries();
    /// The last config's, as `configured_compiler` made it.
    static CONFIGURED: std::cell::RefCell<Option<(Config, Compiler<Fux>)>> =
        const { std::cell::RefCell::new(None) };
}

fn libraries() -> Result<Compiler<Fux>, Failed> {
    let mut compiler = Compiler::new(vocabulary()?);
    compiler.add(&token::source("stdlib.fux", fuxth::STDLIB)?)?;
    compiler.add(&token::source("library.fux", LIBRARY)?)?;
    compiler.prepare();
    Ok(compiler)
}

/// A bare id is the handle it names: `%3` is `str %3 pane`, `@2` is
/// `str @2 tab`, `+1` is `str +1 workspace`; but not where it is the text
/// `str` takes. Nor may `define` or `let` give one as a name: it
/// would always be the handle. Nor may either take the name of a word an
/// id becomes (`pane`): every id would then be theirs.
fn handles(tokens: &[Token]) -> Result<Vec<Token>, fuxth::Error> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut after: Option<&str> = None;
    for token in tokens {
        let id = token.text.parse::<AnyRef>().ok();
        let named = |why: String| fuxth::Error::new(Lang::BadName, why, Some(token));
        let bound = match after {
            Some("let") => "let",
            _ => "defined",
        };
        match (after, id) {
            (Some("let" | "define"), None)
                if Kind::ALL.iter().any(|k| k.name() == &*token.text) =>
            {
                let kind = &token.text;
                return Err(named(format!(
                    "cannot be {bound}: a {kind} id is `str ID {kind}`"
                )));
            }
            (Some("define" | "let"), Some(id)) => {
                return Err(named(format!(
                    "cannot be {bound}: it names a {}",
                    id.kind().name()
                )));
            }
            (None, Some(id)) => {
                let at = |text: &str| Token {
                    text: text.into(),
                    place: token.place.clone(),
                };
                out.extend([at("str"), token.clone(), at(id.kind().name())]);
            }
            (Some(_) | None, _) => out.push(token.clone()),
        }
        // A word that reads the token after it as a word of its own: the
        // text `str` takes, the name `define` or `let` gives.
        let reads = matches!(&*token.text, "str" | "define" | "let");
        after = (after.is_none() && reads).then_some(&*token.text);
    }
    Ok(out)
}

/// Where a program's requests go: a connection made when the first is.
#[derive(Default)]
pub struct Server {
    chosen: Choice,
    requests: Option<client::Requests>,
    /// What stops a run from another thread, if one may.
    stop: Option<Stop>,
}

/// What stops a run from another thread (the prefix then Esc, for a key's
/// program): a flag it looks at between steps and before each request, and
/// the connection of the request under way, if one is, which it shuts down.
#[derive(Clone, Default)]
pub struct Stop {
    flag: Arc<AtomicBool>,
    stream: Arc<Mutex<Option<std::os::unix::net::UnixStream>>>,
}

impl Stop {
    /// Stops the run under way.
    pub fn stop(&self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Some(stream) = self.stream.lock().ok().and_then(|mut s| s.take()) {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    /// `stream` is the connection of the request under way (`None`: no
    /// request is): what a stop shuts down.
    fn hold(&self, stream: Option<std::os::unix::net::UnixStream>) {
        if let Ok(mut held) = self.stream.lock() {
            *held = stream;
        }
    }

    /// Ready for the next run.
    pub fn reset(&self) {
        self.flag.store(false, Ordering::SeqCst);
    }

    /// Whether the run is to stop.
    pub fn stopped(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

impl Server {
    pub fn new(chosen: Choice) -> Server {
        Server {
            chosen,
            requests: None,
            stop: None,
        }
    }

    /// A server whose runs `stop` can stop.
    pub fn stoppable(chosen: Choice, stop: Stop) -> Server {
        Server {
            stop: Some(stop),
            ..Server::new(chosen)
        }
    }

    /// The server for what follows.
    pub fn choose(&mut self, chosen: Choice) {
        self.chosen = chosen;
        self.requests = None;
    }

    /// Its socket.
    pub fn socket(&self) -> Result<SocketPath, Failed> {
        crate::socket::socket_path(&self.chosen).map_err(|e| client_failed(e.into()))
    }
}

/// A run stopped from another thread (`Stop`).
fn cancelled() -> Failed {
    Failed::new(Code::Cancelled, "stopped (the prefix, then Esc)")
}

fn client_failed(error: client::Error) -> Failed {
    Failed::new(error.code(), error.to_string())
}

/// What a program's requests go to, and what does what the client does
/// itself: a server (`Server`), the command line's (`Cli`), or in tests a
/// session itself.
pub trait Asks {
    /// Asks `asked`, what is streamed before its reply going to `out`.
    fn ask(&mut self, asked: &Asked, out: &mut dyn Write) -> Result<Reply, Failed>;
    /// Does what the client does itself: by default, nothing, refused.
    fn local(&mut self, what: &Local, _out: &mut dyn Write) -> Result<Vec<Value<Fux>>, Failed> {
        refuse_local(what)
    }
}

/// The server's: each request asked on one connection, opened at the
/// first.
impl Asks for Server {
    fn ask(&mut self, asked: &Asked, out: &mut dyn Write) -> Result<Reply, Failed> {
        let requests = match &mut self.requests {
            Some(requests) => requests,
            None => {
                let socket = self.socket()?;
                let opened = client::Requests::open(&socket).map_err(client_failed)?;
                self.requests.insert(opened)
            }
        };
        // Held while it asks, so a stop ends a request that waits; a stop
        // that came first is seen once it is held, so none is missed.
        if let Some(stop) = &self.stop {
            stop.hold(requests.stopper());
        }
        let reply = match self.stop.as_ref().is_some_and(Stop::stopped) {
            true => Err(cancelled()),
            false => requests.ask(asked, out).map_err(client_failed),
        };
        // Let go of before the flag is read: a stop from then on cannot
        // shut what is kept.
        if let Some(stop) = &self.stop {
            stop.hold(None);
        }
        // A failed request's connection is not asked again, nor one a stop
        // may have shut, however the request ended.
        if reply.is_err() || self.stop.as_ref().is_some_and(Stop::stopped) {
            self.requests = None;
        }
        reply
    }
}

/// The command line's: a server, the client's own words, where the program
/// came from, and the status `exit` gave, once it has.
pub struct Cli {
    server: Server,
    from: Given,
    status: Option<u8>,
}

/// Where a command line's program came from.
#[derive(Clone, Debug, PartialEq)]
enum Given {
    /// The words given.
    Words,
    /// stdin, which `input` then cannot read.
    Stdin,
    /// A file, `--file FILE`, and the arguments after it.
    File(Vec<String>),
}

impl Asks for Cli {
    fn ask(&mut self, asked: &Asked, out: &mut dyn Write) -> Result<Reply, Failed> {
        self.server.ask(asked, out)
    }
    fn local(&mut self, what: &Local, _out: &mut dyn Write) -> Result<Vec<Value<Fux>>, Failed> {
        let Local::Shell(shell) = what else {
            return local(what, &mut self.server);
        };
        let string = |s: &str| vec![Value::str(s)];
        Ok(match shell {
            Shell::Args => {
                let args = match &self.from {
                    Given::File(args) => args.iter().map(|a| Value::str(a.as_str())).collect(),
                    Given::Words | Given::Stdin => Vec::new(),
                };
                let list = fuxth::List::new(args)
                    .ok_or_else(|| Failed::new(Code::Usage, "more arguments than a list holds"))?;
                vec![Value::List(std::rc::Rc::new(list))]
            }
            Shell::Input => match self.from {
                Given::Stdin => {
                    let why = "stdin: the program itself was read from it";
                    return Err(Failed::new(Code::Read, why));
                }
                Given::Words | Given::File(_) => string(&read_text("stdin", io::stdin())?),
            },
            Shell::ReadFile(path) => string(&read_file(path.as_ref())?),
            Shell::Getenv(name) => string(&getenv(name)?),
            Shell::Warn(line) => {
                writeln!(io::stderr(), "{line}")
                    .map_err(|e| Failed::new(Code::Output, format!("writing to stderr: {e}")))?;
                Vec::new()
            }
            Shell::Exit(status) => {
                self.status = Some(*status);
                Vec::new()
            }
        })
    }
}

/// A name an environment variable can have: not empty, and with no `=` or
/// NUL.
#[derive(Clone, Debug, PartialEq)]
pub struct VarName(String);

impl std::str::FromStr for VarName {
    type Err = String;
    fn from_str(name: &str) -> Result<VarName, String> {
        match name.is_empty() || name.contains(['=', '\0']) {
            true => Err(format!("{name:?}: no variable is named so")),
            false => Ok(VarName(name.to_owned())),
        }
    }
}

/// The value of the environment variable `name`: one not set fails
/// `unset_var`, never an empty string; one that is not text is refused.
fn getenv(VarName(name): &VarName) -> Result<String, Failed> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(std::env::VarError::NotPresent) => {
            Err(Failed::new(Code::UnsetVar, format!("{name} is not set")))
        }
        Err(std::env::VarError::NotUnicode(_)) => Err(Failed::new(
            Code::BadValue,
            format!("{name}: its value is not text"),
        )),
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
    fn ask(&mut self, asked: &Asked, out: &mut dyn Write) -> Result<Reply, Failed> {
        self.asks.ask(asked, out)
    }
    fn local(&mut self, what: &Local, out: &mut dyn Write) -> Result<Vec<Value<Fux>>, Failed> {
        let Local::Open(open) = what else {
            return self.asks.local(what, out);
        };
        let (asks, context) = (&mut *self.asks, &self.context);
        let mut sink = io::sink();
        let screen = open.build(&mut |command| {
            asks.ask(
                &Asked {
                    request: command.into(),
                    context: context.clone(),
                },
                &mut sink,
            )
        })?;
        self.opened = Some(screen);
        Ok(Vec::new())
    }
}

/// The steps a run takes between looks at its budget.
const SLICE: u64 = 100_000;

/// Runs `machine` to its end, asking `asks` what its words ask; its stack
/// at the end. It fails once it has taken more than `steps` steps, if
/// given, or once `stop` is.
pub fn run_capped(
    machine: &mut Machine<Fux>,
    host: &mut Host<'_>,
    asks: &mut dyn Asks,
    steps: Option<u64>,
    stop: Option<&Stop>,
) -> Result<Vec<Value<Fux>>, Failed> {
    loop {
        if stop.is_some_and(Stop::stopped) {
            return Err(cancelled());
        }
        if let Some(steps) = steps.filter(|steps| machine.steps() > *steps) {
            // No try may name it: it ends the run, said at the step reached.
            let failed = Failed::new(Code::Steps, format!("ran more than {steps} steps"));
            pushed(machine, Err(failed))?;
        }
        // A slice ends one step past the budget, never later.
        let slice = steps.map_or(SLICE, |steps| {
            SLICE.min(steps.saturating_sub(machine.steps()).saturating_add(1))
        });
        match machine.run(host, slice).map_err(|e| uncaught(machine, e))? {
            Outcome::Finished => return Ok(machine.stack().to_vec()),
            Outcome::Paused => {}
            Outcome::Suspended(Ask::Server {
                request,
                client,
                then,
            }) => {
                let mut context = host.context.clone();
                context.client = client.or(context.client);
                let asked = Asked {
                    request: *request,
                    context,
                };
                let reply = asks.ask(&asked, host.out);
                let values = reply.and_then(|reply| {
                    // A failure that answered too (a wait's): its answer is
                    // left, printed before why it failed, unless a `try`
                    // catches the failure.
                    if let End::Failed { message, .. } = &reply.end
                        && !message.trim().is_empty()
                        && !reply.text.trim().is_empty()
                    {
                        let answer = Reply {
                            end: End::Done,
                            ..reply.clone()
                        };
                        // Only what it answered: its failure is raised
                        // once, below.
                        let answer = answered(answer, then).unwrap_or_default();
                        pushed(machine, Ok(answer))?;
                    }
                    answered(reply, then)
                });
                pushed(machine, values)?;
            }
            Outcome::Suspended(Ask::Local(what)) => {
                let values = asks.local(&what, host.out);
                // An exit the host took ends the run here, as it stands.
                if let (Local::Shell(Shell::Exit(_)), Ok(_)) = (&what, &values) {
                    return Ok(machine.stack().to_vec());
                }
                pushed(machine, values)?
            }
        }
    }
}

/// Pushes what a word asked for gave; or raises its failure there, for a
/// `try` to catch, else placed at the word.
fn pushed(
    machine: &mut Machine<Fux>,
    values: Result<Vec<Value<Fux>>, Failed>,
) -> Result<(), Failed> {
    let raise = |machine: &mut Machine<Fux>, code: fuxth::Code, message: String| {
        let failure = fuxth::Failure::new(code, message);
        machine.raise(failure).map_err(|e| uncaught(machine, e))
    };
    let values = match values {
        Ok(values) => values,
        Err(failed) => return raise(machine, failed.code.into(), failed.message),
    };
    // A stack too full for them is the word's failure too, for a `try`.
    for value in values {
        if let Err(full) = machine.push(value) {
            return raise(machine, full.code, full.message);
        }
    }
    Ok(())
}

/// A failure no `try` caught, as fux tells it, from `machine` where it
/// failed: one inside a word of the prelude's or library's is placed at
/// the program's step that called it, with the bundled word's own step in
/// the message, and how the program's word is used.
fn uncaught(machine: &Machine<Fux>, mut error: fuxth::Error) -> Failed {
    let bundled = |place: Option<&fuxth::Place>| {
        matches!(place, Some(fuxth::Place::Line { source, .. })
            if matches!(&**source, "stdlib.fux" | "library.fux"))
    };
    if !bundled(error.place.as_ref()) {
        return telling(error);
    }
    let Some(step) = machine
        .calls()
        .into_iter()
        .rev()
        .find(|t| !bundled(Some(&t.place)))
    else {
        return telling(error);
    };
    let inner = error.word.as_deref().unwrap_or_default();
    error.message = format!("inside {}, {inner}: {}", step.text, error.message);
    error.place = Some(step.place);
    error.word = Some(step.text);
    told(error, true)
}

/// A reply, or its failure: why, from its message, else from what it said.
pub fn succeeded(reply: Reply) -> Result<Reply, Failed> {
    match reply.end {
        End::Failed { code, message } => {
            let message = match (message.trim_end(), reply.text.trim_end()) {
                ("", text) => text,
                (message, _) => message,
            };
            Err(Failed::new(code, message))
        }
        End::Done => Ok(reply),
    }
}

/// What a reply puts on the stack, `then` saying what it is; a failure,
/// with its status, if it failed: why, else what it answered.
pub fn answered(reply: Reply, then: Then) -> Result<Vec<Value<Fux>>, Failed> {
    let reply = succeeded(reply)?;
    let text = reply.text.trim_end_matches('\n').to_owned();
    let json = Some(reply.json.trim_end().to_owned()).filter(|j| !j.is_empty());
    // An action's answer is its pane's, when the reply holds one pane's
    // version alone (a wait on a tab's panes answers each).
    let version = match reply.versions.as_slice() {
        [one] => Some(*one),
        _ => None,
    };
    let action = |text: String, json: Option<String>| {
        Answer::value(text, json, version.map_or(Said::Panes, Said::Action))
    };
    Ok(match then {
        Then::Nothing => Vec::new(),
        // A pane an action started: its answer, which is its pane too.
        Then::Handle(Kind::Pane) if version.is_some() => vec![action(text, json)],
        Then::Handle(kind) => match reply.ids.iter().rev().find(|id| id.kind() == kind) {
            Some(id) => vec![Value::App(handle(*id))],
            None => {
                return Err(Failed::new(
                    Code::NoTarget,
                    format!("the server named no {}", kind.name()),
                ));
            }
        },
        Then::Answer(said) => vec![Answer::value(text, json, said)],
        Then::Text => vec![Value::str(text)],
        Then::Value => vec![match reply.value {
            Some(Scalar::Int(n)) => Value::Int(n),
            Some(Scalar::Bool(b)) => Value::Bool(b),
            Some(Scalar::Str(s)) => Value::str(s),
            // JSON that is no one value of fuxth's: `null`, a list, an
            // object, as text that `json` gives as it is.
            None => Answer::value(text.clone(), Some(text), Said::Json),
        }],
        Then::Outcome => vec![action(text, json)],
    })
}

/// A handle for `id`.
pub fn handle(id: AnyRef) -> Fux {
    match id {
        AnyRef::Pane(p) => Fux::Pane(p),
        AnyRef::Tab(t) => Fux::Tab(t),
        AnyRef::Workspace(w) => Fux::Workspace(w),
    }
}

/// What a program does that the client does itself, from the command line:
/// attaching, choosing, starting and stopping a server, and its status.
fn local(what: &Local, server: &mut Server) -> Result<Vec<Value<Fux>>, Failed> {
    match what {
        Local::Server(chosen) => server.choose(chosen.clone()),
        Local::Attach {
            workspace,
            read_only,
            size,
        } => {
            if size.is_none() && std::env::var_os("FUX_PANE").is_some_and(|p| !p.is_empty()) {
                let why = "this is a fux pane: attaching here would show fux inside itself \
                           (unset FUX_PANE to do it anyway)";
                return Err(Failed::new(Code::Nested, why));
            }
            // Before a server is started for it, which would be left behind.
            if size.is_none() && !client::on_one_terminal() {
                return Err(client_failed(client::Error::NotATerminal));
            }
            if size.is_none() && client::window_size().is_none() {
                return Err(client_failed(client::Error::NoSize));
            }
            // One that only watches starts nothing: no server for it.
            let socket = match read_only {
                true => server.socket()?,
                false => started(server)?.0,
            };
            if *read_only && !client::answers(&socket) {
                let why = "no server answers: there is nothing to watch";
                return Err(Failed::new(Code::NoServer, why));
            }
            let (keymap, problem) = configured(server);
            let attach = client::Attaching {
                workspace: workspace.clone(),
                read_only: *read_only,
                keymap,
                problem,
            };
            client::attach(&socket, attach, *size).map_err(client_failed)?;
        }
        Local::Start => {
            if started(server)?.1
                && let (_, Some(problem)) = configured(server)
            {
                let _ = writeln!(io::stderr(), "fux: {problem}");
            }
        }
        Local::ForceKill => client::kill_server(&server.socket()?).map_err(client_failed)?,
        Local::Open(_) | Local::Shell(_) => return refuse_local(what),
        Local::Status => {
            let socket = server.socket()?;
            let probe = client::probe(&socket).map_err(client_failed)?;
            let (text, json) =
                crate::status(&socket, &probe).map_err(|why| Failed::new(Code::NoServer, why))?;
            return Ok(vec![Answer::value(text, Some(json), Said::Status)]);
        }
    }
    Ok(Vec::new())
}

/// The server, started in the background if none answers: its socket, and
/// whether it was started, for the config's settings (`set`) to go to it at
/// once.
fn started(server: &mut Server) -> Result<(SocketPath, bool), Failed> {
    let socket = server.socket()?;
    let start = !client::answers(&socket);
    if start {
        client::start_server(&socket).map_err(client_failed)?;
    }
    Ok((socket, start))
}

/// The client's keymap, its config run on the defaults; and what was wrong
/// with the config, if anything was.
pub fn configured(server: &mut Server) -> (Keymap, Option<String>) {
    match config_keymap(Some(server)) {
        Ok(keymap) => (keymap, None),
        // Half a config is no config: the defaults, and why; none, if they
        // are what failed (the failure names keys.fux).
        Err(failed) => match default_keys() {
            Ok(defaults) => (defaults, Some(format!("config: {failed}"))),
            Err(broken) => (Keymap::empty(), Some(broken.to_string())),
        },
    }
}

/// The default keys: the bundled config `keys.fux`, run on no bindings.
pub fn default_keys() -> Result<Keymap, Failed> {
    let mut keymap = Keymap::empty();
    let dry = &mut Settings::default();
    run_quiet("keys.fux", KEYS, Context::default(), Some(&mut keymap), dry)?;
    Ok(keymap)
}

/// The keymap the config file makes (`run_file`), if there is one; else
/// the defaults.
fn config_keymap(server: Option<&mut Server>) -> Result<Keymap, Failed> {
    match crate::config::default_path().filter(|p| p.exists()) {
        Some(path) => run_file(&path, server),
        None => default_keys(),
    }
}

/// The keymap a config file makes, its words run on the defaults, its
/// settings sent to `server` once it has run, as one request the server
/// puts in force whole, or with none sent (a dry run); or why it fails. A
/// config that fails as it runs sends none; one whose answer is lost may
/// have been put in force, whole. A config attaches nothing, and chooses
/// no other server.
pub fn run_file(path: &std::path::Path, server: Option<&mut Server>) -> Result<Keymap, Failed> {
    let shown = path.display().to_string();
    let text = read_file(path)?;
    let context = client::context();
    let mut keymap = Keymap {
        config: Some(std::sync::Arc::new((shown.clone(), text.clone()))),
        ..default_keys()?
    };
    let mut settings = Settings::default();
    run_quiet(
        &shown,
        &text,
        context.clone(),
        Some(&mut keymap),
        &mut settings,
    )?;
    if let Some(server) = server
        && !settings.0.is_empty()
    {
        let request = crate::command::Command::Set(settings.0).into();
        succeeded(server.ask(&Asked { request, context }, &mut io::sink())?)?;
    }
    Ok(keymap)
}

/// Runs the program `text`, read from `name`, its requests going to `asks`
/// and its output nowhere, for at most `PROGRAM_STEPS` steps.
fn run_quiet(
    name: &str,
    text: &str,
    context: Context,
    keymap: Option<&mut Keymap>,
    asks: &mut dyn Asks,
) -> Result<(), Failed> {
    let mut machine = compile(&token::source(name, text)?)?;
    let mut out = io::sink();
    let mut host = Host::new(context, keymap, &mut out);
    run_capped(&mut machine, &mut host, asks, Some(PROGRAM_STEPS), None).map(drop)
}

/// What a config's requests go to: settings alone, as a config runs each
/// time a client attaches, and must make nothing. Each is kept, in order,
/// for the run's end: half a config is no config.
#[derive(Default)]
struct Settings(Vec<crate::config::Setting>);

impl Asks for Settings {
    fn ask(&mut self, asked: &Asked, _: &mut dyn Write) -> Result<Reply, Failed> {
        let Request::Do(crate::command::Command::Set(settings)) = &asked.request else {
            return Err(Failed::new(
                Code::NotConfig,
                "a config sets and binds: it runs each time a client attaches, so it makes and changes nothing else",
            ));
        };
        self.0.extend(settings.iter().cloned());
        Ok(Reply::default())
    }
}

/// A program that may not do what the client does itself: a config's or a
/// key's.
pub fn refuse_local(what: &Local) -> Result<Vec<Value<Fux>>, Failed> {
    let why = match what {
        Local::Open(_) => "a client's screen opens from its own keys or prompt",
        Local::Attach { .. }
        | Local::Server(_)
        | Local::Start
        | Local::Status
        | Local::ForceKill
        | Local::Shell(_) => "runs from the command line only",
    };
    Err(Failed::new(Code::NotHere, why))
}

/// The most steps a program the client runs takes (a config, a key's or a
/// screen's): one that loops without end fails rather
/// than spinning the client.
pub const PROGRAM_STEPS: u64 = 10_000_000;

/// The `fux` binary's words: attaches with none, runs `-` (stdin) or a file,
/// or runs the words given. What is left on the stack is printed, a value a
/// line. Its exit status (0, or what `exit` gave), or why it failed.
pub fn main(args: &[String], out: &mut dyn Write) -> Result<u8, Failed> {
    let (tokens, from) = program(args)?;
    // Only words can be a file mistaken for a program.
    let mut machine = compiled(None, &tokens).map_err(|e| match from {
        Given::Words => Failed::from(a_file(e, args)),
        Given::Stdin | Given::File(_) => Failed::from(e),
    })?;
    let mut host = Host::new(client::context(), None, out);
    let mut cli = Cli {
        server: Server::default(),
        from,
        status: None,
    };
    let ran = run_capped(&mut machine, &mut host, &mut cli, Some(LINE_STEPS), None);
    // What the words before a failure left is printed too: their answers.
    // The run's failure, if it failed, came first, and is the one told.
    let stack = match &ran {
        Ok(stack) => stack.as_slice(),
        Err(_) => machine.stack(),
    };
    let printed = (stack.iter())
        // An answer with nothing to say (an empty listing) prints no
        // blank line; options or a request, nothing.
        .filter(|value| {
            !matches!(value, Value::App(Fux::Answer(a)) if a.text.is_empty())
                && !matches!(value, Value::App(Fux::Opts(_)))
        })
        .try_for_each(|value| writeln!(host.out, "{}", value.text()))
        .and_then(|()| host.out.flush());
    ran?;
    printed.map_err(|e| Failed::new(Code::Output, format!("writing the output: {e}")))?;
    Ok(cli.status.unwrap_or(0))
}

/// `error`, an unknown word that is a file's path, as the first argument or
/// in the only one (`fux ./job.fux`, `fux 'my job.fux'`), saying how to run
/// it: never run, as no argument is guessed to be a file.
fn a_file(mut error: fuxth::Error, args: &[String]) -> fuxth::Error {
    let word = error.word.as_deref();
    if error.code == Lang::UnknownWord
        && let Some(file) = (args.first())
            .filter(|a| args.len() == 1 || word == Some(a.as_str()))
            .filter(|a| std::path::Path::new(a).is_file())
    {
        let file = crate::words::quote(file);
        error.message += &format!("; {file} is a file, which `fux --file {file}` runs");
    }
    error
}

/// The program `args` give, and where it came from: none attaches; `-` is
/// stdin; `--file FILE ARGS…` a file, the arguments after it its own
/// (`#!/usr/bin/env -S fux --file`); help as other programs give it; any
/// other one argument a whole program, as a shell passes `fux 'ls json'`,
/// never a file, whatever it holds; else each is a word.
fn program(args: &[String]) -> Result<(Vec<Token>, Given), Failed> {
    let words = match args {
        [] => token::arguments(&["attach"]),
        // A program read whole, at most `SOURCE_MAX` of it.
        [dash] if dash == "-" => {
            let text = read_text("stdin", io::stdin())?;
            return Ok((token::source("stdin", &text)?, Given::Stdin));
        }
        [flag, file, rest @ ..] if flag == "--file" => {
            let tokens = token::source(file, &read_file(file.as_ref())?)?;
            return Ok((tokens, Given::File(rest.to_vec())));
        }
        [flag] if flag == "--file" => {
            return Err(Failed::new(
                Code::Usage,
                "--file names the file: fux --file FILE ARGS…",
            ));
        }
        // Help as other programs give it: `help agents TOPIC`, `help json`,
        // `help WORD`, `--help`, `--version`.
        [help, agents, topic] if help == "help" && agents == "agents" => {
            token::arguments(&["str", topic, "help-agents-on"])
        }
        [help, topic] if help == "help" => match topic.as_str() {
            "agents" => token::arguments(&["help-agents"]),
            "json" => token::arguments(&["help-json"]),
            word => token::arguments(&["str", word, "explain"]),
        },
        [flag] if matches!(flag.as_str(), "--help" | "-h") => token::arguments(&["help"]),
        [flag] if matches!(flag.as_str(), "--version" | "-V") => token::arguments(&["version"]),
        [line] => token::source("command line", line)?,
        words => token::arguments(words),
    };
    Ok((words, Given::Words))
}

/// The most steps a command line's program takes: one that loops without
/// end fails rather than spinning.
const LINE_STEPS: u64 = 1_000_000_000;

/// The most text read from a file or stdin may be: a program, or what
/// `read-file` and `input` read.
pub(crate) const SOURCE_MAX: usize = 16 << 20;

fn read_failed(name: &str, why: &str) -> Failed {
    Failed::new(Code::Read, format!("{name}: {why}"))
}

/// The text read from `from`, which `name` names and which may be anything
/// (`/dev/zero`): at most `SOURCE_MAX` bytes of UTF-8.
fn read_text(name: &str, from: impl Read) -> Result<String, Failed> {
    let mut bytes = Vec::new();
    let most = u64::try_from(SOURCE_MAX).unwrap_or(u64::MAX);
    (from.take(most.saturating_add(1)))
        .read_to_end(&mut bytes)
        .map_err(|e| read_failed(name, &e.to_string()))?;
    if bytes.len() > SOURCE_MAX {
        let why = format!("more than {} MiB, the most fux reads", SOURCE_MAX >> 20);
        return Err(read_failed(name, &why));
    }
    String::from_utf8(bytes).map_err(|_| read_failed(name, "not UTF-8 text"))
}

/// The text of the file at `path`, as `read_text` reads it.
fn read_file(path: &std::path::Path) -> Result<String, Failed> {
    let name = path.display().to_string();
    let file = std::fs::File::open(path).map_err(|e| read_failed(&name, &e.to_string()))?;
    read_text(&name, file)
}

#[cfg(test)]
mod tests {
    /// One argument is a whole program, though it holds a `/` or ends
    /// `.fux`: never a file to read, which `--file` names.
    #[test]
    fn one_argument_is_a_program() -> Result<(), super::Failed> {
        let words = |args: &[&str]| -> Result<Vec<String>, super::Failed> {
            let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            let (tokens, given) = super::program(&args)?;
            assert_eq!(given, super::Given::Words);
            Ok(tokens.into_iter().map(|t| t.text.to_string()).collect())
        };
        assert_eq!(
            words(&["%2 str \"ls /tmp\" line"])?,
            ["%2", "str", "ls /tmp", "line"]
        );
        assert_eq!(words(&["./no/such.fux"])?, ["./no/such.fux"]);
        let read = words(&["--file", "./no/such.fux", "b"])
            .err()
            .map(|f| f.code);
        assert_eq!(read, Some(super::Code::Read));
        let none = words(&["--file"]).err().map(|f| f.code);
        assert_eq!(none, Some(super::Code::Usage));
        Ok(())
    }

    /// What a script asks of its process is the command line's alone: a
    /// config's program, as a key's, is refused it.
    #[test]
    fn only_the_command_line_asks_its_process() {
        let words = [
            "args",
            "input",
            "str x read-file",
            "str HOME getenv",
            "str x warn",
            "0 exit",
        ];
        for code in words {
            let dry = &mut super::Settings::default();
            let ran = super::run_quiet("config", code, Default::default(), None, dry);
            assert_eq!(
                ran.err().map(|f| f.code),
                Some(super::Code::NotHere),
                "{code}"
            );
        }
    }

    /// The default keys are a config every line of which binds; what
    /// `list-keys` prints are config lines that make the same keys again,
    /// from any keys.
    #[test]
    fn the_default_keys_are_a_config_list_keys_prints() -> Result<(), super::Failed> {
        let run = |name: &str, text: &str, keymap: &mut super::Keymap| {
            let dry = &mut super::Settings::default();
            super::run_quiet(name, text, Default::default(), Some(keymap), dry)
        };
        let mut keymap = super::default_keys()?;
        let lines = (super::KEYS.lines())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .count();
        assert_eq!(keymap.bindings.all().len(), lines);
        // Quotes, a backslash and a line end, as a config writes them.
        let mine = r#"str M-t str 'me zoom' bind-root str Mine group
            str g str 'me focused horizontal str sh program str "echo it'\''s" arg split drop' bind
            str "a \\ and
            a line" label"#;
        run("mine", mine, &mut keymap)?;
        let mut again = super::Keymap::empty();
        run(
            "other",
            "str C-a prefix str M-n str 'me next-tab' bind-root",
            &mut again,
        )?;
        run("listed", &keymap.describe(), &mut again)?;
        assert_eq!(again, keymap);
        Ok(())
    }

    /// A capped run stops one step past its cap, not at the next slice's
    /// end.
    #[test]
    fn a_cap_is_met_to_the_step() -> Result<(), super::Failed> {
        let mut machine = super::compile(&super::token::source("t", "begin false until")?)?;
        let mut out = std::io::sink();
        let mut host = super::Host::new(Default::default(), None, &mut out);
        let ran = super::run_capped(
            &mut machine,
            &mut host,
            &mut super::Settings::default(),
            Some(20_000),
            None,
        );
        assert_eq!(ran.err().map(|f| f.code), Some(super::Code::Steps));
        assert_eq!(machine.steps(), 20_001);
        Ok(())
    }

    /// Options or a request nothing acts on name the words that would.
    #[test]
    fn what_is_left_names_its_word() {
        for (code, end) in [
            ("%2 str hi paste", "send-keys"),
            ("opts str x in", "run or new-tab"),
        ] {
            let tokens = super::program(&[code.to_owned()]).map(|(tokens, _)| tokens);
            let compiled = super::compile(&tokens.unwrap_or_default());
            let error = compiled.err().map(|e| e.to_string()).unwrap_or_default();
            assert!(
                error.ends_with(&format!("end with {end}")),
                "{code}: {error}"
            );
        }
    }
}
