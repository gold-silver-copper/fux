//! fux's words: each a name, how many values it takes, what it does to the
//! stack, and a line of help; and what each does, run by the program's
//! [`Host`].
use super::{Ask, Fux, Host, Keys, Local, MouseEvent, Opts, Then, Waits};
use crate::command::{
    AnyRef, ClientAction, Command, Extent, Input, Kind, Mouse, MoveTo, PanePick, PaneResize, Pick,
    Resized, Sibling, Subject, SwapWith, Until, WAIT_TIMEOUT, Wait, WsRef,
};
use crate::id::WsId;
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
    word("Starting", "id", 2, "opts id -- opts", "the id to make it with, never given before (%N, @N, +N)"),
    word("Starting", "log-to", 2, "opts file -- opts", "keep everything the program writes in a file too"),
    word("Starting", "kill-first", 1, "opts -- opts", "respawn: end the program first if it still runs"),
    word("Starting", "run", 1, "opts -- pane", "start a program in a tab of its own, in workspace agent; its pane stays once it exits"),
    word("Starting", "split", 2, "pane opts -- pane", "split a pane: a shell, or the program given"),
    word("Starting", "new-tab", 1, "opts -- pane", "a tab with a shell"),
    word("Starting", "new-workspace", 1, "opts -- pane", "a workspace with a tab and a shell"),
    word("Starting", "respawn", 2, "pane opts -- pane", "start again in its pane the program run started"),
    // Waiting
    word("Waiting", "wait-text", 2, "x text -- x", "wait until text shows anew (options, keys or a mouse event)"),
    word("Waiting", "wait-gone", 2, "x text -- x", "wait until text is no longer shown"),
    word("Waiting", "wait-exit", 1, "x -- x", "wait until the program exits"),
    word("Waiting", "wait-bell", 1, "x -- x", "wait until the program rings or notifies"),
    word("Waiting", "timeout", 2, "x secs -- x", "how long to wait at most: an action or capture settles within 10, a wait 60; 0 answers at once"),
    word("Waiting", "wait", 2, "x opts -- answer", "wait for the pane to settle, or as the options say, in a pane, or each pane of a tab or workspace"),
    // Input
    word("Input", "keys", 0, "-- keys", "empty input, for send-keys"),
    word("Input", "text", 2, "keys text -- keys", "text, typed"),
    word("Input", "key", 2, "keys name -- keys", "a key by its name: Enter, C-c, Up, F5"),
    word("Input", "paste", 2, "keys text -- keys", "text as a paste"),
    word("Input", "paste-file", 2, "keys file -- keys", "a file's text as a paste"),
    word("Input", "times", 2, "keys n -- keys", "the input just before it, n times: str Down key 2 times"),
    word("Input", "send-keys", 2, "target keys -- [outcome]", "send to a pane, every pane of a tab, or a client as typed at its terminal"),
    word("Input", "mouse", 3, "action row col -- mouse", "click right middle press release wheel-up wheel-down or move, at a cell from 0"),
    word("Input", "send-mouse", 2, "pane mouse -- [outcome]", "send a mouse event"),
    word("Input", "signal", 2, "pane name -- [answer]", "TERM KILL INT HUP QUIT USR1 USR2 STOP CONT TSTP WINCH, to the pane's foreground; answers once it exits or settles"),
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
    word("Reading", "last-output", 1, "opts -- opts", "capture: the last shell command's output"),
    word("Reading", "rows", 3, "opts from to -- opts", "capture: those rows only, from 0"),
    word("Reading", "cols", 3, "opts from to -- opts", "capture: those columns only, from 0"),
    word("Reading", "ls", 0, "-- listing", "workspaces, tabs, panes, clients and grants"),
    word("Reading", "info", 1, "handle -- record", "a pane, tab or workspace, all of it"),
    word("Reading", "get", 2, "handle field -- value", "one field of a pane, tab or workspace: exited, revision, cwd"),
    word("Reading", "json", 1, "x -- text", "x as JSON; among a capture's options, the capture as JSON"),
    word("Reading", "events", 0, "-- ", "what happens, a line each, as it happens"),
    word("Reading", "events-json", 0, "-- ", "what happens, as JSON lines"),
    word("Reading", "events-of", 1, "handle -- ", "what happens in a pane, tab or workspace"),
    word("Reading", "events-of-json", 1, "handle -- ", "the same, as JSON lines"),
    word("Reading", "dump", 0, "-- text", "everything, as a program that makes it again"),
    word("Reading", "dump-of", 1, "handle -- text", "a tab or workspace, as a program that makes it again"),
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
    word("Server", "set", 2, "setting value -- ", "shell history-lines clipboard buffers bell titles shell-marks update-environment"),
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

/// At most `limit` bytes of the file at `path`, and one more if it has
/// them: never more is read, whatever it is (`/dev/zero`).
fn bounded(path: &str, limit: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut read = Vec::new();
    std::fs::File::open(path)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut read)?;
    Ok(read)
}

/// A failure of a word's own: its code and why.
fn fail(code: &'static str, message: impl Into<String>) -> Failure {
    Failure::new(code, message)
}

/// How a word of fux's is used, for a mistake in using it: what goes
/// before it, what it leaves, and an example.
pub fn usage(word: &str) -> Option<String> {
    let effect = match WORDS.iter().find(|w| w.name == word) {
        Some(w) => w.effect,
        // A word of the prelude's or library's.
        None => {
            crate::help::library()
                .into_iter()
                .find(|(name, ..)| *name == word)?
                .1
        }
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
    // With how the word is used, which says what goes before it.
    let usage = usage(word).map_or_else(String::new, |u| format!(" ({u})"));
    fail(
        "type",
        format!("{word} takes {wanted}, not {}{usage}", got.kind()),
    )
}

fn string(word: &str, value: &Value<Fux>) -> Result<String, Failure> {
    match value {
        Value::Str(s) => Ok(s.to_string()),
        other @ (Value::Int(_) | Value::Bool(_) | Value::List(_) | Value::App(_)) => {
            Err(wrong(word, "a string", other))
        }
    }
}

/// A value as words that take text read it: a string, or a number.
fn text(word: &str, value: &Value<Fux>) -> Result<String, Failure> {
    match value {
        Value::Str(s) => Ok(s.to_string()),
        Value::Int(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        other @ (Value::List(_) | Value::App(_)) => Err(wrong(word, "a string or a number", other)),
    }
}

/// A value as JSON: a list as an array of its values', anything of
/// fux's as its text.
fn json_of(value: &Value<Fux>) -> crate::json::Json<'static> {
    use crate::json::Json;
    match value {
        Value::Int(n) => Json::Number(*n),
        Value::Bool(b) => Json::Bool(*b),
        Value::Str(s) => Json::str(s.to_string()),
        // As deep as lists nest, at most fuxth's limit.
        Value::List(list) => Json::Array(list.items.iter().map(json_of).collect()),
        Value::App(_) => Json::str(super::display(value)),
    }
}

fn int(word: &str, value: &Value<Fux>) -> Result<i64, Failure> {
    match value {
        Value::Int(n) => Ok(*n),
        other @ (Value::Bool(_) | Value::Str(_) | Value::List(_) | Value::App(_)) => {
            Err(wrong(word, "an integer", other))
        }
    }
}

/// A count or a cell: an integer from 0 that fits.
fn count<T: TryFrom<i64>>(word: &str, value: &Value<Fux>) -> Result<T, Failure> {
    let n = int(word, value)?;
    T::try_from(n).map_err(|_| fail("range", format!("{n} is out of range")))
}

fn pane(word: &str, value: &Value<Fux>) -> Result<crate::id::PaneId, Failure> {
    match value {
        Value::App(Fux::Pane(p)) => Ok(*p),
        Value::App(Fux::Answer(answer)) if answer.pane.is_some() => {
            answer.pane.ok_or_else(|| wrong(word, "a pane", value))
        }
        other @ (Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_)) => Err(wrong(word, "a pane (from pane, run or split)", other)),
    }
}

fn client(word: &str, value: &Value<Fux>) -> Result<crate::id::ClientId, Failure> {
    match value {
        Value::App(Fux::Client(c)) => Ok(*c),
        other @ (Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_)) => Err(wrong(word, "a client (from me or client)", other)),
    }
}

/// A pane, tab or workspace.
fn target(word: &str, value: &Value<Fux>) -> Result<AnyRef<WsId>, Failure> {
    match value {
        Value::App(Fux::Pane(p)) => Ok(AnyRef::Pane(*p)),
        Value::App(Fux::Answer(answer)) if answer.pane.is_some() => answer
            .pane
            .map(AnyRef::Pane)
            .ok_or_else(|| wrong(word, "a pane", value)),
        Value::App(Fux::Tab(t)) => Ok(AnyRef::Tab(*t)),
        Value::App(Fux::Workspace(w)) => Ok(AnyRef::Workspace(*w)),
        other @ (Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_)) => Err(wrong(word, "a pane, tab or workspace", other)),
    }
}

fn subject(target: AnyRef<WsId>) -> Subject {
    Subject::from(target.map(WsRef::Id))
}

fn direction(word: &str, value: &Value<Fux>) -> Result<Direction, Failure> {
    match string(word, value)?.as_str() {
        "left" => Ok(Direction::Left),
        "right" => Ok(Direction::Right),
        "up" => Ok(Direction::Up),
        "down" => Ok(Direction::Down),
        other => Err(fail(
            "bad_value",
            format!("{other:?} is not left, right, up or down"),
        )),
    }
}

fn sibling(word: &str, value: &Value<Fux>) -> Result<Sibling, Failure> {
    match string(word, value)?.as_str() {
        "next" => Ok(Sibling::Next),
        "previous" => Ok(Sibling::Previous),
        other => Err(fail(
            "bad_value",
            format!("{other:?} is not next or previous"),
        )),
    }
}

/// Options a word would leave unused, refused rather than ignored: a
/// program for words that start a shell, a typed line for `run`, a tab's
/// name, size or log for `split`, a wait for words that do not wait.
fn ignored(word: &str, built: &Opts) -> Result<(), Failure> {
    let refuse = |what: &str, instead: &str| {
        Err(fail("unused", format!("{word} takes no {what}: {instead}")))
    };
    match word {
        "new-tab" | "new-workspace" if !built.argv.is_empty() => refuse(
            "program",
            "it starts a shell; run starts a program, or `str LINE typed` types one",
        ),
        "new-tab" | "new-workspace" | "split" if !built.waits.until.is_empty() => {
            refuse("wait", "wait on the pane it leaves: PANE opts … wait")
        }
        "run" if built.typed.is_some() => refuse("typed line", "it starts no shell to type into"),
        "split" if built.name.is_some() || built.size.is_some() || built.log.is_some() => refuse(
            "name, size or log",
            "a pane takes its tab's; name, size and log-to are run's",
        ),
        _ => Ok(()),
    }
}

/// What a word given options, keys or a mouse event changes of them.
fn with<T>(
    word: &str,
    value: Value<Fux>,
    change: impl FnOnce(&mut Waits) -> Result<T, Failure>,
) -> Result<Value<Fux>, Failure> {
    let mut value = value;
    let waits = match &mut value {
        Value::App(Fux::Opts(opts)) => &mut opts.waits,
        Value::App(Fux::Keys(keys)) => &mut keys.waits,
        Value::App(Fux::Mouse(mouse)) => &mut mouse.waits,
        other @ (Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_)) => {
            return Err(wrong(word, "options, keys or a mouse event", other));
        }
    };
    change(waits)?;
    Ok(value)
}

fn opts<'a>(word: &str, value: &'a mut Value<Fux>) -> Result<&'a mut Opts, Failure> {
    match value {
        Value::App(Fux::Opts(opts)) => Ok(opts),
        other @ (Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_)) => Err(wrong(word, "options (from opts)", other)),
    }
}

fn keys<'a>(word: &str, value: &'a mut Value<Fux>) -> Result<&'a mut Keys, Failure> {
    match value {
        Value::App(Fux::Keys(keys)) => Ok(keys),
        other @ (Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_)) => Err(wrong(word, "keys (from keys)", other)),
    }
}

/// A wait as the server takes it, armed after an action when `after`.
/// How long an action waits for its pane to settle when it names nothing.
pub const SETTLE_TIMEOUT: Duration = Duration::from_secs(10);

/// The wait an action makes: what it names; or, from a command line
/// (`settle`), for the pane to settle, said with what came.
fn wait(waits: &Waits, after: bool, settle: bool) -> Option<Box<Wait>> {
    let until = match (waits.until.is_empty(), settle) {
        (false, _) => waits.until.clone(),
        (true, true) => vec![Until::Settled],
        (true, false) => return None,
    };
    let settles = until == [Until::Settled];
    Some(Box::new(Wait {
        until,
        timeout: waits.timeout.unwrap_or(if settles {
            SETTLE_TIMEOUT
        } else {
            WAIT_TIMEOUT
        }),
        print: waits.print || settles,
        after,
        echo: Vec::new(),
        marks: waits.marks,
        lines: 0,
        all: true,
    }))
}

fn ask(command: Command, then: Then) -> Ask {
    Ask::Server {
        command,
        client: None,
        then,
    }
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

/// The id `text` gives, for `id`.
fn asked_id(text: &str) -> Result<crate::id::AnyId, Failure> {
    let bad = || fail("bad_value", format!("{text:?} is not %N, @N or +N"));
    match text.chars().next() {
        Some('%') => text.parse().map(AnyRef::Pane).map_err(|_| bad()),
        Some('@') => text.parse().map(AnyRef::Tab).map_err(|_| bad()),
        Some('+') => text.parse().map(AnyRef::Workspace).map_err(|_| bad()),
        _ => Err(bad()),
    }
}

impl fuxth::Host for Host<'_> {
    type Value = Fux;
    type Request = Ask;

    fn print(&mut self, line: &str) -> Result<(), Failure> {
        writeln!(self.out, "{line}").map_err(|e| fail("output", e.to_string()))
    }

    fn call(
        &mut self,
        word: usize,
        args: fuxth::Args<'_, Fux>,
        out: &mut Vec<Value<Fux>>,
    ) -> Result<Done<Ask>, Failure> {
        let Some(spec) = WORDS.get(word) else {
            return Err(fail("unknown_word", "no such word"));
        };
        let name = spec.name;
        let mut args = args.into_iter();
        let mut next = || {
            args.next()
                .ok_or_else(|| fail("stack_underflow", format!("{name} takes {}", spec.takes)))
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
                match next()? {
                    // An empty name names nothing: never the caller's own,
                    // which `here` is, so that an empty variable closes
                    // nothing.
                    Value::Str(s) if s.trim().is_empty() => Err(fail(
                        "no_target",
                        format!("an empty name names no {name}: `here` is the pane this runs in"),
                    )),
                    Value::Str(s) => resolve(s.to_string(), kind),
                    Value::App(app @ (Fux::Pane(_) | Fux::Tab(_) | Fux::Workspace(_))) => {
                        match (kind, &app) {
                            (Kind::Pane, Fux::Pane(_))
                            | (Kind::Tab, Fux::Tab(_))
                            | (Kind::Workspace, Fux::Workspace(_)) => now(out, Value::App(app)),
                            _ => resolve(super::display(&Value::App(app)), kind),
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
                        "bad_value",
                        format!("{text:?} is not a client: clients are cN"),
                    )
                })?;
                now(out, Value::App(Fux::Client(id)))
            }
            "me" => match self.context.client {
                Some(c) => now(out, Value::App(Fux::Client(c))),
                None => Err(fail(
                    "not_a_client",
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
                let argv =
                    crate::words::split(&line).map_err(|e| fail("bad_value", e.to_string()))?;
                if argv.first().is_none_or(|p| p.is_empty()) {
                    return Err(fail("no_program", "an empty command line"));
                }
                opts(name, &mut o)?.argv = argv;
                now(out, o)
            }
            "name" | "cwd" | "program" | "arg" | "typed" | "in" | "id" | "log-to" | "history" => {
                let mut o = next()?;
                let v = next()?;
                let built = opts(name, &mut o)?;
                match name {
                    "name" => built.name = Some(string(name, &v)?),
                    "cwd" => built.cwd = Some(string(name, &v)?),
                    "program" => match built.argv.first_mut() {
                        Some(first) => *first = string(name, &v)?,
                        None => built.argv.push(string(name, &v)?),
                    },
                    "arg" => {
                        if built.argv.is_empty() {
                            return Err(fail("no_program", "arg comes after program"));
                        }
                        built.argv.push(text(name, &v)?);
                    }
                    "typed" => built.typed = Some(string(name, &v)?),
                    "in" => {
                        built.place = Some(match &v {
                            Value::App(Fux::Workspace(w)) => WsRef::Id(*w),
                            // `+N` is a workspace's id, as `workspace` reads it.
                            Value::Str(s) if s.starts_with('+') => WsRef::Id(
                                s.parse()
                                    .map_err(|_| fail("bad_value", format!("{s:?} is not +N")))?,
                            ),
                            other @ (Value::Int(_)
                            | Value::Bool(_)
                            | Value::Str(_)
                            | Value::List(_)
                            | Value::App(_)) => WsRef::Name(string(name, other)?),
                        })
                    }
                    "id" => built.ids.push(asked_id(&string(name, &v)?)?),
                    "log-to" => built.log = Some(string(name, &v)?),
                    _ => built.history = Some(count(name, &v)?),
                }
                now(out, o)
            }
            "size" => {
                let mut o = next()?;
                let (cols, rows) = (count::<u16>(name, &next()?)?, count::<u16>(name, &next()?)?);
                let size = (cols > 0 && rows > 0 && cols <= 500 && rows <= 500)
                    .then(|| fux_vt::Size::new(rows, cols).ok())
                    .flatten()
                    .ok_or_else(|| fail("range", format!("size {cols} {rows}: each 1 to 500")))?;
                opts(name, &mut o)?.size = Some(size);
                now(out, o)
            }
            "env" => {
                let mut o = next()?;
                let (key, value) = (string(name, &next()?)?, text(name, &next()?)?);
                if !crate::command::shell_name(&key) {
                    return Err(fail(
                        "bad_value",
                        format!("{key:?} is no variable's name (letters, digits and _)"),
                    ));
                }
                opts(name, &mut o)?.env.push((key, value));
                now(out, o)
            }
            // On a capture's options, and on what a wait prints,
            // whether among options, keys or a mouse event.
            "marks" => {
                let mut x = next()?;
                if let Value::App(Fux::Opts(opts)) = &mut x {
                    opts.view.marks = true;
                }
                now(
                    out,
                    with(name, x, |w| {
                        w.marks = true;
                        Ok(())
                    })?,
                )
            }
            "horizontal" | "vertical" | "kill-first" | "all-history" | "joined" | "full"
            | "styles" | "main-screen" | "alt-screen" | "last-output" => {
                let mut o = next()?;
                let built = opts(name, &mut o)?;
                match name {
                    "horizontal" => built.axis = Some(Axis::Horizontal),
                    "vertical" => built.axis = Some(Axis::Vertical),
                    "kill-first" => built.kill = true,
                    "all-history" => built.history = Some(usize::MAX),
                    "joined" => built.view.join = true,
                    "full" => built.view.full = true,
                    "styles" => built.view.styles = true,
                    "main-screen" => built.view.main = true,
                    "alt-screen" => built.view.alt = true,
                    _ => built.view.last_output = true,
                }
                now(out, o)
            }
            "rows" | "cols" => {
                let mut o = next()?;
                let (from, to) = (count::<u16>(name, &next()?)?, count::<u16>(name, &next()?)?);
                let view = &mut opts(name, &mut o)?.view;
                match name {
                    "rows" => view.rows = Some((from, to)),
                    _ => view.cols = Some((from, to)),
                }
                now(out, o)
            }
            "run" | "new-tab" | "new-workspace" => {
                let mut o = next()?;
                let built = opts(name, &mut o)?.clone();
                ignored(name, &built)?;
                let typed: Vec<String> = built.typed.into_iter().collect();
                let command = match name {
                    "run" => {
                        if built.argv.is_empty() {
                            return Err(fail(
                                "no_program",
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
                            json: false,
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
                        json: false,
                    },
                    _ => Command::NewWorkspace {
                        name: built.name,
                        cmd: typed,
                        size: built.size,
                        cwd: built.cwd,
                        env: built.env,
                        ids: built.ids,
                        json: false,
                    },
                };
                later(ask(command, Then::Handle(Kind::Pane)))
            }
            "split" => {
                let p = pane(name, &next()?)?;
                let mut o = next()?;
                let built = opts(name, &mut o)?.clone();
                ignored(name, &built)?;
                let run = !built.argv.is_empty();
                let cmd = if run {
                    built.argv
                } else {
                    built.typed.into_iter().collect()
                };
                later(ask(
                    Command::Split {
                        axis: built.axis.unwrap_or(Axis::Horizontal),
                        target: Some(p),
                        cmd,
                        cwd: built.cwd,
                        env: built.env,
                        run,
                        ids: built.ids,
                        json: false,
                    },
                    Then::Handle(Kind::Pane),
                ))
            }
            "respawn" => {
                let p = pane(name, &next()?)?;
                let mut o = next()?;
                let built = opts(name, &mut o)?.clone();
                later(ask(
                    Command::Respawn {
                        target: Some(p),
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
                if bare.is_empty() || t.contains(['\n', '\t']) {
                    return Err(fail(
                        "bad_value",
                        "a text of one line, not empty (^ and $ alone hold nothing), with no tab",
                    ));
                }
                now(
                    out,
                    with(name, x, |w| {
                        w.until.push(if name == "wait-text" {
                            Until::Text(t)
                        } else {
                            Until::Gone(t)
                        });
                        Ok(())
                    })?,
                )
            }
            "timeout" => {
                let x = next()?;
                let n = count::<u64>(name, &next()?)?;
                now(
                    out,
                    with(name, x, |w| {
                        w.timeout = Some(Duration::from_secs(n));
                        Ok(())
                    })?,
                )
            }
            "wait-exit" | "wait-bell" => {
                let x = next()?;
                now(
                    out,
                    with(name, x, |w| {
                        w.until.push(match name {
                            "wait-exit" => Until::Exit,
                            _ => Until::Bell,
                        });
                        Ok(())
                    })?,
                )
            }
            "wait" => {
                let p = target(name, &next()?)?.map(WsRef::Id);
                let mut o = next()?;
                let built = opts(name, &mut o)?.clone();
                let Some(mut wait) = wait(&built.waits, false, true) else {
                    return Err(fail(
                        "no_condition",
                        "wait needs a condition: wait-text, wait-exit, …",
                    ));
                };
                wait.all = false;
                later(ask(
                    Command::Wait {
                        targets: vec![p],
                        wait: *wait,
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
                                "not_a_key",
                                format!("{word:?} is no key's name (key-names lists them)"),
                            )
                        })?;
                        built.inputs.push(Input::Key(press));
                    }
                    "paste" => built.inputs.push(Input::Paste(string(name, &v)?)),
                    "paste-file" => {
                        let path = string(name, &v)?;
                        let read = bounded(&path, 900 * 1024)
                            .map_err(|e| fail("paste", format!("{path}: {e}")))?;
                        if read.len() > 900 * 1024 {
                            return Err(fail(
                                "paste",
                                format!("{path}: more than 900 KiB, the most one paste takes"),
                            ));
                        }
                        let text = String::from_utf8(read)
                            .map_err(|_| fail("paste", format!("{path}: not UTF-8 text")))?;
                        built.inputs.push(Input::Paste(text));
                    }
                    // The input just before it, n times in all.
                    _ => {
                        let n: usize = count(name, &v)
                            .ok()
                            .filter(|n| *n > 0 && *n <= 1000)
                            .ok_or_else(|| fail("range", "a count from 1 to 1000"))?;
                        let Some(last) = built.inputs.last().cloned() else {
                            return Err(fail(
                                "no_keys",
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
                    return Err(fail("no_keys", "send-keys needs keys: keys str hello text"));
                }
                let mut then = wait(&built.waits, true, self.settle);
                if let Some(wait) = &mut then {
                    let (echo, lines) = crate::command::typed_lines(&built.inputs);
                    wait.echo = echo;
                    wait.lines = lines;
                }
                let waited = then.is_some();
                let (targets, to_client) = match &to {
                    // Typed at its terminal: whatever pane they reach is the
                    // client's to choose, so there is none to wait on here.
                    Value::App(Fux::Client(_)) if waited && !self.settle => {
                        return Err(fail(
                            "unused",
                            "send-keys to a client takes no wait: wait on the pane after (PANE opts … wait)",
                        ));
                    }
                    Value::App(Fux::Client(c)) => (Vec::new(), Some(*c)),
                    other @ (Value::Int(_)
                    | Value::Bool(_)
                    | Value::Str(_)
                    | Value::List(_)
                    | Value::App(_)) => {
                        let target = target(name, other)
                            .map_err(|_| wrong(name, "a pane, tab, workspace or client", other))?;
                        (vec![target.map(WsRef::Id)], None)
                    }
                };
                later(ask(
                    Command::SendKeys {
                        targets,
                        client: to_client,
                        inputs: built.inputs,
                        then,
                    },
                    Then::Outcome { waited },
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
                            "bad_value",
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
                let m = match next()? {
                    Value::App(Fux::Mouse(m)) => m,
                    other @ (Value::Int(_)
                    | Value::Bool(_)
                    | Value::Str(_)
                    | Value::List(_)
                    | Value::App(_)) => {
                        return Err(wrong(name, "a mouse event (from mouse)", &other));
                    }
                };
                let then = wait(&m.waits, true, self.settle);
                let waited = then.is_some();
                later(ask(
                    Command::SendMouse {
                        target: Some(p),
                        mouse: m.mouse,
                        row: m.row,
                        col: m.col,
                        then,
                    },
                    Then::Outcome { waited },
                ))
            }
            "signal" => {
                let p = pane(name, &next()?)?;
                let signal = string(name, &next()?)?;
                use fuxix::process::Signal;
                let signal = match signal.as_str() {
                    "TERM" => Signal::Term,
                    "KILL" => Signal::Kill,
                    "INT" => Signal::Int,
                    "HUP" => Signal::Hup,
                    "QUIT" => Signal::Quit,
                    "USR1" => Signal::Usr1,
                    "USR2" => Signal::Usr2,
                    "STOP" => Signal::Stop,
                    "CONT" => Signal::Cont,
                    "TSTP" => Signal::Tstp,
                    "WINCH" => Signal::Winch,
                    other => {
                        return Err(fail(
                            "bad_value",
                            format!(
                                "signal: {other:?} is not TERM, KILL, INT, HUP, QUIT, USR1, USR2, STOP, CONT, TSTP or WINCH"
                            ),
                        ));
                    }
                };
                // From a command line: answers once the program exited or settled.
                let then = wait(&Waits::default(), true, self.settle);
                let waited = then.is_some();
                later(ask(
                    Command::Signal {
                        target: Some(p),
                        signal,
                        then,
                    },
                    Then::Outcome { waited },
                ))
            }
            // ----------------------------------------------------- reading
            "capture" => {
                let p = pane(name, &next()?)?;
                let mut o = next()?;
                let built = opts(name, &mut o)?.clone();
                let then = if built.json { Then::Text } else { Then::Answer };
                // From a command line, read once the pane settles.
                let settle = self
                    .settle
                    .then(|| built.waits.timeout.unwrap_or(SETTLE_TIMEOUT));
                later(ask(
                    Command::CapturePane {
                        target: Some(p),
                        history: built.history,
                        view: built.view,
                        json: built.json,
                        settle,
                    },
                    then,
                ))
            }
            "ls" => later(ask(
                Command::Ls {
                    json: false,
                    target: None,
                    get: None,
                },
                Then::Answer,
            )),
            "info" => {
                let t = target(name, &next()?)?;
                later(ask(
                    Command::Ls {
                        json: false,
                        target: Some(t.map(WsRef::Id)),
                        get: None,
                    },
                    Then::Answer,
                ))
            }
            "get" => {
                let t = target(name, &next()?)?;
                let field = string(name, &next()?)?;
                later(ask(
                    Command::Ls {
                        json: false,
                        target: Some(t.map(WsRef::Id)),
                        get: Some(field),
                    },
                    Then::Value,
                ))
            }
            "json" => match next()? {
                // Among the options, as after what they make: `opts json
                // capture` is `opts capture json`.
                Value::App(Fux::Opts(mut opts)) => {
                    opts.json = true;
                    now(out, Value::App(Fux::Opts(opts)))
                }
                Value::App(Fux::Answer(answer)) => match (&answer.json, &answer.query) {
                    (Some(json), _) => now(out, Value::str(json.clone())),
                    (None, Some(query)) => later(ask(as_json(query.clone()), Then::Text)),
                    (None, None) => now(
                        out,
                        Value::str(crate::json::Json::str(answer.text.clone()).render()),
                    ),
                },
                other @ (Value::Int(_)
                | Value::Bool(_)
                | Value::Str(_)
                | Value::List(_)
                | Value::App(_)) => now(out, Value::str(json_of(&other).render())),
            },
            "events" | "events-json" => later(ask(
                Command::Events {
                    targets: Vec::new(),
                    json: name == "events-json",
                },
                Then::Nothing,
            )),
            "events-of" | "events-of-json" => {
                let t = target(name, &next()?)?;
                later(ask(
                    Command::Events {
                        targets: vec![t.map(WsRef::Id)],
                        json: name == "events-of-json",
                    },
                    Then::Nothing,
                ))
            }
            "dump" => later(ask(Command::DumpLayout { target: None }, Then::Text)),
            "dump-of" => {
                let t = target(name, &next()?)?;
                later(ask(
                    Command::DumpLayout {
                        target: Some(t.map(WsRef::Id)),
                    },
                    Then::Text,
                ))
            }
            // ---------------------------------------------------- changing
            "kill" => {
                let command = match target(name, &next()?)? {
                    AnyRef::Pane(p) => Command::KillPane { target: Some(p) },
                    AnyRef::Tab(t) => Command::KillTab { target: Some(t) },
                    AnyRef::Workspace(w) => Command::KillWorkspace {
                        target: Some(WsRef::Id(w)),
                    },
                };
                later(ask(command, Then::Nothing))
            }
            "rename" | "notify" => {
                let t = target(name, &next()?)?.map(WsRef::Id);
                let given = text(name, &next()?)?;
                let command = match name {
                    "rename" => Command::Rename {
                        target: t,
                        name: given,
                    },
                    _ => Command::Notify {
                        target: Some(t),
                        text: given,
                    },
                };
                later(ask(command, Then::Nothing))
            }
            "notify-all" => later(ask(
                Command::Notify {
                    target: None,
                    text: text(name, &next()?)?,
                },
                Then::Nothing,
            )),
            "resize" => {
                let t = target(name, &next()?)?.map(WsRef::Id);
                let (cols, rows) = (count::<u16>(name, &next()?)?, count::<u16>(name, &next()?)?);
                let size = fux_vt::Size::new(rows, cols)
                    .ok()
                    .filter(|_| cols <= 500 && rows <= 500)
                    .ok_or_else(|| fail("range", "columns and rows each 1 to 500"))?;
                later(ask(
                    Command::Resize {
                        what: Resized::Tabs(t),
                        size: Some(size),
                    },
                    Then::Nothing,
                ))
            }
            "resize-auto" => {
                let t = target(name, &next()?)?.map(WsRef::Id);
                later(ask(
                    Command::Resize {
                        what: Resized::Tabs(t),
                        size: None,
                    },
                    Then::Nothing,
                ))
            }
            "resize-client" => {
                let c = client(name, &next()?)?;
                let (cols, rows) = (count::<u16>(name, &next()?)?, count::<u16>(name, &next()?)?);
                let size = fux_vt::Size::new(rows, cols)
                    .ok()
                    .ok_or_else(|| fail("range", "columns and rows each from 1"))?;
                later(ask(
                    Command::Resize {
                        what: Resized::Client(c),
                        size: Some(size),
                    },
                    Then::Nothing,
                ))
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
                let by = match name {
                    "resize-x" => PaneResize::To {
                        cols: Some(extent),
                        rows: None,
                    },
                    _ => PaneResize::To {
                        cols: None,
                        rows: Some(extent),
                    },
                };
                later(ask(
                    Command::ResizePane {
                        target: Some(p),
                        by,
                    },
                    Then::Nothing,
                ))
            }
            "percent" => {
                let n = int(name, &next()?)?;
                let Some(n) = u8::try_from(n).ok().filter(|n| (1..=100).contains(n)) else {
                    return Err(fail("range", format!("percent takes 1 to 100, not {n}")));
                };
                now(out, Value::App(Fux::Extent(Extent::Percent(n))))
            }
            "move-border" => {
                let p = pane(name, &next()?)?;
                let way = direction(name, &next()?)?;
                let cells = count::<u16>(name, &next()?)?;
                later(ask(
                    Command::ResizePane {
                        target: Some(p),
                        by: PaneResize::Border(way, cells),
                    },
                    Then::Nothing,
                ))
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
                later(ask(
                    Command::MovePane {
                        target: Some(p),
                        to,
                    },
                    Then::Nothing,
                ))
            }
            "move-beside" => {
                let p = pane(name, &next()?)?;
                let way = direction(name, &next()?)?;
                later(ask(
                    Command::MovePane {
                        target: Some(p),
                        to: MoveTo::Beside(way),
                    },
                    Then::Nothing,
                ))
            }
            "swap-panes" => {
                let (a, b) = (pane(name, &next()?)?, pane(name, &next()?)?);
                later(ask(
                    Command::SwapPane {
                        target: Some(a),
                        with: SwapWith::Pane(b),
                    },
                    Then::Nothing,
                ))
            }
            "swap-toward" => {
                let p = pane(name, &next()?)?;
                let way = direction(name, &next()?)?;
                later(ask(
                    Command::SwapPane {
                        target: Some(p),
                        with: SwapWith::Toward(way),
                    },
                    Then::Nothing,
                ))
            }
            "reorder" => {
                let t = target(name, &next()?)?;
                let toward = sibling(name, &next()?)?;
                later(ask(
                    Command::Reorder {
                        subject: subject(t),
                        toward,
                    },
                    Then::Nothing,
                ))
            }
            "start-log" => {
                let p = pane(name, &next()?)?;
                let file = string(name, &next()?)?;
                later(ask(
                    Command::Log {
                        target: Some(p),
                        file: Some(file),
                    },
                    Then::Nothing,
                ))
            }
            "stop-log" => later(ask(
                Command::Log {
                    target: Some(pane(name, &next()?)?),
                    file: None,
                },
                Then::Nothing,
            )),
            "clear-history" => later(ask(
                Command::ClearHistory {
                    target: Some(pane(name, &next()?)?),
                },
                Then::Nothing,
            )),
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
                later(ask(
                    Command::PasteBuffer {
                        index,
                        target: Some(p),
                    },
                    Then::Nothing,
                ))
            }
            // ----------------------------------------------------- clients
            "attach" | "watch" => later(Ask::Local(Local::Attach {
                workspace: None,
                read_only: name == "watch",
            })),
            "attach-to" => {
                let workspace = match next()? {
                    Value::App(Fux::Workspace(w)) => w.to_string(),
                    other @ (Value::Int(_)
                    | Value::Bool(_)
                    | Value::Str(_)
                    | Value::List(_)
                    | Value::App(_)) => string(name, &other)?,
                };
                later(Ask::Local(Local::Attach {
                    workspace: Some(workspace),
                    read_only: false,
                }))
            }
            "headless" => {
                let (cols, rows) = (count::<u16>(name, &next()?)?, count::<u16>(name, &next()?)?);
                if cols == 0 || rows == 0 {
                    return Err(fail("range", "a size of at least 1 by 1"));
                }
                later(Ask::Local(Local::Headless { cols, rows }))
            }
            "focus" => {
                let c = client(name, &next()?)?;
                let pick = match string(name, &next()?)?.as_str() {
                    "next" => PanePick::Step(Sibling::Next),
                    "previous" => PanePick::Step(Sibling::Previous),
                    "last" => PanePick::Last,
                    other => PanePick::Toward(direction(name, &Value::str(other))?),
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
                let t = match next()? {
                    Value::App(Fux::Tab(t)) => t,
                    other @ (Value::Int(_)
                    | Value::Bool(_)
                    | Value::Str(_)
                    | Value::List(_)
                    | Value::App(_)) => {
                        return Err(wrong(name, "a tab", &other));
                    }
                };
                later(act(c, ClientAction::SelectTab(Pick::Id(t))))
            }
            "next-tab" | "previous-tab" => {
                let c = client(name, &next()?)?;
                let step = if name == "next-tab" {
                    Sibling::Next
                } else {
                    Sibling::Previous
                };
                later(act(c, ClientAction::SelectTab(Pick::Step(step))))
            }
            "show-workspace" => {
                let c = client(name, &next()?)?;
                let w = match next()? {
                    Value::App(Fux::Workspace(w)) => WsRef::Id(w),
                    other @ (Value::Int(_)
                    | Value::Bool(_)
                    | Value::Str(_)
                    | Value::List(_)
                    | Value::App(_)) => WsRef::Name(string(name, &other)?),
                };
                later(act(c, ClientAction::SelectWorkspace(Pick::Id(w))))
            }
            "next-workspace" | "previous-workspace" => {
                let c = client(name, &next()?)?;
                let step = if name == "next-workspace" {
                    Sibling::Next
                } else {
                    Sibling::Previous
                };
                later(act(c, ClientAction::SelectWorkspace(Pick::Step(step))))
            }
            "zoom" | "detach" => {
                let c = client(name, &next()?)?;
                let action = match name {
                    "zoom" => ClientAction::Zoom,
                    _ => ClientAction::Detach,
                };
                later(act(c, action))
            }
            // A client's own screens, which it opens itself: from its keys
            // or prompt, here; asked from elsewhere, relayed for it to run.
            "choose-tab"
            | "choose-workspace"
            | "copy-mode"
            | "command-prompt"
            | "choose-tab-for"
            | "choose-workspace-for"
            | "choose-pane"
            | "menu"
            | "rename-prompt"
            | "confirm-close" => {
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
                    true => later(Ask::Local(Local::Open(open))),
                    false => later(act(c, ClientAction::Run(open.code()))),
                }
            }
            "capture-client" => {
                let c = client(name, &next()?)?;
                later(ask_as(
                    c,
                    Command::Client {
                        client: Some(c),
                        action: ClientAction::Capture { json: false },
                    },
                    Then::Answer,
                ))
            }
            // ------------------------------------------------------ server
            "server" => later(Ask::Local(Local::Server(super::Chosen::Name(string(
                name,
                &next()?,
            )?)))),
            "socket" => later(Ask::Local(Local::Server(super::Chosen::Path(string(
                name,
                &next()?,
            )?)))),
            "start" => later(Ask::Local(Local::Start)),
            "status" => later(Ask::Local(Local::Status)),
            "servers" => later(Ask::Local(Local::Servers)),
            "kill-server" => later(ask(Command::KillServer { force: false }, Then::Nothing)),
            "force-kill-server" => later(Ask::Local(Local::ForceKill)),
            "set" => {
                let option = string(name, &next()?)?;
                let value = text(name, &next()?)?;
                later(ask(Command::Set { option, value }, Then::Nothing))
            }
            "grant" => {
                let workspace = match next()? {
                    Value::App(Fux::Workspace(w)) => WsRef::Id(w),
                    other @ (Value::Int(_)
                    | Value::Bool(_)
                    | Value::Str(_)
                    | Value::List(_)
                    | Value::App(_)) => WsRef::Name(string(name, &other)?),
                };
                let socket = string(name, &next()?)?;
                later(ask(
                    Command::Grant {
                        workspace: Some(workspace),
                        socket,
                        revoke: false,
                    },
                    Then::Text,
                ))
            }
            "revoke" => later(ask(
                Command::Grant {
                    workspace: None,
                    socket: string(name, &next()?)?,
                    revoke: true,
                },
                Then::Nothing,
            )),
            "version" => now(out, Value::str(env!("CARGO_PKG_VERSION"))),
            // ------------------------------------------------------ config
            "prefix" | "bind" | "bind-root" | "unbind" | "unbind-root" | "unbind-all" | "label"
            | "group" | "repeat" => self.configure(name, &mut next),
            "list-keys" => {
                // A client's own; else, from the command line, what the
                // config makes.
                let text = match &self.keymap {
                    Some(keymap) => keymap.describe(),
                    None => {
                        let config = crate::config::default_path().filter(|p| p.exists());
                        let keymap = match config {
                            Some(path) => super::dry_config(&path)
                                .map_err(|e| fail("config", e.to_string()))?,
                            None => crate::keymap::Keymap::default(),
                        };
                        keymap.describe()
                    }
                };
                let _ = write!(self.out, "{text}");
                Ok(Done::Now)
            }
            "key-names" => {
                let _ = writeln!(self.out, "{}", crate::keys::all_names().join(" "));
                Ok(Done::Now)
            }
            // -------------------------------------------------------- help
            "help" => {
                let _ = write!(self.out, "{}", crate::help::overview());
                Ok(Done::Now)
            }
            "help-agents" => {
                let _ = write!(self.out, "{}", crate::help::AGENTS);
                Ok(Done::Now)
            }
            "help-json" => {
                let _ = write!(self.out, "{}", crate::help::JSON);
                Ok(Done::Now)
            }
            "explain" => {
                let word = string(name, &next()?)?;
                let text = crate::help::explain(&word).ok_or_else(|| {
                    fail("unknown_word", format!("no word {word:?}: help lists them"))
                })?;
                let _ = write!(self.out, "{text}");
                Ok(Done::Now)
            }
            "check" => {
                let path = string(name, &next()?)?;
                let text = bounded(&path, 16 << 20)
                    .ok()
                    .filter(|t| t.len() <= 16 << 20)
                    .and_then(|t| String::from_utf8(t).ok())
                    .ok_or_else(|| {
                        fail(
                            "read",
                            format!("{path}: not a fuxth file of at most 16 MiB"),
                        )
                    })?;
                let tokens =
                    fuxth::token::source(&path, &text).map_err(|e| fail("check", e.to_string()))?;
                super::compile(&tokens).map_err(|e| fail("check", e.to_string()))?;
                // A config is run dry too: its bindings made, its settings
                // checked, nothing sent.
                let config = std::path::Path::new(&path)
                    .file_name()
                    .is_some_and(|n| n == "config.fux");
                if config {
                    super::dry_config(std::path::Path::new(&path))
                        .map_err(|e| fail("check", e.to_string()))?;
                }
                let _ = writeln!(self.out, "{path}: ok");
                Ok(Done::Now)
            }
            other => Err(fail("unknown_word", format!("{other}: no such word"))),
        }
    }
}

impl Host<'_> {
    /// A config's word: it changes the client's keymap.
    fn configure(
        &mut self,
        name: &str,
        next: &mut dyn FnMut() -> Result<Value<Fux>, Failure>,
    ) -> Result<Done<Ask>, Failure> {
        let Some(keymap) = self.keymap.as_deref_mut() else {
            return Err(fail(
                "not_config",
                format!("{name} changes a client's keys: in its config, or typed at its prompt"),
            ));
        };
        let refused = |why: String| fail("bad_binding", why);
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
                        self.last_bound = Some((keys, true));
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
                        self.last_bound = Some((keys, false));
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
                let Some((keys, root)) = self.last_bound.clone() else {
                    return Err(refused(format!(
                        "{name} changes the binding just made: bind first"
                    )));
                };
                let given = match name {
                    "repeat" => None,
                    _ => Some(string(name, &next()?)?),
                };
                let binding = match root {
                    true => keymap
                        .root
                        .iter_mut()
                        .find(|(k, _)| keys.first() == Some(k))
                        .map(|(_, b)| b),
                    false => keymap.binding_mut(&keys),
                };
                let Some(binding) = binding else {
                    return Err(refused("the binding just made is gone".to_owned()));
                };
                match (name, given) {
                    ("label", Some(text)) => binding.label = text,
                    ("group", Some(text)) => binding.group = text,
                    _ if root => {
                        return Err(refused(
                            "a binding without the prefix does not repeat".to_owned(),
                        ));
                    }
                    _ => keymap.set_repeat(&keys).map_err(refused)?,
                }
            }
        }
        Ok(Done::Now)
    }
}

/// The same query, its answer as JSON.
fn as_json(query: Command) -> Command {
    match query {
        Command::Ls { target, get, .. } => Command::Ls {
            json: true,
            target,
            get,
        },
        Command::CapturePane {
            target,
            history,
            view,
            ..
        } => Command::CapturePane {
            target,
            history,
            view,
            json: true,
            settle: None,
        },
        Command::Client {
            client,
            action: ClientAction::Capture { .. },
        } => Command::Client {
            client,
            action: ClientAction::Capture { json: true },
        },
        other @ (Command::KillServer { .. }
        | Command::NewWorkspace { .. }
        | Command::NewTab { .. }
        | Command::Split { .. }
        | Command::Run { .. }
        | Command::Respawn { .. }
        | Command::Notify { .. }
        | Command::Resize { .. }
        | Command::Wait { .. }
        | Command::SendMouse { .. }
        | Command::KillPane { .. }
        | Command::Signal { .. }
        | Command::KillTab { .. }
        | Command::KillWorkspace { .. }
        | Command::Rename { .. }
        | Command::MovePane { .. }
        | Command::SwapPane { .. }
        | Command::ResizePane { .. }
        | Command::SendKeys { .. }
        | Command::Events { .. }
        | Command::DumpLayout { .. }
        | Command::Grant { .. }
        | Command::Log { .. }
        | Command::ClearHistory { .. }
        | Command::Reorder { .. }
        | Command::Set { .. }
        | Command::Resolve { .. }
        | Command::ListBuffers
        | Command::ShowBuffer { .. }
        | Command::PasteBuffer { .. }
        | Command::Client { .. }) => other,
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
            assert!(!fuxth::PARSING.contains(&name), "{name}");
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

    #[test]
    fn a_default_capture_view_is_plain() {
        assert_eq!(crate::command::CaptureView::default(), Opts::default().view);
    }
}
