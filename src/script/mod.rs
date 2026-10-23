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
use crate::command::{AnyRef, Extent, Input, Kind, Mouse, Request, Until, WsRef};
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::keymap::Keymap;
use crate::layout::Axis;
use crate::protocol::{Asked, Context, End, Reply, Scalar};
use crate::socket::{Choice, SocketPath};
use fuxth::{AppValue, Code as Lang, Compiler, Limits, Machine, Outcome, Token, Value, token};
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use words::{WORDS, Word, usage};

/// A config, by its name and text, whose definitions its bindings and
/// prompt see.
pub type Config = std::sync::Arc<(String, String)>;

/// The bundled words, written in fuxth.
pub const LIBRARY: &str = include_str!("library.fux");

/// fux's default keys: a config, run before the user's.
pub const KEYS: &str = include_str!("keys.fux");

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

/// What a wait waits for, as words give it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Waits {
    pub until: Vec<Until>,
    pub timeout: Option<Duration>,
    pub marks: bool,
}

/// Options, keys, waits and a mouse event, as words give them, and what
/// they are on: nothing, for `run`, `new-tab` and `new-workspace`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Opts {
    pub on: Option<On>,
    pub name: Option<String>,
    pub size: Option<fux_vt::Size>,
    pub cwd: Option<std::path::PathBuf>,
    pub env: crate::command::Env,
    pub argv: Vec<String>,
    pub typed: Option<String>,
    pub axis: Option<Axis>,
    pub place: Option<WsRef>,
    pub ids: crate::id::Wanted,
    pub log: Option<String>,
    pub kill: bool,
    pub waits: Waits,
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
}

/// What the server answered, to print as text, or as JSON: the JSON the
/// server gave with it, read at the same moment; and the code naming its
/// status, if it has one of its own (closed, reading, still busy), which
/// goes where the answer goes.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub text: String,
    pub json: Option<String>,
    pub said: Said,
    pub code: Option<Code>,
}

/// What an answer is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Said {
    Capture,
    Listing,
    /// An action's answer of one pane: a word that wants a pane takes it.
    Action(PaneId),
    /// An action's answer of several panes, or of none.
    Several,
    /// JSON that is no one value of fuxth's.
    Json,
    Status,
    Servers,
}

impl Said {
    /// Its kind, as the vocabulary names it.
    fn name(self) -> &'static str {
        match self {
            Said::Capture => "capture",
            Said::Listing | Said::Status | Said::Servers => "listing",
            Said::Action(_) | Said::Several => "answer",
            Said::Json => "json",
        }
    }

    /// What it is, said where it is refused.
    fn kind(self) -> &'static str {
        match self {
            Said::Capture => "a capture",
            Said::Listing => "a listing",
            Said::Action(_) => "an action's answer",
            Said::Several => "an answer of several panes",
            Said::Json => "a JSON value",
            Said::Status => "a status",
            Said::Servers => "a listing of servers",
        }
    }

    /// The pane it is of.
    pub fn pane(self) -> Option<PaneId> {
        match self {
            Said::Action(pane) => Some(pane),
            Said::Capture
            | Said::Listing
            | Said::Several
            | Said::Json
            | Said::Status
            | Said::Servers => None,
        }
    }
}

impl Answer {
    /// An answer, as a value on the stack.
    fn value(text: String, json: Option<String>, said: Said, code: Option<Code>) -> Value<Fux> {
        Value::App(Fux::Answer(Box::new(Answer {
            text,
            json,
            said,
            code,
        })))
    }
}

/// What an application word asks while the program waits.
#[derive(Clone, Debug)]
pub enum Ask {
    /// A request, as the client `client` (or the caller), and what to make
    /// of its reply.
    Server {
        request: Request,
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
    /// The binding `label`, `group` and `repeat` change.
    pub last_bound: Option<Bound>,
    pub out: &'a mut dyn Write,
    /// A command line an agent or a person runs: an action waits for its
    /// pane to settle unless it names what to wait for. Bindings, configs
    /// and restores act at once.
    pub settle: bool,
}

impl<'a> Host<'a> {
    pub fn new(
        context: Context,
        keymap: Option<&'a mut Keymap>,
        out: &'a mut dyn Write,
        settle: bool,
    ) -> Host<'a> {
        Host {
            context,
            keymap,
            last_bound: None,
            out,
            settle,
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
    fn from(error: fuxth::Error) -> Failed {
        // A word given too few values, the wrong kind, or one out of its
        // range, says how it is used.
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
            | Lang::Fail
            | Lang::TooLong
            | Lang::Contract
            | Lang::Host(_) => false,
        };
        told(error, usage)
    }
}

/// `error` as fux tells it, with how its word is used if `usage`, and
/// for options or a request left over, the words that would take them.
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
    let code = Code::from(error.code);
    // What check read says where its mistake is.
    match code == Code::Check {
        true => Failed::new(code, error.message),
        false => Failed::new(code, error.located()),
    }
}

/// The vocabulary fuxth compiles fux's programs with: fux's kinds, the
/// names its words' effects use, and the words.
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
    Ok(vocabulary)
}

/// Compiles `tokens` after the library: a program ready to run.
pub fn compile(tokens: &[Token]) -> Result<Machine<Fux>, Failed> {
    compile_in(None, tokens)
}

/// Compiles `tokens` after the library and, for a key's or the prompt's
/// program, its client's config's definitions (`config`).
pub fn compile_in(config: Option<&Config>, tokens: &[Token]) -> Result<Machine<Fux>, Failed> {
    compiled(config, tokens).map_err(Failed::from)
}

/// `compile_in`, its mistake as fuxth says it.
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

/// Whether the word `text` reads the token after it as a word of its own:
/// the text `str` takes, the name `define` or `let` gives.
pub(crate) fn reads_token(text: &str) -> bool {
    matches!(text, "str" | "define" | "let")
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
        after = (after.is_none() && reads_token(&token.text)).then_some(&*token.text);
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
/// program): a flag it looks at between steps, and what ends its
/// connection's reads under a request that waits.
#[derive(Clone, Default)]
pub struct Stop {
    flag: Arc<AtomicBool>,
    stream: Arc<Mutex<Option<io::PipeWriter>>>,
}

impl Stop {
    /// Stops the run under way.
    pub fn stop(&self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Ok(mut stream) = self.stream.lock() {
            *stream = None;
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
                let mut opened = client::Requests::open(&socket).map_err(client_failed)?;
                // A connection a stop can end.
                if let Some(stop) = &self.stop
                    && let Ok(mut held) = stop.stream.lock()
                {
                    *held = opened.stopper();
                }
                self.requests.insert(opened)
            }
        };
        let reply = requests.ask(asked, out).map_err(client_failed);
        if reply.is_err() {
            self.requests = None;
        }
        reply
    }
}

/// The command line's: a server, and the client's own words.
#[derive(Default)]
pub struct Cli(pub Server);

impl Asks for Cli {
    fn ask(&mut self, asked: &Asked, out: &mut dyn Write) -> Result<Reply, Failed> {
        self.0.ask(asked, out)
    }
    fn local(&mut self, what: &Local, _out: &mut dyn Write) -> Result<Vec<Value<Fux>>, Failed> {
        local(what, &mut self.0)
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

/// Runs `machine` to its end, asking `asks` what its words ask. Its stack
/// at the end.
pub fn run(
    machine: &mut Machine<Fux>,
    host: &mut Host<'_>,
    asks: &mut dyn Asks,
) -> Result<Vec<Value<Fux>>, Failed> {
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
) -> Result<Vec<Value<Fux>>, Failed> {
    loop {
        if stop.is_some_and(Stop::stopped) {
            return Err(Failed::new(
                Code::Cancelled,
                "stopped (the prefix, then Esc)",
            ));
        }
        if let Some(steps) = steps.filter(|steps| machine.steps() > *steps) {
            let failed = Failed::new(Code::Steps, format!("ran more than {steps} steps"));
            return Err(placed(machine, failed));
        }
        match machine.run(host, SLICE).map_err(|e| ours(machine, e))? {
            Outcome::Finished => return Ok(machine.stack().to_vec()),
            Outcome::Paused => {}
            Outcome::Suspended(Ask::Server {
                request,
                client,
                then,
            }) => {
                let mut context = host.context.clone();
                context.client = client.or(context.client);
                let reply = asks
                    .ask(&Asked { request, context }, host.out)
                    .map_err(|f| placed(machine, f))?;
                match &reply.end {
                    // A failure that answered too (a wait's): its answer is
                    // left, printed before why it failed.
                    End::Failed { message, .. }
                        if !message.trim().is_empty() && !reply.text.trim().is_empty() =>
                    {
                        let answer = Reply {
                            end: End::Done,
                            ..reply.clone()
                        };
                        pushed(machine, answered(answer, then))?;
                    }
                    End::Failed { .. } | End::Done | End::Answered(_) => {}
                }
                pushed(machine, answered(reply, then))?;
            }
            Outcome::Suspended(Ask::Local(what)) => pushed(machine, asks.local(&what, host.out))?,
        }
    }
}

/// Pushes what a word asked for gave, or its failure, placed.
fn pushed(
    machine: &mut Machine<Fux>,
    values: Result<Vec<Value<Fux>>, Failed>,
) -> Result<(), Failed> {
    for value in values.map_err(|f| placed(machine, f))? {
        machine.push(value)?;
    }
    Ok(())
}

/// A failure in doing what the word the program stopped at asks, named
/// with that word and where it is, as the language's own errors are.
fn placed(machine: &Machine<Fux>, failed: Failed) -> Failed {
    let Some(at) = machine.at() else {
        return failed;
    };
    let error = fuxth::Error {
        code: failed.code.into(),
        message: failed.message,
        place: Some(at.place),
        word: Some(at.text),
    };
    Failed {
        message: ours(machine, error).message,
        ..failed
    }
}

/// An error placed where the program is: a failure inside a word of the
/// prelude's or library's is named by the program's step that called it,
/// with the bundled word's own step in the message, and how the program's
/// word is used.
fn ours(machine: &Machine<Fux>, mut error: fuxth::Error) -> Failed {
    let bundled = |place: Option<&fuxth::Place>| {
        matches!(place, Some(fuxth::Place::Line { source, .. })
            if matches!(&**source, "stdlib.fux" | "library.fux"))
    };
    if !bundled(error.place.as_ref()) {
        return Failed::from(error);
    }
    let Some(step) = machine
        .calls()
        .into_iter()
        .rev()
        .find(|t| !bundled(Some(&t.place)))
    else {
        return Failed::from(error);
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
        End::Done | End::Answered(_) => Ok(reply),
    }
}

/// What a reply puts on the stack, `then` saying what it is; a failure,
/// with its status, if it failed: why, else what it answered.
pub fn answered(reply: Reply, then: Then) -> Result<Vec<Value<Fux>>, Failed> {
    let reply = succeeded(reply)?;
    let code = reply.end.code();
    let text = reply.text.trim_end_matches('\n').to_owned();
    let json = Some(reply.json.trim_end().to_owned()).filter(|j| !j.is_empty());
    // An action's answer is its pane's, when the reply names one pane alone
    // (a wait on a tab's panes answers each): never read from what the
    // program wrote.
    let mut panes = reply.ids.iter().filter_map(|id| match id {
        AnyRef::Pane(p) => Some(*p),
        AnyRef::Tab(_) | AnyRef::Workspace(_) => None,
    });
    let pane = match (panes.next(), panes.next()) {
        (Some(pane), None) => Some(pane),
        (Some(_), Some(_)) | (None, _) => None,
    };
    let action = |text: String, json: Option<String>| {
        Answer::value(text, json, pane.map_or(Said::Several, Said::Action), code)
    };
    Ok(match then {
        Then::Nothing => Vec::new(),
        Then::Handle(kind) => match reply.ids.iter().rev().find(|id| id.kind() == kind) {
            // A pane an action settled: its answer, which is its pane too.
            Some(AnyRef::Pane(p)) if json.is_some() && pane == Some(*p) => {
                vec![action(text, json)]
            }
            Some(id) => vec![Value::App(handle(*id))],
            None => {
                return Err(Failed::new(
                    Code::NoTarget,
                    format!("the server named no {}", kind.name()),
                ));
            }
        },
        Then::Answer(said) => vec![Answer::value(text, json, said, code)],
        Then::Text => vec![Value::str(text)],
        Then::Value => vec![match reply.value {
            Some(Scalar::Int(n)) => Value::Int(n),
            Some(Scalar::Bool(b)) => Value::Bool(b),
            Some(Scalar::Str(s)) => Value::str(s),
            // JSON that is no one value of fuxth's: `null`, a list, an
            // object, as text that `json` gives as it is.
            None => Answer::value(text.clone(), Some(text), Said::Json, code),
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
/// attaching, choosing and starting a server, and asking after servers.
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
            if size.is_none() && !io::IsTerminal::is_terminal(&io::stdin()) {
                return Err(client_failed(client::Error::NotATerminal));
            }
            let (socket, _) = started(server)?;
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
        Local::Open(_) => return refuse_local(what),
        Local::Status => {
            let socket = server.socket()?;
            let probe = client::probe(&socket).map_err(client_failed)?;
            let (text, json) = crate::status(&socket, &probe);
            return Ok(vec![answer(&text, &json, Said::Status)]);
        }
        Local::Servers => {
            let fail = |e: crate::socket::Error| client_failed(e.into());
            let (text, json) = crate::servers().map_err(fail)?;
            return Ok(vec![answer(&text, &json, Said::Servers)]);
        }
    }
    Ok(Vec::new())
}

/// An answer the client made itself, as text and JSON.
fn answer(text: &str, json: &str, said: Said) -> Value<Fux> {
    let (text, json) = (text.trim_end().to_owned(), json.trim_end().to_owned());
    Answer::value(text, Some(json), said, None)
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
        // Half a config is no config: the defaults, and why. Defaults that
        // fail are what failed, as the config runs on them: then none.
        Err(failed) => (
            default_keys().unwrap_or_else(|_| Keymap::empty()),
            Some(format!("config: {failed}")),
        ),
    }
}

/// The default keys: the bundled config `keys.fux`, run on no bindings.
pub fn default_keys() -> Result<Keymap, Failed> {
    let mut keymap = Keymap::empty();
    let mut dry = Settings(None);
    run_quiet(
        "keys.fux",
        KEYS,
        Context::default(),
        Some(&mut keymap),
        &mut dry,
    )?;
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
/// settings sent to `server`, or with none checked and sent nowhere (a dry
/// run); or why it fails. A config attaches nothing, and chooses no other
/// server.
pub fn run_file(path: &std::path::Path, server: Option<&mut Server>) -> Result<Keymap, Failed> {
    let shown = path.display().to_string();
    let text = read_file(path, SOURCE_MAX)?;
    let context = client::context();
    let mut keymap = Keymap {
        config: Some(std::sync::Arc::new((shown.clone(), text.clone()))),
        ..default_keys()?
    };
    run_quiet(
        &shown,
        &text,
        context,
        Some(&mut keymap),
        &mut Settings(server),
    )?;
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
    let mut host = Host::new(context, keymap, &mut out, false);
    run_capped(&mut machine, &mut host, asks, Some(PROGRAM_STEPS), None).map(drop)
}

/// What a config's requests go to: settings alone, as a config runs each
/// time a client attaches, and must make nothing. With no server, a dry
/// run: each setting checked, none sent.
struct Settings<'a>(Option<&'a mut Server>);

impl Asks for Settings<'_> {
    fn ask(&mut self, asked: &Asked, out: &mut dyn Write) -> Result<Reply, Failed> {
        use crate::command::Reaches as R;
        match asked.request.reaches() {
            R::Settings { option, value } => match &mut self.0 {
                Some(server) => server.ask(asked, out),
                None => match crate::config::Config::default().set(option, value) {
                    Ok(()) => Ok(Reply::default()),
                    Err(crate::config::Refused(code, why)) => Err(Failed::new(code, why)),
                },
            },
            R::Reads
            | R::Shapes
            | R::Streams
            | R::Server
            | R::Buffers { .. }
            | R::Client { .. } => Err(Failed::new(
                Code::NotConfig,
                "a config sets and binds: it runs each time a client attaches, so it makes and changes nothing else",
            )),
        }
    }
}

/// A program that may not do what the client does itself: a config's, a
/// key's, a saved session's.
pub fn refuse_local(what: &Local) -> Result<Vec<Value<Fux>>, Failed> {
    let why = match what {
        Local::Open(_) => "a client's screen opens from its own keys or prompt",
        Local::Attach { .. }
        | Local::Server(_)
        | Local::Start
        | Local::Status
        | Local::Servers
        | Local::ForceKill => "runs from the command line only",
    };
    Err(Failed::new(Code::NotHere, why))
}

/// Brings back a saved session: its program run against the server on
/// `socket`, ids asked for as saved, programs left for `respawn`.
pub fn restore(socket: &SocketPath, path: &std::path::Path, text: &str) -> Result<(), Failed> {
    let mut context = client::context();
    context.restoring = true;
    // Its panes are its own, not a pane's it runs in.
    (context.pane, context.server) = (None, None);
    let mut server = Server::new(Choice::Path(socket.path().to_string_lossy().into_owned()));
    let name = path.display().to_string();
    run_quiet(&name, text, context, None, &mut Layout(&mut server))
}

/// The most steps a program the client runs takes (a config, a saved
/// session, a key's or a screen's): one that loops without end fails rather
/// than spinning the client.
pub const PROGRAM_STEPS: u64 = 10_000_000;

/// What a saved session's requests go to: those that make and shape panes,
/// tabs and workspaces, and find them; never what stops the server, grants,
/// sets, streams events or reaches a client, whatever the file says.
struct Layout<'a>(&'a mut Server);

impl Asks for Layout<'_> {
    fn ask(&mut self, asked: &Asked, out: &mut dyn Write) -> Result<Reply, Failed> {
        use crate::command::Reaches as R;
        let refused = match asked.request.reaches() {
            R::Server => Some("kill-server, grant or revoke"),
            R::Settings { .. } => Some("set"),
            R::Client { .. } => Some("a client's words"),
            // A stream that never ends would hold the restore, and every
            // request waiting for it, for good.
            R::Streams => Some("events"),
            R::Reads | R::Shapes | R::Buffers { .. } => None,
        };
        match refused {
            Some(word) => Err(Failed::new(
                Code::NotLayout,
                format!("{word} in a saved session: it makes and shapes, and does nothing else"),
            )),
            None => self.0.ask(asked, out),
        }
    }
}

/// The `fux` binary's words: attaches with none, runs `-` (stdin) or a file,
/// or runs the words given. What is left on the stack is printed, a value a
/// line. Its status, or why it failed.
pub fn main(args: &[String], out: &mut dyn Write) -> Result<u8, Failed> {
    let mut machine = compile(&program(args)?)?;
    let mut out = Checked { out, failed: None };
    let mut host = Host::new(client::context(), None, &mut out, true);
    let ran = run_capped(
        &mut machine,
        &mut host,
        &mut Cli::default(),
        Some(LINE_STEPS),
        None,
    );
    // What the words before a failure left is printed too: their answers.
    let stack = match &ran {
        Ok(stack) => stack.as_slice(),
        Err(_) => machine.stack(),
    };
    for value in stack {
        // An answer with nothing to say (a wait for quiet, an empty
        // listing) prints no blank line; options or a request, nothing.
        if matches!(value, Value::App(Fux::Answer(a)) if a.text.is_empty())
            || matches!(value, Value::App(Fux::Opts(_)))
        {
            continue;
        }
        let _ = writeln!(host.out, "{}", value.text());
    }
    let _ = host.out.flush();
    let status = code(&ran?).map_or(0, |code| code.status().get());
    match out.failed {
        Some(error) => Err(Failed::new(
            Code::Output,
            format!("writing the output: {error}"),
        )),
        None => Ok(status),
    }
}

/// The program `args` give: none attaches; `-` is stdin; one argument
/// holding a blank is a whole program, as a shell passes `fux 'ls json'`;
/// else one with a `/` or ending `.fux` names a file; else each is a word.
fn program(args: &[String]) -> Result<Vec<Token>, Failed> {
    Ok(match args {
        [] => token::arguments(&["attach"]),
        // A program read whole, at most `SOURCE_MAX` of it.
        [dash] if dash == "-" => {
            token::source("stdin", &read_text("stdin", io::stdin(), SOURCE_MAX)?)?
        }
        [line] if line.contains(char::is_whitespace) => token::source("command line", line)?,
        [file] if file.contains('/') || file.ends_with(".fux") => {
            token::source(file, &read_file(file.as_ref(), SOURCE_MAX)?)?
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
        words => token::arguments(words),
    })
}

/// The most steps a command line's program takes: one that loops without
/// end fails rather than spinning.
const LINE_STEPS: u64 = 1_000_000_000;

/// The code naming the status of the first value, in order, that is an
/// answer with a status of its own, or a list holding one (an answer
/// dropped or taken by a word took its status with it). A run that
/// finished exits with its status, else 0.
fn code(values: &[Value<Fux>]) -> Option<Code> {
    values.iter().find_map(|value| match value {
        Value::App(Fux::Answer(answer)) => answer.code,
        Value::List(list) => code(list.items()),
        Value::Int(_) | Value::Bool(_) | Value::Str(_) => None,
        Value::App(
            Fux::Pane(_)
            | Fux::Tab(_)
            | Fux::Workspace(_)
            | Fux::Client(_)
            | Fux::Opts(_)
            | Fux::Extent(_),
        ) => None,
    })
}

/// The most a program read from a file or stdin may be, and how that is
/// said.
const SOURCE_MAX: (usize, &str) = (16 << 20, "16 MiB: no program is that long");

fn read_failed(name: &str, why: &str) -> Failed {
    Failed::new(Code::Read, format!("{name}: {why}"))
}

/// The text read from `from`, which `name` names and which may be anything
/// (`/dev/zero`): at most `limit` bytes of UTF-8, `said` saying how many.
fn read_text(name: &str, from: impl Read, (limit, said): (usize, &str)) -> Result<String, Failed> {
    let mut bytes = Vec::new();
    (from.take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1)))
        .read_to_end(&mut bytes)
        .map_err(|e| read_failed(name, &e.to_string()))?;
    if bytes.len() > limit {
        return Err(read_failed(name, &format!("more than {said}")));
    }
    String::from_utf8(bytes).map_err(|_| read_failed(name, "not UTF-8 text"))
}

/// The text of the file at `path`, as `read_text` reads it.
fn read_file(path: &std::path::Path, most: (usize, &str)) -> Result<String, Failed> {
    let name = path.display().to_string();
    let file = std::fs::File::open(path).map_err(|e| read_failed(&name, &e.to_string()))?;
    read_text(&name, file, most)
}

/// Output that keeps its first failure, so that a run whose output cannot
/// be written fails, however its words wrote it.
struct Checked<'a> {
    out: &'a mut dyn Write,
    failed: Option<io::Error>,
}

impl Write for Checked<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.out.write(bytes);
        self.keep(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        let flushed = self.out.flush();
        self.keep(flushed)
    }
}

impl Checked<'_> {
    /// `result`, its error kept if it is the first.
    fn keep<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        if let Err(error) = &result
            && self.failed.is_none()
        {
            self.failed = Some(io::Error::new(error.kind(), error.to_string()));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    /// A whole program in one argument is a program, though a word in it
    /// holds a `/`: never a file to read.
    #[test]
    fn one_argument_with_a_blank_is_a_program() -> Result<(), super::Failed> {
        let tokens = super::program(&["%2 str \"ls /tmp\" line".to_owned()])?;
        let words: Vec<&str> = tokens.iter().map(|t| &*t.text).collect();
        assert_eq!(words, ["%2", "str", "ls /tmp", "line"]);
        Ok(())
    }

    /// The default keys are a config every line of which binds; what
    /// `list-keys` prints are config lines that make the same keys again,
    /// from any keys.
    #[test]
    fn the_default_keys_are_a_config_list_keys_prints() -> Result<(), super::Failed> {
        let run = |name: &str, text: &str, keymap: &mut super::Keymap| {
            let mut dry = super::Settings(None);
            super::run_quiet(name, text, Default::default(), Some(keymap), &mut dry)
        };
        let mut keymap = super::default_keys()?;
        let lines = super::KEYS.lines().filter(|l| l.contains(" bind ")).count();
        assert_eq!(keymap.bindings.all().len(), lines);
        run(
            "mine",
            "str M-t str 'me zoom' bind-root str Mine group",
            &mut keymap,
        )?;
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

    /// Options or a request nothing acts on name the words that would.
    #[test]
    fn what_is_left_names_its_word() {
        for (code, end) in [
            ("%2 str hi paste", "send-keys"),
            ("opts str x in", "run or new-tab"),
        ] {
            let compiled = super::compile(&super::program(&[code.to_owned()]).unwrap_or_default());
            let error = compiled.err().map(|e| e.to_string()).unwrap_or_default();
            assert!(
                error.ends_with(&format!("end with {end}")),
                "{code}: {error}"
            );
        }
    }
}
