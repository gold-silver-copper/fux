//! fux's words: each a name, how many values it takes, what it does to the
//! stack, and a line of help; and what each does, run by the program's
//! [`Host`].
use super::{Ask, Bound, Fux, Host, Keys, Local, MouseEvent, Opts, Said, Then, Waits};
use crate::code::Code;
use crate::command::{
    AnyRef, ClientAction, Command, Extent, Input, Kind, Mouse, MoveTo, PanePick, PaneResize,
    Percent, Pick, Resized, Sibling, Source, SwapWith, Until, Wait, WsRef,
};
use crate::keys::Direction;
use crate::keys::mouse::MouseButton;
use crate::layout::Axis;
use fuxth::{Done, Failure, Value};
use std::time::Duration;

/// A word of fux's vocabulary.
pub struct Word {
    pub name: &'static str,
    pub takes: usize,
    /// What it does to the stack: `pane opts -- pane`.
    pub effect: &'static str,
    pub about: &'static str,
    /// Its heading in `help`.
    pub group: &'static str,
}

impl Word {
    /// How many values it gives, read from its effect: each word after the
    /// `--`, one in brackets given only sometimes.
    pub fn gives(&self) -> std::ops::RangeInclusive<usize> {
        let after = self.effect.split_once("--").map_or("", |(_, after)| after);
        let words = after.split_whitespace();
        let sometimes = words.clone().filter(|w| w.starts_with('[')).count();
        let all = words.count();
        all.saturating_sub(sometimes)..=all
    }
}

const fn word(
    group: &'static str,
    name: &'static str,
    takes: usize,
    effect: &'static str,
    about: &'static str,
) -> Word {
    Word {
        name,
        takes,
        effect,
        about,
        group,
    }
}

/// Every word of fux's, under its heading, in help's order.
#[rustfmt::skip]
pub const WORDS: &[Word] = &[
    // Finding
    word("Finding", "pane", 1, "x -- pane", "the pane x names: a tab's name (its one pane), %N, a pane, or a tab of one pane"),
    word("Finding", "tab", 1, "x -- tab", "the tab x names: its name, @N, or a pane's tab"),
    word("Finding", "workspace", 1, "x -- workspace", "the workspace x names: its name, +N, or a pane's or tab's workspace"),
    word("Finding", "here", 0, "-- pane", "the pane this runs in (FUX_PANE)"),
    word("Finding", "client", 1, "name -- client", "a client by its id, c2"),
    word("Finding", "me", 0, "-- client", "the client whose key or prompt runs this"),
    word("Finding", "focused", 1, "client -- pane", "the pane a client focuses"),
    // Starting
    word("Starting", "opts", 0, "-- opts", "empty options, for run, split, new-tab, new-workspace, respawn, wait and capture"),
    word("Starting", "name", 2, "opts name -- opts", "the tab's or workspace's name"),
    word("Starting", "size", 3, "opts cols rows -- opts", "what its tab is laid out on while no client shows it"),
    word("Starting", "cwd", 2, "opts dir -- opts", "where it starts"),
    word("Starting", "env", 3, "opts key value -- opts", "a variable for it"),
    word("Starting", "program", 2, "opts path -- opts", "the program to start, in place of a shell"),
    word("Starting", "arg", 2, "opts word -- opts", "an argument for the program"),
    word("Starting", "command", 2, "opts line -- opts", "the program and its arguments, one line split as a shell splits it"),
    word("Starting", "typed", 2, "opts line -- opts", "a command line typed into its new shell"),
    word("Starting", "horizontal", 1, "opts -- opts", "split side by side"),
    word("Starting", "vertical", 1, "opts -- opts", "split stacked"),
    word("Starting", "in", 2, "opts workspace -- opts", "the workspace for run or new-tab (a name not there is made)"),
    word("Starting", "id", 2, "opts id -- opts", "the id to make it with (%N, @N, +N): above every id of its kind given, and at most 65536 past the last"),
    word("Starting", "log-to", 2, "opts file -- opts", "keep everything the program writes in a file too"),
    word("Starting", "kill-first", 1, "opts -- opts", "respawn: end the program first if it still runs"),
    word("Starting", "run", 1, "opts -- pane", "start a program in a tab of its own, in workspace agent; its pane stays once it exits"),
    word("Starting", "split", 2, "pane opts -- pane", "split a pane: a shell, or the program given"),
    word("Starting", "new-tab", 1, "opts -- pane", "a tab with a shell"),
    word("Starting", "new-workspace", 1, "opts -- pane", "a workspace with a tab and a shell"),
    word("Starting", "respawn", 2, "pane opts -- pane", "start again in its pane the program run started"),
    // Waiting
    word("Waiting", "wait-text", 2, "x text -- x", "wait until text shows anew, not on the line typed (options, keys or a mouse event)"),
    word("Waiting", "wait-gone", 2, "x text -- x", "wait until text is no longer shown; after run or respawn, once it has been"),
    word("Waiting", "wait-exit", 1, "x -- x", "wait until the program exits"),
    word("Waiting", "wait-bell", 1, "x -- x", "wait until the program rings or notifies"),
    word("Waiting", "timeout", 2, "x secs -- x", "how long to wait at most, else the server's wait-seconds (30); 0 answers at once"),
    word("Waiting", "wait", 2, "x opts -- answer", "wait for the pane to settle, or as the options say, in a pane, or each pane of a tab or workspace"),
    // Input
    word("Input", "keys", 0, "-- keys", "empty input, for send-keys"),
    word("Input", "text", 2, "keys text -- keys", "text, typed"),
    word("Input", "key", 2, "keys name -- keys", "a key by its name: Enter, C-c, Up, F5"),
    word("Input", "paste", 2, "keys text -- keys", "text as a paste, bracketed if the program turned that on (an end mark, ESC [201~, in it then dropped); else as it is"),
    word("Input", "paste-file", 2, "keys file -- keys", "a file's text as a paste, at most 900 KiB, bracketed if the program turned that on (an end mark in it then dropped)"),
    word("Input", "times", 2, "keys n -- keys", "the input just before it, n times (1 to 1000): str Down key 2 times"),
    word("Input", "send-keys", 2, "target keys -- [outcome]", "send to a pane, every pane of a tab, or a client as typed at its terminal; 900 KiB at most in all"),
    word("Input", "mouse", 3, "action row col -- mouse", "click right middle press release wheel-up wheel-down or move, at a cell from 0"),
    word("Input", "send-mouse", 2, "pane mouse -- [outcome]", "send a mouse event"),
    word("Input", "signal", 2, "pane name -- [answer]", "TERM KILL INT HUP QUIT USR1 USR2 STOP CONT TSTP WINCH ILL ABRT FPE SEGV PIPE ALRM, to the pane's foreground; answers once it exits or settles"),
    // Reading
    word("Reading", "capture", 2, "pane opts -- capture", "the screen, as the options say"),
    word("Reading", "history", 2, "opts lines -- opts", "capture: lines of history first"),
    word("Reading", "all-history", 1, "opts -- opts", "capture: all the history first"),
    word("Reading", "joined", 1, "opts -- opts", "capture: wrapped rows joined into their lines"),
    word("Reading", "full", 1, "opts -- opts", "capture: every row and cell, untrimmed"),
    word("Reading", "styles", 1, "opts -- opts", "capture: colours and attributes"),
    word("Reading", "marks", 1, "x -- x", "highlighted text as ⟦this⟧: in a capture, or a wait's screen (options, keys or a mouse event)"),
    word("Reading", "main-screen", 1, "opts -- opts", "capture: what was printed before a program took the screen"),
    word("Reading", "alt-screen", 1, "opts -- opts", "capture: the full screen a program drew last"),
    word("Reading", "last-output", 1, "opts -- opts", "capture: the last shell command's output, or the running one's so far (a full-screen one's: its screen to the cursor); one of main-screen, alt-screen and last-output at most"),
    word("Reading", "rows", 3, "opts from to -- opts", "capture: those rows only, from 0; joined, each line they touch whole"),
    word("Reading", "cols", 3, "opts from to -- opts", "capture: those columns only, from 0"),
    word("Reading", "ls", 0, "-- listing", "workspaces, tabs, panes, clients and grants"),
    word("Reading", "info", 1, "handle -- record", "a pane, tab or workspace, all of it"),
    word("Reading", "get", 2, "handle field -- value", "one field of a pane, tab or workspace: exited, revision, cwd"),
    word("Reading", "json", 1, "x -- text", "x as JSON; among a capture's options, the capture as JSON"),
    word("Reading", "events", 0, "-- ", "what happens, a line each, as it happens"),
    word("Reading", "events-json", 0, "-- ", "what happens, as JSON lines"),
    word("Reading", "events-of", 1, "handle -- ", "what happens in a pane, tab or workspace"),
    word("Reading", "events-of-json", 1, "handle -- ", "the same, as JSON lines"),
    word("Reading", "dump", 0, "-- text", "everything, as a program that makes it again (shells new, nothing typed into them)"),
    word("Reading", "dump-of", 1, "handle -- text", "a tab or workspace, as a program that makes it again (shells new, nothing typed into them)"),
    // Changing
    word("Changing", "kill", 1, "handle -- ", "close a pane, tab or workspace, without asking"),
    word("Changing", "rename", 2, "handle name -- ", "rename a pane, tab or workspace"),
    word("Changing", "notify", 2, "handle text -- ", "a notice on the clients showing it"),
    word("Changing", "notify-all", 1, "text -- ", "a notice on every client"),
    word("Changing", "resize", 3, "handle cols rows -- ", "what a tab (or a pane's) is laid out on while no client shows it"),
    word("Changing", "resize-auto", 1, "handle -- ", "a tab laid out on a client's screen again"),
    word("Changing", "resize-x", 2, "pane width -- ", "a pane's width: cells, or a percent"),
    word("Changing", "resize-y", 2, "pane height -- ", "a pane's height: cells, or a percent"),
    word("Changing", "percent", 1, "n -- size", "n percent of the tab, for resize-x and resize-y"),
    word("Changing", "move-border", 3, "pane way cells -- ", "move the pane's nearest border: left right up down"),
    word("Changing", "move-to", 2, "pane where -- ", "move a pane to a tab, a workspace, new-tab or new-workspace"),
    word("Changing", "move-beside", 2, "pane way -- ", "move a pane beside its neighbour: left right up down"),
    word("Changing", "swap-panes", 2, "pane pane -- ", "swap two panes"),
    word("Changing", "swap-toward", 2, "pane way -- ", "swap a pane with its neighbour"),
    word("Changing", "reorder", 2, "handle way -- ", "move one place: next or previous"),
    word("Changing", "start-log", 2, "pane file -- ", "keep what the pane's program writes from now on"),
    word("Changing", "stop-log", 1, "pane -- ", "stop keeping it"),
    word("Changing", "clear-history", 1, "pane -- ", "drop the pane's history"),
    word("Changing", "buffers", 0, "-- listing", "the paste buffers, newest 0"),
    word("Changing", "buffer", 1, "n -- text", "paste buffer n"),
    word("Changing", "paste-buffer", 2, "pane n -- ", "paste buffer n into a pane"),
    // Clients
    word("Clients", "attach", 0, "-- ", "show the server in this terminal"),
    word("Clients", "attach-to", 1, "workspace -- ", "the same, on a workspace"),
    word("Clients", "watch", 0, "-- ", "show it, keys doing nothing but the prefix then d"),
    word("Clients", "headless", 2, "cols rows -- ", "a client with no terminal, until signalled; prints its id"),
    word("Clients", "focus", 2, "client way -- ", "left right up down next previous or last pane"),
    word("Clients", "focus-pane", 2, "client pane -- ", "focus a pane"),
    word("Clients", "show-tab", 2, "client tab -- ", "show a tab"),
    word("Clients", "next-tab", 1, "client -- ", "the next tab"),
    word("Clients", "previous-tab", 1, "client -- ", "the previous tab"),
    word("Clients", "show-workspace", 2, "client workspace -- ", "show a workspace"),
    word("Clients", "next-workspace", 1, "client -- ", "the next workspace"),
    word("Clients", "previous-workspace", 1, "client -- ", "the previous workspace"),
    word("Clients", "choose-tab", 1, "client -- ", "a list of tabs to go to"),
    word("Clients", "choose-workspace", 1, "client -- ", "a list of workspaces to go to"),
    word("Clients", "choose-tab-for", 2, "client pane -- ", "a list of tabs to move the pane to"),
    word("Clients", "choose-workspace-for", 2, "client pane -- ", "a list of workspaces to move the pane to"),
    word("Clients", "choose-pane", 2, "client pane -- ", "a list of panes to swap the pane with"),
    word("Clients", "menu", 2, "client handle -- ", "actions for a pane, tab or workspace"),
    word("Clients", "rename-prompt", 2, "client handle -- ", "a prompt for its new name"),
    word("Clients", "confirm-close", 2, "client handle -- ", "close it, once confirmed"),
    word("Clients", "zoom", 1, "client -- ", "zoom the focused pane, or restore"),
    word("Clients", "copy-mode", 1, "client -- ", "copy and select in the focused pane"),
    word("Clients", "command-prompt", 1, "client -- ", "a prompt for fuxth"),
    word("Clients", "detach", 1, "client -- ", "detach a client"),
    word("Clients", "capture-client", 1, "client -- capture", "what a client's terminal shows"),
    word("Clients", "resize-client", 3, "client cols rows -- ", "a headless client's size"),
    // Server
    word("Server", "server", 1, "name -- ", "the server named so (NAME.sock in fux's directory), for what follows"),
    word("Server", "socket", 1, "path -- ", "the server on this socket, for what follows"),
    word("Server", "start", 0, "-- ", "start the server in the background, if none answers"),
    word("Server", "status", 0, "-- record", "whether a server answers, and its version"),
    word("Server", "servers", 0, "-- listing", "the servers that answer in fux's directory"),
    word("Server", "kill-server", 0, "-- ", "stop the server: not from its own panes, nor with people attached"),
    word("Server", "force-kill-server", 0, "-- ", "stop it anyway, whatever fux version it is"),
    word("Server", "set", 2, "setting value -- ", "shell history-bytes wait-seconds journal-bytes journal-total clipboard buffers bell titles shell-marks update-environment"),
    word("Server", "grant", 2, "workspace socket -- path", "a socket whose requests reach that workspace alone; its path"),
    word("Server", "revoke", 1, "socket -- ", "close a grant's socket"),
    word("Server", "version", 0, "-- text", "this fux's version"),
    // Config
    word("Config", "prefix", 1, "key -- ", "this client's prefix (config)"),
    word("Config", "bind", 2, "keys code -- ", "keys after the prefix run fuxth (config)"),
    word("Config", "bind-root", 2, "key code -- ", "a key without the prefix runs fuxth (config)"),
    word("Config", "label", 1, "text -- ", "what the column says the last binding does"),
    word("Config", "group", 1, "name -- ", "the column's heading for the last binding"),
    word("Config", "repeat", 0, "-- ", "the last binding's layer repeats until Esc"),
    word("Config", "unbind", 1, "keys -- ", "keys after the prefix run nothing"),
    word("Config", "unbind-root", 1, "key -- ", "a key without the prefix runs nothing"),
    word("Config", "unbind-all", 0, "-- ", "no bindings at all"),
    word("Config", "list-keys", 0, "-- ", "this client's bindings"),
    word("Config", "key-names", 0, "-- ", "the names keys go by"),
    // Help
    word("Help", "help", 0, "-- ", "the words, under their headings"),
    word("Help", "help-agents", 0, "-- ", "how to drive programs from a script or an agent"),
    word("Help", "help-json", 0, "-- ", "what json gives"),
    word("Help", "explain", 1, "word -- ", "one word at length"),
    word("Help", "check", 1, "file -- ", "read a fuxth file without running it: its mistakes; a config.fux is run dry too"),
];

/// A failure of a word's own: its code and why.
fn fail(code: Code, message: impl Into<String>) -> Failure {
    Failure::new(code.into(), message)
}

/// How a word of fux's is used, for a mistake in using it: what goes
/// before it, what it leaves, and an example.
pub fn usage(word: &str) -> Option<String> {
    let library = || {
        let mut library = crate::help::library().into_iter();
        library
            .find(|(name, ..)| *name == word)
            .map(|(_, effect, ..)| effect)
    };
    let core = || fuxth::Builtin::named(word).map(fuxth::Builtin::effect);
    // Fux's, the prelude's or library's, or the core's; else the
    // language's own syntax, said as it is written.
    let effect = match WORDS.iter().find(|w| w.name == word) {
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
    let takes = takes.trim().to_uppercase();
    let leaves = match leaves.trim() {
        "" => String::new(),
        leaves => format!(", which leaves {leaves}"),
    };
    let example = crate::help::example(word).map_or_else(String::new, |e| format!("; e.g. {e}"));
    Some(format!("use: {takes} {word}{leaves}{example}"))
}

fn wrong(word: &str, wanted: &str, got: &Value<Fux>) -> Failure {
    fail(
        Code::Type,
        format!("{word} takes {wanted}, not {}", got.kind()),
    )
}

fn string(word: &str, value: &Value<Fux>) -> Result<String, Failure> {
    let Value::Str(s) = value else {
        return Err(wrong(word, "a string", value));
    };
    Ok(s.to_string())
}

/// A value as words that take text read it: a string, or a number.
fn text(word: &str, value: &Value<Fux>) -> Result<String, Failure> {
    match value {
        Value::Str(s) => Ok(s.to_string()),
        Value::Int(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        other @ (Value::List(_) | Value::App(_)) => {
            Err(wrong(word, "a string, a number or a boolean", other))
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
        // An answer of several panes, a line of JSON each, as an array.
        Value::App(Fux::Answer(answer)) => match &answer.json {
            Some(json) => {
                let mut objects: Vec<Json<'static>> = (json.lines())
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| Json::Raw(line.to_owned().into()))
                    .collect();
                match objects.len() {
                    1 => objects.pop().unwrap_or(Json::Null),
                    _ => Json::Array(objects),
                }
            }
            None => Json::str(answer.text.clone()),
        },
        Value::App(_) => Json::str(value.text()),
    }
}

fn int(word: &str, value: &Value<Fux>) -> Result<i64, Failure> {
    let Value::Int(n) = value else {
        return Err(wrong(word, "an integer", value));
    };
    Ok(*n)
}

/// What a count or a cell can be: from 0 to the most it holds.
trait Count: TryFrom<i64> {
    const MOST: i64;
}

impl Count for u16 {
    const MOST: i64 = u16::MAX as i64;
}

impl Count for u64 {
    const MOST: i64 = i64::MAX;
}

impl Count for usize {
    const MOST: i64 = i64::MAX;
}

/// A count or a cell: an integer from 0 that fits.
fn count<T: Count>(word: &str, value: &Value<Fux>) -> Result<T, Failure> {
    let n = int(word, value)?;
    T::try_from(n).map_err(|_| match T::MOST {
        i64::MAX => fail(Code::Range, format!("{n} is out of range: from 0")),
        most => fail(Code::Range, format!("{n} is out of range: 0 to {most}")),
    })
}

/// A size to lay a tab out on: columns and rows each 1 to 500.
fn laid_out(word: &str, cols: &Value<Fux>, rows: &Value<Fux>) -> Result<fux_vt::Size, Failure> {
    let (cols, rows) = (int(word, cols)?, int(word, rows)?);
    let cell = |n: i64| u16::try_from(n).ok().filter(|n| (1..=500).contains(n));
    cell(cols)
        .zip(cell(rows))
        .and_then(|(cols, rows)| fux_vt::Size::new(rows, cols).ok())
        .ok_or_else(|| {
            fail(
                Code::Range,
                format!("size {cols} {rows}: columns and rows each 1 to 500"),
            )
        })
}

/// The pane a value is: a pane, or an action's answer of one.
fn as_pane(value: &Value<Fux>) -> Option<crate::id::PaneId> {
    if let Value::App(Fux::Answer(answer)) = value {
        return answer.said.pane();
    }
    let Value::App(Fux::Pane(p)) = value else {
        return None;
    };
    Some(*p)
}

fn pane(word: &str, value: &Value<Fux>) -> Result<crate::id::PaneId, Failure> {
    as_pane(value).ok_or_else(|| wrong(word, "a pane (from pane, run or split)", value))
}

fn client(word: &str, value: &Value<Fux>) -> Result<crate::id::ClientId, Failure> {
    let Value::App(Fux::Client(c)) = value else {
        return Err(wrong(word, "a client (from me or client)", value));
    };
    Ok(*c)
}

/// A pane, tab or workspace.
fn target(word: &str, value: &Value<Fux>) -> Result<AnyRef, Failure> {
    if let Some(p) = as_pane(value) {
        Ok(AnyRef::Pane(p))
    } else if let Value::App(Fux::Tab(t)) = value {
        Ok(AnyRef::Tab(*t))
    } else if let Value::App(Fux::Workspace(w)) = value {
        Ok(AnyRef::Workspace(*w))
    } else {
        Err(wrong(word, "a pane, tab or workspace", value))
    }
}

/// A workspace, or its name: `+N` read as its id, as `parse_workspace` reads it.
fn workspace(word: &str, value: &Value<Fux>) -> Result<WsRef, Failure> {
    if let Value::App(Fux::Workspace(w)) = value {
        return Ok(WsRef::Id(*w));
    }
    crate::command::parse_workspace(&string(word, value)?)
        .map_err(|e| fail(Code::BadValue, e.to_string()))
}

fn direction(word: &str, value: &Value<Fux>) -> Result<Direction, Failure> {
    match string(word, value)?.as_str() {
        "left" => Ok(Direction::Left),
        "right" => Ok(Direction::Right),
        "up" => Ok(Direction::Up),
        "down" => Ok(Direction::Down),
        other => Err(fail(
            Code::BadValue,
            format!("{other:?} is not left, right, up or down"),
        )),
    }
}

fn sibling(word: &str, value: &Value<Fux>) -> Result<Sibling, Failure> {
    match string(word, value)?.as_str() {
        "next" => Ok(Sibling::Next),
        "previous" => Ok(Sibling::Previous),
        other => Err(fail(
            Code::BadValue,
            format!("{other:?} is not next or previous"),
        )),
    }
}

/// Options a word would leave unused, refused rather than ignored: it
/// takes those `help` lists for it.
fn unused(word: &str, built: &Opts) -> Result<(), Failure> {
    let takes = |o: &str| crate::help::options(word).is_some_and(|mut t| t.any(|t| t == o));
    let Some(option) = built.given.iter().find(|o| !takes(o)) else {
        return Ok(());
    };
    let instead = match *option {
        "json" => format!("json goes after it: {word} json"),
        "wait-text" | "wait-gone" | "wait-exit" | "wait-bell" if word == "capture" => {
            "it reads once the pane settles (within timeout, wait-seconds if not said); \
             to wait for something first: PANE opts … wait, then PANE opts capture"
                .to_owned()
        }
        _ => {
            let on: Vec<&str> = (crate::help::OPTIONS.iter())
                .filter(|(_, takes)| takes.split_whitespace().any(|t| t == *option))
                .map(|(word, _)| *word)
                .collect();
            format!("{option} goes on {}", on.join(", "))
        }
    };
    Err(fail(
        Code::Unused,
        format!("{word} takes no {option}: {instead}"),
    ))
}

/// The options `value` holds, `word` among those given.
fn option<'a>(word: &'static str, value: &'a mut Value<Fux>) -> Result<&'a mut Opts, Failure> {
    let built = opts(word, value)?;
    built.given.push(word);
    Ok(built)
}

/// What a word given options, keys or a mouse event changes of them,
/// left on the stack.
fn with(
    word: &'static str,
    mut value: Value<Fux>,
    out: &mut Vec<Value<Fux>>,
    change: impl FnOnce(&mut Waits),
) -> Result<Done<Ask>, Failure> {
    if let Value::App(Fux::Opts(opts)) = &mut value {
        opts.given.push(word);
        change(&mut opts.waits);
    } else if let Value::App(Fux::Keys(keys)) = &mut value {
        change(&mut keys.waits);
    } else if let Value::App(Fux::Mouse(mouse)) = &mut value {
        change(&mut mouse.waits);
    } else {
        return Err(wrong(word, "options, keys or a mouse event", &value));
    }
    out.push(value);
    Ok(Done::Now)
}

fn opts<'a>(word: &str, value: &'a mut Value<Fux>) -> Result<&'a mut Opts, Failure> {
    let Value::App(Fux::Opts(opts)) = value else {
        return Err(wrong(word, "options (from opts)", value));
    };
    Ok(opts)
}

/// The options `value` holds, all made: refused if `word` leaves one unused.
fn finished(word: &str, mut value: Value<Fux>) -> Result<Opts, Failure> {
    let built = std::mem::take(opts(word, &mut value)?);
    unused(word, &built)?;
    Ok(built)
}

fn keys<'a>(word: &str, value: &'a mut Value<Fux>) -> Result<&'a mut Keys, Failure> {
    let Value::App(Fux::Keys(keys)) = value else {
        return Err(wrong(word, "keys (from keys)", value));
    };
    Ok(keys)
}

/// The most input one send-keys sends: a request holds it whole.
const INPUT_MAX: usize = 900 * 1024;

/// The wait an action makes, armed before it when `after`: what it names;
/// or, from a command line (`settle`) or given any wait word (`timeout`
/// alone), for each pane to settle.
fn wait(waits: &Waits, after: bool, settle: bool) -> Option<Box<Wait>> {
    let named = *waits != Waits::default();
    (named || settle).then(|| Box::new(waited(waits, after)))
}

/// The wait an input or a signal makes, armed before it, and the answer it
/// gives: whether it waited.
fn acted(waits: &Waits, settle: bool) -> (Option<Box<Wait>>, Then) {
    let then = wait(waits, true, settle);
    let waited = then.is_some();
    (then, Then::Outcome { waited })
}

/// A wait for what `waits` names, else for the pane to settle, said with
/// what came.
fn waited(waits: &Waits, after: bool) -> Wait {
    let settles = waits.until.is_empty();
    Wait {
        until: if settles {
            vec![Until::Settled]
        } else {
            waits.until.clone()
        },
        timeout: waits.timeout,
        print: waits.print || settles,
        after,
        marks: waits.marks,
    }
}

fn ask(command: Command, then: Then) -> Ask {
    Ask::Server {
        command,
        client: None,
        then,
    }
}

/// An order to the server that answers nothing.
fn tell(command: Command) -> Result<Done<Ask>, Failure> {
    Ok(Done::Later(ask(command, Then::Nothing)))
}

/// What this client does itself.
fn local(local: Local) -> Result<Done<Ask>, Failure> {
    Ok(Done::Later(Ask::Local(local)))
}

fn ask_as(client: crate::id::ClientId, command: Command, then: Then) -> Ask {
    Ask::Server {
        command,
        client: Some(client),
        then,
    }
}

fn act(client: crate::id::ClientId, action: ClientAction) -> Ask {
    ask_as(
        client,
        Command::Client {
            client: Some(client),
            action,
        },
        Then::Nothing,
    )
}

/// Whether `word` opens one of a client's own screens: a key's program
/// that may open one holds the keys typed after it for the screen.
pub fn opens(word: &str) -> bool {
    matches!(
        word,
        "menu"
            | "choose-tab"
            | "choose-workspace"
            | "choose-tab-for"
            | "choose-workspace-for"
            | "choose-pane"
            | "rename-prompt"
            | "confirm-close"
            | "command-prompt"
            | "copy-mode"
    )
}

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
            return Err(fail(Code::UnknownWord, "no such word"));
        };
        let name = spec.name;
        let mut args = args.into_iter();
        let mut next = || {
            args.next()
                .ok_or_else(|| fail(Code::StackUnderflow, format!("{name} takes {}", spec.takes)))
        };
        let now = |out: &mut Vec<Value<Fux>>, value: Value<Fux>| {
            out.push(value);
            Ok(Done::Now)
        };
        let later = |ask: Ask| Ok(Done::Later(ask));
        let resolve = |text: String, kind: Kind| {
            later(ask(Command::Resolve { text, kind }, Then::Handle(kind)))
        };
        match name {
            // ----------------------------------------------------- finding
            "pane" | "tab" | "workspace" => {
                let kind = match name {
                    "pane" => Kind::Pane,
                    "tab" => Kind::Tab,
                    _ => Kind::Workspace,
                };
                // An action's answer is its pane.
                let mut found = next()?;
                if let Value::App(Fux::Answer(answer)) = &found
                    && let Some(p) = answer.said.pane()
                {
                    found = Value::App(Fux::Pane(p));
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
                    Value::App(app @ (Fux::Pane(_) | Fux::Tab(_) | Fux::Workspace(_))) => {
                        match (kind, &app) {
                            (Kind::Pane, Fux::Pane(_))
                            | (Kind::Tab, Fux::Tab(_))
                            | (Kind::Workspace, Fux::Workspace(_)) => now(out, Value::App(app)),
                            _ => resolve(Value::App(app).text(), kind),
                        }
                    }
                    other @ (Value::Int(_) | Value::Bool(_) | Value::List(_) | Value::App(_)) => {
                        Err(wrong(name, "a name, or a pane, tab or workspace", &other))
                    }
                }
            }
            "here" => resolve(String::new(), Kind::Pane),
            "client" => {
                let text = string(name, &next()?)?;
                let id = text.parse().map_err(|_| {
                    fail(
                        Code::BadValue,
                        format!("{text:?} is not a client: clients are cN"),
                    )
                })?;
                now(out, Value::App(Fux::Client(id)))
            }
            "me" => match self.context.client {
                Some(c) => now(out, Value::App(Fux::Client(c))),
                None => Err(fail(
                    Code::NotAClient,
                    "me is the client whose key or prompt runs this: this runs from the command line",
                )),
            },
            "focused" => {
                let c = client(name, &next()?)?;
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
            "keys" => now(out, Value::App(Fux::Keys(Box::default()))),
            "command" => {
                let mut o = next()?;
                let line = string(name, &next()?)?;
                let argv = crate::words::split(&line).map_err(|e| fail(Code::OpenQuote, e))?;
                if argv.first().is_none_or(|p| p.is_empty()) {
                    return Err(fail(Code::NoProgram, "an empty command line"));
                }
                option(name, &mut o)?.argv = argv;
                now(out, o)
            }
            "name" | "cwd" | "program" | "arg" | "typed" | "in" | "id" | "log-to" | "history" => {
                let mut o = next()?;
                let v = next()?;
                let built = option(name, &mut o)?;
                match name {
                    "name" => built.name = Some(string(name, &v)?),
                    "cwd" => built.cwd = Some(string(name, &v)?),
                    "program" => match built.argv.first_mut() {
                        Some(first) => *first = string(name, &v)?,
                        None => built.argv.push(string(name, &v)?),
                    },
                    "arg" => {
                        if built.argv.is_empty() {
                            return Err(fail(Code::NoProgram, "arg comes after program"));
                        }
                        built.argv.push(text(name, &v)?);
                    }
                    "typed" => built.typed = Some(string(name, &v)?),
                    "in" => built.place = Some(workspace(name, &v)?),
                    "id" => {
                        let text = string(name, &v)?;
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
                    "log-to" => built.log = Some(string(name, &v)?),
                    _ => built.history = Some(count(name, &v)?),
                }
                now(out, o)
            }
            "size" => {
                let mut o = next()?;
                let size = laid_out(name, &next()?, &next()?)?;
                option(name, &mut o)?.size = Some(size);
                now(out, o)
            }
            "env" => {
                let mut o = next()?;
                let (key, value) = (string(name, &next()?)?, text(name, &next()?)?);
                if !crate::command::shell_name(&key) {
                    return Err(fail(
                        Code::BadValue,
                        format!(
                            "{key:?} is no variable's name (letters, digits and _, not first a digit)"
                        ),
                    ));
                }
                option(name, &mut o)?.env.push((key, value));
                now(out, o)
            }
            // On a capture's options, and on what a wait prints,
            // whether among options, keys or a mouse event.
            "marks" => {
                let mut x = next()?;
                if let Value::App(Fux::Opts(opts)) = &mut x {
                    opts.view.marks = true;
                }
                with(name, x, out, |w| w.marks = true)
            }
            "main-screen" | "alt-screen" | "last-output" => {
                let mut o = next()?;
                let view = &mut option(name, &mut o)?.view;
                if view.source != Source::Shown {
                    let why = format!("{name}: a capture reads one screen, and one is given");
                    return Err(fail(Code::Unused, why));
                }
                view.source = match name {
                    "main-screen" => Source::Main,
                    "alt-screen" => Source::Alternate,
                    _ => Source::LastOutput,
                };
                now(out, o)
            }
            "horizontal" | "vertical" | "kill-first" | "all-history" | "joined" | "full"
            | "styles" => {
                let mut o = next()?;
                let built = option(name, &mut o)?;
                match name {
                    "horizontal" => built.axis = Some(Axis::Horizontal),
                    "vertical" => built.axis = Some(Axis::Vertical),
                    "kill-first" => built.kill = true,
                    "all-history" => built.history = Some(usize::MAX),
                    "joined" => built.view.join = true,
                    "full" => built.view.full = true,
                    _ => built.view.styles = true,
                }
                now(out, o)
            }
            "rows" | "cols" => {
                let mut o = next()?;
                let (from, to) = (count::<u16>(name, &next()?)?, count::<u16>(name, &next()?)?);
                if from > to {
                    return Err(fail(
                        Code::Range,
                        format!("{from} {to}: the first {name} before the last"),
                    ));
                }
                let view = &mut option(name, &mut o)?.view;
                match name {
                    "rows" => view.rows = Some((from, to)),
                    _ => view.cols = Some((from, to)),
                }
                now(out, o)
            }
            "run" | "new-tab" | "new-workspace" => {
                let built = finished(name, next()?)?;
                let typed: Vec<String> = built.typed.into_iter().collect();
                let command = match name {
                    "run" => {
                        if built.argv.is_empty() {
                            return Err(fail(
                                Code::NoProgram,
                                "run needs a program: opts str ./app program run",
                            ));
                        }
                        Command::Run {
                            workspace: built.place,
                            name: built.name,
                            size: built.size,
                            cwd: built.cwd,
                            env: built.env,
                            argv: built.argv,
                            log: built.log,
                            ids: built.ids,
                            then: wait(&built.waits, false, self.settle),
                        }
                    }
                    "new-tab" => Command::NewTab {
                        target: built.place,
                        name: built.name,
                        cmd: typed,
                        size: built.size,
                        cwd: built.cwd,
                        env: built.env,
                        ids: built.ids,
                    },
                    _ => Command::NewWorkspace {
                        name: built.name,
                        cmd: typed,
                        size: built.size,
                        cwd: built.cwd,
                        env: built.env,
                        ids: built.ids,
                    },
                };
                later(ask(command, Then::Handle(Kind::Pane)))
            }
            "split" => {
                let p = pane(name, &next()?)?;
                let built = finished(name, next()?)?;
                let crate::id::Wanted {
                    pane: id,
                    tab: None,
                    workspace: None,
                } = built.ids
                else {
                    return Err(fail(Code::Id, "split makes a pane alone: its id is %N"));
                };
                let (run, cmd) = match (built.argv.is_empty(), built.typed) {
                    (true, typed) => (false, typed.into_iter().collect()),
                    (false, None) => (true, built.argv),
                    (false, Some(_)) => {
                        let why = "split takes no typed with a program: a line is typed into the shell the program replaces";
                        return Err(fail(Code::Unused, why));
                    }
                };
                later(ask(
                    Command::Split {
                        axis: built.axis.unwrap_or(Axis::Horizontal),
                        target: p,
                        cmd,
                        cwd: built.cwd,
                        env: built.env,
                        run,
                        id,
                        then: wait(&built.waits, false, self.settle),
                    },
                    Then::Handle(Kind::Pane),
                ))
            }
            "respawn" => {
                let p = pane(name, &next()?)?;
                let built = finished(name, next()?)?;
                later(ask(
                    Command::Respawn {
                        target: p,
                        kill: built.kill,
                        env: built.env,
                        argv: built.argv,
                        then: wait(&built.waits, false, self.settle),
                    },
                    Then::Handle(Kind::Pane),
                ))
            }
            // ----------------------------------------------------- waiting
            "wait-text" | "wait-gone" => {
                let x = next()?;
                let t = string(name, &next()?)?;
                let bare = t.trim_start_matches('^').trim_end_matches('$').trim();
                // A tab is a blank, as a wait compares: blanks as one.
                if bare.is_empty() || t.contains(['\n', '\r']) {
                    return Err(fail(
                        Code::BadValue,
                        "a text of one line, not empty (^ and $ alone hold nothing)",
                    ));
                }
                let until = match name {
                    "wait-text" => Until::Text(t),
                    _ => Until::Gone(t),
                };
                with(name, x, out, |w| w.until.push(until))
            }
            "timeout" => {
                let x = next()?;
                let n = count::<u64>(name, &next()?)?;
                with(name, x, out, |w| w.timeout = Some(Duration::from_secs(n)))
            }
            "wait-exit" | "wait-bell" => {
                let until = match name {
                    "wait-exit" => Until::Exit,
                    _ => Until::Bell,
                };
                with(name, next()?, out, |w| w.until.push(until))
            }
            "wait" => {
                let p = target(name, &next()?)?;
                let built = finished(name, next()?)?;
                // With a wait's time either way: on a tab or workspace, for
                // each of its panes.
                later(ask(
                    Command::Wait {
                        target: p,
                        wait: waited(&built.waits, false),
                    },
                    Then::Outcome { waited: true },
                ))
            }
            // ------------------------------------------------------- input
            "text" | "key" | "paste" | "paste-file" | "times" => {
                let mut k = next()?;
                let v = next()?;
                let built = keys(name, &mut k)?;
                match name {
                    "text" => built.inputs.push(Input::Text(text(name, &v)?)),
                    "key" => {
                        let word = string(name, &v)?;
                        let press = word.parse().map_err(|_| {
                            fail(
                                Code::NotAKey,
                                format!("{word:?} is no key's name (key-names lists them)"),
                            )
                        })?;
                        built.inputs.push(Input::Key(press));
                    }
                    "paste" => built.inputs.push(Input::Paste(string(name, &v)?)),
                    "paste-file" => {
                        let path = string(name, &v)?;
                        let most = (INPUT_MAX, "900 KiB, the most one paste takes");
                        let text = super::read_file(path.as_ref(), most)
                            .map_err(|f| fail(Code::Paste, f.message))?;
                        built.inputs.push(Input::Paste(text));
                    }
                    // The input just before it, n times in all.
                    _ => {
                        let n = int(name, &v)?;
                        let n = usize::try_from(n)
                            .ok()
                            .filter(|n| (1..=1000).contains(n))
                            .ok_or_else(|| {
                                fail(Code::Range, format!("{n}: a count from 1 to 1000"))
                            })?;
                        let Some(last) = built.inputs.last().cloned() else {
                            return Err(fail(
                                Code::NoKeys,
                                "times repeats the input before it: str Down key 3 times",
                            ));
                        };
                        built
                            .inputs
                            .extend(std::iter::repeat_n(last, n.saturating_sub(1)));
                    }
                }
                now(out, k)
            }
            "send-keys" => {
                let to = next()?;
                let mut k = next()?;
                let built = keys(name, &mut k)?.clone();
                if built.inputs.is_empty() {
                    return Err(fail(
                        Code::NoKeys,
                        "send-keys needs keys: keys str hello text",
                    ));
                }
                // As the request carries them, which must fit in one frame:
                // text and pastes their bytes, keys their names' (`C-M-F12`
                // 7), and each input after the first 5 bytes more.
                let typed: usize = (built.inputs.iter())
                    .map(|input| match input {
                        Input::Text(text) | Input::Paste(text) => text.len(),
                        Input::Key(key) => key.to_string().len(),
                    })
                    .fold(0usize, |sum, n| sum.saturating_add(n))
                    .saturating_add(built.inputs.len().saturating_sub(1).saturating_mul(5));
                if typed > INPUT_MAX {
                    return Err(fail(
                        Code::TooLong,
                        format!(
                            "send-keys: {typed} bytes, more than 900 KiB, the most it sends at once \
                             (text and pastes count their bytes, keys their names', each input \
                             after the first 5 more)"
                        ),
                    ));
                }
                if let Value::App(Fux::Client(c)) = &to {
                    // Typed at its terminal: whatever pane they reach is the
                    // client's to choose, so there is none to wait on here.
                    if built.waits != Waits::default() {
                        return Err(fail(
                            Code::Unused,
                            "send-keys to a client takes no wait: wait on the pane after (PANE opts … wait)",
                        ));
                    }
                    let keys = ClientAction::Keys(built.inputs);
                    let typed = Command::Client {
                        client: Some(*c),
                        action: keys,
                    };
                    // Its answer, empty, as send-keys to a pane leaves one.
                    let answer = Then::Outcome {
                        waited: self.settle,
                    };
                    return later(ask(typed, answer));
                }
                let (then, answer) = acted(&built.waits, self.settle);
                let target = target(name, &to)
                    .map_err(|_| wrong(name, "a pane, tab, workspace or client", &to))?;
                later(ask(
                    Command::SendKeys {
                        target,
                        inputs: built.inputs,
                        then,
                    },
                    answer,
                ))
            }
            "mouse" => {
                let action = string(name, &next()?)?;
                let (row, col) = (count::<u16>(name, &next()?)?, count::<u16>(name, &next()?)?);
                let mouse = match action.as_str() {
                    "click" => Mouse::Click(MouseButton::Left),
                    "right" => Mouse::Click(MouseButton::Right),
                    "middle" => Mouse::Click(MouseButton::Middle),
                    "press" => Mouse::Press(MouseButton::Left),
                    "release" => Mouse::Release(MouseButton::Left),
                    "wheel-up" => Mouse::Wheel(MouseButton::WheelUp),
                    "wheel-down" => Mouse::Wheel(MouseButton::WheelDown),
                    "move" => Mouse::Move,
                    other => {
                        return Err(fail(
                            Code::BadValue,
                            format!(
                                "mouse: {other:?} is not click, right, middle, press, release, wheel-up, wheel-down or move"
                            ),
                        ));
                    }
                };
                now(
                    out,
                    Value::App(Fux::Mouse(Box::new(MouseEvent {
                        mouse,
                        row,
                        col,
                        waits: Waits::default(),
                    }))),
                )
            }
            "send-mouse" => {
                let p = pane(name, &next()?)?;
                let m = next()?;
                let Value::App(Fux::Mouse(m)) = &m else {
                    return Err(wrong(name, "a mouse event (from mouse)", &m));
                };
                let (then, answer) = acted(&m.waits, self.settle);
                later(ask(
                    Command::SendMouse {
                        target: p,
                        mouse: m.mouse,
                        row: m.row,
                        col: m.col,
                        then,
                    },
                    answer,
                ))
            }
            "signal" => {
                let p = pane(name, &next()?)?;
                let signal = string(name, &next()?)?;
                let Some(signal) = fuxix::process::Signal::named(&signal) else {
                    let all = fuxix::process::Signal::ALL.map(fuxix::process::Signal::name);
                    let all = crate::words::listed(&all, "or");
                    return Err(fail(
                        Code::BadValue,
                        format!("signal: {signal:?} is not {all}"),
                    ));
                };
                // From a command line: answers once the program exited or settled.
                let (then, answer) = acted(&Waits::default(), self.settle);
                later(ask(
                    Command::Signal {
                        target: p,
                        signal,
                        then,
                    },
                    answer,
                ))
            }
            // ----------------------------------------------------- reading
            "capture" => {
                let p = pane(name, &next()?)?;
                let built = finished(name, next()?)?;
                let then = if built.json {
                    Then::Json(Said::Capture)
                } else {
                    Then::Answer(Said::Capture)
                };
                // From a command line, or given a timeout, read once the pane
                // settles.
                let timed = built.waits.timeout.is_some();
                let settle = (self.settle || timed).then_some(built.waits.timeout);
                later(ask(
                    Command::CapturePane {
                        target: p,
                        history: built.history,
                        view: built.view,
                        settle,
                    },
                    then,
                ))
            }
            "ls" | "info" | "get" => {
                let target = match name {
                    "ls" => None,
                    _ => Some(target(name, &next()?)?),
                };
                let (get, then) = match name {
                    "get" => (Some(string(name, &next()?)?), Then::Value),
                    _ => (None, Then::Answer(Said::Listing)),
                };
                later(ask(Command::Ls { target, get }, then))
            }
            "json" => match next()? {
                // Among the options, as after what they make: `opts json
                // capture` is `opts capture json`.
                Value::App(Fux::Opts(mut opts)) => {
                    opts.json = true;
                    opts.given.push(name);
                    now(out, Value::App(Fux::Opts(opts)))
                }
                // An answer's JSON as given: of several panes, a line each.
                Value::App(Fux::Answer(answer)) if answer.json.is_some() => {
                    now(out, Value::str(answer.json.unwrap_or_default()))
                }
                other @ (Value::Int(_)
                | Value::Bool(_)
                | Value::Str(_)
                | Value::List(_)
                | Value::App(_)) => now(out, Value::str(json_of(&other).render())),
            },
            "events" | "events-json" | "events-of" | "events-of-json" => {
                let target = match name.starts_with("events-of") {
                    true => Some(target(name, &next()?)?),
                    false => None,
                };
                let json = name.ends_with("json");
                tell(Command::Events { target, json })
            }
            "dump" | "dump-of" => {
                let target = match name {
                    "dump" => None,
                    _ => Some(target(name, &next()?)?),
                };
                later(ask(Command::DumpLayout { target }, Then::Text))
            }
            // ---------------------------------------------------- changing
            "kill" => tell(Command::Kill {
                target: target(name, &next()?)?,
            }),
            "rename" | "notify" => {
                let t = target(name, &next()?)?;
                let given = text(name, &next()?)?;
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
                text: text(name, &next()?)?,
            }),
            "resize" | "resize-auto" => {
                let target = target(name, &next()?)?;
                let size = match name {
                    "resize" => Some(laid_out(name, &next()?, &next()?)?),
                    _ => None,
                };
                tell(Command::Resize(Resized::Tabs(target, size)))
            }
            "resize-client" => {
                let c = client(name, &next()?)?;
                let (cols, rows) = (count::<u16>(name, &next()?)?, count::<u16>(name, &next()?)?);
                let size = fux_vt::Size::new(rows, cols)
                    .ok()
                    .ok_or_else(|| fail(Code::Range, "columns and rows each from 1"))?;
                tell(Command::Resize(Resized::Client(c, size)))
            }
            "resize-x" | "resize-y" => {
                let p = pane(name, &next()?)?;
                let extent = match next()? {
                    Value::App(Fux::Extent(e)) => e,
                    v @ Value::Int(_) => Extent::Cells(count(name, &v)?),
                    other @ (Value::Bool(_) | Value::Str(_) | Value::List(_) | Value::App(_)) => {
                        return Err(wrong(name, "cells or a percent", &other));
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
                let n = int(name, &next()?)?;
                let Some(share) = u8::try_from(n).ok().and_then(Percent::new) else {
                    return Err(fail(
                        Code::Range,
                        format!("percent takes 1 to 100, not {n}"),
                    ));
                };
                now(out, Value::App(Fux::Extent(Extent::Percent(share))))
            }
            "move-border" => {
                let p = pane(name, &next()?)?;
                let way = direction(name, &next()?)?;
                let cells = count::<u16>(name, &next()?)?;
                tell(Command::ResizePane {
                    target: p,
                    by: PaneResize::Border(way, cells),
                })
            }
            "move-to" => {
                let p = pane(name, &next()?)?;
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
                            name,
                            "a tab, a workspace, new-tab or new-workspace",
                            &other,
                        ));
                    }
                };
                tell(Command::MovePane { target: p, to })
            }
            "move-beside" => {
                let p = pane(name, &next()?)?;
                let way = direction(name, &next()?)?;
                tell(Command::MovePane {
                    target: p,
                    to: MoveTo::Beside(way),
                })
            }
            "swap-panes" => {
                let (a, b) = (pane(name, &next()?)?, pane(name, &next()?)?);
                tell(Command::SwapPane {
                    target: a,
                    with: SwapWith::Pane(b),
                })
            }
            "swap-toward" => {
                let p = pane(name, &next()?)?;
                let way = direction(name, &next()?)?;
                tell(Command::SwapPane {
                    target: p,
                    with: SwapWith::Toward(way),
                })
            }
            "reorder" => {
                let t = target(name, &next()?)?;
                let toward = sibling(name, &next()?)?;
                tell(Command::Reorder { target: t, toward })
            }
            "start-log" | "stop-log" => {
                let target = pane(name, &next()?)?;
                let file = match name {
                    "start-log" => Some(string(name, &next()?)?),
                    _ => None,
                };
                tell(Command::Log { target, file })
            }
            "clear-history" => tell(Command::ClearHistory {
                target: pane(name, &next()?)?,
            }),
            "buffers" => later(ask(Command::ListBuffers, Then::Text)),
            "buffer" => later(ask(
                Command::ShowBuffer {
                    index: count(name, &next()?)?,
                },
                Then::Text,
            )),
            "paste-buffer" => {
                let p = pane(name, &next()?)?;
                let index = count(name, &next()?)?;
                tell(Command::PasteBuffer { index, target: p })
            }
            // ----------------------------------------------------- clients
            "attach" | "watch" | "attach-to" => {
                let workspace = match name {
                    "attach-to" => Some(match workspace(name, &next()?)? {
                        WsRef::Id(w) => w.to_string(),
                        WsRef::Name(n) => n,
                    }),
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
                let (cols, rows) = (count::<u16>(name, &next()?)?, count::<u16>(name, &next()?)?);
                let size = fux_vt::Size::new(rows, cols)
                    .map_err(|_| fail(Code::Range, "a size of at least 1 by 1"))?;
                local(Local::Attach {
                    workspace: None,
                    read_only: false,
                    size: Some(size),
                })
            }
            "focus" => {
                let c = client(name, &next()?)?;
                let pick = match string(name, &next()?)?.as_str() {
                    "next" => PanePick::Step(Sibling::Next),
                    "previous" => PanePick::Step(Sibling::Previous),
                    "last" => PanePick::Last,
                    other => {
                        PanePick::Toward(direction(name, &Value::str(other)).map_err(|_| {
                            fail(
                                Code::BadValue,
                                format!(
                                    "{other:?} is not left, right, up, down, next, previous or last"
                                ),
                            )
                        })?)
                    }
                };
                later(act(c, ClientAction::SelectPane(pick)))
            }
            "focus-pane" => {
                let c = client(name, &next()?)?;
                let p = pane(name, &next()?)?;
                later(act(c, ClientAction::SelectPane(PanePick::Id(p))))
            }
            "show-tab" => {
                let c = client(name, &next()?)?;
                let t = next()?;
                let Value::App(Fux::Tab(tab)) = &t else {
                    return Err(wrong(name, "a tab", &t));
                };
                later(act(c, ClientAction::SelectTab(Pick::Id(*tab))))
            }
            "show-workspace" => {
                let c = client(name, &next()?)?;
                let w = workspace(name, &next()?)?;
                later(act(c, ClientAction::SelectWorkspace(Pick::Id(w))))
            }
            "next-tab" | "previous-tab" | "next-workspace" | "previous-workspace" | "zoom"
            | "detach" => {
                let c = client(name, &next()?)?;
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
            // A client's own screens, which it opens itself: from its keys
            // or prompt, here; asked from elsewhere, relayed for it to run.
            name if opens(name) => {
                let c = client(name, &next()?)?;
                use crate::screens::Open;
                let open = match name {
                    "choose-tab" => Open::ChooseTab(None),
                    "choose-workspace" => Open::ChooseWorkspace(None),
                    "copy-mode" => Open::Copy,
                    "command-prompt" => Open::Prompt,
                    "choose-tab-for" => Open::ChooseTab(Some(pane(name, &next()?)?)),
                    "choose-workspace-for" => Open::ChooseWorkspace(Some(pane(name, &next()?)?)),
                    "choose-pane" => Open::ChoosePane(pane(name, &next()?)?),
                    "menu" => Open::Menu(target(name, &next()?)?),
                    "rename-prompt" => Open::Rename(target(name, &next()?)?),
                    _ => Open::Confirm(target(name, &next()?)?),
                };
                match self.context.client == Some(c) {
                    true => local(Local::Open(open)),
                    false => later(act(c, ClientAction::Run(open.code()))),
                }
            }
            "capture-client" => {
                let c = client(name, &next()?)?;
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
            "server" => local(Local::Server(crate::socket::Choice::Name(string(
                name,
                &next()?,
            )?))),
            "socket" => local(Local::Server(crate::socket::Choice::Path(string(
                name,
                &next()?,
            )?))),
            "start" => local(Local::Start),
            "status" => local(Local::Status),
            "servers" => local(Local::Servers),
            "kill-server" => tell(Command::KillServer { force: false }),
            "force-kill-server" => local(Local::ForceKill),
            "set" => {
                let option = string(name, &next()?)?;
                let value = text(name, &next()?)?;
                tell(Command::Set { option, value })
            }
            "grant" => {
                let workspace = workspace(name, &next()?)?;
                let socket = string(name, &next()?)?;
                later(ask(Command::Grant { workspace, socket }, Then::Text))
            }
            "revoke" => tell(Command::Revoke {
                socket: string(name, &next()?)?,
            }),
            "version" => now(out, Value::str(env!("CARGO_PKG_VERSION"))),
            // ------------------------------------------------------ config
            "prefix" | "bind" | "bind-root" | "unbind" | "unbind-root" | "unbind-all" | "label"
            | "group" | "repeat" => self.configure(name, &mut next),
            // A client's own; else, from the command line, what the config
            // makes.
            "list-keys" => {
                let text = match &self.keymap {
                    Some(keymap) => keymap.describe(),
                    None => {
                        let config = crate::config::default_path().filter(|p| p.exists());
                        let keymap = match config {
                            Some(path) => super::run_file(&path, None)
                                .map_err(|e| fail(Code::Config, e.to_string()))?,
                            None => crate::keymap::Keymap::default(),
                        };
                        keymap.describe()
                    }
                };
                self.show(&text)
            }
            "key-names" => self.show(&format!("{}\n", crate::keys::all_names().join(" "))),
            // -------------------------------------------------------- help
            "help" => self.show(&crate::help::overview()),
            "help-agents" => self.show(crate::help::AGENTS),
            "help-json" => self.show(crate::help::JSON),
            "explain" => {
                let word = string(name, &next()?)?;
                let text = crate::help::explain(&word)
                    .ok_or_else(|| fail(Code::UnknownWord, format!("no word {word:?}")))?;
                self.show(&text)
            }
            "check" => {
                let path = string(name, &next()?)?;
                // Read as running it reads it, and refused alike.
                let text = super::read_file(path.as_ref(), super::SOURCE_MAX)
                    .map_err(|f| fail(Code::Read, f.message))?;
                let tokens = fuxth::token::source(&path, &text)
                    .map_err(|e| fail(Code::Check, e.to_string()))?;
                super::compile(&tokens).map_err(|e| fail(Code::Check, e.to_string()))?;
                // A config is run dry too: its bindings made, its settings
                // checked, nothing sent.
                let config = std::path::Path::new(&path)
                    .file_name()
                    .is_some_and(|n| n == "config.fux");
                if config {
                    super::run_file(std::path::Path::new(&path), None)
                        .map_err(|e| fail(Code::Check, e.to_string()))?;
                }
                self.show(&format!("{path}: ok\n"))
            }
            other => Err(fail(Code::UnknownWord, format!("{other}: no such word"))),
        }
    }
}

impl Host<'_> {
    /// Writes `text` out, whether or not it goes.
    fn show(&mut self, text: &str) -> Result<Done<Ask>, Failure> {
        let _ = write!(self.out, "{text}");
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
                format!("{name} changes a client's keys: in its config, or typed at its prompt"),
            ));
        };
        let refused = |why: String| fail(Code::BadBinding, why);
        match name {
            "prefix" => {
                let key = crate::keymap::key(&string(name, &next()?)?).map_err(refused)?;
                keymap.set_prefix(key).map_err(refused)?;
            }
            "bind" | "bind-root" => {
                let keys_text = string(name, &next()?)?;
                let code = string(name, &next()?)?;
                // Checked now, rather than each time its keys are typed.
                let tokens =
                    fuxth::token::source("binding", &code).map_err(|e| refused(e.to_string()))?;
                super::compile(&tokens).map_err(|e| refused(e.to_string()))?;
                let binding = crate::keymap::Binding {
                    label: code.clone(),
                    code,
                    group: String::new(),
                };
                let keys = crate::keymap::keys(&keys_text).map_err(refused)?;
                match (name, keys.as_slice()) {
                    ("bind-root", [key]) => {
                        keymap.bind_root(*key, binding).map_err(refused)?;
                        self.last_bound = Some(Bound::Root(*key));
                    }
                    ("bind-root", _) => return Err(refused("bind-root takes one key".to_owned())),
                    _ => {
                        keymap
                            .bind(
                                &keys,
                                crate::keymap::Binding {
                                    group: "Other".to_owned(),
                                    ..binding
                                },
                                false,
                            )
                            .map_err(refused)?;
                        self.last_bound = Some(Bound::Prefix(keys));
                    }
                }
            }
            "unbind" => {
                let keys = crate::keymap::keys(&string(name, &next()?)?).map_err(refused)?;
                keymap.unbind(&keys).map_err(refused)?;
            }
            "unbind-root" => {
                let key = crate::keymap::key(&string(name, &next()?)?).map_err(refused)?;
                keymap.unbind_root(key).map_err(refused)?;
            }
            "unbind-all" => keymap.unbind_all(),
            _ => {
                // label, group, repeat: the binding just made.
                let Some(bound) = self.last_bound.clone() else {
                    return Err(refused(format!(
                        "{name} changes the binding just made: bind first"
                    )));
                };
                let given = match name {
                    "repeat" => None,
                    _ => Some(string(name, &next()?)?),
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

    /// A word's effect names what it takes and gives: the checker counts
    /// on both.
    #[test]
    fn every_effect_matches_its_word() {
        for word in WORDS {
            let (before, _) = word.effect.split_once("--").unwrap_or_default();
            assert_eq!(
                before.split_whitespace().count(),
                word.takes,
                "{}",
                word.name
            );
        }
        let send = WORDS
            .iter()
            .find(|w| w.name == "send-keys")
            .map(Word::gives);
        assert_eq!(send, Some(0..=1), "an outcome only with a wait");
    }

    /// A wait and an action's wait leave how long to the server
    /// (`wait-seconds`), unless they say: one deadline for both.
    #[test]
    fn waits_leave_their_limit_to_the_server() {
        let settle = waited(&Waits::default(), false);
        assert_eq!((settle.until, settle.timeout), (vec![Until::Settled], None));
        let action = wait(&Waits::default(), true, true).map(|w| w.timeout);
        assert_eq!(action, Some(None));
        let said = Waits {
            timeout: Some(Duration::from_secs(5)),
            ..Waits::default()
        };
        assert_eq!(waited(&said, false).timeout, Some(Duration::from_secs(5)));
    }

    /// An answer in a list is its object; one of several panes, an array of
    /// theirs: JSON either way.
    #[test]
    fn an_answer_of_several_panes_is_an_array() {
        let answer = |json: &str| {
            json_of(&Value::App(Fux::Answer(Box::new(super::super::Answer {
                text: String::new(),
                json: Some(json.to_owned()),
                said: super::super::Said::Several,
            }))))
            .render()
        };
        assert_eq!(answer("{\"pane\":\"%1\"}\n"), "{\"pane\":\"%1\"}");
        assert_eq!(
            answer("{\"pane\":\"%1\"}\n{\"pane\":\"%2\"}\n"),
            "[{\"pane\":\"%1\"},{\"pane\":\"%2\"}]"
        );
    }

    #[test]
    fn a_default_capture_view_is_plain() {
        assert_eq!(crate::command::CaptureView::default(), Opts::default().view);
    }
}
