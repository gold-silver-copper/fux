//! fux's command line, config, bindings and prompt: fuxth programs, run in
//! a client. Words build typed requests (`opts … new-tab`, `%2 … send-keys`)
//! and execution words submit them: the program suspends while the server
//! does what one asks, and goes on with what it answers, as typed values
//! (a pane's handle, a capture), never text to be read back.
//!
//! Every run is fresh: its own stack and dictionary, the bundled library's
//! words first (`library.fux`).
mod callback;
#[cfg(test)]
mod lists;
mod record;
pub(crate) mod settings;
mod source;
mod words;

use crate::client;
use crate::client_state::{ClientState, Draft};
use fux_server::socket::SocketPath;
use fux_wire::answer::{PaneAnswer, PaneResults};
use fux_wire::code::{Code, Raise};
use fux_wire::command::{
    AnyRef, Extent, HistoryBytes, Input, Limit, Program, Request, Run, Version, WsRef,
};
use fux_wire::id::{ClientId, PaneId, TabId, WsId};
use fux_wire::layout::Axis;
use fux_wire::protocol::{Asked, Body, Context, End, Reply, Response};
use fuxth::{AppValue, Code as Lang, Machine, Outcome, Token, Value, token};
use std::io::{self, Read, Write};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub use callback::admit;
pub(crate) use words::NAMES;
pub use words::{Act, After, Group, Keys, Opt, Row, Screen, Takes, Word, from_opts, usage};

/// The sources a client's config was read from, whose definitions its
/// bindings and prompt see.
pub type Config = Arc<source::Frozen>;

/// The bundled words, written in fuxth.
const LIBRARY: &str = include_str!("library.fux");

/// The first line of a fuxth script, which runs it as `fux --file`.
pub const SHEBANG: &str = "#!/usr/bin/env -S fux --file";

/// fux's default keys: a config, run before the user's.
const KEYS: &str = include_str!("keys.fux");
pub(crate) const DEFAULTS: &str = include_str!("client-defaults.fux");

/// A value of fux's own on the stack.
#[derive(Clone, Debug, PartialEq)]
pub enum Fux {
    Null,
    Record(Rc<record::Record>),
    Pane(PaneId, Option<Run>),
    Tab(TabId, Option<Run>),
    Workspace(WsId, Option<Run>),
    Client(ClientId, Option<Run>),
    /// The whole session.
    All,
    /// Options for `new-tab` or `new-workspace` (from `opts`); or a
    /// request: a pane, tab, workspace or client given options, keys or a
    /// wait, for the word that acts on it after
    /// (`%2 str ls text str Enter key send-keys`).
    Opts(Rc<Opts>),
    /// A width or height: cells, or a share of the tab.
    Extent(Extent),
    /// What the server said: a capture or a wait's outcome.
    Answer(Rc<Answer>, Option<Run>),
}

impl AppValue for Fux {
    fn kind(&self) -> &'static str {
        if let Fux::Answer(answer, _) = self {
            return answer.names().1;
        }
        let kind = words::KINDS.iter().find(|(name, ..)| *name == self.name());
        kind.map_or("a value of fux's", |(_, said, _)| said)
    }

    fn name(&self) -> &'static str {
        match self {
            Fux::Null => "null",
            Fux::Record(_) => "record",
            Fux::Pane(_, _) => "pane",
            Fux::Tab(_, _) => "tab",
            Fux::Workspace(_, _) => "workspace",
            Fux::Client(_, _) => "client",
            Fux::All => "all",
            Fux::Opts(opts) => match opts.on {
                None => "opts",
                Some(On::Target(AnyRef::Pane(_)) | On::Version(_)) => "pane-request",
                Some(On::Target(AnyRef::Tab(_))) => "tab-request",
                Some(On::Target(AnyRef::Workspace(_))) => "workspace-request",
                Some(On::Client(_)) => "client-request",
                Some(On::Changed(_)) => "changed-request",
                Some(On::Seen(_)) => "expected-request",
            },
            Fux::Extent(_) => "size",
            Fux::Answer(answer, _) => answer.names().0,
        }
    }

    fn write_text(&self, out: &mut dyn std::fmt::Write) -> Result<(), fuxth::Failure> {
        match self {
            Fux::Null => out.write_str("null"),
            Fux::Record(r) => r.write(out),
            Fux::Pane(p, _) => write!(out, "{p}"),
            Fux::Tab(t, _) => write!(out, "{t}"),
            Fux::Workspace(w, _) => write!(out, "{w}"),
            Fux::Client(c, _) => write!(out, "{c}"),
            Fux::Answer(answer, _) => answer.write_text(out),
            Fux::Extent(Extent::Cells(n)) => write!(out, "{n}"),
            Fux::Extent(Extent::Percent(n)) => write!(out, "{}%", n.get()),
            Fux::All => write!(out, "all"),
            Fux::Opts(_) => write!(out, "({})", self.kind()),
        }
        .map_err(|_| fuxth::Failure::new(Code::Output.into(), "the text could not be written"))
    }
}

/// The server run a value was returned by, retained through request options.
fn scope(value: &Value<Fux>) -> Option<Run> {
    match value {
        Value::App(
            Fux::Pane(_, run)
            | Fux::Tab(_, run)
            | Fux::Workspace(_, run)
            | Fux::Client(_, run)
            | Fux::Answer(_, run),
        ) => *run,
        Value::App(Fux::Opts(opts)) => opts.of,
        Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_)
        | Value::Xt(_)
        | Value::Mark(_)
        | Value::Failure(_)
        | Value::Addr(_) => None,
    }
}

fn same_run(a: Option<Run>, b: Option<Run>) -> Result<Option<Run>, fuxth::Failure> {
    match (a, b) {
        (Some(a), Some(b)) if a != b => Err(fuxth::Failure::new(
            Code::from(Raise::BadValue).into(),
            "handles are from different server runs",
        )),
        (a, b) => Ok(a.or(b)),
    }
}

/// Options, keys and waits, as words give them, and what
/// they are on: nothing, for `new-tab` and `new-workspace`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Opts {
    pub on: Option<On>,
    pub of: Option<Run>,
    pub name: Option<String>,
    pub size: fux_wire::command::Size,
    pub cwd: Option<std::path::PathBuf>,
    pub argv: Vec<String>,
    pub shell: Option<bool>,
    pub history_bytes: Option<HistoryBytes>,
    pub reflow: Option<bool>,
    pub alternate_scroll_lines: Option<fux_wire::command::ScrollLines>,
    pub picture_bytes: Option<fux_wire::command::PictureBytes>,
    pub program_clipboard: Option<bool>,
    pub axis: Option<Axis>,
    /// A split's shares (`shares`).
    pub shares: Option<(std::num::NonZeroU32, std::num::NonZeroU32)>,
    /// The workspaces `in` names, each in the one before.
    pub place: Vec<WsRef>,
    pub kill: bool,
    /// Attaching only to watch (`watch`).
    pub watch: bool,
    /// How long a wait waits before it times out.
    pub timeout: Option<Limit>,
    pub view: fux_wire::command::CaptureView,
    pub history: Option<usize>,
    pub inputs: Vec<Input>,
    /// The words given, for the word they go on to refuse any it would not
    /// use.
    pub given: Vec<words::Opt>,
}

impl Opts {
    pub(crate) fn pane_options(
        &self,
        defaults: fux_wire::command::PaneOptions,
    ) -> fux_wire::command::PaneOptions {
        fux_wire::command::PaneOptions {
            reflow: self.reflow.unwrap_or(defaults.reflow),
            alternate_scroll_lines: self
                .alternate_scroll_lines
                .unwrap_or(defaults.alternate_scroll_lines),
            picture_bytes: self.picture_bytes.unwrap_or(defaults.picture_bytes),
            history_bytes: self.history_bytes.unwrap_or(defaults.history_bytes),
            program_clipboard: self.program_clipboard.unwrap_or(defaults.program_clipboard),
        }
    }

    /// Its program and arguments, moved out: nothing copied.
    pub(crate) fn program(&mut self) -> Result<Program, String> {
        Program::from_argv(std::mem::take(&mut self.argv))
    }

    pub(crate) fn launch(&self, shell: &Program) -> Result<fux_wire::command::Start, String> {
        let argv = if self.argv.is_empty() {
            shell.clone()
        } else {
            Program::from_argv(self.argv.clone())?
        };
        Ok(match self.shell {
            Some(false) => fux_wire::command::Start::Program(argv),
            _ => fux_wire::command::Start::Shell(argv),
        })
    }
}

/// What a request is on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum On {
    Target(AnyRef),
    Client(ClientId),
    /// The version held by an action's or wait's answer: a wait on it
    /// waits from that version.
    Version(Version),
    /// An explicit wait baseline, accepted only by `wait`.
    Changed(Version),
    /// A version a request expects (`expect`): keys go to its pane only
    /// while the pane is still at it.
    Seen(Version),
}

/// What the server answered, as data: printed, its text; given to `json`,
/// its JSON; each made of the one value, read once.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// An action's or wait's on one pane: a word that wants a pane takes
    /// it, and a wait waits from its version.
    Pane(PaneAnswer),
    /// An action's or wait's on a tab or workspace: each pane's that
    /// answered, which is no one pane.
    Panes(Vec<PaneAnswer>),
    Capture(Box<fux_wire::answer::Captured>),
}

impl Answer {
    fn names(&self) -> (&'static str, &'static str) {
        match self {
            Answer::Pane(PaneAnswer::Closed(_)) => ("closed-answer", "a closed pane's answer"),
            Answer::Pane(_) => ("answer", "an action's answer"),
            Answer::Panes(_) => ("answers", "an answer of a tab's or workspace's panes"),
            Answer::Capture(c) => match c.of {
                fux_wire::answer::Of::Pane { .. } => ("capture", "a capture"),
                fux_wire::answer::Of::Client(_) => ("client-capture", "a client's capture"),
            },
        }
    }

    fn write_text(&self, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
        match self {
            Answer::Pane(answer) => answer.write_text(out),
            Answer::Panes(answers) => {
                for (i, answer) in answers.iter().enumerate() {
                    if i > 0 {
                        out.write_char('\n')?;
                    }
                    answer.write_text(out)?;
                }
                Ok(())
            }
            Answer::Capture(captured) => captured.write_text(out),
        }
    }

    pub(crate) fn write_json(&self, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
        match self {
            Answer::Pane(answer) => answer.write_json(out),
            Answer::Panes(answers) => {
                out.write_char('[')?;
                for (i, answer) in answers.iter().enumerate() {
                    if i > 0 {
                        out.write_char(',')?;
                    }
                    answer.write_json(out)?;
                }
                out.write_char(']')
            }
            Answer::Capture(captured) => captured.write_json(out),
        }
    }

    /// It says nothing printed: a capture of no lines, no pane's answers.
    /// A capture of one blank line says that line.
    fn is_empty(&self) -> bool {
        match self {
            Answer::Capture(captured) => captured.lines.is_empty(),
            Answer::Panes(answers) => answers.is_empty(),
            Answer::Pane(_) => false,
        }
    }

    /// The version it holds: a pane's answer's, which a wait on it waits
    /// from; or a pane's capture's, its pane's as it was read, which
    /// `expect` sends at and `changed` waits from.
    pub fn version(&self) -> Option<Version> {
        match self {
            Answer::Pane(answer) => answer.version(),
            Answer::Capture(captured) => match captured.of {
                fux_wire::answer::Of::Pane { version, .. } => Some(version),
                fux_wire::answer::Of::Client(_) => None,
            },
            Answer::Panes(_) => None,
        }
    }

    fn in_run(self, run: Option<Run>) -> Value<Fux> {
        Value::App(Fux::Answer(Rc::new(self), run))
    }
}

/// What an application word asks while the program waits.
#[derive(Clone, Debug)]
pub enum Ask {
    /// A request, as the client `client` (or the caller).
    Server {
        request: Box<Request>,
        client: Option<ClientId>,
        of: Option<Run>,
    },
    /// Something the client itself does that takes over (attaching), or
    /// talks to no server (`status`).
    Local(Local),
    /// The program's own dictionary's records: the word a name names
    /// (`word`), or every word (`dictionary`).
    Dictionary(Option<String>),
}

/// The keys of a binding just made.
#[derive(Clone, Debug, PartialEq)]
pub enum Bound {
    /// Keys after the prefix.
    Prefix(Vec<fux_vt::keys::KeyPress>),
    /// One key, bound without the prefix.
    Root(fux_vt::keys::KeyPress),
    /// One key of a screen of fux's own.
    In(crate::keymap::Modal, fux_vt::keys::KeyPress),
}

/// What the client does itself.
#[derive(Clone, Debug, PartialEq)]
pub enum Local {
    /// This terminal, or a client of no terminal of `size`.
    Attach {
        of: Option<Run>,
        target: Option<fux_wire::command::AttachTarget>,
        read_only: bool,
        size: Option<fux_vt::Size>,
        creation: Box<Opts>,
    },
    Start,
    Status,
    /// Open one of the client's own screens: a client running its keys'
    /// programs does (`client::Keyboard`); nothing else has one.
    Open(fux_wire::callback::Open),
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
/// candidate client state, and what its output goes to.
pub struct Host<'a> {
    pub context: Context,
    /// The whole candidate, changed only by local setters.
    pub state: Option<&'a mut Draft>,
    /// The binding `label`, `group` and `repeat` change.
    pub last_bound: Option<Bound>,
    pub out: &'a mut dyn Write,
    cli: bool,
    /// Whether the word running failed on what one of its values holds,
    /// not on the value: told as the word said it (`words::described`).
    held: bool,
    /// Where `include` and `require` read files.
    pub(crate) files: source::Files,
}

impl<'a> Host<'a> {
    pub fn new(context: Context, state: Option<&'a mut Draft>, out: &'a mut dyn Write) -> Host<'a> {
        Host {
            context,
            state,
            last_bound: None,
            out,
            cli: false,
            held: false,
            files: source::Files::default(),
        }
    }

    fn cli(context: Context, state: &'a mut Draft, out: &'a mut dyn Write) -> Self {
        Self {
            cli: true,
            ..Self::new(context, Some(state), out)
        }
    }

    /// A key's or a prompt's host: its program reads no files (its
    /// config's definitions are read already).
    pub(crate) fn job(context: Context, state: &'a mut Draft, out: &'a mut dyn Write) -> Self {
        Self {
            files: source::Files::sealed(),
            ..Self::new(context, Some(state), out)
        }
    }

    /// Ordinary CLI commands need no keys or preference initialization.
    fn prepare(&mut self) -> Result<(), Failed> {
        if self.cli
            && let Some(state) = self.state.as_deref_mut()
            && !state.initialized()
        {
            *state = default_state()?.draft();
        }
        Ok(())
    }
}

/// A run's failure: its code, cause, source when known, and the steps
/// that had taken effect before it (D-25).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed {
    pub code: Code,
    pub message: String,
    pub place: Option<token::Place>,
    pub actions: Vec<Token>,
}

impl Failed {
    /// A failure of kind `code`, and why.
    pub fn new(code: impl Into<Code>, message: impl Into<String>) -> Failed {
        Failed {
            code: code.into(),
            message: message.into(),
            place: None,
            actions: Vec::new(),
        }
    }

    /// The failure of a run on `machine`, with the steps that took effect
    /// on it.
    fn after(self, machine: &Machine<Fux>) -> Failed {
        Failed {
            actions: machine.actions().cloned().collect(),
            ..self
        }
    }

    /// The cause before its source, for a notice with limited room, then
    /// the steps that had taken effect.
    pub fn notice(&self) -> String {
        let message = match &self.place {
            Some(place) => format!("{} ({place})", self.message),
            None => self.message.clone(),
        };
        let mut notice = Failed::new(self.code, message).to_string();
        for (i, step) in self.actions.iter().enumerate() {
            let said = if i == 0 {
                "; already took effect:"
            } else {
                ","
            };
            notice.push_str(&format!("{said} {}: {}", step.place, step.text));
        }
        notice
    }

    /// Each step that had taken effect, a line each, as stderr says it.
    fn taken(&self) -> impl Iterator<Item = String> + '_ {
        let said = |step: &Token| format!("already took effect: {}: {}", step.place, step.text);
        self.actions.iter().map(said)
    }

    /// The source followed by the cause, for command-line diagnostics.
    fn located(&self) -> String {
        match &self.place {
            Some(place) => format!("{place}: {}", self.message),
            None => self.message.clone(),
        }
    }
}

impl std::error::Error for Failed {}

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
        let lines: Vec<String> = (self.located().lines().map(str::trim))
            .filter(|l| !l.is_empty())
            .map(|line| line.chars().map(escaped).collect())
            .collect();
        write!(f, "{}: {}", self.code, lines.join("\\n"))
    }
}

impl From<fux_wire::code::Error> for Failed {
    fn from(error: fux_wire::code::Error) -> Failed {
        Failed::new(error.code(), error.to_string())
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
    let usage = matches!(error.code, Lang::StackUnderflow | Lang::Type | Lang::Range);
    told(error, usage)
}

/// `error` as fux tells it, with how its word is used if `usage`, and
/// for options or a request left over, the words that would take them: a
/// failure of its code, saying where and why it happened.
fn told(mut error: fuxth::Error, usage: bool) -> Failed {
    let left = words::KINDS
        .iter()
        .any(|(.., why)| *why == Some(&*error.message));
    if error.code == Lang::Unused && left {
        let word = error.word.as_deref().and_then(Word::named);
        error.message += &format!(": end with {}", words::takers(word));
    }
    if usage && let Some(usage) = error.word.as_deref().and_then(words::usage) {
        error.message.push_str(&format!(" ({usage})"));
    }
    if error.code == Lang::UnknownWord {
        error.message.push_str(" (`fux help` lists every word)");
    }
    let place = error.place.take();
    Failed {
        place,
        ..Failed::new(Code::of(error.code), error.located())
    }
}

/// The vocabulary fuxth runs fux's programs with: the kinds that must be
/// used, the words, the codes they raise, and the ids it reads as handles:
/// `%3` is `str %3 pane`, `@2` is `str @2 tab`, `+1` is `str +1
/// workspace`, which no name may be.
pub fn vocabulary() -> Result<fuxth::Vocabulary, fuxth::Error> {
    let mut vocabulary = fuxth::Vocabulary::default();
    for (kind, _, unused) in words::KINDS {
        if let Some(why) = unused {
            vocabulary.must_use(kind, why)?;
        }
    }
    for word in Word::ALL {
        vocabulary.add(word.row().name, word.row().effect)?;
    }
    vocabulary.fails(|id| Some(Word::ALL.get(id)?.codes()));
    vocabulary.writes(&[Code::Output.into()]);
    for code in Code::all().filter(|c| c.raised()) {
        vocabulary.raises(code.into())?;
    }
    vocabulary.literals(|text| Some(text.parse::<AnyRef>().ok()?.kind().name()));
    Ok(vocabulary)
}

/// The bundled dictionary.
fn base() -> Result<Machine<Fux>, Failed> {
    READY.with(|ready| ready.as_ref().map(|(m, _)| m.clone()).map_err(Clone::clone))
}

/// A client's dictionary: the bundled one, the bundled keys' words, and
/// its config's definitions.
fn client_base(config: Option<&Config>) -> Result<Machine<Fux>, Failed> {
    let keyed = KEYED.with(Clone::clone)?;
    match config {
        Some(config) => configured_machine(config, keyed),
        None => Ok(keyed),
    }
}

/// A machine with the library, to read `tokens`: a program ready to run.
pub fn compile(tokens: &[Token]) -> Result<Machine<Fux>, Failed> {
    let mut machine = base()?;
    machine.read(tokens.to_vec(), None);
    Ok(machine)
}

/// A machine with a client's dictionary (`client_base`), for a key's or
/// the prompt's program, to read `tokens`.
pub(crate) fn compiled(config: Option<&Config>, tokens: &[Token]) -> Result<Machine<Fux>, Failed> {
    let mut machine = client_base(config)?;
    machine.read(tokens.to_vec(), None);
    Ok(machine)
}

fn callback_failed(f: fuxth::Failure) -> Failed {
    Failed::new(Code::of(f.code), f.message)
}

/// A key's or a callback's machine: its immutable arguments on the stack,
/// to be used up, each code value among them compiled in its dictionary;
/// then its code, compiled there, to run. Or a line typed at the prompt,
/// to read as a program is. Compiling stops at `stop`.
pub(crate) fn callback_machine(
    config: Option<&Config>,
    program: &crate::screens::Program,
    args: Option<&[fux_wire::callback::Data]>,
    stop: Option<&Stop>,
) -> Result<Machine<Fux>, Failed> {
    let mut machine = client_base(config)?;
    let code = match program {
        crate::screens::Program::Line(line) => {
            machine.read(token::source("key", line)?, None);
            return Ok(machine);
        }
        crate::screens::Program::Code(code) => code,
    };
    for arg in args.unwrap_or_default() {
        let value = callback::value(arg, &mut |code| code_value(&mut machine, code, stop))?;
        machine.push(value);
    }
    let xt = code_value(&mut machine, code, stop)?;
    machine.push(xt);
    if args.is_some() {
        machine.callback();
    }
    machine.read(token::source("key", "execute")?, None);
    Ok(machine)
}

/// `code`, compiled in `machine`'s dictionary as `code … end` compiles
/// its words: its xt. Its immediate words run as a key's program runs,
/// reading no file and stopping at `stop`, and ask nothing of the server;
/// they close only what they open. Its words are said at `key`.
fn code_value(
    machine: &mut Machine<Fux>,
    code: &fux_wire::callback::Code,
    stop: Option<&Stop>,
) -> Result<Value<Fux>, Failed> {
    let place = token::Place::Line {
        source: Arc::from("key"),
        line: 1,
    };
    let word = |w: &str| Token {
        text: Rc::from(w),
        place: place.clone(),
    };
    let words = code.words.iter().map(String::as_str);
    let tokens = ["code"].into_iter().chain(words).chain(["end"]).map(word);
    let depth = machine.stack().len();
    machine.read(tokens.collect(), None);
    let mut out = io::sink();
    let mut host = Host::new(Context::default(), None, &mut out);
    host.files = source::Files::sealed();
    run(machine, &mut host, &mut Compiling, stop)?;
    match machine.pop() {
        Ok(xt @ Value::Xt(_)) if machine.stack().len() == depth => Ok(xt),
        _ => Err(Failed::new(
            Code::BadValue,
            "a code value's words close what they did not open",
        )),
    }
}

/// What compiling a code value asks: nothing of the server.
struct Compiling;

impl Asks for Compiling {
    fn ask(&mut self, _: &Asked) -> Result<Reply, Failed> {
        let why = "a code value's words ask nothing of the server as it is compiled";
        Err(Failed::new(Code::Client, why))
    }
}

/// The dictionary a config leaves: the config read again, from the
/// sources it was read from, on a client state of its own that is let go,
/// as it changes nothing else. Cached for this thread's last config.
fn configured_machine(config: &Config, mut machine: Machine<Fux>) -> Result<Machine<Fux>, Failed> {
    if let Some(ready) = CONFIGURED.with_borrow(|c| {
        (c.as_ref())
            .filter(|(of, _)| of == config)
            .map(|(_, ready)| ready.clone())
    }) {
        return Ok(ready);
    }
    let mut draft = default_state()?.draft();
    machine.read(config.tokens()?, config.path.clone());
    let mut out = io::sink();
    let mut host = Host::new(Context::default(), Some(&mut draft), &mut out);
    host.files = source::Files::frozen(Arc::clone(config));
    run(&mut machine, &mut host, &mut ConfigOnly::default(), None)?;
    machine.clear();
    CONFIGURED.set(Some((Arc::clone(config), machine.clone())));
    Ok(machine)
}

/// The bundled dictionary and the bundled keys' own words, which their
/// bindings run: the keys read on a client state of their own that is let
/// go (`default_state` binds them).
fn keyed() -> Result<Machine<Fux>, Failed> {
    let mut machine = base()?;
    machine.read(token::source("keys.fux", KEYS)?, None);
    quiet(&mut machine, Some(&mut Draft::default()), false)?;
    machine.clear();
    Ok(machine)
}

thread_local! {
    /// The libraries, read once a thread, and their help.
    static READY: Result<(Machine<Fux>, Docs), Failed> = libraries();
    /// The bundled keys', as `keyed` made it.
    static KEYED: Result<Machine<Fux>, Failed> = keyed();
    /// The last config's, as `configured_machine` made it.
    static CONFIGURED: std::cell::RefCell<Option<(Config, Machine<Fux>)>> =
        const { std::cell::RefCell::new(None) };
}

/// The libraries' definitions' help, as fuxth read it.
pub type Docs = Rc<[Rc<fuxth::Doc>]>;

/// The client's libraries, read; the bench counts this initialization
/// cold. Its mistake is told without a word's usage, which would read
/// `READY` as it is made.
pub fn libraries() -> Result<(Machine<Fux>, Docs), Failed> {
    let told = |error| told(error, false);
    let mut machine = Machine::new(vocabulary().map_err(told)?);
    let mut out = io::sink();
    let mut host = Host::new(Context::default(), None, &mut out);
    let mut docs = (machine.library("stdlib.fux", fuxth::STDLIB, &mut host)).map_err(told)?;
    docs.extend(
        machine
            .library("library.fux", LIBRARY, &mut host)
            .map_err(told)?,
    );
    Ok((machine, docs.into()))
}

/// The standard library's and fux's library's words, as help says them.
/// Libraries that fail to read leave none, but no help asks then: every
/// program, `help` too, fails first with why.
pub fn docs() -> Docs {
    READY.with(|ready| {
        ready
            .as_ref()
            .map_or_else(|_| Rc::from([]), |(_, docs)| Rc::clone(docs))
    })
}

/// Where a program's requests go: a connection made when the first is.
#[derive(Default)]
pub struct Server {
    /// Its socket, if not `FUX_SOCKET`'s or the default's.
    socket: Option<std::path::PathBuf>,
    requests: Option<client::Requests>,
    /// This job's source fence, kept on every request and across retries.
    pub after: Option<std::rc::Rc<std::cell::Cell<fux_wire::protocol::Fence>>>,
    /// What stops a run from another thread, if one may.
    stop: Option<Stop>,
    /// Whether a server of another protocol is followed.
    follow: Follow,
    /// A command line's: set once its stdout's reader is gone.
    out: Option<Rc<std::cell::Cell<bool>>>,
}

/// Whether a run follows a server of another protocol: runs its command
/// line again with the server's own binary (`client::follow`).
#[derive(Default)]
enum Follow {
    /// Not a command line's: a key's or a config's program, whose client
    /// stays.
    #[default]
    Never,
    /// The `fux` binary's command line, until it has acted.
    Command { rerun: client::Rerun, acted: Acted },
}

/// Whether a command line's program did anything outside its process yet,
/// which running it again would do twice. Everything it does outside goes
/// through one of two places, which mark it: its requests and local actions
/// (`Cli`'s `Asks`), each once done, so that the connection that met another
/// protocol is not one; and its stdout (`Watched`), before each write.
#[derive(Clone, Default)]
struct Acted(Rc<std::cell::Cell<bool>>);

impl Acted {
    fn mark(&self) {
        self.0.set(true);
    }
    fn marked(&self) -> bool {
        self.0.get()
    }
}

/// A command line's stdout, where all it prints goes: which marks its
/// program acted, and notes its reader gone (a closed pipe).
struct Watched<'a> {
    out: &'a mut dyn Write,
    acted: Acted,
    closed: Rc<std::cell::Cell<bool>>,
}

impl Watched<'_> {
    fn noted<T>(&self, wrote: io::Result<T>) -> io::Result<T> {
        if let Err(error) = &wrote
            && error.kind() == io::ErrorKind::BrokenPipe
        {
            self.closed.set(true);
        }
        wrote
    }
}

impl Write for Watched<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.acted.mark();
        let wrote = self.out.write(bytes);
        self.noted(wrote)
    }
    fn flush(&mut self) -> io::Result<()> {
        let flushed = self.out.flush();
        self.noted(flushed)
    }
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

    /// Refuse a cancelled job before it takes another step.
    pub(crate) fn check(&self) -> Result<(), Failed> {
        if self.stopped() {
            Err(cancelled())
        } else {
            Ok(())
        }
    }
}

impl Server {
    /// The server on `socket`.
    pub fn new(socket: std::path::PathBuf) -> Server {
        Server {
            socket: Some(socket),
            ..Self::default()
        }
    }

    /// The server on `socket`, whose runs `stop` can stop.
    pub fn stoppable(socket: std::path::PathBuf, stop: Stop) -> Server {
        Server {
            stop: Some(stop),
            ..Server::new(socket)
        }
    }

    /// Its socket.
    pub fn socket(&self) -> Result<SocketPath, Failed> {
        fux_server::socket::socket_path(self.socket.as_deref()).map_err(|e| client_failed(e.into()))
    }

    /// A connection's failure, as a run's: a server of another protocol is
    /// followed first, if this run follows one.
    fn failed(&self, error: client::Error) -> Failed {
        client_failed(match (&self.follow, error) {
            (Follow::Command { rerun, acted }, client::Error::Mismatch { said, server })
                if !acted.marked() =>
            {
                client::follow(said, server, rerun)
            }
            (Follow::Command { .. }, client::Error::Mismatch { said, .. }) => {
                client::Error::Unfollowed {
                    said,
                    why: "the program already did something outside fux's own process \
                          (printed, warned, asked a server, started or stopped one, or read \
                          stdin), which running it again would do twice"
                        .to_owned(),
                }
            }
            (_, error) => error,
        })
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
    /// Asks one bounded request and returns its typed reply.
    fn ask(&mut self, asked: &Asked) -> Result<Reply, Failed>;
    /// Publish a complete local state at a commit boundary, if it is shared.
    fn published(&mut self, _state: &ClientState, _context: &Context) {}

    /// Commit all local state.
    fn commit(&mut self, draft: &mut Draft, context: Context) -> Result<(), Failed> {
        let committed = draft
            .clone()
            .finish()
            .map_err(|why| Failed::new(Code::Config, why))?;
        *draft = committed.draft();
        self.published(&committed, &context);
        Ok(())
    }

    /// Pending preferences must arrive before a later server operation.
    fn checkpoint(&mut self, draft: &mut Draft, context: Context) -> Result<(), Failed> {
        if draft.preferences.changed() {
            self.commit(draft, context)?;
        }
        Ok(())
    }

    /// Does what the client does itself: by default, nothing, refused.
    fn local(
        &mut self,
        what: &Local,
        _state: Option<&mut Draft>,
        _out: &mut dyn Write,
    ) -> Result<Vec<Value<Fux>>, Failed> {
        refuse_local(what)
    }
}

/// The server's: each request asked on one connection, opened at the
/// first.
impl Asks for Server {
    fn ask(&mut self, asked: &Asked) -> Result<Reply, Failed> {
        let fenced = self.after.as_ref().map(|after| Asked {
            after: Some(after.get()),
            ..asked.clone()
        });
        let asked = fenced.as_ref().unwrap_or(asked);
        let requests = match &mut self.requests {
            Some(requests) => requests,
            None => {
                let socket = self.socket()?;
                let opened = client::Requests::open(&socket).map_err(|e| self.failed(e))?;
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
            false => (requests.ask(asked, self.out.as_deref())).map_err(client_failed),
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
/// came from, the status `exit` gave, once it has, and whether it acted.
pub struct Cli {
    server: Server,
    from: Given,
    status: Option<u8>,
    acted: Acted,
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
    fn ask(&mut self, asked: &Asked) -> Result<Reply, Failed> {
        let reply = self.server.ask(asked);
        self.acted.mark();
        reply
    }
    fn local(
        &mut self,
        what: &Local,
        state: Option<&mut Draft>,
        _out: &mut dyn Write,
    ) -> Result<Vec<Value<Fux>>, Failed> {
        let values = self.local_act(what, state);
        // What only reads leaves the program as it was to run again; so does
        // a start, which starts nothing where a server answers. A file
        // that may not read the same again marks it itself (`read_file`).
        let reads = matches!(
            what,
            Local::Start
                | Local::Status
                | Local::Shell(Shell::Args | Shell::ReadFile(_) | Shell::Getenv(_))
        );
        if !reads {
            self.acted.mark();
        }
        values
    }
}

impl Cli {
    /// Does what the command line's program asks of its process, or the
    /// client does itself.
    fn local_act(
        &mut self,
        what: &Local,
        state: Option<&mut Draft>,
    ) -> Result<Vec<Value<Fux>>, Failed> {
        let Local::Shell(shell) = what else {
            return local(what, &mut self.server, state);
        };
        let string = |s: &str| vec![Value::str(s)];
        Ok(match shell {
            Shell::Args => {
                let args = match &self.from {
                    Given::File(args) => args.iter().map(|a| Value::str(a.as_str())).collect(),
                    Given::Words | Given::Stdin => Vec::new(),
                };
                let list = fuxth::List::new(args);
                vec![Value::List(Rc::new(list))]
            }
            Shell::Input => match self.from {
                Given::Stdin => {
                    let why = "stdin: the program itself was read from it";
                    return Err(Failed::new(Code::Read, why));
                }
                Given::Words | Given::File(_) => string(&read_text("stdin", io::stdin())?),
            },
            Shell::ReadFile(path) => string(&read_file(path.as_ref(), &self.acted)?),
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
    pub on_commit: Option<&'a mut dyn FnMut(&ClientState)>,
    /// What stops compiling a screen's code values as it opens.
    pub stop: Option<&'a Stop>,
}

impl Asks for AsClient<'_> {
    fn published(&mut self, state: &ClientState, context: &Context) {
        if let Some(publish) = &mut self.on_commit {
            publish(state);
        }
        self.asks.published(state, context);
    }
    fn ask(&mut self, asked: &Asked) -> Result<Reply, Failed> {
        self.asks.ask(asked)
    }
    fn local(
        &mut self,
        what: &Local,
        state: Option<&mut Draft>,
        out: &mut dyn Write,
    ) -> Result<Vec<Value<Fux>>, Failed> {
        match what {
            Local::Open(open) => {
                let screen = admit(open.clone(), state.as_deref(), &self.context, self.stop)?;
                self.opened = Some(screen);
                Ok(Vec::new())
            }
            // The client's own environment: its person's.
            Local::Shell(Shell::Getenv(name)) => Ok(vec![Value::str(getenv(name)?)]),
            Local::Attach { .. } | Local::Start | Local::Status | Local::Shell(_) => {
                self.asks.local(what, state, out)
            }
        }
    }
}

/// The steps a run takes between looks at `stop`.
const SLICE: u64 = 100_000;

/// Runs `machine` to its end, asking `asks` what its words ask; its stack
/// at the end. It fails once `stop` is, if given; else it runs till it
/// ends, or till the user or the OS stops it. Its failure holds the steps
/// that had taken effect.
pub fn run(
    machine: &mut Machine<Fux>,
    host: &mut Host<'_>,
    asks: &mut dyn Asks,
    stop: Option<&Stop>,
) -> Result<Vec<Value<Fux>>, Failed> {
    steps(machine, host, asks, stop).map_err(|failed| failed.after(machine))
}

/// `run`'s steps.
fn steps(
    machine: &mut Machine<Fux>,
    host: &mut Host<'_>,
    asks: &mut dyn Asks,
    stop: Option<&Stop>,
) -> Result<Vec<Value<Fux>>, Failed> {
    loop {
        if let Some(stop) = stop {
            stop.check()?;
        }
        match machine.run(host, SLICE).map_err(|e| uncaught(machine, e))? {
            Outcome::Finished => return Ok(machine.stack().to_vec()),
            Outcome::Paused => {}
            Outcome::Suspended(Ask::Server {
                request,
                client,
                of,
            }) => {
                if let Some(draft) = host.state.as_deref_mut()
                    && let Err(failed) = asks.checkpoint(draft, host.context.clone())
                {
                    pushed(machine, Err(failed))?;
                    continue;
                }
                let mut context = host.context.clone();
                context.client = client.or(context.client);
                context.of = of;
                let asked = Asked {
                    after: None,
                    request: *request,
                    context,
                };
                match asks.ask(&asked) {
                    Ok(reply) => match reply.end() {
                        End::Failed { code, message } => raised(machine, (code, message), reply)?,
                        End::Done => {
                            if acts(&asked.request) {
                                took_effect(machine);
                            }
                            pushed(machine, answered(reply))?
                        }
                    },
                    Err(failed) => pushed(machine, Err(failed))?,
                }
            }
            Outcome::Suspended(Ask::Local(what)) => {
                if matches!(what, Local::Attach { .. })
                    && let Err(failed) = host.prepare()
                {
                    pushed(machine, Err(failed))?;
                    continue;
                }
                if matches!(what, Local::Open(_))
                    && let Some(draft) = host.state.as_deref_mut()
                    && let Err(failed) = asks.checkpoint(draft, host.context.clone())
                {
                    pushed(machine, Err(failed))?;
                    continue;
                }
                let values = asks.local(&what, host.state.as_deref_mut(), host.out);
                // An exit the host took ends the run here, as it stands.
                match (&what, &values) {
                    (Local::Shell(Shell::Exit(_)), Ok(_)) => return Ok(machine.stack().to_vec()),
                    (Local::Attach { .. } | Local::Open(_) | Local::Start, Ok(_)) => {
                        took_effect(machine)
                    }
                    _ => {}
                }
                pushed(machine, values)?
            }
            Outcome::Suspended(Ask::Dictionary(name)) => {
                let records = dictionary(machine, name.as_deref());
                pushed(machine, records)?
            }
        }
    }
}

/// The record of the word `name` names in `machine`'s dictionary now, or,
/// with no name, a list of each word's, in the order they were defined.
fn dictionary(machine: &Machine<Fux>, name: Option<&str>) -> Result<Vec<Value<Fux>>, Failed> {
    let mut known = fuxth::Known::default();
    let mut word = |definition: &fuxth::Definition<'_>| {
        let codes = machine.codes(definition, &mut known);
        record::word(definition, codes.as_deref())
    };
    let Some(name) = name else {
        let records = machine.dictionary().iter().map(&mut word).collect();
        return Ok(vec![Value::List(Rc::new(fuxth::List::new(records)))]);
    };
    match machine.definition(name) {
        Some(definition) => Ok(vec![word(&definition)]),
        None => Err(Failed::new(
            Raise::UnknownWord,
            format!("{name:?}: no such word"),
        )),
    }
}

/// Whether a request changes something, so that a failure after it lists
/// it: all but those that only read.
fn acts(request: &Request) -> bool {
    use fux_wire::command::{ClientAction, Command};
    !matches!(
        request,
        Request::Wait(_)
            | Request::NextEvents { .. }
            | Request::Do(
                Command::Inspect { .. }
                    | Command::CapturePane { .. }
                    | Command::Resolve { .. }
                    | Command::Members { .. }
                    | Command::Client {
                        action: ClientAction::Capture,
                        ..
                    }
            )
    )
}

/// The word the machine waited at took effect: listed, at the program's
/// step that ran it, with the failures after it.
fn took_effect(machine: &mut Machine<Fux>) {
    machine.acted(|t| bundled(&t.place));
}

/// Whether `place` is in the prelude's or the library's source.
fn bundled(place: &fuxth::Place) -> bool {
    matches!(place, fuxth::Place::Line { source, .. }
        if matches!(&**source, "stdlib.fux" | "library.fux"))
}

/// Raises a failed reply's failure at the word. A pane's answer's
/// failure (`PaneAnswer::FAILURES`) carries its answer, as its `value` where a `try`
/// catches it; uncaught, the answer is left, told on stderr after why it failed.
/// Any other fails alone: no other failure answers.
fn raised(
    machine: &mut Machine<Fux>,
    (code, message): (Code, String),
    reply: Reply,
) -> Result<(), Failed> {
    let answer = answered(reply);
    let Ok(Some(answer)) =
        answer.map(|mut a| a.pop().filter(|_| PaneAnswer::FAILURES.contains(&code)))
    else {
        return pushed(machine, Err(Failed::new(code, message)));
    };
    let failure = fuxth::Failure::new(code.into(), message);
    match machine.raise(failure, Some(answer.clone())) {
        Ok(()) => Ok(()),
        Err(error) => {
            // Only the original uncaught wait failure tells its answer.
            if error.code == code.into() {
                machine.push(answer);
            }
            Err(uncaught(machine, error))
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
        machine
            .raise(failure, None)
            .map_err(|e| uncaught(machine, e))
    };
    let values = match values {
        Ok(values) => values,
        Err(failed) => return raise(machine, failed.code.into(), failed.located()),
    };
    for value in values {
        machine.push(value);
    }
    Ok(())
}

/// A failure no `try` caught, as fux tells it, from `machine` where it
/// failed: one inside a word of the prelude's or library's is placed at
/// the program's step that called it, with the bundled word's own step in
/// the message, and how the program's word is used.
/// A failure raised again is placed by the calls where it first happened.
fn uncaught(machine: &Machine<Fux>, mut error: fuxth::Error) -> Failed {
    let step = (error.place.as_ref().is_some_and(bundled))
        .then(|| machine.failed_calls().rev().find(|t| !bundled(&t.place)));
    let Some(step) = step.flatten() else {
        return telling(error);
    };
    let inner = error.word.as_deref().unwrap_or_default();
    error.message = format!("inside {}, {inner}: {}", step.text, error.message);
    error.place = Some(step.place);
    error.word = Some(step.text);
    told(error, true)
}

/// What a reply's answer puts on the stack: nothing, a string, handles, or
/// an answer of fux's; a field's value as fuxth's own where it is one.
pub fn answered(reply: Reply) -> Result<Vec<Value<Fux>>, Failed> {
    let run = reply.run;
    let body = match reply.response {
        Response::Answered(PaneResults::One(answer)) => {
            return Ok(vec![Answer::Pane(answer).in_run(run)]);
        }
        Response::Answered(PaneResults::Many(answers)) => {
            return Ok(vec![Answer::Panes(answers).in_run(run)]);
        }
        Response::Returned(body) => body,
        Response::Failed { .. } => return Ok(Vec::new()),
    };
    let handles = |ids: Vec<AnyRef>| {
        let ids = ids
            .into_iter()
            .map(|id| Value::App(handle(id, run)))
            .collect();
        let list = fuxth::List::new(ids);
        Ok::<_, Failed>(Value::List(Rc::new(list)))
    };
    Ok(vec![match body {
        Body::Empty => return Ok(Vec::new()),
        Body::Found(id) => Value::App(handle(id, run)),
        Body::Handles(ids) => handles(ids)?,
        Body::Capture(captured) => Answer::Capture(captured).in_run(run),
        Body::Snapshot(snapshot) => {
            record::snapshot(*snapshot).map_err(|e| Failed::new(Code::of(e.code), e.message))?
        }
        Body::Events(batch) => {
            record::events(*batch).map_err(|e| Failed::new(Code::of(e.code), e.message))?
        }
        Body::Field(json) => {
            record::from_json(&json).map_err(|e| Failed::new(Code::of(e.code), e.message))?
        }
    }])
}

/// A handle for `id`.
pub fn handle(id: AnyRef, run: Option<Run>) -> Fux {
    match id {
        AnyRef::Pane(p) => Fux::Pane(p, run),
        AnyRef::Tab(t) => Fux::Tab(t, run),
        AnyRef::Workspace(w) => Fux::Workspace(w, run),
    }
}

/// What a program does that the client does itself, from the command line:
/// attaching, starting and stopping a server, and its status.
fn local(
    what: &Local,
    server: &mut Server,
    draft: Option<&mut Draft>,
) -> Result<Vec<Value<Fux>>, Failed> {
    match what {
        Local::Attach {
            of,
            target,
            read_only,
            size,
            creation,
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
            if size.is_none() && !client::waitable() {
                return Err(client_failed(client::Error::Unwaitable));
            }
            if size.is_none() && client::window_size().is_none() {
                return Err(client_failed(client::Error::NoSize));
            }
            let draft = draft.ok_or_else(|| {
                Failed::new(Code::Config, "attach needs this client's local state")
            })?;
            let defaults = draft
                .clone()
                .finish()
                .map_err(|why| Failed::new(Code::Config, why))?;
            let typing = size.is_none() && !read_only;
            let (committed, problem) = if typing {
                configured(&defaults)
            } else {
                (defaults, None)
            };
            let explicit = target.is_some() || creation.given.contains(&words::Opt::Name);
            let target = if explicit {
                target.clone()
            } else {
                committed.attach.target.clone()
            };
            let of = if explicit {
                *of
            } else {
                same_run(*of, committed.attach.of)
                    .map_err(|why| Failed::new(Code::BadValue, why.message))?
            };
            let mut creation = creation.as_ref().clone();
            if !creation.given.contains(&words::Opt::Name) {
                creation.name = committed.attach.name.clone();
            }
            words::check_attach_creation(&creation, target.is_some(), *read_only)
                .map_err(|why| Failed::new(Code::of(why.code), why.message))?;
            // Watching or naming a run starts no replacement server.
            let socket = match *read_only || of.is_some() {
                true => server.socket()?,
                false => started(server, committed.preferences.journal_entries)?,
            };
            if *read_only && !client::answers(&socket) {
                let why = "no server answers: there is nothing to watch";
                return Err(Failed::new(Code::NoServer, why));
            }
            *draft = committed.draft();
            let attach = client::Attaching {
                of,
                target,
                read_only: *read_only,
                creation,
                state: committed,
                problem,
            };
            client::attach(&socket, attach, *size).map_err(|e| server.failed(e))?;
        }
        Local::Start => {
            let draft = draft.ok_or_else(|| {
                Failed::new(Code::Config, "start needs this client's local state")
            })?;
            let journal = (draft.preferences.journal_entries())
                .map_err(|why| Failed::new(Code::Config, why))?;
            started(server, journal)?;
        }
        Local::Open(_) | Local::Shell(_) => return refuse_local(what),
        Local::Status => {
            let socket = server.socket()?;
            let probe = client::probe(&socket).map_err(client_failed)?;
            let status =
                crate::status(&socket, &probe).map_err(|why| Failed::new(Code::NoServer, why))?;
            return Ok(vec![
                record::from_json(&status).map_err(|e| Failed::new(Code::of(e.code), e.message))?,
            ]);
        }
    }
    Ok(Vec::new())
}

/// The server's socket, starting it in the background if none answers,
/// its journal keeping `journal` events.
fn started(
    server: &mut Server,
    journal: fux_wire::command::JournalEntries,
) -> Result<SocketPath, Failed> {
    let socket = server.socket()?;
    if !client::answers(&socket) {
        client::start_server(&socket, journal).map_err(client_failed)?;
    }
    Ok(socket)
}

/// A typing terminal's whole state and one diagnostic if it failed.
pub fn configured(defaults: &ClientState) -> (ClientState, Option<String>) {
    match config_state(defaults) {
        Ok(state) => (state, None),
        Err(failed) => (defaults.clone(), Some(failed.notice())),
    }
}

/// Complete bundled policy and keys. Missing initialization is an error.
pub fn default_state() -> Result<ClientState, Failed> {
    let mut draft = Draft::default();
    for (name, text) in [("client-defaults.fux", DEFAULTS), ("keys.fux", KEYS)] {
        run_quiet(&token::source(name, text)?, Some(&mut draft), true)?;
    }
    draft.finish().map_err(|why| Failed::new(Code::Config, why))
}

/// The config file, a fuxth program a typing terminal runs as it attaches:
/// `$XDG_CONFIG_HOME/fux/config.fux`, else `~/.config/fux/config.fux`.
pub fn default_path() -> Option<std::path::PathBuf> {
    let set = |name: &str| std::env::var_os(name).filter(|d| !d.is_empty());
    let config = set("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| set("HOME").map(|home| std::path::PathBuf::from(home).join(".config")))?;
    Some(config.join("fux").join("config.fux"))
}

fn config_state(defaults: &ClientState) -> Result<ClientState, Failed> {
    let Some(path) = default_path() else {
        return Ok(defaults.clone());
    };
    let config = match source::Frozen::file(&path) {
        Ok(config) => config,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                && path
                    .symlink_metadata()
                    .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
        {
            return Ok(defaults.clone());
        }
        Err(error) => return Err(read_failed(&path.display().to_string(), &error.to_string())),
    };
    run_config_on(config, defaults)
}

/// Reads a config file and runs it without a transport capability, its
/// sources kept for its bindings.
pub fn run_file(path: &std::path::Path) -> Result<ClientState, Failed> {
    let config = source::Frozen::file(path)
        .map_err(|e| read_failed(&path.display().to_string(), &e.to_string()))?;
    run_config_on(config, &default_state()?)
}

fn run_config_on(config: source::Frozen, defaults: &ClientState) -> Result<ClientState, Failed> {
    let mut draft = defaults.draft();
    let mut machine = client_base(None)?;
    machine.read(config.tokens()?, config.path.clone());
    let files = quiet(&mut machine, Some(&mut draft), false)?;
    draft.config = Some(Arc::new(config.with(files)));
    draft.finish().map_err(|why| Failed::new(Code::Config, why))
}

/// Runs config code with only local reads and draft mutations, in a
/// client's dictionary.
fn run_quiet(tokens: &[Token], state: Option<&mut Draft>, diagnostics: bool) -> Result<(), Failed> {
    quiet(&mut compiled(None, tokens)?, state, diagnostics).map(drop)
}

/// Runs `machine`'s config code as `run_quiet` does: the files it read.
fn quiet(
    machine: &mut Machine<Fux>,
    state: Option<&mut Draft>,
    diagnostics: bool,
) -> Result<source::Files, Failed> {
    let mut out = io::sink();
    let mut host = Host::new(Context::default(), state, &mut out);
    run(machine, &mut host, &mut ConfigOnly { diagnostics }, None)?;
    Ok(host.files)
}

/// Config evaluation has no server or connection, even through imports.
#[derive(Default)]
struct ConfigOnly {
    diagnostics: bool,
}

impl Asks for ConfigOnly {
    fn checkpoint(&mut self, _: &mut Draft, _: Context) -> Result<(), Failed> {
        Ok(())
    }
    fn ask(&mut self, _: &Asked) -> Result<Reply, Failed> {
        Err(Failed::new(
            Code::NotConfig,
            "a config changes only its own client state",
        ))
    }
    fn local(
        &mut self,
        what: &Local,
        _: Option<&mut Draft>,
        _: &mut dyn Write,
    ) -> Result<Vec<Value<Fux>>, Failed> {
        match what {
            Local::Shell(Shell::Getenv(name)) => Ok(vec![Value::str(getenv(name)?)]),
            Local::Shell(Shell::Warn(line)) if self.diagnostics => {
                writeln!(io::stderr(), "{line}")
                    .map_err(|e| Failed::new(Code::Output, format!("writing to stderr: {e}")))?;
                Ok(Vec::new())
            }
            Local::Attach { .. }
            | Local::Start
            | Local::Status
            | Local::Open(_)
            | Local::Shell(_) => refuse_local(what),
        }
    }
}

/// A program that may not do what the client does itself: a config's or a
/// key's. No file gave it arguments: `args` is none, as for any program
/// but a file's.
fn refuse_local(what: &Local) -> Result<Vec<Value<Fux>>, Failed> {
    if let Local::Shell(Shell::Args) = what {
        return Ok(vec![Value::List(Rc::new(fuxth::List::new(Vec::new())))]);
    }
    let why = match what {
        Local::Open(_) => "a client's screen opens from its own keys or prompt",
        Local::Attach { .. } | Local::Start | Local::Status | Local::Shell(_) => {
            "runs from the command line only"
        }
    };
    Err(Failed::new(Code::NotHere, why))
}

/// The `fux` binary's words: attaches with none, runs `-` (stdin) or a file,
/// or runs the words given. What is left on the stack is printed, a value a
/// line (`written`). Its exit status (0, what `exit` gave, or `PIPE`), or
/// why it failed, which it has told on stderr, with what was left. With
/// `argv`, the process's whole command line, a server of another protocol
/// is followed: the process runs it again with the server's binary
/// (`client::follow`); with none, as in tests, never.
pub fn main(
    args: &[String],
    argv: Option<Vec<std::ffi::OsString>>,
    out: &mut dyn Write,
) -> Result<u8, Failed> {
    let acted = Acted::default();
    let closed = Rc::new(std::cell::Cell::new(false));
    let mut out = Watched {
        out,
        acted: acted.clone(),
        closed: closed.clone(),
    };
    let mut left = Vec::new();
    match ran(args, argv, &mut out, acted, &mut left) {
        // Its reader gone, fux ends quietly, whatever the program did then,
        // failed or caught it and went on.
        _ if closed.get() => Ok(PIPE),
        // stdout holds only what the program printed. Why it failed goes to
        // stderr, then the steps that had taken effect, then what the words
        // before the failure left, their answers; nowhere is left to say
        // that stderr failed.
        Err(failed) => {
            let mut err = io::stderr();
            let _ = writeln!(err, "fux: {failed}");
            for step in failed.taken() {
                let _ = writeln!(err, "fux: {step}");
            }
            let _ = printed(&left, &mut err, Batch::Whole);
            Err(failed)
        }
        ran => ran,
    }
}

/// The status a shell gives `cat` that SIGPIPE ended, which fux ends with
/// once its stdout's reader is gone (`fux all str '' events | head -1`): so
/// a pipeline under `set -o pipefail` tells a program cut short from one
/// that finished, as with `cat`.
const PIPE: u8 = 128 + 13;

/// `main`'s run, its output going to `out`; failed, what was left in `left`.
fn ran(
    args: &[String],
    argv: Option<Vec<std::ffi::OsString>>,
    out: &mut Watched<'_>,
    acted: Acted,
    left: &mut Vec<Value<Fux>>,
) -> Result<u8, Failed> {
    if matches!(args, [flag] if matches!(flag.as_str(), "--version" | "-V")) {
        writeln!(out, "{}", fux_wire::protocol::Build::OURS).map_err(output_failed)?;
        return Ok(0);
    }
    let (tokens, path, from, program) = program(args)?;
    let again = (argv.clone()).map(|argv| client::Rerun {
        argv,
        program: program.clone(),
    });
    let rerun = argv.map(|argv| client::Rerun { argv, program });
    let mut machine = base().map_err(|failed| uncompiled(failed, again.as_ref()))?;
    // A file with no canonical path (`<(…)`) requires relative to the
    // working directory, as stdin does.
    machine.read(tokens, path.as_ref().and_then(|p| p.canonicalize().ok()));
    let mut state = Draft::default();
    // A program file that may not read the same again (a pipe, `<(…)`)
    // is not read again by the server's binary.
    if path
        .as_ref()
        .is_some_and(|p| !p.metadata().is_ok_and(|m| m.is_file()))
    {
        acted.mark();
    }
    let closed = out.closed.clone();
    let mut host = Host::cli(client::context(), &mut state, out);
    host.files.acted = Some(acted.clone());
    let mut cli = Cli {
        server: Server {
            follow: rerun.map_or(Follow::Never, |rerun| Follow::Command {
                rerun,
                acted: acted.clone(),
            }),
            out: Some(closed),
            ..Server::default()
        },
        from,
        status: None,
        acted,
    };
    let ran = run(&mut machine, &mut host, &mut cli, None);
    // A word this fux lacks, met before the program did anything outside
    // it, may be the server's: its binary is followed.
    let ran = ran.map_err(|failed| match failed.code == Code::of(Lang::UnknownWord) {
        true if !cli.acted.marked() => uncompiled(failed, again.as_ref()),
        true | false => failed,
    });
    match ran {
        // Its output failing, the run still took its steps.
        Ok(stack) => match printed(&stack, host.out, Batch::Events) {
            Ok(()) => Ok(cli.status.unwrap_or(0)),
            Err(failed) => Err(failed.after(&machine)),
        },
        Err(failed) => {
            *left = machine.stack().to_vec();
            Err(failed)
        }
    }
}

/// How a `next-events` batch left on the stack is written.
#[derive(Clone, Copy)]
enum Batch {
    /// On stdout: its events alone, refused if it missed entries.
    Events,
    /// In a failure's report on stderr: its whole record, never refused,
    /// so what comes after it is told too.
    Whole,
}

/// Writes what a run left, a value a line.
fn printed(stack: &[Value<Fux>], out: &mut dyn Write, batch: Batch) -> Result<(), Failed> {
    let mut out = io::BufWriter::with_capacity(64 << 10, out);
    (stack.iter()).try_for_each(|value| written(&mut out, value, batch))?;
    out.flush().map_err(output_failed)
}

/// A command line this fux cannot read, `failed`, which the server's may:
/// a server of another protocol on the socket chosen before the program
/// (`FUX_SOCKET`, else `server.sock`) is followed (`client::follow`) with
/// `rerun`, if given, as nothing it did reached outside. Else why it
/// failed; or, a mismatch found and not followed, the mismatch, with that
/// as context.
fn uncompiled(failed: Failed, rerun: Option<&client::Rerun>) -> Failed {
    let Some(rerun) = rerun else {
        return failed;
    };
    let greeted = Server::default()
        .socket()
        .map(|socket| client::greet(&socket));
    let Ok(Err(client::Error::Mismatch { said, server })) = greeted else {
        return failed;
    };
    let unfollowed = client_failed(client::follow(said, server, rerun));
    Failed {
        message: format!("{}; this fux cannot read it: {failed}", unfollowed.message),
        ..unfollowed
    }
}

fn output_failed(error: io::Error) -> Failed {
    Failed::new(Code::Output, format!("writing the output: {error}"))
}

/// Writes a value left on the stack, without holding a second whole copy:
/// a list an item a line, a list in it as one line of JSON; a `next-events`
/// batch as `batch` says. An empty string, list, capture or answer of no
/// panes writes nothing, and options or a request nothing; an empty string
/// in a list is a blank line.
fn written(out: &mut dyn Write, value: &Value<Fux>, batch: Batch) -> Result<(), Failed> {
    match value {
        Value::List(list) => list.items().iter().try_for_each(|item| line(out, item)),
        Value::Str(text) if text.is_empty() => Ok(()),
        Value::App(Fux::Answer(answer, _)) if answer.is_empty() => Ok(()),
        Value::App(Fux::Opts(_)) => Ok(()),
        Value::App(Fux::Record(record)) if matches!(batch, Batch::Events) => {
            match (record.printed_events()).map_err(|f| Failed::new(Code::of(f.code), f.message))? {
                Some(events) => written(out, &events, batch),
                None => line(out, value),
            }
        }
        Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::App(_)
        | Value::Xt(_)
        | Value::Mark(_)
        | Value::Failure(_)
        | Value::Addr(_) => line(out, value),
    }
}

/// Writes one item and its line end: its text, or a list's JSON.
fn line(out: &mut dyn Write, value: &Value<Fux>) -> Result<(), Failed> {
    struct Out<'a>(&'a mut dyn Write, Option<io::Error>);
    impl std::fmt::Write for Out<'_> {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            self.0.write_all(text.as_bytes()).map_err(|error| {
                self.1 = Some(error);
                std::fmt::Error
            })
        }
    }
    let mut stream = Out(out, None);
    let wrote = match value {
        Value::List(_) => words::write_json_value(value, &mut stream)
            .map_err(|_| Failed::new(Code::Output, "the text could not be written")),
        Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::App(_)
        | Value::Xt(_)
        | Value::Mark(_)
        | Value::Failure(_)
        | Value::Addr(_) => {
            (value.write_text(&mut stream)).map_err(|f| Failed::new(Code::of(f.code), f.message))
        }
    };
    if let Some(error) = stream.1 {
        return Err(output_failed(error));
    }
    wrote?;
    writeln!(out).map_err(output_failed)
}

/// A program as `program` gives it: its words, its file, where it came
/// from, and its text if it was stdin's.
type Loaded = (
    Vec<Token>,
    Option<std::path::PathBuf>,
    Given,
    Option<String>,
);

/// The program `args` give, its file, where it came from, and its text if
/// that was stdin, which it has then read: none attaches; `-` is stdin;
/// `--file FILE ARGS…` a file, any that reads, the arguments after it its
/// own (`#!/usr/bin/env -S fux --file`); help as other programs give it;
/// any other one argument a whole program, as a shell passes `fux 'ls
/// json'`, never a file, whatever it holds; else each is a word.
fn program(args: &[String]) -> Result<Loaded, Failed> {
    let words = match args {
        [] => token::arguments(&["opts", "attach"]),
        // A program read whole.
        [dash] if dash == "-" => {
            let text = read_text("stdin", io::stdin())?;
            let tokens = token::source("stdin", &text)?;
            return Ok((tokens, None, Given::Stdin, Some(text)));
        }
        [flag, file, rest @ ..] if flag == "--file" => {
            let failed = |e: io::Error| read_failed(file, &e.to_string());
            let path = source::identity(std::path::Path::new(file)).map_err(failed)?;
            let text = source::read(&path).map_err(failed)?;
            let tokens = token::source(&path.display().to_string(), &text)?;
            return Ok((tokens, Some(path), Given::File(rest.to_vec()), None));
        }
        [flag] if flag == "--file" => {
            return Err(Failed::new(
                Code::Usage,
                "--file names the file: fux --file FILE ARGS…",
            ));
        }
        // Help as other programs give it: `help WORD` (or a topic of the
        // guide), `--help`. Release flags are handled before compilation.
        [help, topic] if help == "help" => token::arguments(&["str", topic, "explain"]),
        [flag] if matches!(flag.as_str(), "--help" | "-h") => token::arguments(&["help"]),
        [line] => {
            let tokens = token::source("command line", line)?;
            return Ok((tokens, None, Given::Words, None));
        }
        words => token::arguments(words),
    };
    Ok((words, None, Given::Words, None))
}

fn read_failed(name: &str, why: &str) -> Failed {
    Failed::new(Code::Read, format!("{why} ({name})"))
}

/// The UTF-8 text read from `from`, which `name` names, to its end.
fn read_text(name: &str, mut from: impl Read) -> Result<String, Failed> {
    let mut bytes = Vec::new();
    (from.read_to_end(&mut bytes)).map_err(|e| read_failed(name, &e.to_string()))?;
    String::from_utf8(bytes).map_err(|_| read_failed(name, "not UTF-8 text"))
}

/// The text of the file at `path`, as `read_text` reads it. What is not a
/// regular file (a pipe, a terminal, `/dev/stdin`) may not give the same
/// again: reading it marks the program `acted`, before anything is read.
fn read_file(path: &std::path::Path, acted: &Acted) -> Result<String, Failed> {
    let name = path.display().to_string();
    let file = std::fs::File::open(path).map_err(|e| read_failed(&name, &e.to_string()))?;
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        acted.mark();
    }
    read_text(&name, file)
}

/// A session asked in-process, and programs run against one held reply:
/// for the tests (fux-server's among them) and the fuzz targets.
#[cfg(any(test, feature = "in-process"))]
pub mod testing {
    /// A session, as what a program's requests go to (`Session::answer`).
    pub struct InProcess<'s>(pub &'s mut fux_server::session::Session);

    impl super::Asks for InProcess<'_> {
        fn published(
            &mut self,
            state: &crate::client_state::ClientState,
            context: &fux_wire::protocol::Context,
        ) {
            self.0
                .present(context.client, state.preferences.presentation());
        }
        fn ask(
            &mut self,
            asked: &fux_wire::protocol::Asked,
        ) -> Result<super::Reply, super::Failed> {
            Ok(self.0.answer(asked))
        }
    }

    /// A session with a terminal client of `rows` by `cols` attached, its
    /// first workspace started as this client's defaults start one.
    pub fn attached(
        rows: u16,
        cols: u16,
    ) -> Result<(fux_server::session::Session, fux_wire::id::ClientId), String> {
        use fux_wire::command::{InitialWorkspace, Run, Start};
        let run = Run::now().ok_or("no clock")?;
        let preferences = super::default_state()
            .map_err(|e| e.to_string())?
            .preferences;
        let mut session = fux_server::session::Session::new(
            "/nonexistent/fux.sock".into(),
            false,
            run,
            preferences.journal_entries,
        );
        let size = fux_vt::Size::new(rows, cols).map_err(|e| e.to_string())?;
        let attach = fux_wire::protocol::Attach {
            initial: Some(InitialWorkspace {
                name: None,
                start: Start::Shell(preferences.shell.clone()),
                options: preferences.pane_options(),
                cwd: std::env::current_dir().map_err(|e| e.to_string())?,
            }),
            ..fux_wire::protocol::Attach::of(size, preferences.presentation())
        };
        let client =
            (session.attach_as(attach, std::time::Instant::now())).map_err(|e| e.to_string())?;
        Ok((session, client))
    }

    pub fn evaluate(
        code: &str,
        state: Option<&mut super::Draft>,
        asks: &mut impl super::Asks,
        out: &mut dyn std::io::Write,
    ) -> Result<Vec<fuxth::Value<super::Fux>>, super::Failed> {
        let mut machine = super::compile(&fuxth::token::source("test", code)?)?;
        let mut host = super::Host::new(fux_wire::protocol::Context::default(), state, out);
        super::run(&mut machine, &mut host, asks, None)
    }

    pub fn held_reply(
        code: &str,
        reply: super::Reply,
        out: &mut Vec<u8>,
    ) -> Result<Vec<fuxth::Value<super::Fux>>, super::Failed> {
        struct Once(Option<super::Reply>);
        impl super::Asks for Once {
            fn ask(
                &mut self,
                asked: &fux_wire::protocol::Asked,
            ) -> Result<super::Reply, super::Failed> {
                if let fux_wire::command::Request::Do(fux_wire::command::Command::Resolve {
                    text,
                    ..
                }) = &asked.request
                {
                    let id = text.parse().map_err(|()| {
                        super::Failed::new(fux_wire::code::Code::Usage, "test target")
                    })?;
                    return Ok(super::Reply {
                        run: self.0.as_ref().and_then(|r| r.run),
                        ..fux_wire::protocol::Body::Found(id).into()
                    });
                }
                self.0.take().ok_or_else(|| {
                    super::Failed::new(
                        fux_wire::code::Code::Usage,
                        "held field made another request",
                    )
                })
            }
        }
        evaluate(code, None, &mut Once(Some(reply)), out)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::testing::{evaluate, held_reply};
    use crate::TestResult;
    use fux_wire::json::Json;

    /// Pane outcomes survive the wire with one status owner, including
    /// mixed collections whose first failing member names the whole.
    #[test]
    fn pane_outcomes_determine_the_reply_status() -> Result<(), Box<dyn std::error::Error>> {
        use fux_wire::answer::{PaneAnswer, PaneResults};
        use fux_wire::code::Code;
        use fux_wire::id::PaneId;
        use fux_wire::protocol::{End, Reply};
        let version = "%2:0.0.0.0.0:48211-1867a3f9c2b8e7d6"
            .parse()
            .map_err(|()| "version")?;
        let changed = PaneAnswer::Changed {
            version,
            changed: vec![fux_wire::answer::Change::Screen],
            ended: None,
        };
        let closed = PaneAnswer::Closed(PaneId::of(3));
        let timeout = PaneAnswer::TimedOut {
            since: version,
            limit: std::time::Duration::from_millis(1500),
        };
        for (results, code) in [
            (PaneResults::One(PaneAnswer::Acted(version)), None),
            (PaneResults::One(changed.clone()), None),
            (PaneResults::One(closed.clone()), Some(Code::Closed)),
            (PaneResults::One(timeout.clone()), Some(Code::Timeout)),
            (PaneResults::Many(vec![]), None),
            (
                PaneResults::Many(vec![changed.clone(), closed.clone(), timeout.clone()]),
                Some(Code::Closed),
            ),
            (
                PaneResults::Many(vec![changed, timeout, closed]),
                Some(Code::Timeout),
            ),
        ] {
            let reply = Reply {
                run: Some(version.run),
                ..results.into()
            };
            let bytes = fux_wire::wire::encode(&reply);
            let decoded = fux_wire::wire::Reader::new(&bytes).whole::<Reply>()?;
            assert_eq!(decoded, reply);
            assert_eq!(decoded.end().code(), code);
            assert_eq!(
                decoded.end().status(),
                code.map_or(0, |code| code.status().get())
            );
            let values = crate::script::answered(decoded.clone()).map_err(|f| f.to_string())?;
            let json = values
                .first()
                .map(|v| match v {
                    fuxth::Value::App(crate::script::Fux::Answer(a, run)) => {
                        assert_eq!(*run, Some(version.run));
                        {
                            let mut out = String::new();
                            assert!(a.write_json(&mut out).is_ok());
                            out
                        }
                    }
                    fuxth::Value::Int(_)
                    | fuxth::Value::Xt(_)
                    | fuxth::Value::Mark(_)
                    | fuxth::Value::Failure(_)
                    | fuxth::Value::Addr(_)
                    | fuxth::Value::Bool(_)
                    | fuxth::Value::Str(_)
                    | fuxth::Value::List(_)
                    | fuxth::Value::App(_) => String::new(),
                })
                .ok_or("answer")?;
            assert_eq!(json.contains(r#""status":124"#), has_timeout(&decoded));
            if let End::Failed { message, .. } = decoded.end() {
                assert!(!message.is_empty());
                if has_timeout(&decoded) {
                    assert!(message.contains("unchanged for 1.5s"), "{message}");
                }
            }
        }
        Ok(())
    }

    fn has_timeout(reply: &super::Reply) -> bool {
        use fux_wire::code::Code;
        matches!(&reply.response, super::Response::Answered(answers) if answers.all().iter().any(|a| a.failure() == Some(Code::Timeout)))
    }

    /// The default-history styled capture, decoded: its JSON is made and
    /// printed whole.
    #[test]
    fn default_styled_capture_json_is_printed_whole() -> TestResult {
        let version: fux_wire::command::Version =
            "%1:0.0.0.0.0:1-1".parse().map_err(|()| "version")?;
        let capture = fux_wire::answer::Captured::default_styled(version);
        let reply = super::Reply {
            run: Some(version.run),
            ..super::Body::Capture(Box::new(capture)).into()
        };
        let bytes = fux_wire::wire::encode(&reply);
        let reply = fux_wire::wire::Reader::new(&bytes).whole::<super::Reply>()?;
        let mut out = Vec::new();
        let result = held_reply("%1 all-history styles capture json print", reply, &mut out);
        assert!(result.is_ok(), "{result:?}");
        assert!(out.len() > 20 << 20, "{}", out.len());
        assert!(out.starts_with(b"{\"pane\":\"%1\"") && out.ends_with(b"}\n"));
        Ok(())
    }

    #[test]
    fn ended_answers_expose_both_exit_fields() -> TestResult {
        use super::*;
        use fux_wire::answer::Ended;
        let version: Version = "%1:0.7.0.0.0:1-1".parse().map_err(|()| "version")?;
        for (ended, exited, signal) in [
            (Ended::Exited(7), Value::Int(7), Value::App(Fux::Null)),
            (Ended::Signalled(9), Value::App(Fux::Null), Value::Int(9)),
            (Ended::Lost, Value::App(Fux::Null), Value::App(Fux::Null)),
        ] {
            let answer = PaneAnswer::Changed {
                version,
                changed: Vec::new(),
                ended: Some(ended),
            };
            let mut json = String::new();
            answer.write_json(&mut json)?;
            assert!(json.contains("\"exited\":"), "{json}");
            assert!(json.contains("\"signal\":"), "{json}");
            let reply = Reply {
                run: Some(version.run),
                response: Response::Answered(PaneResults::One(answer)),
            };
            let values = held_reply(
                "%1 0 timeout wait let held held str exited get held str signal get",
                reply,
                &mut Vec::new(),
            )?;
            assert_eq!(values, vec![exited, signal]);
        }
        Ok(())
    }

    fn large_plain(version: super::Version) -> fux_wire::answer::Captured {
        fux_wire::answer::Captured {
            of: fux_wire::answer::Of::Pane {
                version,
                wrap_pending: false,
                state: Vec::new(),
            },
            screen_rows: 24,
            screen_cols: 80,
            screen_cursor: None,
            lines: vec![fux_wire::answer::Line {
                text: std::iter::repeat_n('x', 17_000_000).collect(),
                runs: Vec::new(),
            }],
            styles: Vec::new(),
            styled: false,
            marks: false,
        }
    }

    #[test]
    fn telling_an_uncaught_failure_allocates_no_more_at_depth() -> TestResult {
        use super::*;
        let allocator = fux_vt_peak::ThreadPeak;
        let mut told = Vec::new();
        for depth in [10, 5000] {
            let code = format!(
                "define deep dup 0 eq if drop list end first else 1 sub recurse 1 add then end {depth} deep"
            );
            let mut machine = compile(&token::source("deep", &code)?)?;
            let mut sink = io::sink();
            let mut host = Host::new(Context::default(), None, &mut sink);
            let error = machine.run(&mut host, u64::MAX).err().ok_or("no failure")?;
            let base = allocator.live();
            allocator.reset_peak();
            let failed = uncaught(&machine, error);
            told.push((allocator.peak().saturating_sub(base), failed.message));
        }
        let [(shallow, said), (deep, again)] = told.as_slice() else {
            return Err("two depths".into());
        };
        assert!(
            said.contains("inside first") && said == again,
            "{said} {again}"
        );
        assert_eq!(shallow, deep);
        Ok(())
    }

    #[test]
    fn final_output_streams_a_large_string_without_copying_it() -> TestResult {
        use std::io::Write;
        struct Count(usize);
        impl Write for Count {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self.0.saturating_add(bytes.len());
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let text =
            fuxth::Value::<super::Fux>::str(std::iter::repeat_n('x', 2 << 20).collect::<String>());
        let mut sink = Count(0);
        let allocator = fux_vt_peak::ThreadPeak;
        let base = allocator.live();
        allocator.reset_peak();
        {
            let mut out = std::io::BufWriter::with_capacity(64 << 10, &mut sink);
            super::written(&mut out, &text, super::Batch::Events)?;
            out.flush()?;
        }
        assert_eq!(sink.0, (2 << 20) + 1);
        assert!(allocator.peak().saturating_sub(base) <= 64 << 10);
        Ok(())
    }

    /// What a run leaves prints a value a line, and a list an item a line:
    /// a list in it as a line of JSON, an empty string in it a blank line;
    /// an empty string or list prints nothing.
    #[test]
    fn a_list_left_prints_an_item_a_line() -> TestResult {
        let mut out = Vec::new();
        let program = "list 1 str '' list 2 str x end end str '' list end 3";
        assert_eq!(super::main(&[program.into()], None, &mut out)?, 0);
        assert_eq!(String::from_utf8(out)?, "1\n\n[2,\"x\"]\n3\n");
        Ok(())
    }

    /// A closed pipe a program caught still ends it with `PIPE`.
    #[test]
    fn a_caught_closed_pipe_still_ends_quietly() -> TestResult {
        struct Closed;
        impl std::io::Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let program = "str output try str x print catch drop end";
        assert_eq!(
            super::main(&[program.into()], None, &mut Closed)?,
            super::PIPE
        );
        Ok(())
    }

    /// A batch that missed entries is refused as it is printed, and nothing
    /// of it written; one with none is its events alone.
    #[test]
    fn a_batch_left_that_missed_entries_is_refused() -> TestResult {
        use fux_wire::events::{Batch, Status};
        let next = "1:1-0".parse().map_err(|()| "cursor")?;
        for (missed, written) in [(2, None), (0, Some(""))] {
            let batch = Batch {
                next,
                missed,
                status: Status::Timeout,
                events: Vec::new(),
            };
            let mut out = Vec::new();
            let printed = super::written(
                &mut out,
                &super::record::events(batch).map_err(|f| f.message)?,
                super::Batch::Events,
            );
            let said = printed.map_err(|f| f.to_string());
            match written {
                Some(text) => assert_eq!(String::from_utf8(out)?, text),
                None => assert!(
                    out.is_empty() && said.is_err_and(|e| e.contains("2 journal entries missed")),
                ),
            }
        }
        Ok(())
    }

    #[test]
    fn final_output_reports_the_writers_failure() {
        struct Broken;
        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("broken writer"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let failed = super::main(&["1".into()], None, &mut Broken).err();
        assert!(
            failed.is_some_and(|f| f.code == fux_wire::code::Code::Output
                && f.message == "writing the output: broken writer")
        );
    }

    #[test]
    fn large_plain_capture_crosses_the_machine_boundary() -> TestResult {
        let version: super::Version = "%1:0.7.0.0.0:1-1".parse().map_err(|()| "version")?;
        let capture = large_plain(version);
        let reply = super::Reply {
            run: Some(version.run),
            ..fux_wire::protocol::Body::Capture(Box::new(capture)).into()
        };
        let mut out = Vec::new();
        held_reply("%1 all-history capture print", reply, &mut out)?;
        assert_eq!(out.len(), 17_000_001);
        Ok(())
    }

    #[test]
    fn held_answers_and_captures_project_without_refreshing() -> TestResult {
        use super::*;
        let version: Version = "%1:0.7.0.0.0:1-1".parse().map_err(|()| "version")?;
        let reply = Reply {
            run: Some(version.run),
            response: Response::Answered(PaneResults::One(PaneAnswer::Acted(version))),
        };
        assert_eq!(
            held_reply(
                "%1 str x text send-keys str version get",
                reply,
                &mut Vec::new()
            )?,
            vec![Value::str(version.to_string())]
        );
        let capture = fux_wire::answer::Captured {
            of: fux_wire::answer::Of::Pane {
                version,
                wrap_pending: false,
                state: vec![("last_status".into(), Json::Int(7))],
            },
            screen_rows: 24,
            screen_cols: 80,
            screen_cursor: None,
            lines: Vec::new(),
            styles: Vec::new(),
            styled: false,
            marks: false,
        };
        let reply = Reply {
            run: Some(version.run),
            ..Body::Capture(Box::new(capture)).into()
        };
        assert_eq!(
            held_reply("%1 capture str last_status get", reply, &mut Vec::new())?,
            vec![Value::Int(7)]
        );
        let reply = Reply {
            run: Some(version.run),
            response: Response::Answered(PaneResults::One(PaneAnswer::Closed(version.pane))),
        };
        assert_eq!(
            held_reply(
                "str closed try %1 0 timeout wait catch str value get str pane get end",
                reply,
                &mut Vec::new()
            )?,
            vec![Value::App(Fux::Pane(version.pane, Some(version.run)))]
        );
        let client = fux_wire::id::ClientId::of(1);
        let capture = fux_wire::answer::Captured {
            of: fux_wire::answer::Of::Client(client),
            screen_rows: 24,
            screen_cols: 80,
            screen_cursor: Some((7, 3)),
            lines: vec![fux_wire::answer::Line {
                text: "x".into(),
                runs: vec![fux_wire::answer::Run {
                    bytes: 1,
                    cells: 1,
                    style: 0,
                }],
            }],
            styles: vec![fux_vt::Attributes::new(
                fux_vt::Color::Idx(1),
                fux_vt::Color::Default,
            )],
            styled: true,
            marks: false,
        };
        let reply = Reply {
            run: Some(version.run),
            ..Body::Capture(Box::new(capture)).into()
        };
        let reply = fux_wire::wire::Reader::new(&fux_wire::wire::encode(&reply)).whole()?;
        let values = held_reply(
            "str c1 client capture let held held str client get held str screen_cursor get held str lines get first held str styles get first first str fg get",
            reply,
            &mut Vec::new(),
        )?;
        assert_eq!(
            values,
            vec![
                Value::App(Fux::Client(client, Some(version.run))),
                Value::List(Rc::new(fuxth::List::new(vec![
                    Value::Int(7),
                    Value::Int(3)
                ]))),
                Value::str("x"),
                Value::Int(1)
            ]
        );
        Ok(())
    }

    #[test]
    fn version_text_from_another_run_is_read_without_asking() -> TestResult {
        use super::*;
        // A key's program runs on its server's run; the version is another's.
        let context = Context {
            of: Some("2-0".parse().map_err(|()| "a run")?),
            ..Context::default()
        };
        let other = "%1:0.0.0.0.0:1-0";
        let code = format!("str {other} version str {other} counts");
        let mut machine = compile(&fuxth::token::source("key", &code)?)?;
        let mut out = Vec::new();
        let mut host = Host::new(context, None, &mut out);
        let left = run(&mut machine, &mut host, &mut ConfigOnly::default(), None)?;
        let text: Vec<String> = (left.iter().map(Value::text))
            .collect::<Result<_, _>>()
            .map_err(|f| f.message)?;
        assert_eq!(text, [other, "0 0 0 0 0"]);
        Ok(())
    }

    /// A version's counts are decimal, as the server writes them, whatever
    /// base the program reads its own numbers in.
    /// `word` and `dictionary` read the program's own dictionary, with
    /// no server: a word's record says where it came from, what it declares,
    /// what made it and what it can fail with, and the program's words come
    /// last, each once.
    #[test]
    fn the_dictionary_is_the_programs_own_as_records() -> TestResult {
        use super::*;
        let code = "define sq dup mul end define sq2 sq end define sq 0 end
            str sq word str dup word str split word str add word dictionary";
        let mut machine = compile(&fuxth::token::source("cli", code)?)?;
        let mut out = Vec::new();
        let mut host = Host::new(Context::default(), None, &mut out);
        let left = run(&mut machine, &mut host, &mut ConfigOnly::default(), None)?;
        let json: Vec<String> = left
            .iter()
            .map(|v| words::json_text(v).text())
            .collect::<Result<_, _>>()
            .map_err(|f| f.message)?;
        let [sq, dup, split, add, dictionary] = json.as_slice() else {
            return Err(format!("{json:?}").into());
        };
        let record = |name: &str, provenance: &str, effect: &str, about: &str, definition: &str| {
            format!(
                "{{\"name\":\"{name}\",\"provenance\":\"{provenance}\",\"effect\":{effect},\"documentation\":{about},\"definition\":\"{definition}\",\"codes\":"
            )
        };
        let sq_now = record("sq", "run", "null", "null", "define sq 0 end") + "[]}";
        assert_eq!(*sq, sq_now);
        let a = "\"a copy of the top\"";
        assert_eq!(
            *dup,
            record("dup", "library", "\"a -- a a\"", a, "define dup 0 pick end")
                + "[\"stack_underflow\"]}"
        );
        assert!(split.contains("\"provenance\":\"fux\",\"effect\":\"subject -- answer\",\"documentation\":\"split a pane"), "{split}");
        assert!(add.contains("\"provenance\":\"core\""), "{add}");
        assert!(
            add.ends_with(
                "\"definition\":\"native\",\"codes\":[\"stack_underflow\",\"type\",\"invalid_address\"]}"
            ),
            "{add}"
        );
        assert!(
            dictionary.ends_with(&format!(
                ",{}[\"stack_underflow\",\"type\"]}},{sq}]",
                record("sq2", "run", "null", "null", "define sq2 sq end")
            )),
            "{dictionary}"
        );
        let mut machine = compile(&fuxth::token::source("cli", "str nosuch word")?)?;
        let failed = run(&mut machine, &mut host, &mut ConfigOnly::default(), None).err();
        assert_eq!(failed.map(|f| f.code), Some(Code::from(Raise::UnknownWord)));
        Ok(())
    }

    #[test]
    fn counts_read_a_version_in_decimal_whatever_the_base() -> TestResult {
        use super::*;
        let code = "hex str %1:0.1552.0.0.0:1-0 counts 2 base store str 101 number";
        let mut machine = compile(&fuxth::token::source("key", code)?)?;
        let mut out = Vec::new();
        let mut host = Host::new(Context::default(), None, &mut out);
        let left = run(&mut machine, &mut host, &mut ConfigOnly::default(), None)?;
        let text: Vec<String> = (left.iter().map(Value::text))
            .collect::<Result<_, _>>()
            .map_err(|f| f.message)?;
        assert_eq!(text, ["0 1552 0 0 0", "5"]);
        Ok(())
    }

    #[test]
    fn attach_words_accept_compiled_action_answers_and_keep_their_run() -> TestResult {
        use super::*;
        use fux_wire::command::{AttachTarget, Command, Request};
        struct Record {
            version: Version,
            sent: usize,
            attached: Option<(Option<Run>, Option<AttachTarget>, Option<fux_vt::Size>)>,
        }
        impl Asks for Record {
            fn ask(&mut self, asked: &Asked) -> Result<Reply, Failed> {
                let mut reply: Reply = if matches!(
                    asked.request,
                    Request::Do(fux_wire::command::Command::Resolve { .. })
                ) {
                    Body::Found(AnyRef::Pane(self.version.pane)).into()
                } else if matches!(asked.request, Request::Do(Command::SendKeys { .. })) {
                    self.sent = self.sent.saturating_add(1);
                    PaneResults::One(PaneAnswer::Acted(self.version)).into()
                } else {
                    return Err(Failed::new(Code::Usage, "unexpected request"));
                };
                reply.run = Some(self.version.run);
                Ok(reply)
            }
            fn local(
                &mut self,
                what: &Local,
                _: Option<&mut Draft>,
                _: &mut dyn Write,
            ) -> Result<Vec<Value<Fux>>, Failed> {
                let Local::Attach {
                    of, target, size, ..
                } = what
                else {
                    return Err(Failed::new(Code::Usage, "unexpected local action"));
                };
                self.attached = Some((*of, target.clone(), *size));
                Ok(Vec::new())
            }
        }
        let version: Version = "%1:0.0.0.0.0:1-2".parse().map_err(|()| "version")?;
        for action in ["attach-defaults", "attach", "80 24 size attach"] {
            let code = format!("%1 str hello text send-keys {action}");
            let mut machine = compile(&token::source("answer-attach", &code)?)?;
            let mut draft = default_state()?.draft();
            let mut sink = io::sink();
            let mut host = Host::new(Context::default(), Some(&mut draft), &mut sink);
            let mut record = Record {
                version,
                sent: 0,
                attached: None,
            };
            run(&mut machine, &mut host, &mut record, None)?;
            assert_eq!(record.sent, 1);
            let target = Some(AttachTarget::Pane(version.pane));
            if action == "attach-defaults" {
                let defaults = draft.attach.ok_or("attach defaults")?;
                assert_eq!((defaults.of, defaults.target), (Some(version.run), target));
            } else {
                let size = action
                    .starts_with("80")
                    .then(|| fux_vt::Size::new(24, 80))
                    .transpose()?;
                assert_eq!(record.attached, Some((Some(version.run), target, size)));
            }
        }
        // A baseline is no target: the word refuses it before it attaches.
        for baseline in ["changed", "expect"] {
            for action in ["attach", "attach-defaults"] {
                let code = format!("str {version} {baseline} {action}");
                let mut machine = compile(&token::source("baseline", &code)?)?;
                let mut draft = default_state()?.draft();
                let defaults = draft.attach.clone();
                let mut sink = io::sink();
                let mut host = Host::new(Context::default(), Some(&mut draft), &mut sink);
                let mut record = Record {
                    version,
                    sent: 0,
                    attached: None,
                };
                let ran = run(&mut machine, &mut host, &mut record, None);
                assert!(ran.is_err(), "{code}");
                assert_eq!(record.attached, None, "{code}");
                assert_eq!(draft.attach, defaults, "{code}");
            }
        }
        Ok(())
    }

    #[test]
    fn release_flags_bypass_the_operand_taking_version_word() -> TestResult {
        for flag in ["--version", "-V"] {
            let mut out = Vec::new();
            assert_eq!(super::main(&[flag.into()], None, &mut out)?, 0);
            assert_eq!(
                String::from_utf8(out)?,
                format!(
                    "fux {} (protocol {})\n",
                    env!("CARGO_PKG_VERSION"),
                    fux_wire::protocol::PROTOCOL
                )
            );
        }
        assert!(super::main(&["version".into()], None, &mut Vec::new()).is_err());
        Ok(())
    }

    #[test]
    fn every_observation_wait_encodes_its_timeout_and_a_bare_one_none() -> TestResult {
        use super::*;
        use fux_wire::command::{AnyRef, Command, Request};
        use fux_wire::wire::Reader;
        struct Record(Vec<Option<u32>>);
        impl Asks for Record {
            fn ask(&mut self, asked: &Asked) -> Result<Reply, Failed> {
                let bytes = fux_wire::wire::encode(asked);
                let decoded: Asked = Reader::new(&bytes)
                    .whole()
                    .map_err(|e| Failed::new(Code::Usage, e.to_string()))?;
                if let Request::Wait(wait) = decoded.request {
                    self.0.push(wait.timeout.map(Limit::seconds));
                    Err(Failed::new(Code::NoServer, "recorded"))
                } else if let Request::Do(fux_wire::command::Command::Resolve { .. }) =
                    decoded.request
                {
                    Ok(Body::Found(AnyRef::Pane(
                        "%1".parse()
                            .map_err(|e| Failed::new(Code::Usage, format!("{e:?}")))?,
                    ))
                    .into())
                } else if let Request::Do(Command::SendKeys { .. }) = decoded.request {
                    let version = "%1:0.0.0.0.0:1-2"
                        .parse()
                        .map_err(|()| Failed::new(Code::Usage, "version"))?;
                    Ok(PaneResults::One(PaneAnswer::Acted(version)).into())
                } else {
                    Err(Failed::new(Code::Usage, "unexpected request"))
                }
            }
        }
        let bundled = default_state()?;
        for subject in [
            "%1",
            "str %1:0.0.0.0.0:1-2 changed",
            "%1 str Enter key send-keys",
        ] {
            for (option, limit) in [("", None), ("0 timeout", Some(0)), ("7 timeout", Some(7))] {
                let mut draft = bundled.draft();
                let mut out = io::sink();
                let mut record = Record(Vec::new());
                let failed = evaluate(
                    &format!("{subject} {option} wait"),
                    Some(&mut draft),
                    &mut record,
                    &mut out,
                )
                .err()
                .ok_or("wait was not recorded")?;
                assert_eq!(failed.code, Code::NoServer);
                assert_eq!(record.0, [limit], "{subject} {option}");
            }
        }
        Ok(())
    }

    #[test]
    fn local_json_accepts_observations_from_different_runs() -> TestResult {
        use super::*;
        struct Reports(std::collections::VecDeque<Run>);
        impl Asks for Reports {
            fn ask(&mut self, _: &Asked) -> Result<Reply, Failed> {
                let run = self
                    .0
                    .pop_front()
                    .ok_or_else(|| Failed::new(Code::Usage, "run"))?;
                let cursor = format!("0:{run}")
                    .parse()
                    .map_err(|()| Failed::new(Code::Usage, "cursor"))?;
                Ok(Reply {
                    run: Some(run),
                    ..Body::Snapshot(Box::new(fux_wire::inspect::Snapshot {
                        run,
                        cursor,
                        scope: fux_wire::inspect::Scope::All,
                        projection: fux_wire::inspect::Projection::Facts,
                        workspaces: Vec::new(),
                        tabs: Vec::new(),
                        panes: Vec::new(),
                        clients: Vec::new(),
                    }))
                    .into()
                })
            }
        }
        let runs = ["1-0", "2-0"].map(|s| s.parse().map_err(|()| "run"));
        let mut reports = Reports(runs.into_iter().collect::<Result<_, _>>()?);
        let mut sink = Vec::new();
        evaluate(
            "ls let a ls let b list a b end json print",
            None,
            &mut reports,
            &mut sink,
        )?;
        let text = String::from_utf8(sink)?;
        assert!(
            text.starts_with('[')
                && text.contains("\"run\":\"1-0\"")
                && text.contains("\"run\":\"2-0\"")
        );
        Ok(())
    }

    #[test]
    fn arrange_reconciles_its_pane_leaf_runs() -> TestResult {
        use super::*;
        use fux_wire::command::{Command, Request};
        struct Leaves {
            runs: std::collections::VecDeque<Run>,
            submitted: Vec<Option<Run>>,
        }
        impl Asks for Leaves {
            fn ask(&mut self, asked: &Asked) -> Result<Reply, Failed> {
                let mut run = None;
                let body = match asked.request {
                    Request::Do(fux_wire::command::Command::Resolve { .. }) => {
                        Body::Found(AnyRef::Tab(TabId::of(1)))
                    }
                    Request::Do(Command::Members { .. }) => {
                        run = self.runs.pop_front();
                        Body::Handles(vec![AnyRef::Pane(PaneId::of(1))])
                    }
                    Request::Do(Command::Arrange { .. }) => {
                        self.submitted.push(asked.context.of);
                        Body::Empty
                    }
                    Request::Do(_) | Request::Wait(_) | Request::NextEvents { .. } => {
                        return Err(Failed::new(Code::Usage, "unexpected request"));
                    }
                };
                Ok(Reply { run, ..body.into() })
            }
        }
        let ours: Run = "1-0".parse().map_err(|()| "run")?;
        let other: Run = "2-0".parse().map_err(|()| "run")?;
        for runs in [[ours, ours], [ours, other]] {
            let mut leaves = Leaves {
                runs: runs.into(),
                submitted: Vec::new(),
            };
            let mut machine = compile(&token::source(
                "layout",
                "@1 panes let a @1 panes let b @1 str @1:0:1-0 list list str horizontal 1 1 1 2 end a first b first end arrange",
            )?)?;
            let mut sink = Vec::new();
            let mut host = Host::new(Context::default(), None, &mut sink);
            let result = run(&mut machine, &mut host, &mut leaves, None);
            if runs == [ours, ours] {
                result?;
                assert_eq!(leaves.submitted, [Some(ours)]);
            } else {
                assert!(result.is_err_and(|f| f.code == Code::BadValue));
                assert!(leaves.submitted.is_empty());
            }
        }
        Ok(())
    }

    #[test]
    fn layout_scalar_refusals_do_not_copy_retained_strings() -> TestResult {
        use super::*;
        struct NoRequests;
        impl Asks for NoRequests {
            fn ask(&mut self, _: &Asked) -> Result<Reply, Failed> {
                Err(Failed::new(
                    Code::Usage,
                    "invalid scalars must not submit a request",
                ))
            }
        }
        let longest = "@4294967295:18446744073709551615:4294967295-ffffffffffffffff";
        assert!(longest.parse::<fux_wire::command::LayoutRevision>().is_ok());
        let allocator = fux_vt_peak::ThreadPeak;
        let bad = Value::str(std::iter::repeat_n('x', 500_000).collect::<String>());
        let mut peaks = Vec::new();
        for proposal in [
            "t bad list end arrange",
            "t str @1:0:1-0 list list bad 1 1 0 0 end end arrange",
        ] {
            let code = format!(
                "input let bad @1 let t 0 16 range each drop str bad_value try {proposal} catch drop end end"
            );
            let mut machine = compile(&token::source("scalar", &code)?)?;
            let mut sink = Vec::new();
            let mut host = Host::new(Context::default(), None, &mut sink);
            assert!(matches!(
                machine.run(&mut host, u64::MAX)?,
                Outcome::Suspended(Ask::Local(Local::Shell(Shell::Input)))
            ));
            machine.push(bad.clone());
            assert!(matches!(
                machine.run(&mut host, u64::MAX)?,
                Outcome::Suspended(Ask::Server { .. })
            ));
            machine.push(Value::App(Fux::Tab(TabId::of(1), None)));
            let base = allocator.live();
            allocator.reset_peak();
            run(&mut machine, &mut host, &mut NoRequests, None)?;
            peaks.push(allocator.peak().saturating_sub(base));
        }
        assert!(
            peaks.iter().all(|p| *p < 64 << 10),
            "scalar refusal peaks: {peaks:?}"
        );
        Ok(())
    }

    #[test]
    fn one_argument_is_a_program() -> Result<(), super::Failed> {
        let words = |args: &[&str]| -> Result<Vec<String>, super::Failed> {
            let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            let (tokens, _, given, _) = super::program(&args)?;
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

    #[test]
    fn only_the_command_line_asks_its_process() -> Result<(), super::Failed> {
        let words = ["input", "str x read-file", "str x warn", "0 exit"];
        for code in words {
            let ran = super::run_quiet(&super::token::source("config", code)?, None, false);
            assert_eq!(
                ran.err().map(|f| f.code),
                Some(super::Code::NotHere),
                "{code}"
            );
        }
        Ok(())
    }

    #[test]
    fn attach_defaults_replace_targets_and_clear_names_locally() -> Result<(), super::Failed> {
        use fux_wire::command::{AttachTarget, WsRef};
        let mut draft = super::default_state()?.draft();
        let mut run = |code: &str| {
            super::run_quiet(
                &super::token::source("config", code)?,
                Some(&mut draft),
                false,
            )
        };
        run("opts str work in attach-defaults")?;
        assert_eq!(
            draft.attach.as_ref().and_then(|a| a.name.as_deref()),
            Some("main")
        );
        assert_eq!(
            draft.attach.as_ref().and_then(|a| a.target.clone()),
            Some(AttachTarget::Workspace(WsRef::Name("work".into())))
        );
        for code in [
            "opts str other in attach-defaults",
            "opts str fresh name attach-defaults",
            "opts attach-defaults",
        ] {
            super::run_quiet(
                &super::token::source("config", code)?,
                Some(&mut draft),
                false,
            )?;
        }
        let cleared = draft
            .clone()
            .finish()
            .map_err(|why| super::Failed::new(super::Code::Config, why))?;
        assert!(cleared.attach.target.is_none() && cleared.attach.name.is_none());
        for code in [
            "opts str work in str first name attach-defaults",
            "opts str work in str other in attach-defaults",
            "opts 80 24 size attach-defaults",
        ] {
            assert!(
                super::run_quiet(
                    &super::token::source("config", code)?,
                    Some(&mut draft),
                    false
                )
                .is_err(),
                "{code}"
            );
            assert_eq!(
                draft
                    .clone()
                    .finish()
                    .map_err(|why| super::Failed::new(super::Code::Config, why))?,
                cleared
            );
        }
        Ok(())
    }

    #[test]
    fn the_default_keys_are_a_config_list_keys_prints() -> Result<(), super::Failed> {
        let run = |name: &str, text: &str, state: &mut crate::client_state::Draft| {
            super::run_quiet(&super::token::source(name, text)?, Some(state), false)
        };
        let mut keymap = super::default_state()?.draft();
        let (screens, keys): (Vec<&str>, Vec<&str>) = (super::KEYS.lines())
            .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("define "))
            .partition(|l| l.contains(" bind-in") || l.contains(" bind-action"));
        assert_eq!(
            keymap.keys.bindings.all().len(),
            keys.len().saturating_sub(1)
        );
        let listed = keymap.keys.describe();
        assert_eq!(
            listed.matches(" bind-in").count() + listed.matches(" bind-action").count(),
            screens.len()
        );
        // Quotes, a backslash and a line end, as a config writes them.
        let mine = r#"str M-t code me focused zoom end bind-root str Mine group
            str g code me focused str horizontal axis str sh program str "echo it's" arg split drop end bind
            str "a \\ and
            a line" label"#;
        run("mine", mine, &mut keymap)?;
        let mut again = super::default_state()?.draft();
        run(
            "other",
            "str C-a prefix str M-n code me me focused tab str next step end bind-root",
            &mut again,
        )?;
        run("listed", &keymap.keys.describe(), &mut again)?;
        assert_eq!(again.keys, keymap.keys);
        Ok(())
    }

    #[test]
    fn what_is_left_names_its_word() {
        let opts = super::words::KINDS
            .iter()
            .find(|(name, ..)| *name == "opts");
        let from_opts = super::from_opts("and");
        assert!(
            opts.is_some_and(|(_, said, _)| said.contains(&from_opts)),
            "{from_opts}"
        );
        for (code, end) in [
            ("%2 str hi paste", "send-keys"),
            ("opts str x in", "new-tab, attach or attach-defaults"),
        ] {
            let ran = evaluate(code, None, &mut Refuse, &mut Vec::new());
            let error = ran.err().map(|e| e.to_string()).unwrap_or_default();
            assert!(
                error.ends_with(&format!("end with {end}")),
                "{code}: {error}"
            );
        }
    }

    /// A server that finds what an id names, and refuses anything else a
    /// word would ask of it.
    struct Refuse;
    impl super::Asks for Refuse {
        fn ask(&mut self, asked: &super::Asked) -> Result<super::Reply, super::Failed> {
            use fux_wire::command::{Command, Request};
            let refused = || super::Failed::new(super::Code::Usage, "the word asked");
            let Request::Do(Command::Resolve { text, .. }) = &asked.request else {
                return Err(refused());
            };
            let id = text.parse().map_err(|()| refused())?;
            Ok(super::Body::Found(id).into())
        }
    }

    /// A bound gives one code whichever side refuses first: `failed`'s
    /// codes, the word's then the server's (or the screen's admission).
    fn both(failed: [Result<(), super::Failed>; 2]) -> [Option<fux_wire::code::Code>; 2] {
        failed.map(|f| f.err().map(|f| f.code))
    }

    /// A list nested 100,000 deep is written as JSON, and let go, without
    /// recursion; made a callback's argument, one deeper than the wire
    /// carries is refused, not walked.
    #[test]
    fn a_deep_list_is_written_as_json_and_refused_past_the_wire() -> TestResult {
        let deep =
            "list end let l 0 begin list l end let l 1 add dup 100000 eq until drop l json len";
        let left = evaluate(deep, None, &mut Refuse, &mut Vec::new())?;
        assert_eq!(left, vec![fuxth::Value::Int(200_002)]);
        let mut value = fuxth::Value::<super::Fux>::Int(0);
        for _ in 0..fux_wire::callback::DEPTH {
            value = fuxth::Value::List(std::rc::Rc::new(fuxth::List::new(vec![value])));
        }
        assert!(
            super::callback::data(&value).is_ok(),
            "as deep as the wire carries"
        );
        value = fuxth::Value::List(std::rc::Rc::new(fuxth::List::new(vec![value])));
        assert!(super::callback::data(&value).is_err());
        Ok(())
    }

    /// A screen past what a grid holds is `size` from the word `size` and
    /// from the server, as a client's terminal is resized, alike.
    #[test]
    fn a_screen_past_a_grid_is_size_from_the_word_and_the_server() -> TestResult {
        use super::testing::attached;
        let most = u16::MAX;
        let word = format!("opts str new-workspace in {most} {most} size new-tab");
        let word = evaluate(&word, None, &mut Refuse, &mut Vec::new());
        let (mut session, client) = attached(8, 30)?;
        let server = session.resize(client, most, most, std::time::Instant::now());
        let server = server.map(drop).map_err(super::Failed::from);
        let size = Some(fux_wire::code::Code::Size);
        assert_eq!(both([word.map(drop), server]), [size; 2]);
        Ok(())
    }
}
