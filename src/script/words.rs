//! fux's words: each a name, what it does to the stack, in the names of
//! the kinds it takes and gives (`KINDS`, `NAMES`), and a line of help;
//! and what each does, run by the program's [`Host`].
use super::{Ask, Bound, Fux, Host, Local, On, Opts, Said, Then};
use crate::code::Code;
use crate::command::{
    AnyRef, Button, ClientAction, Command, Extent, Input, Kind, Mouse, MoveTo, PanePick,
    PaneResize, Percent, Pick, Request, Resized, Sibling, Since, Source, SwapWith, Usage, Wait,
    Wheel, WsRef,
};
use crate::help;
use crate::id::{ClientId, PaneId, Wanted};
use crate::keys::Direction;
use crate::layout::Axis;
use crate::screens::Open;
use fuxth::{Code as Lang, Done, Failure, Value};
use std::ops::RangeInclusive;
use std::time::Duration;

/// A word of fux's vocabulary.
pub struct Word {
    pub name: &'static str,
    /// What it does to the stack, in the names of `KINDS` and `NAMES`:
    /// `x secs -- x`. fuxth reads it, for how many values the word takes
    /// and gives, and their kinds.
    pub effect: &'static str,
    pub about: &'static str,
    /// Its heading in `help`.
    pub group: &'static str,
}

const fn word(
    group: &'static str,
    name: &'static str,
    effect: &'static str,
    about: &'static str,
) -> Word {
    Word {
        name,
        effect,
        about,
        group,
    }
}

/// fux's kinds of value, each as its values' `name` says it, as an error
/// says it, and, for one that must be used, what one left over is told.
#[rustfmt::skip]
pub const KINDS: [(&str, &str, Option<&str>); 11] = [
    ("pane", "a pane", None), ("tab", "a tab", None), ("workspace", "a workspace", None),
    ("client", "a client", None), ("size", "a size (from percent)", None),
    ("answer", "an action's answer", None), ("capture", "a capture", None),
    ("listing", "a listing", None), ("json", "a JSON value", None),
    ("opts", "options (from opts, for run, new-tab and new-workspace; a pane's words take theirs on the pane: `%2 all-history capture`)", Some("nothing takes these options")),
    ("request", "a request (a pane given options, keys or a wait)", Some("nothing acts on this request")),
];

/// The names the words' effects use beyond the kinds' own, each with the
/// kinds it stands for (`from>to`: what a kind is once the word passes it
/// through), and how an error says them; then the names of a string, and
/// of an integer.
#[rustfmt::skip]
pub const NAMES: &[(&str, &str, &str)] = &[
    ("x", "opts request pane>request tab>request workspace>request client>request answer>request",
        "the pane it goes on, under its own values (or a tab, workspace, client, a request on one, or opts)"),
    ("target", "pane tab workspace client answer request", "a pane, tab, workspace or client, or a request on one"),
    ("subject", "pane answer request", "a pane, or a request on one"),
    ("place", "pane tab workspace answer request", "a pane, tab or workspace, or a request on one"),
    ("pane", "pane answer", "a pane (from pane, run or split)"),
    ("handle", "pane tab workspace answer", "a pane, tab or workspace"),
    ("ref", "string pane tab workspace answer", "a name, or a pane, tab or workspace"),
    ("text", "string integer boolean", "a string, a number or a boolean"),
    ("var", "string integer boolean pane tab workspace answer", "a string, a number, a boolean, or a pane, tab or workspace"),
    ("value", "integer boolean string list pane tab workspace client size answer capture listing json",
        "a value (options and requests are none: what acts on them gives one)"),
    ("json", "json string", "JSON"),
    ("record", "listing", "a listing"),
    ("width", "integer size", "cells, or a percent"),
    ("height", "integer size", "cells, or a percent"),
    ("where", "tab workspace string", "a tab, a workspace, new-tab or new-workspace"),
    ("workspace", "workspace string", "a workspace, or its name"),
];
pub const STRINGS: &str =
    "name dir key path line id file field setting socket way action code keys word hex version";
pub const INTEGERS: &str = "cols rows secs n row col lines from to cells";

/// Every word of fux's, under its heading, in help's order.
#[rustfmt::skip]
pub const WORDS: &[Word] = &[
    // Finding
    word("Finding", "pane", "ref -- pane", "the pane ref names: a tab's name (its one pane), %N, a pane, or a tab of one pane"),
    word("Finding", "tab", "ref -- tab", "the tab ref names: its name, @N, or a pane's tab"),
    word("Finding", "workspace", "ref -- workspace", "the workspace ref names: its name, +N, or a pane's or tab's workspace"),
    word("Finding", "here", "-- pane", "the pane this runs in (FUX_PANE)"),
    word("Finding", "client", "name -- client", "a client by its id, c2"),
    word("Finding", "me", "-- client", "the client whose key or prompt runs this"),
    word("Finding", "focused", "client -- pane", "the pane a client focuses"),
    // Starting
    word("Starting", "opts", "-- opts", "empty options, for run, new-tab and new-workspace (the other words take them on the pane)"),
    word("Starting", "name", "x name -- x", "the tab's or workspace's name"),
    word("Starting", "size", "x cols rows -- x", "what its tab is laid out on while no client shows it"),
    word("Starting", "cwd", "x dir -- x", "where it starts"),
    word("Starting", "env", "x key var -- x", "a variable for it: text, or a pane, tab or workspace as its id (%3; an action's answer, its pane)"),
    word("Starting", "program", "x path -- x", "the program to start, in place of a shell"),
    word("Starting", "arg", "x text -- x", "an argument for the program"),
    word("Starting", "command", "x line -- x", "the program and its arguments, one line split as a shell splits it"),
    word("Starting", "typed", "x line -- x", "a command line typed into its new shell, at once: its terminal echoes it above the first prompt too"),
    word("Starting", "horizontal", "x -- x", "split side by side"),
    word("Starting", "vertical", "x -- x", "split stacked"),
    word("Starting", "in", "x workspace -- x", "the workspace for run or new-tab (a name not there is made)"),
    word("Starting", "id", "x id -- x", "the id to make it with (%N, @N, +N): above every id of its kind given, and at most 65536 past the last"),
    word("Starting", "log-to", "x file -- x", "keep everything the program writes in a file too"),
    word("Starting", "kill-first", "x -- x", "respawn: end the program first if it still runs"),
    word("Starting", "run", "opts -- pane", "start a program in a tab of its own, in workspace agent; its pane stays once it exits"),
    word("Starting", "split", "subject -- pane", "split a pane: a shell, or the program given"),
    word("Starting", "new-tab", "opts -- pane", "a tab with a shell"),
    word("Starting", "new-workspace", "opts -- pane", "a workspace with a tab and a shell"),
    word("Starting", "respawn", "subject -- pane", "start again in its pane the program run started"),
    // Waiting
    word("Waiting", "changed", "version -- request", "a wait from this version (%2:0.57.0.3.0:RUN, as an action, a wait or `str version get` gives it) of the pane it names, not from now"),
    word("Waiting", "timeout", "x secs -- x", "how long a wait waits at most, else the server's wait-seconds (30)"),
    word("Waiting", "wait", "place -- answer", "wait until the pane's version differs from the one changed gives, else an action's answer's (%2 str y text send-keys wait), else now's, or the pane closes; a tab or workspace: any of the panes there as it begins"),
    // Input
    word("Input", "text", "x text -- x", "text, typed"),
    word("Input", "key", "x key -- x", "a key by its name: Enter, C-c, Up, F5"),
    word("Input", "paste", "x text -- x", "text as a paste, bracketed if the program turned that on (an end mark, ESC [201~, in it then dropped); else as it is"),
    word("Input", "paste-file", "x file -- x", "a file's text as a paste, at most 900 KiB, bracketed if the program turned that on (an end mark in it then dropped)"),
    word("Input", "bytes", "x hex -- x", "bytes as they are, in hex (1b5b41, c3): part of a key's sequence or of a character"),
    word("Input", "times", "x n -- x", "the input just before it, n times (1 to 1000): str Down key 2 times"),
    word("Input", "send-keys", "target -- answer", "send the keys on it to a pane, every pane of a tab, or a client as typed at its terminal; 900 KiB at most in all"),
    word("Input", "mouse", "x action row col -- x", "click right middle press release wheel-up wheel-down or move, at a cell from 0"),
    word("Input", "send-mouse", "subject -- answer", "send the mouse event on it"),
    word("Input", "signal", "subject name -- answer", "TERM KILL INT HUP QUIT USR1 USR2 STOP CONT TSTP WINCH ILL ABRT FPE SEGV PIPE ALRM, to the pane's foreground"),
    // Reading
    word("Reading", "capture", "subject -- capture", "the screen, as the words on the pane say"),
    word("Reading", "history", "x lines -- x", "capture: lines of history first"),
    word("Reading", "all-history", "x -- x", "capture: all the history first"),
    word("Reading", "wrapped", "x -- x", "capture: each screen row a line, as wrapped at the pane's width (the cells a mouse event counts), not joined into the lines printed"),
    word("Reading", "full", "x -- x", "capture: every row and cell, untrimmed"),
    word("Reading", "styles", "x -- x", "capture: colours and attributes"),
    word("Reading", "marks", "x -- x", "capture: highlighted text (reverse video, or a background colour) as ⟦this⟧"),
    word("Reading", "main-screen", "x -- x", "capture: what was printed before a program took the screen"),
    word("Reading", "alt-screen", "x -- x", "capture: the full screen a program drew last"),
    word("Reading", "last-output", "x -- x", "capture: the last shell command's output, or the running one's so far (a full-screen one's: its screen to the cursor); fails no_marks where no command was marked (OSC 133); one of main-screen, alt-screen and last-output at most"),
    word("Reading", "rows", "x from to -- x", "capture: those rows only, from 0, each line they touch whole (with wrapped, those rows alone)"),
    word("Reading", "cols", "x from to -- x", "capture: those columns only, from 0"),
    word("Reading", "ls", "-- listing", "workspaces, tabs, panes, clients and grants"),
    word("Reading", "info", "handle -- record", "a pane, tab or workspace, all of it"),
    word("Reading", "get", "handle field -- value", "one field of a pane, tab or workspace: version, exited, last_status, idle_ms, cwd"),
    word("Reading", "json", "value -- json", "value as JSON (an answer as its JSON); a failed wait stops the program before json runs"),
    word("Reading", "events", "--", "what happens, a line each, as it happens"),
    word("Reading", "events-json", "--", "what happens, as JSON lines"),
    word("Reading", "events-of", "handle --", "what happens in a pane, tab or workspace"),
    word("Reading", "events-of-json", "handle --", "the same, as JSON lines"),
    word("Reading", "dump", "-- text", "everything, as a program that makes it again (shells new, nothing typed into them)"),
    word("Reading", "dump-of", "handle -- text", "a tab or workspace, as a program that makes it again (shells new, nothing typed into them)"),
    // Changing
    word("Changing", "kill", "handle --", "close a pane, tab or workspace, without asking"),
    word("Changing", "rename", "handle text --", "rename a pane, tab or workspace"),
    word("Changing", "notify", "handle text --", "a notice on the clients showing it"),
    word("Changing", "notify-all", "text --", "a notice on every client"),
    word("Changing", "resize", "handle cols rows --", "what a tab (or a pane's) is laid out on while no client shows it"),
    word("Changing", "resize-auto", "handle --", "a tab laid out on a client's screen again"),
    word("Changing", "resize-x", "pane width --", "a pane's width: cells, or a percent"),
    word("Changing", "resize-y", "pane height --", "a pane's height: cells, or a percent"),
    word("Changing", "percent", "n -- size", "n percent of the tab, for resize-x and resize-y"),
    word("Changing", "move-border", "pane way cells --", "move the pane's nearest border: left right up down"),
    word("Changing", "move-to", "pane where --", "move a pane to a tab, a workspace, new-tab or new-workspace"),
    word("Changing", "move-beside", "pane way --", "move a pane beside its neighbour: left right up down"),
    word("Changing", "swap-panes", "pane pane --", "swap two panes"),
    word("Changing", "swap-toward", "pane way --", "swap a pane with its neighbour"),
    word("Changing", "reorder", "handle way --", "move one place: next or previous"),
    word("Changing", "start-log", "pane file --", "keep what the pane's program writes from now on"),
    word("Changing", "stop-log", "pane --", "stop keeping it"),
    word("Changing", "clear-history", "pane --", "drop the pane's history"),
    word("Changing", "buffers", "-- text", "the paste buffers, newest 0"),
    word("Changing", "buffer", "n -- text", "paste buffer n"),
    word("Changing", "paste-buffer", "pane n --", "paste buffer n into a pane"),
    // Clients
    word("Clients", "attach", "--", "show the server in this terminal"),
    word("Clients", "attach-to", "workspace --", "the same, on a workspace"),
    word("Clients", "watch", "--", "show it, keys doing nothing but the prefix then d"),
    word("Clients", "headless", "cols rows --", "a client with no terminal, until signalled; prints its id"),
    word("Clients", "focus", "client way --", "left right up down next previous or last pane"),
    word("Clients", "focus-pane", "client pane --", "focus a pane"),
    word("Clients", "show-tab", "client tab --", "show a tab"),
    word("Clients", "next-tab", "client --", "the next tab"),
    word("Clients", "previous-tab", "client --", "the previous tab"),
    word("Clients", "show-workspace", "client workspace --", "show a workspace"),
    word("Clients", "next-workspace", "client --", "the next workspace"),
    word("Clients", "previous-workspace", "client --", "the previous workspace"),
    word("Clients", "choose-tab", "client --", "a list of tabs to go to"),
    word("Clients", "choose-workspace", "client --", "a list of workspaces to go to"),
    word("Clients", "choose-tab-for", "client pane --", "a list of tabs to move the pane to"),
    word("Clients", "choose-workspace-for", "client pane --", "a list of workspaces to move the pane to"),
    word("Clients", "choose-pane", "client pane --", "a list of panes to swap the pane with"),
    word("Clients", "rename-prompt", "client handle --", "a prompt for its new name"),
    word("Clients", "confirm-close", "client handle --", "close it, once confirmed"),
    word("Clients", "zoom", "client --", "zoom the focused pane, or restore"),
    word("Clients", "copy-mode", "client --", "copy and select in the focused pane"),
    word("Clients", "command-prompt", "client --", "a prompt for fuxth"),
    word("Clients", "detach", "client --", "detach a client"),
    word("Clients", "capture-client", "client -- capture", "what a client's terminal shows"),
    word("Clients", "resize-client", "client cols rows --", "a headless client's size"),
    // Server
    word("Server", "server", "name --", "the server named so (NAME.sock in fux's directory), for what follows"),
    word("Server", "socket", "path --", "the server on this socket, for what follows"),
    word("Server", "start", "--", "start the server in the background, if none answers"),
    word("Server", "status", "-- record", "whether a server answers, and its version"),
    word("Server", "servers", "-- listing", "the servers that answer in fux's directory"),
    word("Server", "kill-server", "--", "stop the server: not from its own panes, nor with people attached"),
    word("Server", "force-kill-server", "--", "stop it anyway, whatever fux version it is"),
    word("Server", "set", "setting text --", "shell history-bytes wait-seconds clipboard buffers bell titles update-environment"),
    word("Server", "grant", "workspace socket -- path", "a socket whose requests reach that workspace alone; its path"),
    word("Server", "revoke", "socket --", "close a grant's socket"),
    word("Server", "version", "-- text", "this fux's version (a pane's version is `str version get` on the pane)"),
    // Config
    word("Config", "prefix", "key --", "this client's prefix (config)"),
    word("Config", "bind", "keys code --", "keys after the prefix run fuxth (config)"),
    word("Config", "bind-root", "key code --", "a key without the prefix runs fuxth (config)"),
    word("Config", "label", "text --", "what the column says the last binding does"),
    word("Config", "group", "name --", "the column's heading for the last binding"),
    word("Config", "repeat", "--", "the last binding's layer repeats until Esc"),
    word("Config", "unbind", "keys --", "keys after the prefix run nothing"),
    word("Config", "unbind-root", "key --", "a key without the prefix runs nothing"),
    word("Config", "unbind-all", "--", "no bindings at all"),
    word("Config", "list-keys", "--", "this client's keys, as config lines that make them"),
    word("Config", "key-names", "--", "the names keys go by"),
    // Help
    word("Help", "help", "--", "the words, under their headings"),
    word("Help", "help-agents", "--", "how to drive programs from a script or an agent: the first screen, enough for most work"),
    word("Help", "help-agents-on", "word --", "one topic of that guide at length (recipes keys read shells programs servers), or all of it"),
    word("Help", "help-json", "--", "what json gives"),
    word("Help", "explain", "word --", "one word at length"),
    word("Help", "check", "file --", "read a fuxth file without running it: its mistakes; a config.fux is run dry too"),
];

/// A failure of a word's own: its code and why.
fn fail(code: impl Into<Lang>, message: impl Into<String>) -> Failure {
    Failure::new(code.into(), message)
}

/// How a word of fux's is used, for a mistake in using it: what goes
/// before it, what it leaves, and an example.
pub fn usage(word: &str) -> Option<String> {
    let library = || {
        (help::library().into_iter())
            .find(|(name, ..)| *name == word)
            .map(|(_, effect, ..)| effect)
    };
    let core = || fuxth::Builtin::named(word).map(fuxth::Builtin::effect);
    // Fux's, the prelude's or library's, or the core's; else the
    // language's own syntax, said as it is written.
    let fuxs = WORDS.iter().find(|w| w.name == word);
    let effect = match fuxs {
        Some(w) => w.effect,
        None => match library().or_else(core) {
            Some(effect) => effect,
            None => {
                let (shape, _) = fuxth::Parsing::named(word)?.syntax();
                return Some(format!("use: {shape}"));
            }
        },
    };
    let (takes, leaves) = effect.split_once("--").unwrap_or((effect, ""));
    // What a request's word goes on: the pane, or opts.
    let takes = takes.trim().to_uppercase();
    let request = takes.strip_prefix('X');
    let takes = match request.filter(|r| r.is_empty() || r.starts_with(' ')) {
        Some(rest) => format!("PANE-OR-OPTS{rest}"),
        None => takes,
    };
    let leaves = match leaves.trim() {
        "" => String::new(),
        "x" => ", which leaves a request (or opts) for the word that acts".to_owned(),
        leaves => format!(", which leaves {leaves}"),
    };
    let example = help::example(word).map_or_else(String::new, |e| format!("; e.g. {e}"));
    // A capture's option says so: it goes between opts and capture.
    let kind = match fuxs.is_some_and(|w| w.about.starts_with("capture: ")) {
        true => "a capture option, ",
        false => "",
    };
    Some(format!("use: {kind}{takes} {word}{leaves}{example}"))
}

fn wrong(wanted: &str, got: &Value<Fux>) -> Failure {
    fail(Lang::Type, format!("takes {wanted}, not {}", got.kind()))
}

fn string(value: &Value<Fux>) -> Result<String, Failure> {
    let Value::Str(s) = value else {
        return Err(wrong("a string", value));
    };
    Ok(s.to_string())
}

/// A value as words that take text read it: a string, or a number.
fn text(value: &Value<Fux>) -> Result<String, Failure> {
    match value {
        Value::Str(s) => Ok(s.to_string()),
        Value::Int(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        other @ (Value::List(_) | Value::App(_)) => {
            Err(wrong("a string, a number or a boolean", other))
        }
    }
}

/// A value as JSON: a list as an array of its values', an answer as its
/// JSON, as alone (a capture's, a listing's or an action's object; of
/// several panes, an array of theirs), anything else of fux's as its text.
fn json_of(value: &Value<Fux>) -> crate::json::Json<'static> {
    use crate::json::Json;
    match value {
        Value::Int(n) => Json::Number(*n),
        Value::Bool(b) => Json::Bool(*b),
        Value::Str(s) => Json::str(s.to_string()),
        // As deep as lists nest, at most fuxth's limit.
        Value::List(list) => Json::Array(list.items().iter().map(json_of).collect()),
        // An answer, a line of JSON a pane: one pane's as its object; one
        // of several panes (a tab's or workspace's wait), as an array,
        // however many answered.
        Value::App(Fux::Answer(answer)) => match &answer.json {
            Some(json) => {
                let mut objects: Vec<Json<'static>> = (json.lines())
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| Json::Raw(line.to_owned().into()))
                    .collect();
                match (answer.said, objects.as_mut_slice()) {
                    (said, [one]) if said != Said::Panes => std::mem::replace(one, Json::Null),
                    _ => Json::Array(objects),
                }
            }
            None => Json::str(answer.text.clone()),
        },
        Value::App(_) => Json::str(value.text()),
    }
}

fn int(value: &Value<Fux>) -> Result<i64, Failure> {
    let Value::Int(n) = value else {
        return Err(wrong("an integer", value));
    };
    Ok(*n)
}

/// What a cell can be, and a count: from 0 to the most each holds.
const CELL: RangeInclusive<i64> = 0..=u16::MAX as i64;
const COUNT: RangeInclusive<i64> = 0..=i64::MAX;

/// An integer within `range`, as the type it goes in.
fn within<T: TryFrom<i64>>(value: &Value<Fux>, range: RangeInclusive<i64>) -> Result<T, Failure> {
    let n = int(value)?;
    let fits = T::try_from(n).ok().filter(|_| range.contains(&n));
    fits.ok_or_else(|| {
        let said = match (range.start(), range.end()) {
            (from, &i64::MAX) => format!("from {from}"),
            (from, to) => format!("{from} to {to}"),
        };
        fail(Lang::Range, format!("{n} is out of range: {said}"))
    })
}

/// One of a closed set, by its name; refused, naming each, if none.
fn one_of<T: Copy>(value: &Value<Fux>, all: &[(&str, T)]) -> Result<T, Failure> {
    let given = string(value)?;
    let found = all.iter().find(|(name, _)| *name == given);
    found.map(|(_, t)| *t).ok_or_else(|| {
        let names: Vec<&str> = all.iter().map(|(name, _)| *name).collect();
        let names = crate::words::listed(&names, "or");
        fail(Code::BadValue, format!("{given:?} is not {names}"))
    })
}

/// The most columns, and rows, a tab is laid out on.
const TAB_MOST: u16 = 500;

/// A size: columns and rows each 1 to `most` (a tab's laid out on,
/// `TAB_MOST`; a client's, as many as a cell holds).
fn laid_out(cols: &Value<Fux>, rows: &Value<Fux>, most: u16) -> Result<fux_vt::Size, Failure> {
    let cells = 1..=i64::from(most);
    let (cols, rows) = (within(cols, cells.clone())?, within(rows, cells)?);
    fux_vt::Size::new(rows, cols).map_err(|e| fail(Lang::Range, e.to_string()))
}

/// The pane a value is: a pane, or an action's answer of one.
fn as_pane(value: &Value<Fux>) -> Option<PaneId> {
    if let Value::App(Fux::Answer(answer)) = value {
        return answer.said.pane();
    }
    let Value::App(Fux::Pane(p)) = value else {
        return None;
    };
    Some(*p)
}

fn pane(value: &Value<Fux>) -> Result<PaneId, Failure> {
    as_pane(value).ok_or_else(|| wrong("a pane (from pane, run or split)", value))
}

fn client(value: &Value<Fux>) -> Result<ClientId, Failure> {
    let Value::App(Fux::Client(c)) = value else {
        return Err(wrong("a client (from me or client)", value));
    };
    Ok(*c)
}

/// A pane, tab or workspace.
fn target(value: &Value<Fux>) -> Result<AnyRef, Failure> {
    if let Some(p) = as_pane(value) {
        Ok(AnyRef::Pane(p))
    } else if let Value::App(Fux::Tab(t)) = value {
        Ok(AnyRef::Tab(*t))
    } else if let Value::App(Fux::Workspace(w)) = value {
        Ok(AnyRef::Workspace(*w))
    } else {
        Err(wrong("a pane, tab or workspace", value))
    }
}

/// A workspace, or its name: `+N` read as its id, as `parse_workspace` reads it.
fn workspace(value: &Value<Fux>) -> Result<WsRef, Failure> {
    if let Value::App(Fux::Workspace(w)) = value {
        return Ok(WsRef::Id(*w));
    }
    crate::command::parse_workspace(&string(value)?)
        .map_err(|e| fail(Code::BadValue, e.to_string()))
}

fn direction(value: &Value<Fux>) -> Result<Direction, Failure> {
    one_of(value, &Direction::ALL.map(|way| (way.name(), way)))
}

/// Options a word would leave unused, refused rather than ignored: it
/// takes those `help` lists for it.
fn unused(word: &str, built: &Opts) -> Result<(), Failure> {
    let takes = |o: &str| help::options(word).is_some_and(|mut t| t.any(|t| t == o));
    let Some(option) = built.given.iter().find(|o| !takes(o)) else {
        return Ok(());
    };
    let instead = format!("takes no {option}: {option} goes on {}", takers(option));
    Err(fail(Lang::Unused, instead))
}

/// What a request is on: a pane, tab or workspace, a client, or the
/// version an action's or wait's answer holds, of its pane.
fn on(value: &Value<Fux>) -> Result<On, Failure> {
    if let Value::App(Fux::Client(c)) = value {
        return Ok(On::Client(*c));
    }
    if let Some(version) = held(value) {
        return Ok(On::Version(version));
    }
    target(value).map(On::Target)
}

/// What a wait or an action on `target` answers: a pane's, its pane; a
/// tab's or workspace's, no pane, however many panes it has or answered
/// (name it again to act on it again), and as JSON an array.
fn answer_of(target: AnyRef) -> Then {
    match target {
        AnyRef::Pane(_) => Then::Outcome,
        AnyRef::Tab(_) | AnyRef::Workspace(_) => Then::Answer(Said::Panes),
    }
}

/// The version an action's answer, or a wait's, holds: its pane's from
/// before what it did. A wait on it waits from there, so nothing that came
/// between is missed.
fn held(value: &Value<Fux>) -> Option<crate::command::Version> {
    match value {
        Value::App(Fux::Answer(answer)) => match answer.said {
            Said::Action(version) => Some(version),
            Said::Capture
            | Said::Listing
            | Said::Panes
            | Said::Json
            | Said::Status
            | Said::Servers => None,
        },
        Value::Int(_) | Value::Bool(_) | Value::Str(_) | Value::List(_) | Value::App(_) => None,
    }
}

/// The options or request `value` is, `word` given it: options stay
/// options; what else it goes on (a pane, tab, workspace or client, an
/// action's answer) becomes a request on that.
fn modifying(word: &'static str, value: Value<Fux>) -> Result<Box<Opts>, Failure> {
    let mut built = match value {
        Value::App(Fux::Opts(built)) => built,
        other @ (Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_)) => {
            let on = on(&other)
                .map_err(|_| wrong("a pane, tab, workspace or client, or options", &other))?;
            Box::new(Opts {
                on: Some(on),
                ..Opts::default()
            })
        }
    };
    built.given.push(word);
    Ok(built)
}

/// What `mouse` takes, by name.
const MICE: [(&str, Mouse); 8] = [
    ("click", Mouse::Click(Button::Left)),
    ("right", Mouse::Click(Button::Right)),
    ("middle", Mouse::Click(Button::Middle)),
    ("press", Mouse::Press(Button::Left)),
    ("release", Mouse::Release(Button::Left)),
    ("wheel-up", Mouse::Wheel(Wheel::Up)),
    ("wheel-down", Mouse::Wheel(Wheel::Down)),
    ("move", Mouse::Move),
];

/// What a word of a request's (an option, a wait, keys, a mouse event)
/// changes of it, `next` giving the values it takes after it.
fn modify(
    name: &str,
    built: &mut Opts,
    next: &mut dyn FnMut() -> Result<Value<Fux>, Failure>,
) -> Result<(), Failure> {
    match name {
        "name" => built.name = Some(string(&next()?)?),
        "cwd" => built.cwd = Some(string(&next()?)?.into()),
        "typed" => built.typed = Some(string(&next()?)?),
        "in" => built.place = Some(workspace(&next()?)?),
        "log-to" => built.log = Some(string(&next()?)?),
        "history" => built.history = Some(within(&next()?, COUNT)?),
        "all-history" => built.history = Some(usize::MAX),
        "size" => built.size = Some(laid_out(&next()?, &next()?, TAB_MOST)?),
        "horizontal" => built.axis = Some(Axis::Horizontal),
        "vertical" => built.axis = Some(Axis::Vertical),
        "kill-first" => built.kill = true,
        "wrapped" => built.view.wrapped = true,
        "full" => built.view.full = true,
        "styles" => built.view.styles = true,
        "marks" => built.view.marks = true,
        "timeout" => built.timeout = Some(Duration::from_secs(within(&next()?, COUNT)?)),
        "text" => built.inputs.push(Input::Text(text(&next()?)?)),
        "paste" => built.inputs.push(Input::Paste(text(&next()?)?)),
        "bytes" => built.inputs.push(Input::Bytes(hex(&string(&next()?)?)?)),
        "command" => {
            let line = string(&next()?)?;
            let argv = crate::words::split(&line).map_err(|e| fail(Lang::OpenQuote, e))?;
            if argv.first().is_none_or(|p| p.is_empty()) {
                return Err(fail(Code::NoProgram, "an empty command line"));
            }
            built.argv = argv;
        }
        "program" => {
            let program = string(&next()?)?;
            match built.argv.first_mut() {
                Some(first) => *first = program,
                None => built.argv.push(program),
            }
        }
        "arg" => {
            let arg = text(&next()?)?;
            if built.argv.is_empty() {
                return Err(fail(Code::NoProgram, "comes after program"));
            }
            built.argv.push(arg);
        }
        "id" => {
            let text = string(&next()?)?;
            let bad = format!("{text:?} is not %N, @N or +N");
            let ids = &mut built.ids;
            let given = match text.parse().map_err(|()| fail(Code::BadValue, bad))? {
                AnyRef::Pane(id) => ids.pane.replace(id).is_some(),
                AnyRef::Tab(id) => ids.tab.replace(id).is_some(),
                AnyRef::Workspace(id) => ids.workspace.replace(id).is_some(),
            };
            if given {
                let why = format!("id {text}: one of its kind is given already");
                return Err(fail(Code::Id, why));
            }
        }
        "env" => {
            let (key, value) = (string(&next()?)?, next()?);
            // A pane, tab or workspace as fux writes it (`%3`).
            let value = match (target(&value), text(&value)) {
                (Ok(handle), _) => handle.to_string(),
                (Err(_), Ok(text)) => text,
                (Err(_), Err(_)) => {
                    let what = "a string, a number, a boolean, or a pane, tab or workspace";
                    return Err(wrong(what, &value));
                }
            };
            built
                .env
                .push(key, value)
                .map_err(|why| fail(Code::BadValue, why))?;
        }
        "main-screen" | "alt-screen" | "last-output" => {
            if built.view.source != Source::Shown {
                let why = "a capture reads one screen, and one is given";
                return Err(fail(Lang::Unused, why));
            }
            built.view.source = match name {
                "main-screen" => Source::Main,
                "alt-screen" => Source::Alternate,
                _ => Source::LastOutput,
            };
        }
        "rows" | "cols" => {
            let (from, to) = (within(&next()?, CELL)?, within(&next()?, CELL)?);
            if from > to {
                let why = format!("{from} {to}: the first {name} before the last");
                return Err(fail(Lang::Range, why));
            }
            match name {
                "rows" => built.view.rows = Some((from, to)),
                _ => built.view.cols = Some((from, to)),
            }
        }
        "key" => {
            let word = string(&next()?)?;
            let press = word.parse().map_err(|_| {
                let closest = closest_key(&word)
                    .map_or_else(String::new, |k| format!("; the closest is `{k}`"));
                fail(
                    Code::NotAKey,
                    format!("{word:?} is no key's name{closest} (key-names lists them)"),
                )
            })?;
            built.inputs.push(Input::Key(press));
        }
        "paste-file" => {
            let path = string(&next()?)?;
            let most = (INPUT_MAX, "900 KiB, the most one paste takes");
            let text =
                super::read_file(path.as_ref(), most).map_err(|f| fail(Code::Paste, f.message))?;
            built.inputs.push(Input::Paste(text));
        }
        // The input just before it, n times in all: refused before it is
        // copied if that is more than send-keys sends.
        "times" => {
            let n: usize = within(&next()?, 1..=1000)?;
            let Some(last) = built.inputs.last().cloned() else {
                return Err(fail(
                    Code::NoKeys,
                    "repeats the input before it: str Down key 3 times",
                ));
            };
            let more = sent(std::slice::from_ref(&last)).saturating_add(5);
            fits(sent(&built.inputs).saturating_add(more.saturating_mul(n.saturating_sub(1))))?;
            built
                .inputs
                .extend(std::iter::repeat_n(last, n.saturating_sub(1)));
        }
        "mouse" => {
            let mouse = one_of(&next()?, &MICE)?;
            built.mouse = Some((mouse, within(&next()?, CELL)?, within(&next()?, CELL)?));
        }
        _ => return Err(fail(Lang::UnknownWord, format!("{name}: no such word"))),
    }
    Ok(())
}

/// What an action acts on, and what it was given: a request; or a pane,
/// tab, workspace or client alone, given nothing. Refused if `word`
/// leaves something given unused.
fn request(word: &str, value: Value<Fux>) -> Result<(On, Opts), Failure> {
    let mut built = match value {
        Value::App(Fux::Opts(built)) => *built,
        other @ (Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_)) => Opts {
            on: Some(on(&other)?),
            ..Opts::default()
        },
    };
    let Some(on) = built.on.take() else {
        return Err(fail(
            Lang::Type,
            "takes a pane, not options: what opts gives is for run, new-tab and new-workspace; \
             the rest go on the pane (`%2 all-history capture`, `%2 vertical split`)",
        ));
    };
    unused(word, &built)?;
    Ok((on, built))
}

/// The pane an action acts on, and what it was given.
fn request_pane(word: &str, value: Value<Fux>) -> Result<(PaneId, Opts), Failure> {
    match request(word, value)? {
        (On::Target(AnyRef::Pane(p)), built) => Ok((p, built)),
        (On::Version(version), built) => Ok((version.pane, built)),
        (On::Target(AnyRef::Tab(_) | AnyRef::Workspace(_)) | On::Client(_), _) => Err(fail(
            Lang::Type,
            "takes a pane (from pane, run or split), or a request on one",
        )),
    }
}

/// The options `value` holds, for `word`, which makes what they are for:
/// refused if it leaves one unused.
fn finished(word: &str, value: Value<Fux>) -> Result<Opts, Failure> {
    match value {
        Value::App(Fux::Opts(built)) if built.on.is_none() => {
            unused(word, &built)?;
            Ok(*built)
        }
        other @ (Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_)) => Err(wrong("options (from opts): opts … run", &other)),
    }
}

/// The key name nearest `word`, its modifiers kept (`C-Bakspace`:
/// `C-BSpace`), if one is near enough to be what was meant.
fn closest_key(word: &str) -> Option<String> {
    let mut rest = word;
    while let Some((prefix, tail)) = rest.split_at_checked(2)
        && !tail.is_empty()
        && ["C-", "M-", "S-"]
            .iter()
            .any(|m| m.eq_ignore_ascii_case(prefix))
    {
        rest = tail;
    }
    let mods = word.get(..word.len().saturating_sub(rest.len()))?;
    let lower = rest.to_lowercase();
    // An alias near it is said by the name it stands for.
    let aliases = crate::keys::aliases().filter_map(|alias| {
        let press: crate::keys::KeyPress = alias.parse().ok()?;
        Some((alias.to_owned(), press.to_string()))
    });
    let names = crate::keys::all_names().into_iter().map(|n| (n.clone(), n));
    let (far, name) = (names.chain(aliases))
        .map(|(spelt, name)| (fuxth::distance(&lower, &spelt.to_lowercase()), name))
        .min()?;
    (far <= lower.chars().count().div_ceil(2)).then(|| format!("{}{name}", mods.to_uppercase()))
}

/// The words that take `option`, listed; every word that takes options,
/// for a word that is none.
pub(super) fn takers(option: &str) -> String {
    let takes = |w: &&str| help::options(w).is_some_and(|mut t| t.any(|t| t == option));
    let words: Vec<&str> = help::OPTIONS.iter().map(|(w, ..)| *w).collect();
    let on: Vec<&str> = words.iter().copied().filter(takes).collect();
    crate::words::listed(if on.is_empty() { &words } else { &on }, "or")
}

/// The bytes `hex` spells, two digits each (`1b5b41`, `c3 a9`; blanks
/// between bytes ignored).
fn hex(hex: &str) -> Result<Vec<u8>, Failure> {
    let bad = || {
        fail(
            Code::BadValue,
            format!("{hex:?}: bytes in hex, two digits each: 1b5b41"),
        )
    };
    let mut digits = hex
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_digit(16));
    let mut bytes = Vec::new();
    while let Some(high) = digits.next() {
        let (Some(high), Some(Some(low))) = (high, digits.next()) else {
            return Err(bad());
        };
        bytes.extend(u8::try_from(high.saturating_mul(16).saturating_add(low)).ok());
    }
    match bytes.is_empty() {
        true => Err(bad()),
        false => Ok(bytes),
    }
}

/// The most input one send-keys sends: a request holds it whole.
const INPUT_MAX: usize = 900 * 1024;

/// The bytes `inputs` take as the request carries them, which must fit in
/// one frame: text and pastes their bytes, keys their names' (`C-M-F12`
/// 7), and each input after the first 5 bytes more.
fn sent(inputs: &[Input]) -> usize {
    (inputs.iter())
        .map(|input| match input {
            Input::Text(text) | Input::Paste(text) => text.len(),
            Input::Key(key) => key.to_string().len(),
            Input::Bytes(bytes) => bytes.len(),
        })
        .fold(0usize, usize::saturating_add)
        .saturating_add(inputs.len().saturating_sub(1).saturating_mul(5))
}

/// `typed` bytes of input, refused past what one send-keys sends.
fn fits(typed: usize) -> Result<(), Failure> {
    match typed > INPUT_MAX {
        true => Err(fail(
            Lang::TooLong,
            format!(
                "{typed} bytes, more than 900 KiB, the most it sends at once \
                 (text and pastes count their bytes, keys their names', each input \
                 after the first 5 more)"
            ),
        )),
        false => Ok(()),
    }
}

fn ask(request: impl Into<Request>, then: Then) -> Ask {
    Ask::Server {
        request: Box::new(request.into()),
        client: None,
        then,
    }
}

/// An order to the server that answers nothing.
fn tell(request: impl Into<Request>) -> Result<Done<Ask>, Failure> {
    Ok(Done::Later(ask(request, Then::Nothing)))
}

/// What this client does itself.
fn local(local: Local) -> Result<Done<Ask>, Failure> {
    Ok(Done::Later(Ask::Local(local)))
}

fn ask_as(client: ClientId, command: Command, then: Then) -> Ask {
    Ask::Server {
        request: Box::new(command.into()),
        client: Some(client),
        then,
    }
}

fn act(client: ClientId, action: ClientAction) -> Ask {
    ask_as(
        client,
        Command::Client {
            client: Some(client),
            action,
        },
        Then::Nothing,
    )
}

/// How a screen's word makes its screen from what follows the client.
enum Opens {
    Alone(Open),
    Pane(fn(PaneId) -> Open),
    Target(fn(AnyRef) -> Open),
}

/// The words that open one of a client's own screens.
#[rustfmt::skip]
const SCREENS: &[(&str, Opens)] = &[
    ("choose-tab", Opens::Alone(Open::ChooseTab(None))),
    ("choose-workspace", Opens::Alone(Open::ChooseWorkspace(None))),
    ("copy-mode", Opens::Alone(Open::Copy)),
    ("command-prompt", Opens::Alone(Open::Prompt)),
    ("choose-tab-for", Opens::Pane(|p| Open::ChooseTab(Some(p)))),
    ("choose-workspace-for", Opens::Pane(|p| Open::ChooseWorkspace(Some(p)))),
    ("choose-pane", Opens::Pane(Open::ChoosePane)),
    ("rename-prompt", Opens::Target(Open::Rename)),
    ("confirm-close", Opens::Target(Open::Confirm)),
];

impl fuxth::Host for Host<'_> {
    type Value = Fux;
    type Request = Ask;

    fn print(&mut self, line: &str) -> Result<(), Failure> {
        writeln!(self.out, "{line}").map_err(|e| fail(Code::Output, e.to_string()))
    }

    fn call(
        &mut self,
        word: usize,
        args: fuxth::Args<'_, Fux>,
        out: &mut Vec<Value<Fux>>,
    ) -> Result<Done<Ask>, Failure> {
        let Some(spec) = WORDS.get(word) else {
            return Err(fail(Lang::UnknownWord, "no such word"));
        };
        let name = spec.name;
        let mut args = args.into_iter();
        let mut next = || {
            args.next().ok_or_else(|| {
                fail(
                    Lang::StackUnderflow,
                    format!("too few values: {}", spec.effect),
                )
            })
        };
        let now = |out: &mut Vec<Value<Fux>>, value: Value<Fux>| {
            out.push(value);
            Ok(Done::Now)
        };
        let later = |ask: Ask| Ok(Done::Later(ask));
        let resolve = |text: String, kind: Kind| {
            later(ask(Command::Resolve { text, kind }, Then::Handle(kind)))
        };
        // A word of a request's, as its effect says: on options, or on what
        // the word that acts after acts on.
        if spec.effect.starts_with("x ") {
            let mut built = modifying(name, next()?)?;
            modify(name, &mut built, &mut next)?;
            return now(out, Value::App(Fux::Opts(built)));
        }
        // A client's own screens, which it opens itself: from its keys or
        // prompt, here; asked from elsewhere, relayed for it to run.
        if let Some((_, opens)) = SCREENS.iter().find(|(word, _)| *word == name) {
            let c = client(&next()?)?;
            let open = match opens {
                Opens::Alone(open) => open.clone(),
                Opens::Pane(open) => open(pane(&next()?)?),
                Opens::Target(open) => open(target(&next()?)?),
            };
            return match self.context.client == Some(c) {
                true => local(Local::Open(open)),
                false => later(act(c, ClientAction::Run(open.code()))),
            };
        }
        match name {
            // ----------------------------------------------------- finding
            "pane" | "tab" | "workspace" => {
                let kind = Kind::ALL.into_iter().find(|k| k.name() == name);
                let kind = kind.unwrap_or(Kind::Workspace);
                // A handle (an action's answer is its pane) of the kind is
                // itself; another kind's is looked for by its id.
                let found = next()?;
                if let Ok(id) = target(&found) {
                    return match id.kind() == kind {
                        true => now(out, Value::App(super::handle(id))),
                        false => resolve(id.to_string(), kind),
                    };
                }
                match found {
                    // An empty name names nothing: never the caller's own,
                    // which `here` is, so that an empty variable closes
                    // nothing.
                    Value::Str(s) if s.trim().is_empty() => Err(fail(
                        Code::NoTarget,
                        format!("an empty name names no {name}: `here` is the pane this runs in"),
                    )),
                    Value::Str(s) => resolve(s.to_string(), kind),
                    other @ (Value::Int(_) | Value::Bool(_) | Value::List(_) | Value::App(_)) => {
                        Err(wrong("a name, or a pane, tab or workspace", &other))
                    }
                }
            }
            "here" => resolve(String::new(), Kind::Pane),
            "client" => {
                let text = string(&next()?)?;
                let id = text
                    .parse()
                    .map_err(|e: Usage| fail(Code::BadValue, e.to_string()))?;
                now(out, Value::App(Fux::Client(id)))
            }
            "me" => match self.context.client {
                Some(c) => now(out, Value::App(Fux::Client(c))),
                None => Err(fail(
                    Code::NotAClient,
                    "the client whose key or prompt runs this: this runs from the command line",
                )),
            },
            "focused" => {
                let c = client(&next()?)?;
                later(ask_as(
                    c,
                    Command::Resolve {
                        text: String::new(),
                        kind: Kind::Pane,
                    },
                    Then::Handle(Kind::Pane),
                ))
            }
            // ---------------------------------------------------- starting
            "opts" => now(out, Value::App(Fux::Opts(Box::default()))),
            "changed" => {
                let text = string(&next()?)?;
                let Ok(version) = text.parse() else {
                    let why = format!(
                        "{text:?} is no version: PANE:STARTS.SCREEN.BELLS.DONE.ENDED:RUN, as an action answers it"
                    );
                    return Err(fail(Code::BadValue, why));
                };
                let built = Opts {
                    on: Some(On::Version(version)),
                    given: vec!["changed"],
                    ..Opts::default()
                };
                now(out, Value::App(Fux::Opts(Box::new(built))))
            }
            "run" | "new-tab" | "new-workspace" => {
                let built = finished(name, next()?)?;
                let command = match name {
                    "run" => {
                        if built.argv.is_empty() {
                            return Err(fail(
                                Code::NoProgram,
                                "needs a program: opts str ./app program run",
                            ));
                        }
                        let run = Command::Run {
                            workspace: built.place,
                            name: built.name,
                            size: built.size,
                            cwd: built.cwd,
                            env: built.env,
                            argv: built.argv,
                            log: built.log,
                            ids: built.ids,
                        };
                        return later(ask(run, Then::Handle(Kind::Pane)));
                    }
                    "new-tab" => Command::NewTab {
                        target: built.place,
                        name: built.name,
                        typed: built.typed,
                        size: built.size,
                        cwd: built.cwd,
                        env: built.env,
                        ids: built.ids,
                    },
                    _ => Command::NewWorkspace {
                        name: built.name,
                        typed: built.typed,
                        size: built.size,
                        cwd: built.cwd,
                        env: built.env,
                        ids: built.ids,
                    },
                };
                later(ask(command, Then::Handle(Kind::Pane)))
            }
            "split" => {
                let (p, built) = request_pane(name, next()?)?;
                let Wanted {
                    pane: id,
                    tab: None,
                    workspace: None,
                } = built.ids
                else {
                    return Err(fail(Code::Id, "makes a pane alone: its id is %N"));
                };
                if !built.argv.is_empty() && built.typed.is_some() {
                    let why = "takes no typed with a program: a line is typed into the shell the program replaces";
                    return Err(fail(Lang::Unused, why));
                }
                let split = Command::Split {
                    axis: built.axis.unwrap_or(Axis::Horizontal),
                    target: p,
                    argv: built.argv,
                    typed: built.typed,
                    cwd: built.cwd,
                    env: built.env,
                    id,
                };
                later(ask(split, Then::Handle(Kind::Pane)))
            }
            "respawn" => {
                let (p, built) = request_pane(name, next()?)?;
                let respawn = Command::Respawn {
                    target: p,
                    kill: built.kill,
                    env: built.env,
                    argv: built.argv,
                };
                later(ask(respawn, Then::Handle(Kind::Pane)))
            }
            // ----------------------------------------------------- waiting
            "wait" => {
                let (on, built) = request(name, next()?)?;
                // On a tab or workspace, in each of its panes.
                let (since, then) = match on {
                    On::Version(version) => (Since::Version(version), Then::Outcome),
                    On::Target(target) => (Since::Now(target), answer_of(target)),
                    On::Client(_) => {
                        let why = "takes a pane, tab or workspace, not a client";
                        return Err(fail(Lang::Type, why));
                    }
                };
                let wait = Wait {
                    since,
                    timeout: built.timeout,
                };
                later(ask(Request::Wait(wait), then))
            }
            // ------------------------------------------------------- input
            "send-keys" => {
                let (on, built) = request(name, next()?)?;
                if built.inputs.is_empty() {
                    return Err(fail(
                        Code::NoKeys,
                        "needs keys: %2 str hello text send-keys",
                    ));
                }
                fits(sent(&built.inputs))?;
                let target = match on {
                    On::Target(target) => target,
                    On::Version(version) => AnyRef::Pane(version.pane),
                    On::Client(c) => {
                        // Typed at its terminal: whatever pane they reach is
                        // the client's to choose, so there is none to wait on
                        // here.
                        if built.inputs.iter().any(|i| matches!(i, Input::Bytes(_))) {
                            return Err(fail(
                                Lang::Unused,
                                "to a client takes keys, text and pastes: bytes go to a pane",
                            ));
                        }
                        let keys = ClientAction::Keys(built.inputs);
                        let typed = Command::Client {
                            client: Some(c),
                            action: keys,
                        };
                        // Its answer, empty: the client's keys go to the pane
                        // it chooses, so there is no one version to answer.
                        return later(ask(typed, Then::Outcome));
                    }
                };
                let keys = Command::SendKeys {
                    target,
                    inputs: built.inputs,
                };
                later(ask(keys, answer_of(target)))
            }
            "send-mouse" => {
                let (p, built) = request_pane(name, next()?)?;
                let Some((mouse, row, col)) = built.mouse else {
                    return Err(fail(
                        Code::NoKeys,
                        "needs a mouse event: %2 str click 3 10 mouse send-mouse",
                    ));
                };
                let mouse = Command::SendMouse {
                    target: p,
                    mouse,
                    row,
                    col,
                };
                later(ask(mouse, Then::Outcome))
            }
            "signal" => {
                let (p, _) = request_pane(name, next()?)?;
                let signals = fuxix::process::Signal::ALL.map(|signal| (signal.name(), signal));
                let signal = one_of(&next()?, &signals)?;
                let signal = Command::Signal { target: p, signal };
                later(ask(signal, Then::Outcome))
            }
            // ----------------------------------------------------- reading
            "capture" => {
                let (p, built) = request_pane(name, next()?)?;
                let capture = Command::CapturePane {
                    target: p,
                    history: built.history,
                    view: built.view,
                };
                later(ask(capture, Then::Answer(Said::Capture)))
            }
            "ls" | "info" | "get" => {
                let target = match name {
                    "ls" => None,
                    _ => Some(target(&next()?)?),
                };
                let (get, then) = match name {
                    "get" => (Some(string(&next()?)?), Then::Value),
                    _ => (None, Then::Answer(Said::Listing)),
                };
                later(ask(Command::Ls { target, get }, then))
            }
            "json" => match next()? {
                // Options and requests are no value to convert: what acts
                // on them gives one.
                Value::App(Fux::Opts(_)) => Err(fail(
                    Lang::Unused,
                    "converts a value: it goes after what acts on the pane (%2 capture json)",
                )),
                other @ (Value::Int(_)
                | Value::Bool(_)
                | Value::Str(_)
                | Value::List(_)
                | Value::App(_)) => now(out, Value::str(json_of(&other).render())),
            },
            "events" | "events-json" | "events-of" | "events-of-json" => {
                let target = match name.starts_with("events-of") {
                    true => Some(target(&next()?)?),
                    false => None,
                };
                let json = name.ends_with("json");
                tell(Request::Events(target, json))
            }
            "dump" | "dump-of" => {
                let target = match name {
                    "dump" => None,
                    _ => Some(target(&next()?)?),
                };
                later(ask(Command::DumpLayout { target }, Then::Text))
            }
            // ---------------------------------------------------- changing
            "kill" => tell(Command::Kill {
                target: target(&next()?)?,
            }),
            "rename" | "notify" => {
                let t = target(&next()?)?;
                let given = text(&next()?)?;
                tell(match name {
                    "rename" => Command::Rename {
                        target: t,
                        name: given,
                    },
                    _ => Command::Notify {
                        target: Some(t),
                        text: given,
                    },
                })
            }
            "notify-all" => tell(Command::Notify {
                target: None,
                text: text(&next()?)?,
            }),
            "resize" | "resize-auto" => {
                let target = target(&next()?)?;
                let size = match name {
                    "resize" => Some(laid_out(&next()?, &next()?, TAB_MOST)?),
                    _ => None,
                };
                tell(Command::Resize(Resized::Tabs(target, size)))
            }
            "resize-client" => {
                let c = client(&next()?)?;
                let size = laid_out(&next()?, &next()?, u16::MAX)?;
                tell(Command::Resize(Resized::Client(c, size)))
            }
            "resize-x" | "resize-y" => {
                let p = pane(&next()?)?;
                let extent = match next()? {
                    Value::App(Fux::Extent(e)) => e,
                    v @ Value::Int(_) => Extent::Cells(within(&v, CELL)?),
                    other @ (Value::Bool(_) | Value::Str(_) | Value::List(_) | Value::App(_)) => {
                        return Err(wrong("cells or a percent", &other));
                    }
                };
                let axis = match name {
                    "resize-x" => Axis::Horizontal,
                    _ => Axis::Vertical,
                };
                let by = PaneResize::To(axis, extent);
                tell(Command::ResizePane { target: p, by })
            }
            "percent" => {
                let n = within(&next()?, Percent::SHARES)?;
                let share = Percent::new(n).ok_or_else(|| fail(Lang::Range, "no share"))?;
                now(out, Value::App(Fux::Extent(Extent::Percent(share))))
            }
            "move-border" => {
                let p = pane(&next()?)?;
                let way = direction(&next()?)?;
                let cells = within(&next()?, CELL)?;
                tell(Command::ResizePane {
                    target: p,
                    by: PaneResize::Border(way, cells),
                })
            }
            // A pane and a way.
            "move-beside" | "swap-toward" => {
                let (target, way) = (pane(&next()?)?, direction(&next()?)?);
                tell(match name {
                    "move-beside" => Command::MovePane {
                        target,
                        to: MoveTo::Beside(way),
                    },
                    _ => Command::SwapPane {
                        target,
                        with: SwapWith::Toward(way),
                    },
                })
            }
            "move-to" => {
                let p = pane(&next()?)?;
                let to = match next()? {
                    Value::App(Fux::Tab(t)) => MoveTo::Tab(t),
                    Value::App(Fux::Workspace(w)) => MoveTo::Workspace(WsRef::Id(w)),
                    Value::Str(s) if **s == *"new-tab" => MoveTo::NewTab,
                    Value::Str(s) if **s == *"new-workspace" => MoveTo::NewWorkspace,
                    other @ (Value::Int(_)
                    | Value::Bool(_)
                    | Value::Str(_)
                    | Value::List(_)
                    | Value::App(_)) => {
                        return Err(wrong(
                            "a tab, a workspace, new-tab or new-workspace",
                            &other,
                        ));
                    }
                };
                tell(Command::MovePane { target: p, to })
            }
            "swap-panes" => {
                let (a, b) = (pane(&next()?)?, pane(&next()?)?);
                tell(Command::SwapPane {
                    target: a,
                    with: SwapWith::Pane(b),
                })
            }
            "reorder" => {
                let t = target(&next()?)?;
                let siblings = [("next", Sibling::Next), ("previous", Sibling::Previous)];
                let toward = one_of(&next()?, &siblings)?;
                tell(Command::Reorder { target: t, toward })
            }
            "start-log" | "stop-log" => {
                let target = pane(&next()?)?;
                let file = match name {
                    "start-log" => Some(string(&next()?)?),
                    _ => None,
                };
                tell(Command::Log { target, file })
            }
            "clear-history" => tell(Command::ClearHistory {
                target: pane(&next()?)?,
            }),
            "buffers" => later(ask(Command::ListBuffers, Then::Text)),
            "buffer" => later(ask(
                Command::ShowBuffer {
                    index: within(&next()?, COUNT)?,
                },
                Then::Text,
            )),
            "paste-buffer" => {
                let p = pane(&next()?)?;
                let index = within(&next()?, COUNT)?;
                tell(Command::PasteBuffer { index, target: p })
            }
            // ----------------------------------------------------- clients
            "attach" | "watch" | "attach-to" => {
                let workspace = match name {
                    "attach-to" => Some(workspace(&next()?)?),
                    _ => None,
                };
                let read_only = name == "watch";
                local(Local::Attach {
                    workspace,
                    read_only,
                    size: None,
                })
            }
            "headless" => {
                let size = laid_out(&next()?, &next()?, u16::MAX)?;
                local(Local::Attach {
                    workspace: None,
                    read_only: false,
                    size: Some(size),
                })
            }
            "focus" => {
                let c = client(&next()?)?;
                let ways = Direction::ALL.map(|way| (way.name(), PanePick::Toward(way)));
                let steps = [
                    ("next", PanePick::Step(Sibling::Next)),
                    ("previous", PanePick::Step(Sibling::Previous)),
                    ("last", PanePick::Last),
                ];
                let pick = one_of(&next()?, &[ways.as_slice(), &steps].concat())?;
                later(act(c, ClientAction::SelectPane(pick)))
            }
            "focus-pane" => {
                let c = client(&next()?)?;
                let p = pane(&next()?)?;
                later(act(c, ClientAction::SelectPane(PanePick::Id(p))))
            }
            "show-tab" => {
                let c = client(&next()?)?;
                let t = next()?;
                let Value::App(Fux::Tab(tab)) = &t else {
                    return Err(wrong("a tab", &t));
                };
                later(act(c, ClientAction::SelectTab(Pick::Id(*tab))))
            }
            "show-workspace" => {
                let c = client(&next()?)?;
                let w = workspace(&next()?)?;
                later(act(c, ClientAction::SelectWorkspace(Pick::Id(w))))
            }
            "next-tab" | "previous-tab" | "next-workspace" | "previous-workspace" | "zoom"
            | "detach" => {
                let c = client(&next()?)?;
                let step = match name.starts_with("next") {
                    true => Sibling::Next,
                    false => Sibling::Previous,
                };
                let action = match name {
                    "zoom" => ClientAction::Zoom,
                    "detach" => ClientAction::Detach,
                    "next-tab" | "previous-tab" => ClientAction::SelectTab(Pick::Step(step)),
                    _ => ClientAction::SelectWorkspace(Pick::Step(step)),
                };
                later(act(c, action))
            }
            "capture-client" => {
                let c = client(&next()?)?;
                later(ask_as(
                    c,
                    Command::Client {
                        client: Some(c),
                        action: ClientAction::Capture,
                    },
                    Then::Answer(Said::Capture),
                ))
            }
            // ------------------------------------------------------ server
            "server" => local(Local::Server(crate::socket::Choice::Name(
                string(&next()?)?,
            ))),
            "socket" => local(Local::Server(crate::socket::Choice::Path(
                string(&next()?)?,
            ))),
            "start" => local(Local::Start),
            "status" => local(Local::Status),
            "servers" => local(Local::Servers),
            "kill-server" => tell(Command::KillServer { force: false }),
            "force-kill-server" => local(Local::ForceKill),
            "set" => {
                let option = string(&next()?)?;
                let value = text(&next()?)?;
                tell(Command::Set { option, value })
            }
            "grant" => {
                let workspace = workspace(&next()?)?;
                let socket = string(&next()?)?;
                later(ask(Request::Grant(workspace, socket), Then::Text))
            }
            "revoke" => tell(Request::Revoke(string(&next()?)?)),
            "version" => now(out, Value::str(env!("CARGO_PKG_VERSION"))),
            // ------------------------------------------------------ config
            "prefix" | "bind" | "bind-root" | "unbind" | "unbind-root" | "unbind-all" | "label"
            | "group" | "repeat" => self.configure(name, &mut next),
            // A client's own; else, from the command line, what the config
            // makes.
            "list-keys" => {
                let text = match &self.keymap {
                    Some(keymap) => keymap.describe(),
                    None => (super::config_keymap(None))
                        .map_err(|e| fail(Code::Config, e.to_string()))?
                        .describe(),
                };
                self.show(&text)
            }
            "key-names" => self.show(&format!("{}\n", crate::keys::all_names().join(" "))),
            // -------------------------------------------------------- help
            "help" => self.show(&help::overview()),
            "help-agents" => self.show(help::agents_first()),
            "help-agents-on" => {
                let topic = string(&next()?)?;
                let text = help::agents_topic(&topic).ok_or_else(|| {
                    let topics = help::agents_topics().join(" ");
                    fail(
                        Code::BadValue,
                        format!("no topic {topic:?}: {topics}, or all"),
                    )
                })?;
                self.show(&text)
            }
            "help-json" => self.show(help::JSON),
            "explain" => {
                let word = string(&next()?)?;
                let text = help::explain(&word)
                    .ok_or_else(|| fail(Lang::UnknownWord, format!("no word {word:?}")))?;
                self.show(&text)
            }
            "check" => {
                let path = string(&next()?)?;
                // Read as running it reads it, and refused alike.
                let text = super::read_file(path.as_ref(), super::SOURCE_MAX)
                    .map_err(|f| fail(Code::Read, f.message))?;
                // Its mistake, with where it is, as the file's own.
                let checked = |failed: super::Failed| fail(Code::Check, failed.message);
                let tokens = fuxth::token::source(&path, &text)
                    .map_err(|e| checked(super::Failed::from(e)))?;
                super::compile(&tokens).map_err(checked)?;
                // A config is run dry too: its bindings made, its settings
                // checked, nothing sent.
                let config = std::path::Path::new(&path)
                    .file_name()
                    .is_some_and(|n| n == "config.fux");
                if config {
                    super::run_file(std::path::Path::new(&path), None).map_err(checked)?;
                }
                self.show(&format!("{path}: ok\n"))
            }
            other => Err(fail(Lang::UnknownWord, format!("{other}: no such word"))),
        }
    }
}

impl Host<'_> {
    /// Writes `text` out.
    fn show(&mut self, text: &str) -> Result<Done<Ask>, Failure> {
        write!(self.out, "{text}").map_err(|e| fail(Code::Output, e.to_string()))?;
        Ok(Done::Now)
    }

    /// A config's word: it changes the client's keymap.
    fn configure(
        &mut self,
        name: &str,
        next: &mut dyn FnMut() -> Result<Value<Fux>, Failure>,
    ) -> Result<Done<Ask>, Failure> {
        let Some(keymap) = self.keymap.as_deref_mut() else {
            return Err(fail(
                Code::NotConfig,
                "changes a client's keys: in its config, or typed at its prompt",
            ));
        };
        let refused = |why: String| fail(Code::BadBinding, why);
        match name {
            "prefix" => {
                let key = crate::keymap::key(&string(&next()?)?).map_err(refused)?;
                keymap.set_prefix(key).map_err(refused)?;
            }
            "bind" | "bind-root" => {
                let keys_text = string(&next()?)?;
                let code = string(&next()?)?;
                // Checked now, rather than each time its keys are typed:
                // with the config's definitions, its mistake said as the
                // binding's.
                let mistake = |e: fuxth::Error| {
                    let mut failed =
                        refused(format!("{}, in the binding for {keys_text}", e.message));
                    failed.word = e.word;
                    failed
                };
                let tokens = fuxth::token::source("binding", &code).map_err(mistake)?;
                super::compiled(keymap.config.as_ref(), &tokens).map_err(mistake)?;
                let binding = crate::keymap::Binding::new(code, name == "bind-root");
                let keys = crate::keymap::keys(&keys_text).map_err(refused)?;
                match (name, keys.as_slice()) {
                    ("bind-root", [key]) => {
                        keymap.bind_root(*key, binding).map_err(refused)?;
                        self.last_bound = Some(Bound::Root(*key));
                    }
                    ("bind-root", _) => return Err(refused("takes one key".to_owned())),
                    _ => {
                        keymap.bind(&keys, binding, false).map_err(refused)?;
                        self.last_bound = Some(Bound::Prefix(keys));
                    }
                }
            }
            "unbind" => {
                let keys = crate::keymap::keys(&string(&next()?)?).map_err(refused)?;
                keymap.unbind(&keys).map_err(refused)?;
            }
            "unbind-root" => {
                let key = crate::keymap::key(&string(&next()?)?).map_err(refused)?;
                keymap.unbind_root(key).map_err(refused)?;
            }
            "unbind-all" => keymap.unbind_all(),
            _ => {
                // label, group, repeat: the binding just made.
                let Some(bound) = self.last_bound.clone() else {
                    return Err(refused(
                        "changes the binding just made: bind first".to_owned(),
                    ));
                };
                let given = match name {
                    "repeat" => None,
                    "label" => Some(text(&next()?)?),
                    _ => Some(string(&next()?)?),
                };
                let binding = match &bound {
                    Bound::Root(key) => (keymap.root.iter_mut())
                        .find(|(k, _)| k == key)
                        .map(|(_, b)| b),
                    Bound::Prefix(keys) => keymap.binding_mut(keys),
                };
                let Some(binding) = binding else {
                    return Err(refused("the binding just made is gone".to_owned()));
                };
                match (name, given) {
                    ("label", Some(text)) => binding.label = text,
                    ("group", Some(text)) => binding.group = text,
                    _ => match &bound {
                        Bound::Root(_) => {
                            return Err(refused(
                                "a binding without the prefix does not repeat".to_owned(),
                            ));
                        }
                        Bound::Prefix(keys) => keymap.set_repeat(keys).map_err(refused)?,
                    },
                }
            }
        }
        Ok(Done::Now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No two words share a name, and none is a word of the language.
    #[test]
    fn every_word_has_its_own_name() {
        let mut names: Vec<&str> = WORDS.iter().map(|w| w.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "a name given twice");
        for name in names {
            assert!(fuxth::Parsing::named(name).is_none(), "{name}");
            assert!(
                fuxth::Builtin::ALL.iter().all(|b| b.name() != name),
                "{name}"
            );
            assert!(
                !name.chars().any(|c| ";|&<>()$`*?[{!~#'\"\\".contains(c)),
                "{name}: a character a shell treats specially"
            );
        }
    }

    /// `signal`'s help names every signal it takes.
    #[test]
    fn every_signal_is_told_of() {
        let signal = WORDS.iter().find(|w| w.name == "signal");
        let (named, _) = signal
            .and_then(|w| w.about.split_once(','))
            .unwrap_or_default();
        let mut named: Vec<&str> = named.split_whitespace().collect();
        let mut all = fuxix::process::Signal::ALL
            .map(fuxix::process::Signal::name)
            .to_vec();
        named.sort_unstable();
        all.sort_unstable();
        assert_eq!(named, all);
    }

    /// `set`'s help and the README's table name every setting, in order.
    #[test]
    fn every_setting_is_told_of() {
        let set = WORDS.iter().find(|w| w.name == "set");
        let help: Vec<&str> =
            (set.map(|w| w.about.split_whitespace().collect())).unwrap_or_default();
        assert_eq!(help, crate::config::NAMES);
        let readme = include_str!("../../README.md");
        for name in crate::config::NAMES {
            assert!(readme.contains(&format!("\n| `{name}` |")), "{name}");
        }
    }

    /// Each word's effect is in the names of fux's kinds, strictly: the
    /// vocabulary is made, no name in any effect unknown.
    #[test]
    fn every_effect_names_known_kinds() {
        let made = super::super::vocabulary().map(drop);
        assert_eq!(made, Ok(()));
    }

    #[test]
    fn an_answer_of_several_panes_is_an_array() {
        let answer = |json: &str| {
            json_of(&Value::App(Fux::Answer(Box::new(super::super::Answer {
                text: String::new(),
                json: Some(json.to_owned()),
                said: super::super::Said::Panes,
            }))))
            .render()
        };
        // However many answered: one is an array too.
        assert_eq!(answer("{\"pane\":\"%1\"}\n"), "[{\"pane\":\"%1\"}]");
        assert_eq!(
            answer("{\"pane\":\"%1\"}\n{\"pane\":\"%2\"}\n"),
            "[{\"pane\":\"%1\"},{\"pane\":\"%2\"}]"
        );
    }

    /// A request costs the steps the program took to make it, not a whole
    /// slice of the cap: 200 settings are far under `PROGRAM_STEPS`, and a
    /// loop still meets the cap.
    #[test]
    fn a_request_costs_its_own_steps_against_the_cap() {
        let run = |config: &str| {
            super::super::run_quiet(
                "config",
                config,
                crate::client::context(),
                None,
                &mut super::super::Settings(None),
            )
            .map_err(|failed| failed.code)
        };
        let settings: String = std::iter::repeat_n("str clipboard str off set\n", 200).collect();
        assert_eq!(run(&settings), Ok(()));
        let forever = "begin false until";
        assert_eq!(run(forever), Err(Code::Steps));
    }

    #[test]
    fn a_default_capture_view_is_plain() {
        assert_eq!(crate::command::CaptureView::default(), Opts::default().view);
    }
}
