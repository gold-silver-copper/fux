//! fux's command line, config, bindings and prompt: grift programs, run in
//! a client. Words build typed requests (`(new-tab (options))`, `(send-keys
//! (text (pane 2) "x"))`) and acting words submit them: a host function
//! blocks while the server does what one asks, and answers grift data (a
//! pane's handle, a capture's record), never text to be read back.
//!
//! Every run has an interpreter nothing else ran in. A command line builds
//! one; a client's worker builds the next after each job, off the key's path
//! (`client::Keyboard`), so nothing a run left behind reaches the next.
mod callback;
mod docs;
pub use docs::{Doc, docs};

#[cfg(test)]
mod lists;
mod record;
pub(crate) mod settings;
mod source;
#[cfg(test)]
mod tests;
mod words;

use crate::client;
use crate::client_state::{ClientState, Draft};
use fux_server::socket::SocketPath;
use fux_wire::answer::{PaneAnswer, PaneResults};
use fux_wire::code::Code;
use fux_wire::command::{
    AnyRef, Extent, HistoryBytes, Input, Limit, Program, Request, Run, Version, WsRef,
};
use fux_wire::id::{ClientId, PaneId, TabId, WsId};
use fux_wire::layout::Axis;
use fux_wire::protocol::{Asked, Body, Context, End, Reply, Response};
mod bridge;
mod value;
pub use bridge::{
    Interpreter, Machine, Runtime, callback_machine, compile, evaluate_job, interpreter,
    load_libraries, replay_config,
};
use std::io::{self, Read, Write};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
pub use value::{Done, Failure, List, Value, quoted};

pub use callback::admit;
pub use words::{Act, After, Group, Keys, Opt, Row, Screen, Takes, Word, from_opts, usage};

/// The sources a client's config was read from, whose definitions its
/// bindings and prompt see.
pub type Config = Arc<source::Frozen>;

/// The bundled operations, written in grift.
const LIBRARY: &str = include_str!("library.grift");

/// The first line of a grift script, which runs it as `fux --file`.
pub const SHEBANG: &str = "#!/usr/bin/env -S fux --file";

/// fux's default keys: a config, run before the user's.
const KEYS: &str = include_str!("keys.grift");
pub(crate) const DEFAULTS: &str = include_str!("client-defaults.grift");
/// What differs on Windows, run after them: the default shell.
const WINDOWS_DEFAULTS: &str = include_str!("windows.grift");

/// A value of fux's own.
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
    /// A code value another client sent, as its form: `execute!` evaluates
    /// it in the caller's context, then calls it.
    Code(Rc<Sent>),
}

pub type Sent = fux_wire::callback::Code;

impl Fux {
    fn kind(&self) -> &'static str {
        if let Fux::Record(record) = self
            && let Some(said) = record.held_kind().map(|(_, said)| said)
        {
            return said;
        }
        let kind = words::KINDS.iter().find(|(name, ..)| *name == self.name());
        kind.map_or("a value of fux's", |(_, said, _)| said)
    }

    fn name(&self) -> &'static str {
        match self {
            Fux::Null => "none",
            Fux::Record(record) => record.held_kind().map_or("record", |(name, _)| name),
            Fux::Pane(_, _) => "pane",
            Fux::Tab(_, _) => "tab",
            Fux::Workspace(_, _) => "workspace",
            Fux::Client(_, _) => "client",
            Fux::All => "all",
            Fux::Opts(opts) => match opts.on {
                None => "options",
                Some(On::Target(AnyRef::Pane(_)) | On::Version(_)) => "pane-request",
                Some(On::Target(AnyRef::Tab(_))) => "tab-request",
                Some(On::Target(AnyRef::Workspace(_))) => "workspace-request",
                Some(On::Client(_)) => "client-request",
                Some(On::Changed(_)) => "changed-request",
                Some(On::Seen(_)) => "expected-request",
            },
            Fux::Extent(_) => "size",
            Fux::Code(_) => "code",
        }
    }

    fn write_text(&self, out: &mut dyn std::fmt::Write) -> Result<(), Failure> {
        match self {
            Fux::Null => out.write_str("none"),
            Fux::Record(r) if r.is_held() => r.write_held(out),
            Fux::Record(r) => r.write(out),
            Fux::Pane(p, _) => write!(out, "{p}"),
            Fux::Tab(t, _) => write!(out, "{t}"),
            Fux::Workspace(w, _) => write!(out, "{w}"),
            Fux::Client(c, _) => write!(out, "{c}"),
            Fux::Extent(Extent::Cells(n)) => write!(out, "{n}"),
            Fux::Extent(Extent::Percent(n)) => write!(out, "{}%", n.get()),
            Fux::All => write!(out, "all"),
            Fux::Opts(_) => write!(out, "({})", self.kind()),
            Fux::Code(code) => out.write_str(&code.text()),
        }
        .map_err(|_| Failure::new(Code::Output, "the text could not be written"))
    }
}

/// The run `value` is of, checked: a server's answer names its pane and
/// version too, which must agree, as a program may have typed it.
fn checked_scope(value: &Value<Fux>) -> Result<Option<Run>, Failure> {
    let mut run = scope(value);
    if let Value::App(Fux::Record(record)) = value
        && record.is_held()
    {
        if let Ok(pane) = record.get("pane") {
            run = same_run(run, scope(&pane))?;
        }
        run = same_run(run, record.version().map(|version| version.run))?;
    }
    Ok(run)
}

/// The server run a value was returned by, retained through request options.
fn scope(value: &Value<Fux>) -> Option<Run> {
    match value {
        Value::App(
            Fux::Pane(_, run) | Fux::Tab(_, run) | Fux::Workspace(_, run) | Fux::Client(_, run),
        ) => *run,
        Value::App(Fux::Record(record)) => record.of,
        Value::App(Fux::Opts(opts)) => opts.of,
        Value::Float(_)
        | Value::Symbol(_)
        | Value::Bytes(_)
        | Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_) => None,
    }
}

fn same_run(a: Option<Run>, b: Option<Run>) -> Result<Option<Run>, Failure> {
    match (a, b) {
        (Some(a), Some(b)) if a != b => Err(Failure::new(
            Code::BadValue,
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
    /// What it goes on, as it was given, and what it was built with: its
    /// grift data, which `words::replayed` makes it again from.
    pub subject: Option<Value<Fux>>,
    pub steps: Vec<(words::Opt, Vec<Value<Fux>>)>,
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
                fux_wire::answer::Of::Client { .. } => ("client-capture", "a client's capture"),
            },
        }
    }

    fn in_run(self, run: Option<Run>) -> Result<Value<Fux>, Failure> {
        record::datum(&self, run)
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
}

/// The keys of a binding just made.
#[derive(Clone, Debug, PartialEq)]
pub enum Bound {
    /// Keys from the root.
    Keys(Vec<fux_vt::keys::KeyPress>),
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
        /// A typing terminal's config, evaluated with the bundled state.
        configured: Option<Box<Result<ClientState, Failed>>>,
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
    file: Option<std::path::PathBuf>,
    actions: Vec<String>,
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
            file: None,
            actions: Vec::new(),
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

    /// Ready for a client's next job, as `Host::job` makes a host: its
    /// client state `draft`, as `context`.
    pub(crate) fn next_job(&mut self, draft: Draft, context: Context) {
        if let Some(state) = self.state.as_deref_mut() {
            *state = draft;
        }
        self.context = context;
        self.last_bound = None;
        self.held = false;
        self.files = source::Files::sealed();
        self.file = None;
        self.actions.clear();
    }

    /// `call` took effect: told if a later step fails, as many as a notice
    /// can say.
    fn took(&mut self, call: String) {
        const MOST: usize = 64;
        match self.actions.len() {
            count if count < MOST => self.actions.push(call),
            MOST => self.actions.push("and more, not listed".to_owned()),
            _ => {}
        }
    }

    /// The candidate state, for a client's worker.
    pub(crate) fn draft(&mut self) -> Option<&mut Draft> {
        self.state.as_deref_mut()
    }

    /// Ordinary CLI commands need no keys or preference initialization.
    /// The bundled state, and with `typing` the config on it, in one
    /// interpreter, as attaching waits on them: the config's outcome.
    fn prepare(&mut self, typing: bool) -> Result<Option<Result<ClientState, Failed>>, Failed> {
        if self.cli
            && let Some(state) = self.state.as_deref_mut()
            && !state.initialized()
        {
            let (defaults, configured) = bundled(typing)?;
            *state = defaults.draft();
            return Ok(configured);
        }
        Ok(None)
    }
}

/// A run's failure: its code, cause, source when known, and the steps
/// that had taken effect before it (D-25).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed {
    pub code: Code,
    pub kind: Option<String>,
    pub message: String,
    pub place: Option<String>,
    pub actions: Vec<String>,
}

impl Failed {
    /// A failure of kind `code`, and why.
    pub fn new(code: impl Into<Code>, message: impl Into<String>) -> Failed {
        Failed {
            code: code.into(),
            kind: None,
            message: message.into(),
            place: None,
            actions: Vec::new(),
        }
    }

    /// The cause before its source, for a notice with limited room, then
    /// the steps that had taken effect.
    pub fn notice(&self) -> String {
        let message = match &self.place {
            Some(place) => format!("{} ({place})", self.message),
            None => self.message.clone(),
        };
        let mut shown = self.clone();
        shown.message = message;
        shown.place = None;
        if !self.actions.is_empty() {
            shown.message = format!(
                "already took effect: {}; {}",
                self.actions.join(", "),
                shown.message
            );
        }
        shown.to_string()
    }

    /// Each step that had taken effect, a line each, as stderr says it.
    fn taken(&self) -> impl Iterator<Item = String> + '_ {
        self.actions
            .iter()
            .map(|step| format!("already took effect: {step}"))
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
        write!(
            f,
            "{}: {}",
            self.kind.as_deref().unwrap_or(self.code.name()),
            lines.join("\\n")
        )
    }
}

impl From<fux_wire::code::Error> for Failed {
    fn from(error: fux_wire::code::Error) -> Failed {
        Failed::new(error.code(), error.to_string())
    }
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
    stream: Arc<Mutex<Option<fux_server::socket::Client>>>,
    interrupt: Arc<Mutex<Option<grift::Interrupter>>>,
}

impl Stop {
    /// Stops the run under way.
    pub fn stop(&self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Ok(mut held) = self.interrupt.lock()
            && let Some(stop) = held.as_mut()
        {
            let _ = stop.stop();
        }
        if let Some(stream) = self.stream.lock().ok().and_then(|mut s| s.take()) {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    /// `stream` is the connection of the request under way (`None`: no
    /// request is): what a stop shuts down.
    fn hold(&self, stream: Option<fux_server::socket::Client>) {
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

    /// Pending preferences and keys must arrive before a later server
    /// operation, a relayed read of them among them.
    fn checkpoint(&mut self, draft: &mut Draft, context: Context) -> Result<(), Failed> {
        if draft.changed() {
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
                let list = List::new(args);
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
    /// What stops reading a screen's code values as it opens.
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

/// Whether a request changes something, so that a failure after it lists
/// it: all but those that only read.
fn acts(request: &Request) -> bool {
    use fux_wire::command::{ClientAction, Command};
    !matches!(
        request,
        Request::Wait(_)
            | Request::NextEvents { .. }
            | Request::Read { .. }
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

/// What a reply's answer gives: nothing, a string, handles, or
/// an answer of fux's; a field's value as grift's own where it is one.
pub fn answered(reply: Reply) -> Result<Vec<Value<Fux>>, Failed> {
    let run = reply.run;
    let body = match reply.response {
        Response::Answered(PaneResults::One(answer)) => {
            return Ok(vec![
                Answer::Pane(answer).in_run(run).map_err(callback_failed)?,
            ]);
        }
        Response::Answered(PaneResults::Many(answers)) => {
            return Ok(vec![
                Answer::Panes(answers)
                    .in_run(run)
                    .map_err(callback_failed)?,
            ]);
        }
        Response::Returned(body) => body,
        Response::Failed { .. } => return Ok(Vec::new()),
    };
    let handles = |ids: Vec<AnyRef>| {
        let ids = ids
            .into_iter()
            .map(|id| Value::App(handle(id, run)))
            .collect();
        let list = List::new(ids);
        Ok::<_, Failed>(Value::List(Rc::new(list)))
    };
    Ok(vec![match body {
        Body::Empty => return Ok(Vec::new()),
        Body::Found(id) => Value::App(handle(id, run)),
        Body::Handles(ids) => handles(ids)?,
        Body::Capture(captured) => Answer::Capture(captured)
            .in_run(run)
            .map_err(callback_failed)?,
        Body::Snapshot(snapshot) => {
            record::snapshot(*snapshot).map_err(|e| Failed::new(Code::of(e.code), e.message))?
        }
        Body::Events(batch) => {
            record::events(*batch).map_err(|e| Failed::new(Code::of(e.code), e.message))?
        }
        Body::Field(json) => {
            record::pane_fact(&json, run).map_err(|e| Failed::new(Code::of(e.code), e.message))?
        }
        // Its code values kept as their words, which `execute` compiles.
        Body::Read(data) => callback::value(&data)?,
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
            configured: evaluated,
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
            let (committed, problem) = match evaluated {
                Some(evaluated) if typing => told((**evaluated).clone(), &defaults),
                Some(_) | None if typing => configured(&defaults),
                Some(_) | None => (defaults, None),
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
            let (socket, notices) = match *read_only || of.is_some() {
                true => (server.socket()?, Vec::new()),
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
                notices,
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
/// its journal keeping `journal` events, and what one started said.
fn started(
    server: &mut Server,
    journal: fux_wire::command::JournalEntries,
) -> Result<(SocketPath, Vec<String>), Failed> {
    let socket = server.socket()?;
    let notices = match client::answers(&socket) {
        true => Vec::new(),
        false => client::start_server(&socket, journal).map_err(client_failed)?,
    };
    Ok((socket, notices))
}

/// A typing terminal's whole state and one diagnostic if it failed.
pub fn configured(defaults: &ClientState) -> (ClientState, Option<String>) {
    told(config_state(defaults), defaults)
}

/// A config's outcome: its state; or, said on stderr, the bundled state and
/// the notice a client shows.
fn told(
    outcome: Result<ClientState, Failed>,
    defaults: &ClientState,
) -> (ClientState, Option<String>) {
    match outcome {
        Ok(state) => (state, None),
        Err(failed) => {
            let _ = writeln!(io::stderr(), "fux: {failed}");
            (defaults.clone(), Some(failed.notice()))
        }
    }
}

/// Complete bundled policy and keys, checked before a client becomes live.
pub fn default_state() -> Result<ClientState, Failed> {
    bundled(false).map(|(defaults, _)| defaults)
}

/// The bundled state, and with `config` the outcome of the user's config on
/// it, each evaluated in one interpreter.
fn bundled(config: bool) -> Result<(ClientState, Option<Result<ClientState, Failed>>), Failed> {
    let mut draft = Draft::default();
    let windows = cfg!(windows).then_some(("windows.grift", WINDOWS_DEFAULTS));
    let sources = [("client-defaults.grift", DEFAULTS), ("keys.grift", KEYS)];
    let mut sink = io::sink();
    let mut host = Host::new(Context::default(), Some(&mut draft), &mut sink);
    let mut asks = ConfigOnly { diagnostics: true };
    let mut state = Runtime::new(&mut host, &mut asks, None);
    let mut g = interpreter(&mut state)?;
    for (name, text) in sources.into_iter().chain(windows) {
        evaluate_job(&mut g, &mut state, &Machine::source(name, text))?;
    }
    let finished = |state: &mut Runtime<'_, '_, ConfigOnly>| {
        let draft = state.host.draft().map(std::mem::take).unwrap_or_default();
        draft.finish().map_err(|why| Failed::new(Code::Config, why))
    };
    let defaults = finished(&mut state)?;
    if !config {
        return Ok((defaults, None));
    }
    state.asks.diagnostics = false;
    let outcome = match config_source() {
        Ok(None) => Ok(defaults.clone()),
        Err(failed) => Err(failed),
        Ok(Some(config)) => {
            let mut machine = Machine::source(&config.name, &config.text);
            machine.path = config.path.clone();
            if let Some(draft) = state.host.draft() {
                *draft = defaults.draft();
            }
            evaluate_job(&mut g, &mut state, &machine).and_then(|_| {
                let files = std::mem::take(&mut state.host.files);
                if let Some(draft) = state.host.draft() {
                    draft.config = Some(Arc::new(config.with(files)));
                }
                finished(&mut state)
            })
        }
    };
    Ok((defaults, Some(outcome)))
}

/// The config file, a grift program a typing terminal runs as it attaches:
/// `$XDG_CONFIG_HOME/fux/config.grift`, else `~/.config/fux/config.grift`
/// (Windows: `%APPDATA%\\fux\\config.grift`).
pub fn default_path() -> Option<std::path::PathBuf> {
    Some(fuxix::file::config_dir()?.join("fux").join("config.grift"))
}

fn config_state(defaults: &ClientState) -> Result<ClientState, Failed> {
    match config_source()? {
        Some(config) => run_config_on(config, defaults),
        None => Ok(defaults.clone()),
    }
}

/// The user's config, as it reads now: none if there is no such file.
fn config_source() -> Result<Option<source::Frozen>, Failed> {
    let Some(path) = default_path() else {
        return Ok(None);
    };
    match source::Frozen::file(&path) {
        Ok(config) => Ok(Some(config)),
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                && path
                    .symlink_metadata()
                    .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(read_failed(&path.display().to_string(), &error.to_string())),
    }
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
    let mut machine = Machine::source(&config.name, &config.text);
    machine.path = config.path.clone();
    let mut sink = io::sink();
    let mut host = Host::new(Context::default(), Some(&mut draft), &mut sink);
    host.file = config.path.clone();
    run(&mut machine, &mut host, &mut ConfigOnly::default(), None)?;
    draft.config = Some(Arc::new(config.with(host.files)));
    draft.finish().map_err(|why| Failed::new(Code::Config, why))
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
        return Ok(vec![Value::List(Rc::new(List::new(Vec::new())))]);
    }
    let why = match what {
        Local::Open(_) => "a client's screen opens from its own keys or prompt",
        Local::Attach { .. } | Local::Start | Local::Status | Local::Shell(_) => {
            "runs from the command line only"
        }
    };
    Err(Failed::new(Code::NotHere, why))
}

fn callback_failed(f: Failure) -> Failed {
    Failed::new(f.code, f.message)
}
fn output_failed(e: io::Error) -> Failed {
    Failed::new(Code::Output, format!("writing the output: {e}"))
}
fn read_failed(name: &str, why: &str) -> Failed {
    Failed::new(Code::Read, format!("{name}: {why}"))
}
fn read_text(name: &str, mut from: impl Read) -> Result<String, Failed> {
    let mut bytes = Vec::new();
    from.read_to_end(&mut bytes)
        .map_err(|e| read_failed(name, &e.to_string()))?;
    String::from_utf8(bytes).map_err(|_| read_failed(name, "not UTF-8 text"))
}
fn read_file(path: &std::path::Path, acted: &Acted) -> Result<String, Failed> {
    let file = std::fs::File::open(path)
        .map_err(|e| read_failed(&path.display().to_string(), &e.to_string()))?;
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        acted.mark();
    }
    read_text(&path.display().to_string(), file)
}
/// Evaluate a program against a host. Grift runs on the existing client worker.
pub fn run(
    machine: &mut Machine,
    host: &mut Host<'_>,
    asks: &mut dyn Asks,
    stop: Option<&Stop>,
) -> Result<Vec<Value<Fux>>, Failed> {
    let values = bridge::run(machine, host, asks, stop)?;
    refuse_unfinished(&values, host)?;
    Ok(values)
}

/// A request left unasked at a program's end did nothing, which it says.
pub fn refuse_unfinished(values: &[Value<Fux>], host: &Host<'_>) -> Result<(), Failed> {
    for value in values {
        let items = match value {
            Value::List(items) => items.items(),
            value @ Value::Int(_)
            | value @ Value::Float(_)
            | value @ Value::Bool(_)
            | value @ Value::Str(_)
            | value @ Value::Symbol(_)
            | value @ Value::App(_)
            | value @ Value::Bytes(_) => std::slice::from_ref(value),
        };
        for item in items {
            if let Value::App(Fux::Opts(options)) = item {
                let end = if options.inputs.is_empty() {
                    from_opts("or")
                } else {
                    "send-keys".to_owned()
                };
                let mut failure =
                    Failed::new(Code::Unused, format!("{}: end with {end}", item.kind()));
                failure.actions = host.actions.clone();
                return Err(failure);
            }
        }
    }
    Ok(())
}
/// One quoted grift program; every following shell argument stays a string.
pub fn main(
    args: &[String],
    argv: Option<Vec<std::ffi::OsString>>,
    out: &mut dyn Write,
) -> Result<u8, Failed> {
    let result = main_program(args, argv, out);
    if let Err(failure) = &result {
        let _ = writeln!(io::stderr(), "fux: {failure}");
        for step in failure.taken() {
            let _ = writeln!(io::stderr(), "fux: {step}");
        }
    }
    result
}
fn main_program(
    args: &[String],
    argv: Option<Vec<std::ffi::OsString>>,
    out: &mut dyn Write,
) -> Result<u8, Failed> {
    if matches!(args,[v] if v=="--version"||v=="-V") {
        writeln!(out, "{}", fux_wire::protocol::Build::OURS).map_err(output_failed)?;
        return Ok(0);
    }
    let (mut machine, from, program) = match args {
        [] => (
            Machine::source("command line", "(attach (options))"),
            Given::Words,
            None,
        ),
        [p] if p == "-" => {
            let text = read_text("stdin", io::stdin())?;
            (Machine::source("stdin", &text), Given::Stdin, Some(text))
        }
        [flag] if flag == "--file" => {
            return Err(Failed::new(Code::Usage, "--file needs a FILE"));
        }
        [flag, file, rest @ ..] if flag == "--file" => {
            let path =
                source::identity(file.as_ref()).map_err(|e| read_failed(file, &e.to_string()))?;
            let text = source::read(&path).map_err(|e| read_failed(file, &e.to_string()))?;
            let mut m = Machine::source(&path.display().to_string(), &text);
            m.path = Some(path);
            (m, Given::File(rest.to_vec()), None)
        }
        [flag] if flag == "--help" || flag == "-h" => (
            Machine::source("command line", "(help)"),
            Given::Words,
            None,
        ),
        [program, rest @ ..] => (
            Machine::source("command line", program),
            Given::File(rest.to_vec()),
            None,
        ),
    };
    let again = argv.clone().map(|argv| client::Rerun {
        argv,
        program: program.clone(),
    });
    let acted = Acted::default();
    if machine
        .path
        .as_ref()
        .is_some_and(|path| !path.metadata().is_ok_and(|metadata| metadata.is_file()))
    {
        acted.mark();
    }
    let closed = Rc::new(std::cell::Cell::new(false));
    let mut watched = Watched {
        out,
        acted: acted.clone(),
        closed: closed.clone(),
    };
    let mut cli = Cli {
        server: Server {
            follow: argv.map_or(Follow::Never, |argv| Follow::Command {
                rerun: client::Rerun { argv, program },
                acted: acted.clone(),
            }),
            out: Some(closed.clone()),
            ..Server::default()
        },
        from,
        status: None,
        acted: acted.clone(),
    };
    let mut state = Draft::default();
    let mut host = Host::cli(client::context(), &mut state, &mut watched);
    host.files.acted = Some(acted);
    let result = run(&mut machine, &mut host, &mut cli, None)
        .map_err(|failed| {
            if failed.code == Code::UnknownWord && !cli.acted.marked() {
                uncompiled(failed, again.as_ref())
            } else {
                failed
            }
        })
        .and_then(|values| {
            for v in &values {
                if matches!(v, Value::App(Fux::Opts(_))) {
                    return Err(Failed::new(Code::Unused, "options need an operation"));
                }
                match v {
                    Value::List(items) => {
                        for item in items.items() {
                            writeln!(host.out, "{}", value::Text(item)).map_err(output_failed)?;
                        }
                    }
                    Value::Str(text) if text.is_empty() => {}
                    Value::App(Fux::Record(record)) if record.is_batch() => {
                        for event in record.printed_events().map_err(callback_failed)? {
                            writeln!(host.out, "{}", value::Text(&event)).map_err(output_failed)?;
                        }
                    }
                    Value::App(Fux::Record(record)) if record.says_nothing() => {}
                    Value::Int(_)
                    | Value::Float(_)
                    | Value::Bool(_)
                    | Value::Str(_)
                    | Value::Symbol(_)
                    | Value::App(_)
                    | Value::Bytes(_) => {
                        writeln!(host.out, "{}", value::Text(v)).map_err(output_failed)?
                    }
                }
            }
            Ok(cli.status.unwrap_or(0))
        })
        .map_err(|mut failure| {
            if failure.actions.is_empty() {
                failure.actions = host.actions.clone();
            }
            failure
        });
    if closed.get() {
        return Ok(141);
    }
    result
}
/// Public record names follow grift's spelling; native wire names stay private.
/// The grift field name corresponding to a native observation field.
pub fn field_name(name: &str) -> String {
    if name == "alt_screen" {
        return "alternate-screen".into();
    }
    name.split('_')
        .map(|word| match word {
            "cols" => "columns",
            "col" => "column",
            "len" => "length",
            "pid" => "process-id",
            "cwd" => "directory",
            "argv" => "arguments",
            "tty" => "terminal",
            "seq" => "sequence",
            "app" => "application",
            word => word,
        })
        .collect::<Vec<_>>()
        .join("-")
}
fn native_field(name: &str) -> String {
    if name == "alternate-screen" {
        return "alt_screen".into();
    }
    name.replace("process-id", "pid")
        .split('-')
        .map(|word| match word {
            "columns" => "cols",
            "column" => "col",
            "length" => "len",
            "directory" => "cwd",
            "arguments" => "argv",
            "terminal" => "tty",
            "sequence" => "seq",
            "application" => "app",
            word => word,
        })
        .collect::<Vec<_>>()
        .join("_")
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
    ) -> Result<Vec<super::Value<super::Fux>>, super::Failed> {
        let mut machine = super::Machine::source("test", code);
        let mut host = super::Host::new(fux_wire::protocol::Context::default(), state, out);
        super::run(&mut machine, &mut host, asks, None)
    }

    pub fn held_reply(
        code: &str,
        reply: super::Reply,
        out: &mut Vec<u8>,
    ) -> Result<Vec<super::Value<super::Fux>>, super::Failed> {
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

    /// A batch's events as `next-events` prints them, a line each, but
    /// for the fields named in `without`.
    pub fn event_lines(
        batch: fux_wire::events::Batch,
        without: &[&str],
    ) -> Result<String, super::Failure> {
        use super::Value;
        let not_a = |what| super::Failure::new(fux_wire::code::Code::Fail, what);
        let Value::App(super::Fux::Record(record)) = super::record::events(batch)? else {
            return Err(not_a("a batch's record"));
        };
        let Value::List(events) = record.get("events")? else {
            return Err(not_a("a batch's events"));
        };
        let mut lines = String::new();
        for event in events.items() {
            let Value::App(super::Fux::Record(event)) = event else {
                return Err(not_a("an event's record"));
            };
            let mut event = (**event).clone();
            event
                .fields
                .retain(|(key, _)| !without.iter().any(|name| super::field_name(name) == *key));
            bare(
                &Value::App(super::Fux::Record(super::Rc::new(event))),
                &mut lines,
            )
            .map_err(|_| not_a("an event's text"))?;
            lines.push('\n');
        }
        Ok(lines)
    }

    /// `value` as data, its handles as a person reads them: a test's lines
    /// don't hold the run of the session that made them.
    fn bare(value: &super::Value<super::Fux>, out: &mut String) -> std::fmt::Result {
        use super::{Fux, Value};
        use std::fmt::Write;
        match value {
            Value::App(Fux::Record(record)) => {
                out.write_char('(')?;
                for (i, (key, value)) in record.fields.iter().enumerate() {
                    if i > 0 {
                        out.write_char(' ')?;
                    }
                    write!(out, "({key} ")?;
                    bare(value, out)?;
                    out.write_char(')')?;
                }
                out.write_char(')')
            }
            Value::List(items) => {
                out.write_char('(')?;
                for (i, item) in items.items().iter().enumerate() {
                    if i > 0 {
                        out.write_char(' ')?;
                    }
                    bare(item, out)?;
                }
                out.write_char(')')
            }
            Value::App(
                app @ (Fux::Pane(..) | Fux::Tab(..) | Fux::Workspace(..) | Fux::Client(..)),
            ) => app.write_text(out).map_err(|_| std::fmt::Error),
            other @ Value::Int(_)
            | other @ Value::Float(_)
            | other @ Value::Bool(_)
            | other @ Value::Str(_)
            | other @ Value::Symbol(_)
            | other @ Value::App(_)
            | other @ Value::Bytes(_) => other.write_data(out),
        }
    }
}

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
