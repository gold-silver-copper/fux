//! What each word leaves on the stack, a row per word, target kind, wait
//! and context, each asserting the whole stack the word leaves above a
//! sentinel. The rows hold what the words do (BASE's behaviour, wrong parts
//! included); a fix changes its rows. Expected stacks are written by hand.
use crate::session::testing;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Context {
    /// A command line: no client, settling as `fux WORDS` does.
    Cli,
    /// A key's binding on client c1: acting at once.
    Key,
}
use Context::{Cli, Key};

struct Row {
    context: Context,
    word: &'static str,
    kind: &'static str,
    wait: bool,
    code: &'static str,
    left: &'static [&'static str],
}

const fn row(
    context: Context,
    word: &'static str,
    kind: &'static str,
    wait: bool,
    code: &'static str,
    left: &'static [&'static str],
) -> Row {
    Row {
        context,
        word,
        kind,
        wait,
        code,
        left,
    }
}

/// The words whose effect is a range or whose target is a pane, a tab or
/// a workspace, as at 8fe55f1e, and the target kinds each was given.
const WORDS: &[&str] = &[
    "send-keys",
    "send-mouse",
    "signal",
    "wait",
    "info",
    "get",
    "events-of",
    "events-of-json",
    "dump-of",
    "kill",
    "rename",
    "notify",
    "resize",
    "resize-auto",
    "reorder",
    "menu",
    "rename-prompt",
    "confirm-close",
];
const KINDS: &[&str] = &["pane", "tab", "workspace", "client"];

#[rustfmt::skip]
const ROWS: &[Row] = &[
    row(Cli, "send-keys", "pane", false, "%1 keys str x text send-keys", &["string SENTINEL", "answer %1 empty · 1 bytes unread"]),
    row(Key, "send-keys", "pane", false, "%1 keys str x text send-keys", &["string SENTINEL"]),
    row(Cli, "send-keys", "pane", true, "%1 keys str x text str x wait-text send-keys", &["fails timeout 124"]),
    row(Key, "send-keys", "pane", true, "%1 keys str x text str x wait-text send-keys", &["fails timeout 124"]),
    row(Key, "send-keys", "pane", true, "%1 keys str x text 1 timeout send-keys", &["string SENTINEL", "answer %1 empty · 1 bytes unread"]),
    row(Cli, "send-mouse", "pane", false, "%1 str click 0 0 mouse send-mouse", &["string SENTINEL", "answer %1 empty · 12 bytes unread"]),
    row(Key, "send-mouse", "pane", false, "%1 str click 0 0 mouse send-mouse", &["string SENTINEL"]),
    row(Cli, "send-mouse", "pane", true, "%1 str click 0 0 mouse str x wait-text send-mouse", &["fails timeout 124"]),
    row(Key, "send-mouse", "pane", true, "%1 str click 0 0 mouse str x wait-text send-mouse", &["fails timeout 124"]),
    row(Cli, "signal", "pane", false, "%1 str WINCH signal", &["fails only_shell 1"]),
    row(Key, "signal", "pane", false, "%1 str WINCH signal", &["fails only_shell 1"]),
    row(Cli, "wait", "pane", false, "%1 opts wait", &["string SENTINEL", "answer %1 empty"]),
    row(Key, "wait", "pane", false, "%1 opts wait", &["string SENTINEL", "answer %1 empty"]),
    row(Cli, "wait", "pane", true, "%1 opts str x wait-text wait", &["string SENTINEL", "answer %1 empty · met \"x\""]),
    row(Key, "wait", "pane", true, "%1 opts str x wait-text wait", &["string SENTINEL", "answer %1 empty · met \"x\""]),
    row(Cli, "info", "pane", false, "%1 info", &["string SENTINEL", "answer   @1 main"]),
    row(Key, "info", "pane", false, "%1 info", &["string SENTINEL", "answer   @1 main"]),
    row(Cli, "get", "pane", false, "%1 str id get", &["string SENTINEL", "string %1"]),
    row(Key, "get", "pane", false, "%1 str id get", &["string SENTINEL", "string %1"]),
    row(Cli, "events-of", "pane", false, "%1 events-of", &["string SENTINEL"]),
    row(Key, "events-of", "pane", false, "%1 events-of", &["string SENTINEL"]),
    row(Cli, "events-of-json", "pane", false, "%1 events-of-json", &["string SENTINEL"]),
    row(Key, "events-of-json", "pane", false, "%1 events-of-json", &["string SENTINEL"]),
    row(Cli, "dump-of", "pane", false, "%1 dump-of", &["string SENTINEL", "string # fux: running this makes it again"]),
    row(Key, "dump-of", "pane", false, "%1 dump-of", &["string SENTINEL", "string # fux: running this makes it again"]),
    row(Cli, "kill", "pane", false, "%1 kill", &["string SENTINEL"]),
    row(Key, "kill", "pane", false, "%1 kill", &["string SENTINEL"]),
    row(Cli, "rename", "pane", false, "%1 str n rename", &["string SENTINEL"]),
    row(Key, "rename", "pane", false, "%1 str n rename", &["string SENTINEL"]),
    row(Cli, "notify", "pane", false, "%1 str hi notify", &["string SENTINEL"]),
    row(Key, "notify", "pane", false, "%1 str hi notify", &["string SENTINEL"]),
    row(Cli, "resize", "pane", false, "%1 40 10 resize", &["string SENTINEL"]),
    row(Key, "resize", "pane", false, "%1 40 10 resize", &["string SENTINEL"]),
    row(Cli, "resize-auto", "pane", false, "%1 resize-auto", &["string SENTINEL"]),
    row(Key, "resize-auto", "pane", false, "%1 resize-auto", &["string SENTINEL"]),
    row(Cli, "reorder", "pane", false, "%1 str next reorder", &["fails nothing_there 1"]),
    row(Key, "reorder", "pane", false, "%1 str next reorder", &["fails nothing_there 1"]),
    row(Cli, "menu", "pane", false, "str c1 client %1 menu", &["string SENTINEL"]),
    row(Key, "menu", "pane", false, "str c1 client %1 menu", &["string SENTINEL"]),
    row(Cli, "rename-prompt", "pane", false, "str c1 client %1 rename-prompt", &["string SENTINEL"]),
    row(Key, "rename-prompt", "pane", false, "str c1 client %1 rename-prompt", &["string SENTINEL"]),
    row(Cli, "confirm-close", "pane", false, "str c1 client %1 confirm-close", &["string SENTINEL"]),
    row(Key, "confirm-close", "pane", false, "str c1 client %1 confirm-close", &["string SENTINEL"]),
    row(Cli, "send-keys", "tab", false, "@1 keys str x text send-keys", &["string SENTINEL", "answer %1 empty · 1 bytes unread"]),
    row(Key, "send-keys", "tab", false, "@1 keys str x text send-keys", &["string SENTINEL"]),
    row(Cli, "send-keys", "tab", true, "@1 keys str x text str x wait-text send-keys", &["fails timeout 124"]),
    row(Key, "send-keys", "tab", true, "@1 keys str x text str x wait-text send-keys", &["fails timeout 124"]),
    row(Cli, "send-mouse", "tab", false, "@1 str click 0 0 mouse send-mouse", &["fails type 1"]),
    row(Key, "send-mouse", "tab", false, "@1 str click 0 0 mouse send-mouse", &["fails type 1"]),
    row(Cli, "send-mouse", "tab", true, "@1 str click 0 0 mouse str x wait-text send-mouse", &["fails type 1"]),
    row(Key, "send-mouse", "tab", true, "@1 str click 0 0 mouse str x wait-text send-mouse", &["fails type 1"]),
    row(Cli, "signal", "tab", false, "@1 str WINCH signal", &["fails type 1"]),
    row(Key, "signal", "tab", false, "@1 str WINCH signal", &["fails type 1"]),
    row(Cli, "wait", "tab", false, "@1 opts wait", &["string SENTINEL", "answer %1 empty"]),
    row(Key, "wait", "tab", false, "@1 opts wait", &["string SENTINEL", "answer %1 empty"]),
    row(Cli, "wait", "tab", true, "@1 opts str x wait-text wait", &["string SENTINEL", "answer %1 empty · met \"x\""]),
    row(Key, "wait", "tab", true, "@1 opts str x wait-text wait", &["string SENTINEL", "answer %1 empty · met \"x\""]),
    row(Cli, "info", "tab", false, "@1 info", &["string SENTINEL", "answer   @1 main"]),
    row(Key, "info", "tab", false, "@1 info", &["string SENTINEL", "answer   @1 main"]),
    row(Cli, "get", "tab", false, "@1 str id get", &["string SENTINEL", "string @1"]),
    row(Key, "get", "tab", false, "@1 str id get", &["string SENTINEL", "string @1"]),
    row(Cli, "events-of", "tab", false, "@1 events-of", &["string SENTINEL"]),
    row(Key, "events-of", "tab", false, "@1 events-of", &["string SENTINEL"]),
    row(Cli, "events-of-json", "tab", false, "@1 events-of-json", &["string SENTINEL"]),
    row(Key, "events-of-json", "tab", false, "@1 events-of-json", &["string SENTINEL"]),
    row(Cli, "dump-of", "tab", false, "@1 dump-of", &["string SENTINEL", "string # fux: running this makes it again"]),
    row(Key, "dump-of", "tab", false, "@1 dump-of", &["string SENTINEL", "string # fux: running this makes it again"]),
    row(Cli, "kill", "tab", false, "@1 kill", &["string SENTINEL"]),
    row(Key, "kill", "tab", false, "@1 kill", &["string SENTINEL"]),
    row(Cli, "rename", "tab", false, "@1 str n rename", &["string SENTINEL"]),
    row(Key, "rename", "tab", false, "@1 str n rename", &["string SENTINEL"]),
    row(Cli, "notify", "tab", false, "@1 str hi notify", &["string SENTINEL"]),
    row(Key, "notify", "tab", false, "@1 str hi notify", &["string SENTINEL"]),
    row(Cli, "resize", "tab", false, "@1 40 10 resize", &["string SENTINEL"]),
    row(Key, "resize", "tab", false, "@1 40 10 resize", &["string SENTINEL"]),
    row(Cli, "resize-auto", "tab", false, "@1 resize-auto", &["string SENTINEL"]),
    row(Key, "resize-auto", "tab", false, "@1 resize-auto", &["string SENTINEL"]),
    row(Cli, "reorder", "tab", false, "@1 str next reorder", &["fails nothing_there 1"]),
    row(Key, "reorder", "tab", false, "@1 str next reorder", &["fails nothing_there 1"]),
    row(Cli, "menu", "tab", false, "str c1 client @1 menu", &["string SENTINEL"]),
    row(Key, "menu", "tab", false, "str c1 client @1 menu", &["string SENTINEL"]),
    row(Cli, "rename-prompt", "tab", false, "str c1 client @1 rename-prompt", &["string SENTINEL"]),
    row(Key, "rename-prompt", "tab", false, "str c1 client @1 rename-prompt", &["string SENTINEL"]),
    row(Cli, "confirm-close", "tab", false, "str c1 client @1 confirm-close", &["string SENTINEL"]),
    row(Key, "confirm-close", "tab", false, "str c1 client @1 confirm-close", &["string SENTINEL"]),
    row(Cli, "send-keys", "workspace", false, "+1 keys str x text send-keys", &["string SENTINEL", "answer %1 empty · 1 bytes unread"]),
    row(Key, "send-keys", "workspace", false, "+1 keys str x text send-keys", &["string SENTINEL"]),
    row(Cli, "send-keys", "workspace", true, "+1 keys str x text str x wait-text send-keys", &["fails timeout 124"]),
    row(Key, "send-keys", "workspace", true, "+1 keys str x text str x wait-text send-keys", &["fails timeout 124"]),
    row(Cli, "send-mouse", "workspace", false, "+1 str click 0 0 mouse send-mouse", &["fails type 1"]),
    row(Key, "send-mouse", "workspace", false, "+1 str click 0 0 mouse send-mouse", &["fails type 1"]),
    row(Cli, "send-mouse", "workspace", true, "+1 str click 0 0 mouse str x wait-text send-mouse", &["fails type 1"]),
    row(Key, "send-mouse", "workspace", true, "+1 str click 0 0 mouse str x wait-text send-mouse", &["fails type 1"]),
    row(Cli, "signal", "workspace", false, "+1 str WINCH signal", &["fails type 1"]),
    row(Key, "signal", "workspace", false, "+1 str WINCH signal", &["fails type 1"]),
    row(Cli, "wait", "workspace", false, "+1 opts wait", &["string SENTINEL", "answer %1 empty"]),
    row(Key, "wait", "workspace", false, "+1 opts wait", &["string SENTINEL", "answer %1 empty"]),
    row(Cli, "wait", "workspace", true, "+1 opts str x wait-text wait", &["string SENTINEL", "answer %1 empty · met \"x\""]),
    row(Key, "wait", "workspace", true, "+1 opts str x wait-text wait", &["string SENTINEL", "answer %1 empty · met \"x\""]),
    row(Cli, "info", "workspace", false, "+1 info", &["string SENTINEL", "answer +1 main"]),
    row(Key, "info", "workspace", false, "+1 info", &["string SENTINEL", "answer +1 main"]),
    row(Cli, "get", "workspace", false, "+1 str id get", &["string SENTINEL", "string +1"]),
    row(Key, "get", "workspace", false, "+1 str id get", &["string SENTINEL", "string +1"]),
    row(Cli, "events-of", "workspace", false, "+1 events-of", &["string SENTINEL"]),
    row(Key, "events-of", "workspace", false, "+1 events-of", &["string SENTINEL"]),
    row(Cli, "events-of-json", "workspace", false, "+1 events-of-json", &["string SENTINEL"]),
    row(Key, "events-of-json", "workspace", false, "+1 events-of-json", &["string SENTINEL"]),
    row(Cli, "dump-of", "workspace", false, "+1 dump-of", &["string SENTINEL", "string # fux: running this makes it again"]),
    row(Key, "dump-of", "workspace", false, "+1 dump-of", &["string SENTINEL", "string # fux: running this makes it again"]),
    row(Cli, "kill", "workspace", false, "+1 kill", &["string SENTINEL"]),
    row(Key, "kill", "workspace", false, "+1 kill", &["string SENTINEL"]),
    row(Cli, "rename", "workspace", false, "+1 str n rename", &["string SENTINEL"]),
    row(Key, "rename", "workspace", false, "+1 str n rename", &["string SENTINEL"]),
    row(Cli, "notify", "workspace", false, "+1 str hi notify", &["string SENTINEL"]),
    row(Key, "notify", "workspace", false, "+1 str hi notify", &["string SENTINEL"]),
    row(Cli, "resize", "workspace", false, "+1 40 10 resize", &["string SENTINEL"]),
    row(Key, "resize", "workspace", false, "+1 40 10 resize", &["string SENTINEL"]),
    row(Cli, "resize-auto", "workspace", false, "+1 resize-auto", &["string SENTINEL"]),
    row(Key, "resize-auto", "workspace", false, "+1 resize-auto", &["string SENTINEL"]),
    row(Cli, "reorder", "workspace", false, "+1 str next reorder", &["fails nothing_there 1"]),
    row(Key, "reorder", "workspace", false, "+1 str next reorder", &["fails nothing_there 1"]),
    row(Cli, "menu", "workspace", false, "str c1 client +1 menu", &["string SENTINEL"]),
    row(Key, "menu", "workspace", false, "str c1 client +1 menu", &["string SENTINEL"]),
    row(Cli, "rename-prompt", "workspace", false, "str c1 client +1 rename-prompt", &["string SENTINEL"]),
    row(Key, "rename-prompt", "workspace", false, "str c1 client +1 rename-prompt", &["string SENTINEL"]),
    row(Cli, "confirm-close", "workspace", false, "str c1 client +1 confirm-close", &["string SENTINEL"]),
    row(Key, "confirm-close", "workspace", false, "str c1 client +1 confirm-close", &["string SENTINEL"]),
    row(Cli, "send-keys", "client", false, "str c1 client keys str x text send-keys", &["string SENTINEL", "answer "]),
    row(Key, "send-keys", "client", false, "str c1 client keys str x text send-keys", &["string SENTINEL"]),
    row(Cli, "send-keys", "client", true, "str c1 client keys str x text str x wait-text send-keys", &["fails unused 1"]),
    row(Key, "send-keys", "client", true, "str c1 client keys str x text str x wait-text send-keys", &["fails unused 1"]),
    row(Cli, "send-mouse", "client", false, "str c1 client str click 0 0 mouse send-mouse", &["fails type 1"]),
    row(Key, "send-mouse", "client", false, "str c1 client str click 0 0 mouse send-mouse", &["fails type 1"]),
    row(Cli, "send-mouse", "client", true, "str c1 client str click 0 0 mouse str x wait-text send-mouse", &["fails type 1"]),
    row(Key, "send-mouse", "client", true, "str c1 client str click 0 0 mouse str x wait-text send-mouse", &["fails type 1"]),
    row(Cli, "signal", "client", false, "str c1 client str WINCH signal", &["fails type 1"]),
    row(Key, "signal", "client", false, "str c1 client str WINCH signal", &["fails type 1"]),
    row(Cli, "wait", "client", false, "str c1 client opts wait", &["fails type 1"]),
    row(Key, "wait", "client", false, "str c1 client opts wait", &["fails type 1"]),
    row(Cli, "wait", "client", true, "str c1 client opts str x wait-text wait", &["fails type 1"]),
    row(Key, "wait", "client", true, "str c1 client opts str x wait-text wait", &["fails type 1"]),
    row(Cli, "info", "client", false, "str c1 client info", &["fails type 1"]),
    row(Key, "info", "client", false, "str c1 client info", &["fails type 1"]),
    row(Cli, "get", "client", false, "str c1 client str id get", &["fails type 1"]),
    row(Key, "get", "client", false, "str c1 client str id get", &["fails type 1"]),
    row(Cli, "events-of", "client", false, "str c1 client events-of", &["fails type 1"]),
    row(Key, "events-of", "client", false, "str c1 client events-of", &["fails type 1"]),
    row(Cli, "events-of-json", "client", false, "str c1 client events-of-json", &["fails type 1"]),
    row(Key, "events-of-json", "client", false, "str c1 client events-of-json", &["fails type 1"]),
    row(Cli, "dump-of", "client", false, "str c1 client dump-of", &["fails type 1"]),
    row(Key, "dump-of", "client", false, "str c1 client dump-of", &["fails type 1"]),
    row(Cli, "kill", "client", false, "str c1 client kill", &["fails type 1"]),
    row(Key, "kill", "client", false, "str c1 client kill", &["fails type 1"]),
    row(Cli, "rename", "client", false, "str c1 client str n rename", &["fails type 1"]),
    row(Key, "rename", "client", false, "str c1 client str n rename", &["fails type 1"]),
    row(Cli, "notify", "client", false, "str c1 client str hi notify", &["fails type 1"]),
    row(Key, "notify", "client", false, "str c1 client str hi notify", &["fails type 1"]),
    row(Cli, "resize", "client", false, "str c1 client 40 10 resize", &["fails type 1"]),
    row(Key, "resize", "client", false, "str c1 client 40 10 resize", &["fails type 1"]),
    row(Cli, "resize-auto", "client", false, "str c1 client resize-auto", &["fails type 1"]),
    row(Key, "resize-auto", "client", false, "str c1 client resize-auto", &["fails type 1"]),
    row(Cli, "reorder", "client", false, "str c1 client str next reorder", &["fails type 1"]),
    row(Key, "reorder", "client", false, "str c1 client str next reorder", &["fails type 1"]),
    row(Cli, "menu", "client", false, "str c1 client str c1 client menu", &["fails type 1"]),
    row(Key, "menu", "client", false, "str c1 client str c1 client menu", &["fails type 1"]),
    row(Cli, "rename-prompt", "client", false, "str c1 client str c1 client rename-prompt", &["fails type 1"]),
    row(Key, "rename-prompt", "client", false, "str c1 client str c1 client rename-prompt", &["fails type 1"]),
    row(Cli, "confirm-close", "client", false, "str c1 client str c1 client confirm-close", &["fails type 1"]),
    row(Key, "confirm-close", "client", false, "str c1 client str c1 client confirm-close", &["fails type 1"]),
];

/// Each row's stack, in a fresh session: a shell pane %1 in tab @1 of
/// workspace +1, showing `x` and asking for mouse reports, and client c1
/// attached.
fn left(row: &Row) -> Result<Vec<String>, String> {
    let (mut session, client) = testing::attached(24, 80)?;
    let pane = *session.panes.keys().next().ok_or("no pane")?;
    session.output(pane, b"x\r\n\x1b[?1000h");
    let client = (row.context == Key).then_some(client);
    let code = format!("str SENTINEL {}", row.code);
    Ok(testing::stack_as(
        &mut session,
        client,
        row.context == Cli,
        &code,
    ))
}

#[test]
fn each_word_leaves_what_its_row_says() -> Result<(), String> {
    let print = std::env::var_os("STACKS_PRINT").is_some();
    let mut wrong = Vec::new();
    for row in ROWS {
        let got = left(row)?;
        if print {
            println!("{:?} {:?} => {:?}", row.context, row.code, got);
        }
        let want: Vec<String> = (row.left.iter()).map(|s| (*s).to_owned()).collect();
        if got != want {
            wrong.push(format!(
                "{:?} `{}`: left {got:?}, not {want:?}",
                row.context, row.code
            ));
        }
    }
    match wrong.is_empty() {
        true => Ok(()),
        false => Err(wrong.join("\n")),
    }
}

#[test]
fn every_word_and_kind_has_rows() -> Result<(), String> {
    for word in WORDS {
        for kind in KINDS {
            for context in [Cli, Key] {
                if !ROWS
                    .iter()
                    .any(|r| r.word == *word && r.kind == *kind && r.context == context)
                {
                    return Err(format!("no row for {word} given a {kind} ({context:?})"));
                }
            }
        }
    }
    for word in ["send-keys", "send-mouse", "wait"] {
        if !ROWS.iter().any(|r| r.word == word && r.wait) {
            return Err(format!("no row for {word} with a wait"));
        }
    }
    Ok(())
}
