//! fux's command line, config, bindings and prompt: fuxth programs, run in
//! a client. Words build typed requests (`opts … run`, `%2 … send-keys`)
//! and execution words submit them: the program suspends while the server
//! does what one asks, and goes on with what it answers, as typed values
//! (a pane's handle, a capture), never text to be read back.
//!
//! Every run is fresh: its own stack and dictionary, the bundled library's
//! words first (`library.fux`).
mod record;
pub(crate) mod settings;
mod source;
mod words;

use crate::answer::{PaneAnswer, PaneResults};
use crate::client;
use crate::client_state::{ClientState, Draft};
use crate::code::{Code, Raise};
use crate::command::{
    AnyRef, EnvName, Extent, HistoryBytes, Input, Kind, Limit, Program, Request, Run, Version,
    WsRef,
};
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::json::Json;
use crate::layout::Axis;
use crate::protocol::{Asked, Body, Context, End, Reply, Response};
use crate::socket::{Choice, SocketPath};
use fuxth::{AppValue, Code as Lang, Compiler, Limits, Machine, Outcome, Token, Value, token};
use std::io::{self, Read, Write};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) use words::INPUT_MAX;
pub use words::{Act, After, Group, Keys, Opt, Row, Screen, Takes, Word, from_opts, usage};

/// The frozen source image whose definitions a client's bindings and prompt see.
pub type Config = Arc<fuxth::SourceImage>;

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
    /// Rendered observations exceeding ordinary language string limits.
    Rendered(fuxth::Shared<String>),
    Pane(PaneId, Option<Run>),
    Tab(TabId, Option<Run>),
    Workspace(WsId, Option<Run>),
    Client(ClientId, Option<Run>),
    /// The whole session, optionally with an event replay cursor.
    All(Option<crate::events::Cursor>),
    /// Options for `run`, `new-tab` or `new-workspace` (from `opts`); or a
    /// request: a pane, tab, workspace or client given options, keys or a
    /// wait, for the word that acts on it after
    /// (`%2 str ls text str Enter key send-keys`).
    Opts(Rc<Opts>),
    /// A width or height: cells, or a share of the tab.
    Extent(Extent),
    /// What the server said: a capture, a listing, a wait's outcome.
    Answer(fuxth::Shared<Answer>, Option<Run>),
}

impl AppValue for Fux {
    fn kind(&self) -> &'static str {
        if let Fux::Answer(answer, _) = self {
            return answer.kind();
        }
        let kind = words::KINDS.iter().find(|(name, ..)| *name == self.name());
        kind.map_or("a value of fux's", |(_, said, _)| said)
    }

    fn name(&self) -> &'static str {
        match self {
            Fux::Null => "null",
            Fux::Record(_) => "record",
            Fux::Rendered(_) => "rendered",
            Fux::Pane(_, _) => "pane",
            Fux::Tab(_, _) => "tab",
            Fux::Workspace(_, _) => "workspace",
            Fux::Client(_, _) => "client",
            Fux::All(None) => "all",
            Fux::All(Some(_)) => "events-request",
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
            Fux::Answer(answer, _) => answer.name(),
        }
    }

    const TEXT_LIMIT: usize = crate::wire::MAX_ALLOCATED;
    const IMPORT_LIMIT: usize = crate::wire::MAX_ALLOCATED;
    fn import_bytes(&self) -> Option<usize> {
        if let Fux::Answer(answer, _) = self
            && matches!(&**answer, Answer::Capture(_))
        {
            return Some(answer.retained_bytes());
        }
        matches!(self, Fux::Rendered(_)).then(|| self.text_bytes())
    }

    fn retain(&self, imports: &fuxth::Imports) -> Result<(), fuxth::Failure> {
        match self {
            Fux::Record(record) => record.retain(imports),
            Fux::Answer(answer, _) => answer.reserve(imports, answer.retained_bytes()),
            Fux::Rendered(text) => text.reserve(imports, self.text_bytes()),
            Fux::Opts(opts) => opts.lease.reserve(
                imports,
                opts.heap_bytes()
                    .saturating_add(std::mem::size_of::<[usize; 2]>()),
            ),
            Fux::Null
            | Fux::Pane(..)
            | Fux::Tab(..)
            | Fux::Workspace(..)
            | Fux::Client(..)
            | Fux::All(_)
            | Fux::Extent(_) => Ok(()),
        }
    }

    fn weight(&self) -> u64 {
        match self {
            Fux::Record(r) => r.weight(),
            Fux::Rendered(text) => text.len() as u64,
            Fux::Answer(a, _) => match &**a {
                Answer::Capture(c) => {
                    let state = match &c.of {
                        crate::capture::Of::Pane { state, .. } => state
                            .iter()
                            .map(|(k, v)| (k.len() as u64).saturating_add(v.weight()))
                            .fold(1u64, u64::saturating_add),
                        crate::capture::Of::Client(_) => 1,
                    };
                    c.lines.iter().flatten().fold(state, |n, cut| {
                        n.saturating_add(cut.text.len() as u64).saturating_add(1)
                    })
                }
                Answer::Pane(_) | Answer::Panes(_) => {
                    u64::try_from(self.text_bytes()).unwrap_or(u64::MAX).max(1)
                }
                Answer::Status(listing) => listing.weight(),
            },
            Fux::All(_)
            | Fux::Null
            | Fux::Pane(..)
            | Fux::Tab(..)
            | Fux::Workspace(..)
            | Fux::Client(..)
            | Fux::Opts(_)
            | Fux::Extent(_) => u64::try_from(self.text_bytes()).unwrap_or(u64::MAX).max(1),
        }
    }
    fn depth(&self) -> u8 {
        match self {
            Fux::Record(r) => r.depth(),
            Fux::Rendered(_)
            | Fux::All(_)
            | Fux::Null
            | Fux::Pane(..)
            | Fux::Tab(..)
            | Fux::Workspace(..)
            | Fux::Client(..)
            | Fux::Opts(_)
            | Fux::Extent(_)
            | Fux::Answer(..) => 0,
        }
    }

    fn text_bytes(&self) -> usize {
        if let Fux::Rendered(text) = self {
            return fuxth::Shared::<String>::bytes(text.capacity());
        }

        if let Fux::Record(r) = self {
            return r.bytes();
        }
        if let Fux::Answer(answer, _) = self {
            let scratch = match &**answer {
                Answer::Capture(c) => c.text_scratch(),
                Answer::Pane(_) | Answer::Panes(_) | Answer::Status(_) => 0,
            };
            answer.retained_bytes().saturating_add(scratch)
        } else if let Fux::Opts(opts) = self {
            opts.heap_bytes()
        } else {
            0
        }
    }

    fn write_text(&self, out: &mut dyn std::fmt::Write) -> Result<(), fuxth::Failure> {
        match self {
            Fux::Null => out.write_str("null"),
            Fux::Record(r) => r.write_text(out),
            Fux::Rendered(text) => out.write_str(text),
            Fux::Pane(p, _) => write!(out, "{p}"),
            Fux::Tab(t, _) => write!(out, "{t}"),
            Fux::Workspace(w, _) => write!(out, "{w}"),
            Fux::Client(c, _) => write!(out, "{c}"),
            Fux::Answer(answer, _) => answer.write_text(out),
            Fux::Extent(Extent::Cells(n)) => write!(out, "{n}"),
            Fux::Extent(Extent::Percent(n)) => write!(out, "{}%", n.get()),
            Fux::All(None) => write!(out, "all"),
            Fux::All(Some(_)) | Fux::Opts(_) => write!(out, "({})", self.kind()),
        }
        .map_err(|_| words::observation_failure(crate::wire::Budget::exceeded()))
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
        Value::Int(_) | Value::Bool(_) | Value::Str(_) | Value::List(_) | Value::App(_) => None,
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
/// they are on: nothing, for `run`, `new-tab` and `new-workspace`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Opts {
    lease: fuxth::ImportLease,
    pub on: Option<On>,
    pub of: Option<Run>,
    pub name: Option<String>,
    pub size: crate::command::Size,
    pub cwd: Option<std::path::PathBuf>,
    pub env: crate::command::EnvPatch,
    pub argv: Vec<String>,
    pub shell: Option<bool>,
    pub history_bytes: Option<HistoryBytes>,
    pub reflow: Option<bool>,
    pub alternate_scroll_lines: Option<crate::command::ScrollLines>,
    pub picture_bytes: Option<crate::command::PictureBytes>,
    pub program_clipboard: Option<bool>,
    pub copy_environment: Option<Vec<EnvName>>,
    pub axis: Option<Axis>,
    /// A split's shares (`shares`).
    pub shares: Option<(std::num::NonZeroU32, std::num::NonZeroU32)>,
    pub place: Option<WsRef>,
    pub kill: bool,
    /// Attaching only to watch (`watch`).
    pub watch: bool,
    /// Where a stream of events reads from (`from`), else now.
    pub from: Option<crate::events::Cursor>,
    /// How long a wait waits before it times out.
    pub timeout: Option<Limit>,
    pub view: crate::command::CaptureView,
    pub history: Option<usize>,
    pub inputs: Vec<Input>,
    /// The words given, for the word they go on to refuse any it would not
    /// use.
    pub given: Vec<words::Opt>,
}

impl Opts {
    pub(crate) fn pane_options(
        &self,
        defaults: crate::command::PaneOptions,
    ) -> crate::command::PaneOptions {
        crate::command::PaneOptions {
            reflow: self.reflow.unwrap_or(defaults.reflow),
            alternate_scroll_lines: self
                .alternate_scroll_lines
                .unwrap_or(defaults.alternate_scroll_lines),
            picture_bytes: self.picture_bytes.unwrap_or(defaults.picture_bytes),
            history_bytes: self.history_bytes.unwrap_or(defaults.history_bytes),
            program_clipboard: self.program_clipboard.unwrap_or(defaults.program_clipboard),
        }
    }

    pub(crate) fn program(&self) -> Result<Program, String> {
        let mut words = self.argv.iter().cloned();
        Program::new(words.next().unwrap_or_default(), words.collect())
    }

    pub(crate) fn launch(
        &self,
        shell: &Program,
        selection: &[EnvName],
    ) -> Result<(crate::command::Start, crate::command::EnvPatch), String> {
        let argv = if self.argv.is_empty() {
            shell.clone()
        } else {
            self.program()?
        };
        let mut env = crate::command::EnvPatch::selected(
            self.copy_environment.as_deref().unwrap_or(selection),
        )?;
        env.overlay(&self.env)?;
        let start = if self.shell == Some(false) {
            crate::command::Start::Program(argv)
        } else {
            crate::command::Start::Shell(argv)
        };
        Ok((start, env))
    }

    fn heap_bytes(&self) -> usize {
        let mut bytes = std::mem::size_of::<Opts>()
            .saturating_add(self.name.as_ref().map_or(0, String::capacity))
            .saturating_add(self.cwd.as_ref().map_or(0, std::path::PathBuf::capacity))
            .saturating_add(self.env.heap_bytes())
            .saturating_add(
                self.argv
                    .capacity()
                    .saturating_mul(std::mem::size_of::<String>()),
            )
            .saturating_add(
                self.inputs
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Input>()),
            )
            .saturating_add(
                self.given
                    .capacity()
                    .saturating_mul(std::mem::size_of::<words::Opt>()),
            );
        if let Some(names) = &self.copy_environment {
            bytes = bytes.saturating_add(
                names
                    .capacity()
                    .saturating_mul(std::mem::size_of::<EnvName>()),
            );
            for name in names {
                bytes = bytes.saturating_add(name.heap_bytes());
            }
        }
        for arg in &self.argv {
            bytes = bytes.saturating_add(arg.capacity());
        }
        if let Some(WsRef::Name(name)) = &self.place {
            bytes = bytes.saturating_add(name.capacity());
        }
        for input in &self.inputs {
            bytes = bytes.saturating_add(match input {
                Input::Key(_) => 0,
                Input::Text(text) | Input::Paste(text) => text.capacity(),
                Input::Bytes(bytes) => bytes.capacity(),
            });
        }
        bytes
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
    Capture(Box<crate::capture::Captured>),
    /// `status`: the server that answers, and its version.
    Status(Json),
}

impl Answer {
    fn retained_bytes(&self) -> usize {
        let mut budget = crate::wire::Budget::with_limit(usize::MAX);
        if budget
            .charge(fuxth::Shared::<Answer>::bytes(0))
            .and_then(|()| self.charge(&mut budget))
            .is_err()
        {
            return usize::MAX;
        }
        usize::MAX.saturating_sub(budget.remaining())
    }

    /// Its kind, as the vocabulary names it.
    fn name(&self) -> &'static str {
        match self {
            Answer::Pane(PaneAnswer::Closed(_)) => "closed-answer",
            Answer::Pane(_) => "answer",
            Answer::Panes(_) => "answers",
            Answer::Capture(captured) => match captured.of {
                crate::capture::Of::Pane { .. } => "capture",
                crate::capture::Of::Client(_) => "client-capture",
            },
            Answer::Status(_) => "listing",
        }
    }

    /// What it is, said where it is refused.
    fn kind(&self) -> &'static str {
        match self {
            Answer::Pane(PaneAnswer::Closed(_)) => "a closed pane's answer",
            Answer::Pane(_) => "an action's answer",
            Answer::Panes(_) => "an answer of a tab's or workspace's panes",
            Answer::Capture(captured) => match captured.of {
                crate::capture::Of::Pane { .. } => "a capture",
                crate::capture::Of::Client(_) => "a client's capture",
            },
            Answer::Status(_) => "a status",
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
            Answer::Status(status) => write_status(status, out),
        }
    }

    fn charge(&self, budget: &mut crate::wire::Budget) -> Result<(), crate::session::Error> {
        match self {
            Answer::Capture(captured) => {
                budget.slots::<crate::capture::Captured>(1)?;
                captured.charge(budget)
            }
            Answer::Status(json) => json.charge(budget),
            Answer::Pane(answer) => answer.charge(budget),
            Answer::Panes(answers) => {
                budget.slots::<PaneAnswer>(answers.capacity())?;
                for answer in answers {
                    answer.charge(budget)?;
                }
                Ok(())
            }
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
            Answer::Status(json) => json.write(out),
        }
    }

    /// It says nothing printed: a capture of no lines, no pane's answers.
    /// A capture of one blank line says that line, and a listing says at
    /// least where events read from.
    fn is_empty(&self) -> bool {
        match self {
            Answer::Capture(captured) => captured.lines.is_empty(),
            Answer::Panes(answers) => answers.is_empty(),
            Answer::Pane(_) | Answer::Status(_) => false,
        }
    }

    /// The version it holds: a pane's answer's, which a wait on it waits
    /// from; or a pane's capture's, its pane's as it was read, which
    /// `expect` sends at and `changed` waits from.
    pub fn version(&self) -> Option<Version> {
        match self {
            Answer::Pane(answer) => answer.version(),
            Answer::Capture(captured) => match captured.of {
                crate::capture::Of::Pane { version, .. } => Some(version),
                crate::capture::Of::Client(_) => None,
            },
            Answer::Panes(_) | Answer::Status(_) => None,
        }
    }

    /// An answer, as a value on the stack.
    fn value(self) -> Value<Fux> {
        self.in_run(None)
    }

    fn in_run(self, run: Option<Run>) -> Value<Fux> {
        Value::App(Fux::Answer(fuxth::Shared::new(self), run))
    }
}

/// `status`'s text: the server's version and socket, and if it speaks
/// another protocol, how to stop it.
fn write_status(status: &Json, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
    let field = |key| crate::answer::Field(status.get(key), "");
    write!(
        out,
        "fux {} at {}",
        field("server_release"),
        field("socket")
    )?;
    if status.get("compatible") == Some(&Json::Bool(false)) {
        write!(
            out,
            " speaks protocol {}, not this fux's ({}, fux {}): `fux force-kill-server` stops it",
            field("server_protocol"),
            field("protocol"),
            field("release")
        )?;
    }
    Ok(())
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
}

/// The keys of a binding just made.
#[derive(Clone, Debug, PartialEq)]
pub enum Bound {
    /// Keys after the prefix.
    Prefix(Vec<crate::keys::KeyPress>),
    /// One key, bound without the prefix.
    Root(crate::keys::KeyPress),
    /// One key of a screen of fux's own.
    In(crate::keymap::Modal, crate::keys::KeyPress),
}

/// What the client does itself.
#[derive(Clone, Debug, PartialEq)]
pub enum Local {
    /// This terminal, or a client of no terminal of `size`.
    Attach {
        of: Option<Run>,
        target: Option<crate::command::AttachTarget>,
        read_only: bool,
        size: Option<fux_vt::Size>,
        creation: Box<Opts>,
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
/// candidate client state, and what its output goes to.
pub struct Host<'a> {
    pub context: Context,
    /// The whole candidate, changed only by local setters.
    pub state: Option<&'a mut Draft>,
    /// The binding `label`, `group` and `repeat` change.
    pub last_bound: Option<Bound>,
    pub out: &'a mut dyn Write,
    cli: bool,
}

impl<'a> Host<'a> {
    pub fn new(context: Context, state: Option<&'a mut Draft>, out: &'a mut dyn Write) -> Host<'a> {
        Host {
            context,
            state,
            last_bound: None,
            out,
            cli: false,
        }
    }

    fn cli(context: Context, state: &'a mut Draft, out: &'a mut dyn Write) -> Self {
        Self {
            cli: true,
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

/// A run's failure: its code, cause, and source when known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed {
    pub code: Code,
    pub message: String,
    pub place: Option<token::Place>,
}

impl Failed {
    /// A failure of kind `code`, and why.
    pub fn new(code: impl Into<Code>, message: impl Into<String>) -> Failed {
        Failed {
            code: code.into(),
            message: message.into(),
            place: None,
        }
    }
    /// The cause before its source, for a notice with limited room.
    pub fn notice(&self) -> String {
        let message = match &self.place {
            Some(place) => format!("{} ({place})", self.message),
            None => self.message.clone(),
        };
        Failed::new(self.code, message).to_string()
    }

    /// The source followed by the cause, for command-line diagnostics.
    fn located(&self) -> String {
        match &self.place {
            Some(place) => format!("{place}: {}", self.message),
            None => self.message.clone(),
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
        let lines: Vec<String> = (self.located().lines().map(str::trim))
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
        Lang::Require
        | Lang::OpenQuote
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
        let word = error.word.as_deref().and_then(Word::named);
        error.message += &format!(": end with {}", words::takers(word));
    }
    if usage && let Some(usage) = error.word.as_deref().and_then(words::usage) {
        error.message.push_str(&format!(" ({usage})"));
    }
    if error.code == Lang::UnknownWord {
        error.message.push_str(" (`fux help` lists every word)");
    }
    if Code::of(error.code) == Code::Check {
        (error.place, error.word) = (None, None);
    }
    let place = error.place.take();
    Failed {
        code: Code::of(error.code),
        message: error.located(),
        place,
    }
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
    for (kind, also) in words::TAKEN_AS {
        vocabulary.taken_as(kind, also)?;
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
    for word in Word::ALL {
        vocabulary.add(word.row().name, word.row().effect)?;
    }
    for code in Code::all().filter(|c| c.raised()) {
        vocabulary.raises(code.into())?;
    }
    // A wait that failed answered too: its handler gets the answer.
    vocabulary.carries(Code::Closed.into(), "closed-outcome")?;
    vocabulary.carries(Code::Timeout.into(), "outcome")?;
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
    let broken = |f: Failed| fuxth::Error::new(Lang::Contract, f.located(), None);
    let ready = READY.with(|ready| ready.as_ref().map(|(c, _)| c.clone()).map_err(Clone::clone));
    let mut compiler = ready.map_err(broken)?;
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
    compiler.add(&handles(&config.tokens())?)?;
    let ready = compiler.definitions();
    CONFIGURED.set(Some((Arc::clone(config), ready.clone())));
    Ok(ready)
}

thread_local! {
    /// The standard library and fux's, compiled and checked once a thread,
    /// for every program after; and their words' help.
    static READY: Result<(Compiler<Fux>, Rc<[fuxth::Doc]>), Failed> = libraries();
    /// The last config's, as `configured_compiler` made it.
    static CONFIGURED: std::cell::RefCell<Option<(Config, Compiler<Fux>)>> =
        const { std::cell::RefCell::new(None) };
}

/// The client's libraries, compiled and checked without the thread-local
/// cache; the bench counts this initialization cold. Its mistake is told
/// without a word's usage, which would read `READY` as it is made.
pub fn libraries() -> Result<(Compiler<Fux>, Rc<[fuxth::Doc]>), Failed> {
    let told = |error| told(error, false);
    let mut compiler = Compiler::new(vocabulary().map_err(told)?);
    let mut docs = compiler
        .library("stdlib.fux", fuxth::STDLIB)
        .map_err(told)?;
    docs.extend(compiler.library("library.fux", LIBRARY).map_err(told)?);
    compiler.inputs("dump", "scope").map_err(told)?;
    for word in [
        "layout-current",
        "even-h",
        "even-v",
        "main-h",
        "main-v",
        "tiled",
    ] {
        compiler.inputs(word, "tab").map_err(told)?;
    }
    compiler.prepare();
    Ok((compiler, docs.into()))
}

/// The standard library's and fux's library's words, as help says them.
/// Libraries that fail to compile leave none, but no help asks then: every
/// program, `help` too, fails first with why.
pub fn docs() -> Rc<[fuxth::Doc]> {
    READY.with(|ready| {
        ready
            .as_ref()
            .map_or_else(|_| Rc::from([]), |(_, docs)| Rc::clone(docs))
    })
}

/// A bare id is the handle it names: `%3` is `str %3 pane`, `@2` is
/// `str @2 tab`, `+1` is `str +1 workspace`; but not where it is the token
/// a parsing word reads (`str`'s text). Nor may one be given as a name
/// (by `define` or `let`): it would always be the handle. Nor may a name
/// be a word an id becomes (`pane`): every id would then be its.
fn handles(tokens: &[Token]) -> Result<Vec<Token>, fuxth::Error> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut after: Option<fuxth::Read> = None;
    for token in tokens {
        let id = token.text.parse::<AnyRef>().ok();
        let named = |why: String| fuxth::Error::new(Lang::BadName, why, Some(token));
        match (after, id) {
            (Some(fuxth::Read::Name), None)
                if Kind::ALL.iter().any(|k| k.name() == &*token.text) =>
            {
                let kind = &token.text;
                return Err(named(format!(
                    "cannot be defined: a {kind} id is `str ID {kind}`"
                )));
            }
            (Some(fuxth::Read::Name), Some(id)) => {
                return Err(named(format!(
                    "cannot be defined: it names a {}",
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
            (Some(fuxth::Read::Name), None) | (Some(fuxth::Read::Text), _) | (None, None) => {
                out.push(token.clone())
            }
        }
        let reads = fuxth::Parsing::named(&token.text).and_then(fuxth::Parsing::reads);
        after = after.is_none().then_some(reads).flatten();
    }
    Ok(out)
}

/// Where a program's requests go: a connection made when the first is.
#[derive(Default)]
pub struct Server {
    chosen: Choice,
    requests: Option<client::Requests>,
    /// This job's source fence, kept on every request and across retries.
    pub(crate) after: Option<std::rc::Rc<std::cell::Cell<crate::protocol::Fence>>>,
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
            after: None,
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
    fn ask(&mut self, asked: &Asked, out: &mut dyn Write) -> Result<Reply, Failed> {
        let fenced = self.after.as_ref().map(|after| Asked {
            after: Some(after.get()),
            ..asked.clone()
        });
        let asked = fenced.as_ref().unwrap_or(asked);
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
    fn local(
        &mut self,
        what: &Local,
        state: Option<&mut Draft>,
        _out: &mut dyn Write,
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
                let list = fuxth::List::new(args)
                    .ok_or_else(|| Failed::new(Code::Usage, "more arguments than a list holds"))?;
                vec![Value::List(std::rc::Rc::new(list))]
            }
            Shell::Input => match self.from {
                Given::Stdin => {
                    let why = "stdin: the program itself was read from it";
                    return Err(Failed::new(Code::Read, why));
                }
                Given::Words | Given::File(_) => {
                    string(&read_text("stdin", io::stdin(), SOURCE_MAX)?)
                }
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
    pub on_commit: Option<&'a mut dyn FnMut(&ClientState)>,
}

impl Asks for AsClient<'_> {
    fn published(&mut self, state: &ClientState, context: &Context) {
        if let Some(publish) = &mut self.on_commit {
            publish(state);
        }
        self.asks.published(state, context);
    }
    fn ask(&mut self, asked: &Asked, out: &mut dyn Write) -> Result<Reply, Failed> {
        self.asks.ask(asked, out)
    }
    fn local(
        &mut self,
        what: &Local,
        state: Option<&mut Draft>,
        out: &mut dyn Write,
    ) -> Result<Vec<Value<Fux>>, Failed> {
        let Local::Open(open) = what else {
            return self.asks.local(what, state, out);
        };
        let (asks, context) = (&mut *self.asks, &self.context);
        let mut sink = io::sink();
        let screen = open.clone().build(&mut |command| {
            asks.ask(
                &Asked {
                    after: None,
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
                match asks.ask(&asked, host.out) {
                    Ok(reply) => match reply.end() {
                        End::Failed { code, message } => raised(machine, (code, message), reply)?,
                        End::Done => pushed(machine, answered(reply))?,
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
                if let (Local::Shell(Shell::Exit(_)), Ok(_)) = (&what, &values) {
                    return Ok(machine.stack().to_vec());
                }
                pushed(machine, values)?
            }
        }
    }
}

/// Raises a failed reply's failure at the word. A pane's answer's
/// failure (`PaneAnswer::FAILURES`) carries its answer, which a `try` that catches it gets for its
/// message; uncaught, the answer is left, printed before why it failed.
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
    match machine.raise_with(fuxth::Failure::new(code.into(), message), answer.clone()) {
        Ok(()) => Ok(()),
        Err(error) => {
            // Only the original uncaught wait failure prints its answer.
            // Admission or recovery refusals must not import it again.
            if error.code == code.into() {
                drop(machine.import(answer));
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
        machine.raise(failure).map_err(|e| uncaught(machine, e))
    };
    let values = match values {
        Ok(values) => values,
        Err(failed) => return raise(machine, failed.code.into(), failed.located()),
    };
    // A stack too full for them is the word's failure too, for a `try`.
    for value in values {
        if let Err(full) = machine.import(value) {
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
    let step = bundled(error.place.as_ref()).then(|| {
        machine
            .calls()
            .into_iter()
            .rev()
            .find(|t| !bundled(Some(&t.place)))
    });
    let Some(step) = step.flatten() else {
        return telling(error);
    };
    let inner = error.word.as_deref().unwrap_or_default();
    error.message = format!("inside {}, {inner}: {}", step.text, error.message);
    error.place = Some(step.place);
    error.word = Some(step.text);
    told(error, true)
}

/// A reply's answer, or its failure.
pub fn succeeded(reply: Reply) -> Result<Reply, Failed> {
    match reply.end() {
        End::Failed { code, message } => Err(Failed::new(code, message)),
        End::Done => Ok(reply),
    }
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
        let list = fuxth::List::new(ids)
            .ok_or_else(|| Failed::new(Code::from(Raise::TooLong), "more ids than a list holds"))?;
        Ok::<_, Failed>(Value::List(std::rc::Rc::new(list)))
    };
    Ok(vec![match body {
        Body::Empty => return Ok(Vec::new()),
        Body::Text(text) => Value::str(text.trim_end_matches('\n')),
        Body::Found(id) => Value::App(handle(id, run)),
        Body::Handles(ids) => handles(ids)?,
        Body::Choices { rows, .. } => handles(rows.into_iter().map(|(id, _)| id).collect())?,
        Body::Capture(captured) => Answer::Capture(captured).in_run(run),
        Body::Snapshot(snapshot) => {
            if run != Some(snapshot.run) {
                return Err(Failed::new(
                    Code::of(fuxth::Code::Contract),
                    "snapshot run differs from reply",
                ));
            }
            snapshot
                .validate()
                .map_err(|e| Failed::new(Code::of(fuxth::Code::Contract), e.to_string()))?;
            record::snapshot(*snapshot).map_err(|e| Failed::new(Code::of(e.code), e.message))?
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
/// attaching, choosing, starting and stopping a server, and its status.
fn local(
    what: &Local,
    server: &mut Server,
    draft: Option<&mut Draft>,
) -> Result<Vec<Value<Fux>>, Failed> {
    match what {
        Local::Server(chosen) => server.choose(chosen.clone()),
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
                false => started(server)?,
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
            client::attach(&socket, attach, *size).map_err(client_failed)?;
        }
        Local::Start => {
            started(server)?;
        }
        Local::ForceKill => client::kill_server(&server.socket()?).map_err(client_failed)?,
        Local::Open(_) | Local::Shell(_) => return refuse_local(what),
        Local::Status => {
            let socket = server.socket()?;
            let probe = client::probe(&socket).map_err(client_failed)?;
            let status =
                crate::status(&socket, &probe).map_err(|why| Failed::new(Code::NoServer, why))?;
            return Ok(vec![Answer::Status(status).value()]);
        }
    }
    Ok(Vec::new())
}

/// The server's socket, starting it in the background if none answers.
fn started(server: &mut Server) -> Result<SocketPath, Failed> {
    let socket = server.socket()?;
    if !client::answers(&socket) {
        client::start_server(&socket).map_err(client_failed)?;
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
    let shown = path.display().to_string();
    let (path, file) = match source::root(&path) {
        Ok(file) => file,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                && path
                    .symlink_metadata()
                    .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
        {
            return Ok(defaults.clone());
        }
        Err(error) => return Err(read_failed(&shown, &error.to_string())),
    };
    run_config_on(source::opened_file(path, file)?, defaults)
}

/// Reads a frozen config graph and runs it without a transport capability.
pub fn run_file(path: &std::path::Path) -> Result<ClientState, Failed> {
    run_config_on(source::file(path)?, &default_state()?)
}

fn run_config(image: fuxth::SourceImage) -> Result<ClientState, Failed> {
    run_config_on(image, &default_state()?)
}

fn run_config_on(image: fuxth::SourceImage, defaults: &ClientState) -> Result<ClientState, Failed> {
    let tokens = image.tokens();
    let mut draft = defaults.draft();
    draft.config = Some(Arc::new(image));
    run_quiet(&tokens, Some(&mut draft), false)?;
    draft.finish().map_err(|why| Failed::new(Code::Config, why))
}

/// Runs bounded config code with only local reads and draft mutations.
fn run_quiet(tokens: &[Token], state: Option<&mut Draft>, diagnostics: bool) -> Result<(), Failed> {
    let mut machine = compile(tokens)?;
    let mut out = io::sink();
    let mut host = Host::new(Context::default(), state, &mut out);
    run_capped(
        &mut machine,
        &mut host,
        &mut ConfigOnly { diagnostics },
        Some(PROGRAM_STEPS),
        None,
    )
    .map(drop)
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
    fn ask(&mut self, _: &Asked, _: &mut dyn Write) -> Result<Reply, Failed> {
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
            | Local::Server(_)
            | Local::Start
            | Local::Status
            | Local::ForceKill
            | Local::Open(_)
            | Local::Shell(_) => refuse_local(what),
        }
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
    if matches!(args, [flag] if matches!(flag.as_str(), "--version" | "-V")) {
        writeln!(out, "{}", env!("CARGO_PKG_VERSION"))
            .map_err(|e| Failed::new(Code::Output, format!("writing the output: {e}")))?;
        return Ok(0);
    }
    let (tokens, from) = program(args)?;
    // Only words can be a file mistaken for a program.
    let mut machine = compiled(None, &tokens).map_err(|e| match from {
        Given::Words => Failed::from(a_file(e, args)),
        Given::Stdin | Given::File(_) => Failed::from(e),
    })?;
    let mut state = Draft::default();
    let mut host = Host::cli(client::context(), &mut state, out);
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
            !matches!(value, Value::App(Fux::Answer(a, _)) if a.is_empty())
                && !matches!(value, Value::App(Fux::Opts(_)))
        })
        .try_for_each(|value| {
            let text = value
                .text()
                .map_err(|f| Failed::new(Code::of(f.code), f.message))?;
            writeln!(host.out, "{text}")
                .map_err(|e| Failed::new(Code::Output, format!("writing the output: {e}")))
        })
        .and_then(|()| {
            host.out
                .flush()
                .map_err(|e| Failed::new(Code::Output, format!("writing the output: {e}")))
        });
    ran?;
    printed?;
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
        [] => token::arguments(&["default-attach"]),
        // A program read whole, at most `SOURCE_MAX` of it.
        [dash] if dash == "-" => {
            let text = read_text("stdin", io::stdin(), SOURCE_MAX)?;
            return Ok((source::text("stdin", &text)?.tokens(), Given::Stdin));
        }
        [flag, file, rest @ ..] if flag == "--file" => {
            let tokens = source::file(file.as_ref())?.tokens();
            return Ok((tokens, Given::File(rest.to_vec())));
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
        [line] => return Ok((source::text("command line", line)?.tokens(), Given::Words)),
        words => token::arguments(words),
    };
    Ok((source::arguments(&words)?.tokens(), Given::Words))
}

/// The most steps a command line's program takes: one that loops without
/// end fails rather than spinning.
pub(crate) const LINE_STEPS: u64 = 1_000_000_000;

/// The most text read from a file or stdin may be: a program, or what
/// `read-file` and `input` read.
pub(crate) use fuxth::SOURCE_MAX;

fn read_failed(name: &str, why: &str) -> Failed {
    Failed::new(Code::Read, format!("{why} ({name})"))
}

/// The text read from `from`, which `name` names and which may be anything
/// (`/dev/zero`): at most `limit` bytes of UTF-8.
fn read_text(name: &str, from: impl Read, limit: usize) -> Result<String, Failed> {
    let mut bytes = Vec::new();
    let most = u64::try_from(limit).unwrap_or(u64::MAX);
    (from.take(most.saturating_add(1)))
        .read_to_end(&mut bytes)
        .map_err(|e| read_failed(name, &e.to_string()))?;
    if bytes.len() > limit {
        let why = format!("more than {limit} bytes, the most fux reads");
        return Err(read_failed(name, &why));
    }
    String::from_utf8(bytes).map_err(|_| read_failed(name, "not UTF-8 text"))
}

/// The text of the file at `path`, as `read_text` reads it.
fn read_file(path: &std::path::Path) -> Result<String, Failed> {
    let name = path.display().to_string();
    let file = std::fs::File::open(path).map_err(|e| read_failed(&name, &e.to_string()))?;
    read_text(&name, file, SOURCE_MAX)
}

#[cfg(test)]
mod tests {
    fn held_reply(
        code: &str,
        reply: super::Reply,
        out: &mut Vec<u8>,
    ) -> Result<Vec<fuxth::Value<super::Fux>>, super::Failed> {
        struct Once(Option<super::Reply>);
        impl super::Asks for Once {
            fn ask(
                &mut self,
                asked: &crate::protocol::Asked,
                _: &mut dyn std::io::Write,
            ) -> Result<super::Reply, super::Failed> {
                if let crate::command::Request::Do(crate::command::Command::Resolve {
                    text, ..
                }) = &asked.request
                {
                    let id = text.parse().map_err(|()| {
                        super::Failed::new(crate::code::Code::Usage, "test target")
                    })?;
                    return Ok(super::Reply {
                        run: self.0.as_ref().and_then(|r| r.run),
                        ..crate::protocol::Body::Found(id).into()
                    });
                }
                self.0.take().ok_or_else(|| {
                    super::Failed::new(crate::code::Code::Usage, "held field made another request")
                })
            }
        }
        let tokens = fuxth::token::source("test", code).map_err(super::Failed::from)?;
        let mut machine = super::compile(&tokens)?;
        let mut host = super::Host::new(crate::protocol::Context::default(), None, out);
        super::run_capped(&mut machine, &mut host, &mut Once(Some(reply)), None, None)
    }

    #[test]
    fn ended_answers_expose_both_exit_fields() -> Result<(), Box<dyn std::error::Error>> {
        use super::*;
        use crate::process::Ended;
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
            )
            .map_err(|e| e.to_string())?;
            assert_eq!(values, vec![exited, signal]);
        }
        Ok(())
    }

    #[test]
    fn retained_captures_exhaust_the_grant_but_polling_releases_it()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::*;
        struct Captures {
            count: usize,
        }
        impl Asks for Captures {
            fn ask(
                &mut self,
                asked: &crate::protocol::Asked,
                _: &mut dyn std::io::Write,
            ) -> Result<Reply, Failed> {
                let version: Version = "%1:0.7.0.0.0:1-1"
                    .parse()
                    .map_err(|()| Failed::new(Code::Usage, "version"))?;
                if matches!(
                    asked.request,
                    Request::Do(crate::command::Command::Resolve { .. })
                ) {
                    return Ok(Reply {
                        run: Some(version.run),
                        ..Body::Found(AnyRef::Pane(version.pane)).into()
                    });
                }
                self.count += 1;
                let capture = crate::capture::Captured {
                    of: crate::capture::Of::Pane {
                        version,
                        wrap_pending: false,
                        state: Vec::new(),
                    },
                    screen_rows: 24,
                    screen_cols: 80,
                    screen_cursor: None,
                    lines: vec![vec![crate::capture::Cut {
                        text: std::iter::repeat_n('x', 17_000_000).collect(),
                        cells: 17_000_000,
                        style: fux_vt::Attributes::default(),
                    }]],
                    styles: false,
                    marks: false,
                };
                Ok(Reply {
                    run: Some(version.run),
                    ..Body::Capture(Box::new(capture)).into()
                })
            }
        }
        for retain in [false, true] {
            let source = if retain {
                std::iter::repeat_n("%1 all-history capture ", 9).collect::<String>()
            } else {
                "0 20 range each drop %1 all-history capture drop end".into()
            };
            let mut machine =
                compile(&fuxth::token::source("test", &source)?).map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            let mut host = Host::new(crate::protocol::Context::default(), None, &mut out);
            let mut asks = Captures { count: 0 };
            let result = run_capped(&mut machine, &mut host, &mut asks, None, None);
            if retain {
                assert_eq!(
                    result.map(|_| ()).map_err(|e| e.code),
                    Err(Code::from(crate::code::Raise::TooLong))
                );
                assert!(asks.count < 9);
            } else {
                assert!(result.map_err(|e| e.to_string())?.is_empty());
                assert_eq!(asks.count, 20);
            }
        }
        Ok(())
    }

    #[test]
    fn large_plain_capture_crosses_the_machine_boundary() -> Result<(), Box<dyn std::error::Error>>
    {
        let version: super::Version = "%1:0.7.0.0.0:1-1".parse().map_err(|()| "version")?;
        let capture = crate::capture::Captured {
            of: crate::capture::Of::Pane {
                version,
                wrap_pending: false,
                state: Vec::new(),
            },
            screen_rows: 24,
            screen_cols: 80,
            screen_cursor: None,
            lines: vec![vec![crate::capture::Cut {
                text: std::iter::repeat_n('x', 17_000_000).collect(),
                cells: 17_000_000,
                style: fux_vt::Attributes::default(),
            }]],
            styles: false,
            marks: false,
        };
        let reply = super::Reply {
            run: Some(version.run),
            ..crate::protocol::Body::Capture(Box::new(capture)).into()
        };
        assert!(crate::wire::size(&reply)? <= crate::wire::MAX_REPLY);
        let mut out = Vec::new();
        held_reply("%1 all-history capture print", reply, &mut out).map_err(|e| e.to_string())?;
        assert_eq!(out.len(), 17_000_001);
        Ok(())
    }

    #[test]
    fn large_capture_json_crosses_the_machine_boundary_with_its_byte_grant()
    -> Result<(), Box<dyn std::error::Error>> {
        let version = "%1:0.0.0.0.0:1-1".parse().map_err(|()| "version")?;
        let line: Vec<_> = (0u8..100)
            .map(|color| crate::capture::Cut {
                text: "x".into(),
                cells: 1,
                style: fux_vt::Attributes::new(fux_vt::Color::Idx(color), fux_vt::Color::Default),
            })
            .collect();
        let capture = crate::capture::Captured {
            of: crate::capture::Of::Pane {
                version,
                wrap_pending: false,
                state: Vec::new(),
            },
            screen_rows: 40,
            screen_cols: 120,
            screen_cursor: None,
            lines: std::iter::repeat_n(line, 9931).collect(),
            styles: true,
            marks: false,
        };
        let reply = super::Reply {
            run: Some(version.run),
            ..crate::protocol::Body::Capture(Box::new(capture)).into()
        };
        assert!(crate::wire::size(&reply)? <= crate::wire::MAX_REPLY);
        let mut out = Vec::new();
        let result = held_reply(
            "%1 all-history styles capture let h h json print list h h h end json",
            reply,
            &mut out,
        );
        assert!(
            out.len() as u64 > fuxth::TOTAL,
            "admitted capture JSON was not printed: {result:?}"
        );
        assert!(out.starts_with(b"{\"pane\":\"%1\"") && out.ends_with(b"}\n"));
        assert_eq!(
            result.map_err(|e| e.code),
            Err(crate::code::Code::from(crate::code::Raise::TooLong))
        );
        Ok(())
    }

    #[test]
    fn held_answers_and_captures_project_without_refreshing()
    -> Result<(), Box<dyn std::error::Error>> {
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
            )
            .map_err(|e| e.to_string())?,
            vec![Value::str(version.to_string())]
        );
        let capture = crate::capture::Captured {
            of: crate::capture::Of::Pane {
                version,
                wrap_pending: false,
                state: vec![("last_status".into(), Json::Int(7))],
            },
            screen_rows: 24,
            screen_cols: 80,
            screen_cursor: None,
            lines: Vec::new(),
            styles: false,
            marks: false,
        };
        let reply = Reply {
            run: Some(version.run),
            ..Body::Capture(Box::new(capture)).into()
        };
        assert_eq!(
            held_reply("%1 capture str last_status get", reply, &mut Vec::new())
                .map_err(|e| e.to_string())?,
            vec![Value::Int(7)]
        );
        let reply = Reply {
            run: Some(version.run),
            response: Response::Answered(PaneResults::One(PaneAnswer::Closed(version.pane))),
        };
        assert_eq!(
            held_reply(
                "str closed try %1 0 timeout wait catch drop str pane get end",
                reply,
                &mut Vec::new()
            )
            .map_err(|e| e.to_string())?,
            vec![Value::App(Fux::Pane(version.pane, Some(version.run)))]
        );
        let client = crate::id::ClientId::of(1);
        let capture = crate::capture::Captured {
            of: crate::capture::Of::Client(client),
            screen_rows: 24,
            screen_cols: 80,
            screen_cursor: Some((7, 3)),
            lines: vec![vec![crate::capture::Cut {
                text: "x".into(),
                cells: 1,
                style: fux_vt::Attributes::new(fux_vt::Color::Idx(1), fux_vt::Color::Default),
            }]],
            styles: true,
            marks: false,
        };
        let reply = Reply {
            run: Some(version.run),
            ..Body::Capture(Box::new(capture)).into()
        };
        let values = held_reply("str c1 client capture let held held str client get held str screen_cursor get held str lines get first held str styles get first first str fg get", reply, &mut Vec::new()).map_err(|e| e.to_string())?;
        assert_eq!(
            values,
            vec![
                Value::App(Fux::Client(client, Some(version.run))),
                Value::List(Rc::new(
                    fuxth::List::new(vec![Value::Int(7), Value::Int(3)]).ok_or("cursor")?
                )),
                Value::str("x"),
                Value::Int(1)
            ]
        );
        Ok(())
    }

    #[test]
    fn attach_words_accept_compiled_action_answers_and_keep_their_run()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::*;
        use crate::command::{AttachTarget, Command, Request};
        struct Record {
            version: Version,
            sent: usize,
            attached: Option<(Option<Run>, Option<AttachTarget>, Option<fux_vt::Size>)>,
        }
        impl Asks for Record {
            fn ask(&mut self, asked: &Asked, _: &mut dyn Write) -> Result<Reply, Failed> {
                let mut reply: Reply = if matches!(
                    asked.request,
                    Request::Do(crate::command::Command::Resolve { .. })
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
            let mut machine =
                compile(&token::source("answer-attach", &code)?).map_err(|e| e.to_string())?;
            let mut draft = default_state().map_err(|e| e.to_string())?.draft();
            let mut sink = io::sink();
            let mut host = Host::new(Context::default(), Some(&mut draft), &mut sink);
            let mut record = Record {
                version,
                sent: 0,
                attached: None,
            };
            run_capped(
                &mut machine,
                &mut host,
                &mut record,
                Some(PROGRAM_STEPS),
                None,
            )
            .map_err(|e| e.to_string())?;
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
        for baseline in ["changed", "expect"] {
            for action in ["attach", "attach-defaults"] {
                let code = format!("str {version} {baseline} {action}");
                assert!(
                    compile(&token::source("baseline", &code)?).is_err(),
                    "{code}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn release_flags_bypass_the_operand_taking_version_word()
    -> Result<(), Box<dyn std::error::Error>> {
        for flag in ["--version", "-V"] {
            let mut out = Vec::new();
            assert_eq!(
                super::main(&[flag.into()], &mut out).map_err(|e| e.to_string())?,
                0
            );
            assert_eq!(
                String::from_utf8(out)?,
                format!("{}\n", env!("CARGO_PKG_VERSION"))
            );
        }
        assert!(super::main(&["version".into()], &mut Vec::new()).is_err());
        Ok(())
    }

    #[test]
    fn every_observation_wait_encodes_its_clients_limit() -> Result<(), Box<dyn std::error::Error>>
    {
        use super::*;
        use crate::command::{AnyRef, Command, Request};
        use crate::wire::Reader;
        struct Record(Vec<u32>);
        impl Asks for Record {
            fn ask(&mut self, asked: &Asked, _: &mut dyn Write) -> Result<Reply, Failed> {
                let bytes = crate::wire::encode(asked)
                    .map_err(|e| Failed::new(Code::Usage, e.to_string()))?;
                let decoded: Asked = Reader::new(&bytes)
                    .whole()
                    .map_err(|e| Failed::new(Code::Usage, e.to_string()))?;
                if let Request::Wait(wait) = decoded.request {
                    self.0.push(wait.timeout.seconds());
                    Err(Failed::new(Code::NoServer, "recorded"))
                } else if let Request::Do(crate::command::Command::Resolve { .. }) = decoded.request
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
        let bundled = default_state().map_err(|e| e.to_string())?;
        let personal = run_config(
            source::text("personal", "str wait-seconds 2 set").map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        for subject in [
            "%1",
            "str %1:0.0.0.0.0:1-2 changed",
            "%1 str Enter key send-keys",
        ] {
            for (state, option, limit) in [
                (&personal, "", 2),
                (&bundled, "", 30),
                (&personal, "0 timeout", 0),
            ] {
                let mut draft = state.draft();
                let mut out = io::sink();
                let mut host = Host::new(Context::default(), Some(&mut draft), &mut out);
                let mut machine =
                    compile(&token::source("wait", &format!("{subject} {option} wait"))?)
                        .map_err(|e| e.to_string())?;
                let mut record = Record(Vec::new());
                let failed = run_capped(&mut machine, &mut host, &mut record, None, None)
                    .err()
                    .ok_or("wait was not recorded")?;
                assert_eq!(failed.code, Code::NoServer);
                assert_eq!(record.0, [limit], "{subject} {option}");
            }
        }
        Ok(())
    }

    #[test]
    fn local_json_accepts_observations_from_different_runs()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::*;
        struct Reports(std::collections::VecDeque<Run>);
        impl Asks for Reports {
            fn ask(&mut self, _: &Asked, _: &mut dyn Write) -> Result<Reply, Failed> {
                let run = self
                    .0
                    .pop_front()
                    .ok_or_else(|| Failed::new(Code::Usage, "run"))?;
                let events = format!("0:{run}")
                    .parse()
                    .map_err(|()| Failed::new(Code::Usage, "cursor"))?;
                Ok(Reply {
                    run: Some(run),
                    ..Body::Snapshot(Box::new(crate::inspect::Snapshot {
                        run,
                        events,
                        scope: crate::inspect::Scope::All,
                        projection: crate::inspect::Projection::Facts,
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
        let mut machine = compile(&token::source(
            "reports",
            "ls let a ls let b list a b end json print",
        )?)
        .map_err(|e| e.to_string())?;
        let mut sink = Vec::new();
        let mut host = Host::new(Context::default(), None, &mut sink);
        run_capped(&mut machine, &mut host, &mut reports, None, None).map_err(|e| e.to_string())?;
        let text = String::from_utf8(sink)?;
        assert!(
            text.starts_with('[')
                && text.contains("\"run\":\"1-0\"")
                && text.contains("\"run\":\"2-0\"")
        );
        Ok(())
    }

    #[test]
    fn arrange_reconciles_its_pane_leaf_runs() -> Result<(), Box<dyn std::error::Error>> {
        use super::*;
        use crate::command::{Command, Request};
        struct Leaves {
            runs: std::collections::VecDeque<Run>,
            submitted: Vec<Option<Run>>,
        }
        impl Asks for Leaves {
            fn ask(&mut self, asked: &Asked, _: &mut dyn Write) -> Result<Reply, Failed> {
                let mut run = None;
                let body = match asked.request {
                    Request::Do(crate::command::Command::Resolve { .. }) => {
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
                    Request::Do(_) | Request::Wait(_) | Request::Events(..) => {
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
            let mut machine = compile(&token::source("layout", "@1 panes let a @1 panes let b @1 str @1:0:1-0 list a first b first end str horizontal even-of arrange")?).map_err(|e| e.to_string())?;
            let mut sink = Vec::new();
            let mut host = Host::new(Context::default(), None, &mut sink);
            let result = run_capped(&mut machine, &mut host, &mut leaves, None, None);
            if runs == [ours, ours] {
                result.map_err(|e| e.to_string())?;
                assert_eq!(leaves.submitted, [Some(ours)]);
            } else {
                assert!(result.is_err_and(|f| f.code == Code::BadValue));
                assert!(leaves.submitted.is_empty());
            }
        }
        Ok(())
    }

    #[test]
    fn layout_scalar_refusals_do_not_copy_retained_strings()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::*;
        struct NoRequests;
        impl Asks for NoRequests {
            fn ask(&mut self, _: &Asked, _: &mut dyn Write) -> Result<Reply, Failed> {
                Err(Failed::new(
                    Code::Usage,
                    "invalid scalars must not submit a request",
                ))
            }
        }
        let longest = "@4294967295:18446744073709551615:4294967295-ffffffffffffffff";
        assert!(longest.parse::<crate::command::LayoutRevision>().is_ok());
        let allocator = fux_vt_peak::ThreadPeak;
        let bad = Value::str(std::iter::repeat_n('x', fuxth::LONGEST / 2).collect::<String>());
        let mut peaks = Vec::new();
        for proposal in [
            "t bad list end arrange",
            "t str @1:0:1-0 list list bad 1 1 0 0 end end arrange",
        ] {
            let code = format!(
                "input let bad @1 let t 0 16 range each drop str bad_value try {proposal} catch 2drop end end"
            );
            let mut machine =
                compile(&token::source("scalar", &code)?).map_err(|e| e.to_string())?;
            let mut sink = Vec::new();
            let mut host = Host::new(Context::default(), None, &mut sink);
            assert!(matches!(
                machine.run(&mut host, u64::MAX)?,
                Outcome::Suspended(Ask::Local(Local::Shell(Shell::Input)))
            ));
            machine.import(bad.clone())?;
            assert!(matches!(
                machine.run(&mut host, u64::MAX)?,
                Outcome::Suspended(Ask::Server { .. })
            ));
            machine.import(Value::App(Fux::Tab(TabId::of(1), None)))?;
            let base = allocator.live();
            allocator.reset_peak();
            // Imports and host arguments charge their expanded work even when shared;
            // the property here is bounded allocation during scalar refusal.
            run_capped(
                &mut machine,
                &mut host,
                &mut NoRequests,
                Some(1_000_000),
                None,
            )
            .map_err(|e| e.to_string())?;
            peaks.push(allocator.peak().saturating_sub(base));
        }
        assert!(
            peaks.iter().all(|p| *p < 64 << 10),
            "scalar refusal peaks: {peaks:?}"
        );
        Ok(())
    }

    /// Certain target/version errors fail preflight, before even an earlier
    /// print can run. Pane versions and the bundled recipes remain accepted.
    #[test]
    fn target_and_version_boundaries_are_checked_before_effects()
    -> Result<(), Box<dyn std::error::Error>> {
        let version = "%2:0.0.0.0.0:48211-1867a3f9c2b8e7d6";
        for code in [
            "str c1 client capture expect".to_owned(),
            "str c1 client capture changed".to_owned(),
            "str c1 client capture version".to_owned(),
            "@1 version".to_owned(),
            "%1 str horizontal axis 30 str horizontal resize-pane".to_owned(),
            "%1 str horizontal axis str click 0 0 send-mouse".to_owned(),
            "str c1 client 0 timeout wait".to_owned(),
            format!("str {version} expect 0 timeout wait"),
            format!("str {version} changed str x text send-keys"),
            format!("str {version} changed all-history capture"),
            format!("str {version} changed events"),
            "str x kill".to_owned(),
        ] {
            let tokens =
                super::token::source("boundary", &format!("str SIDE-EFFECT print {code}"))?;
            let error = super::compile(&tokens)
                .err()
                .ok_or_else(|| format!("accepted {code}"))?;
            assert_eq!(
                error.code,
                super::Code::from(crate::code::Raise::Type),
                "{code}: {error}"
            );
        }
        // Values obtained from a closed-only producer are statically disjoint
        // from versions. A catch remains string | payload, since ordinary
        // message failures are still permitted.
        for kind in ["closed-answer", "client-capture"] {
            for suffix in ["changed drop", "expect drop", "version drop"] {
                let mut vocabulary = super::vocabulary()?;
                vocabulary.add("held", &format!("-- {kind}"))?;
                let mut compiler = fuxth::Compiler::<super::Fux>::new(vocabulary);
                let source = format!("held str SIDE-EFFECT print {suffix}");
                let error = compiler
                    .add(&super::token::source("versionless", &source)?)
                    .and_then(|()| compiler.finish())
                    .err()
                    .ok_or_else(|| format!("accepted {source}"))?;
                assert_eq!(error.code, fuxth::Code::Type, "{source}: {error}");
            }
        }
        for code in [
            "%1 all-history capture expect str x text send-keys wait capture drop".to_owned(),
            "%1 str x text send-keys 0 quiet %1 capture drop".to_owned(),
            format!("str {version} changed 0 timeout wait capture drop"),
            "str c1 client capture json drop".to_owned(),
        ] {
            super::compile(&super::token::source("recipe", &code)?).map_err(|f| f.to_string())?;
        }
        Ok(())
    }

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
    fn only_the_command_line_asks_its_process() -> Result<(), super::Failed> {
        let words = ["args", "input", "str x read-file", "str x warn", "0 exit"];
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
        use crate::command::{AttachTarget, WsRef};
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

    /// The default keys are a config every line of which binds; what
    /// list-keys prints reconstructs the same keys from any starting keys.
    #[test]
    fn the_default_keys_are_a_config_list_keys_prints() -> Result<(), super::Failed> {
        let run = |name: &str, text: &str, state: &mut crate::client_state::Draft| {
            super::run_quiet(&super::token::source(name, text)?, Some(state), false)
        };
        let mut keymap = super::default_state()?.draft();
        let (screens, keys): (Vec<&str>, Vec<&str>) = (super::KEYS.lines())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .partition(|l| l.contains(" bind-in"));
        assert_eq!(
            keymap.keys.bindings.all().len(),
            keys.len().saturating_sub(1)
        );
        let listed = keymap.keys.describe();
        assert_eq!(listed.matches(" bind-in").count(), screens.len());
        // Quotes, a backslash and a line end, as a config writes them.
        let mine = r#"str M-t str 'me zoom' bind-root str Mine group
            str g str 'me focused str horizontal axis str sh program str "echo it'\''s" arg split drop' bind
            str "a \\ and
            a line" label"#;
        run("mine", mine, &mut keymap)?;
        let mut again = super::default_state()?.draft();
        run(
            "other",
            "str C-a prefix str M-n str 'me str next step-tab' bind-root",
            &mut again,
        )?;
        run("listed", &keymap.keys.describe(), &mut again)?;
        assert_eq!(again.keys, keymap.keys);
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
            &mut super::ConfigOnly::default(),
            Some(20_000),
            None,
        );
        assert_eq!(ran.err().map(|f| f.code), Some(super::Code::Steps));
        assert_eq!(machine.steps(), 20_001);
        Ok(())
    }

    /// Options or a request nothing acts on name the words that would, as
    /// the kind's own description does.
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
            ("opts str x in", "run, new-tab, attach or attach-defaults"),
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
