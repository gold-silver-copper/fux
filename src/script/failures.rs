//! The failures an agent can cause, a row each: the input, and the code and
//! exit status it fails with. At least one row per error code an agent can
//! cause from a command line or a key, and one per path to it; the codes
//! only a real server, socket or file can give are listed apart with why.
//! The rows hold what fux does at 8fe55f1e; a change of code or status
//! changes its row, and is listed in the PR as a change for agents.
use crate::code::Code;
use crate::session::testing;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Context {
    /// A command line: no client, settling as `fux WORDS` does.
    Cli,
    /// The same, after a program run started, %2, exited with status 3.
    CliExited,
    /// A key's binding on client c1: acting at once.
    Key,
}
use Context::{Cli, CliExited, Key};

struct Fails {
    code: &'static str,
    context: Context,
    input: &'static str,
    /// The code and status it fails with, as `fails CODE STATUS`.
    with: &'static str,
}

const fn fails(
    code: &'static str,
    context: Context,
    input: &'static str,
    with: &'static str,
) -> Fails {
    Fails {
        code,
        context,
        input,
        with,
    }
}

#[rustfmt::skip]
const ROWS: &[Fails] = &[
    fails("no_pane", Cli, "%9 kill", "fails no_pane 1"),
    fails("no_pane", Cli, "str nosuch pane", "fails no_pane 1"),
    fails("no_tab", Cli, "@9 kill", "fails no_tab 1"),
    fails("no_tab", Cli, "str nosuch tab", "fails no_tab 1"),
    fails("no_workspace", Cli, "+9 kill", "fails no_workspace 1"),
    fails("no_client", Cli, "str c9 client next-tab", "fails no_client 1"),
    fails("no_buffer", Cli, "5 buffer", "fails no_buffer 1"),
    fails("no_target", Cli, "opts new-workspace drop str main tab", "fails no_target 1"),
    fails("no_field", Cli, "%1 str nosuch get", "fails no_field 1"),
    fails("nothing_there", Cli, "@1 str next reorder", "fails nothing_there 1"),
    fails("name_taken", Cli, "opts str main name new-tab drop opts str main name new-tab", "fails name_taken 1"),
    fails("id", Cli, "opts str %1 id new-tab", "fails id 1"),
    fails("id", Cli, "opts str %5 id str %6 id new-tab", "fails id 1"),
    fails("id", Cli, "%1 opts str @5 id split", "fails id 1"),
    fails("bad_value", Cli, "%1 str sideways move-beside", "fails bad_value 1"),
    fails("bad_value", Cli, "%1 str NOPE signal", "fails bad_value 1"),
    fails("bad_value", Cli, "str +x workspace", "fails bad_value 1"),
    fails("bad_value", Cli, "opts str +x in new-tab", "fails bad_value 1"),
    fails("usage", Cli, "%1 opts str 'a\u{1}b' typed split", "fails usage 2"),
    fails("not_running", CliExited, "%2 keys str x text send-keys", "fails not_running 1"),
    fails("exited", CliExited, "%2 opts str x wait-text wait", "fails exited 3"),
    fails("only_shell", Cli, "%1 str WINCH signal", "fails only_shell 1"),
    fails("size", Cli, "%1 opts split drop @1 4 2 resize", "fails size 1"),
    fails("mouse", Cli, "%1 str click 0 0 mouse send-mouse", "fails mouse 1"),
    fails("not_a_directory", Cli, "opts str /nonexistent cwd new-tab", "fails not_a_directory 1"),
    fails("not_headless", Cli, "str c1 client 10 10 resize-client", "fails not_headless 1"),
    fails("timeout", Cli, "%1 opts str never wait-text 1 timeout wait", "fails timeout 124"),
    fails("not_here", Key, "str c1 client 10 10 headless", "fails not_here 1"),
    fails("unused", Cli, "%1 opts str x wait-text capture", "fails unused 1"),
    fails("unused", Cli, "%1 opts main-screen alt-screen capture", "fails unused 1"),
    fails("unused", Cli, "%1 opts str /bin/cat program str hi typed split", "fails unused 1"),
    fails("not_a_key", Cli, "%1 keys str Nope key send-keys", "fails not_a_key 1"),
    fails("no_keys", Cli, "%1 keys send-keys", "fails no_keys 1"),
    fails("not_a_client", Cli, "me", "fails not_a_client 1"),
    fails("paste", Cli, "%1 keys str /nonexistent paste-file send-keys", "fails paste 1"),
    fails("config", Cli, "str bell str maybe set", "fails config 1"),
    fails("read", Cli, "str /nonexistent.fux check", "fails read 1"),
    fails("open_quote", Cli, "str 'abc", "fails open_quote 1"),
    fails("open_quote", Cli, "opts str \"'abc\" command run", "fails open_quote 1"),
    fails("open_quote", Cli, "str shell str \"'abc\" set", "fails open_quote 1"),
    fails("unknown_word", Cli, "nosuchword", "fails unknown_word 1"),
    fails("missing_token", Cli, "str", "fails missing_token 1"),
    fails("bad_name", Cli, "define 1 end", "fails bad_name 1"),
    fails("bad_name", Cli, "str x let pane", "fails bad_name 1"),
    fails("bad_name", Cli, "%1 str 'a\u{61C}b' rename", "fails bad_name 1"),
    fails("not_top_level", Cli, "if define x end then", "fails not_top_level 1"),
    fails("unmatched", Cli, "end", "fails unmatched 1"),
    fails("unclosed", Cli, "define x", "fails unclosed 1"),
    fails("stack_underflow", Cli, "drop", "fails stack_underflow 1"),
    fails("stack_overflow", Cli, "begin 1 false until", "fails stack_overflow 1"),
    fails("call_depth", Cli, "define f f 1 end f", "fails call_depth 1"),
    fails("type", Cli, "str a 1 add", "fails type 1"),
    fails("type", Cli, "5 70000 0 mouse", "fails type 1"),
    fails("div_by_zero", Cli, "1 0 div", "fails div_by_zero 1"),
    fails("overflow", Cli, "9223372036854775807 1 add", "fails overflow 1"),
    fails("unset", Cli, "false if 1 let n then n", "fails unset 1"),
    fails("range", Cli, "list end 3 at", "fails range 1"),
    fails("fail", Cli, "str boom fail", "fails fail 1"),
    fails("not_run", Cli, "%1 opts respawn", "fails not_run 1"),
    fails("log", Cli, "%1 str /nonexistent/dir/log start-log", "fails log 1"),
    fails("people_attached", Cli, "kill-server", "fails people_attached 1"),
    fails("bad_binding", Cli, "str x str nosuchword bind", "fails bad_binding 1"),
    fails("no_program", Cli, "opts str '' command run", "fails no_program 1"),
];

/// The codes no row here gives, each with why.
#[rustfmt::skip]
const ELSEWHERE: &[(&str, &str)] = &[
    ("no_server", "the client finds no server: tests/agents.rs and the CLI, not a session"),
    ("mismatch", "a server of another protocol version: the client's handshake"),
    ("client", "the client's own failures (its terminal, its socket)"),
    ("not_granted", "a request over a granted socket, outside its workspace: the server's grant check"),
    ("grant", "making a grant's socket: the server's"),
    ("start", "a program that cannot start: this harness starts none"),
    ("signal", "sending a signal to a program that runs: needs a process"),
    ("stray_pane", "a command from a pane of another server: needs FUX_PANE and a server"),
    ("selection", "copy mode's selection, from its keys (copy.rs tests)"),
    ("ids_exhausted", "every id of a kind used: 65536 panes"),
    ("still_running", "respawn of a program that still runs: needs a process"),
    ("not_reading", "keys a program has no room to read: needs a process"),
    ("own_server", "kill-server from a pane of that server: needs FUX_PANE and a server"),
    ("closed", "a wait whose pane closed while it waited: needs time and a process"),
    ("gone", "an events stream whose target went: the server's stream"),
    ("lost", "an events stream whose reader fell behind: the server's stream"),
    ("stopped", "a wait on a program reading its terminal: needs a process"),
    ("cancelled", "the prefix then Esc while a key's program runs: needs a client"),
    ("not_layout", "a word a saved session may not hold: running a dump file"),
    ("not_config", "a word a config may not hold: running config.fux"),
    ("check", "a fuxth file with mistakes: check reads a file"),
    ("output", "writing to a closed stdout: the CLI"),
    ("nested", "attach from inside a fux pane: the CLI with FUX_PANE"),
    ("too_long", "a typed line past the PTY's line limit: needs a process"),
    ("steps", "a key's program past its step budget: the client runs it with one, this harness without"),
    ("contract", "a word of fux's leaving a count its effect doesn't allow: no input can cause it"),
];

fn failed(row: &Fails) -> Result<String, String> {
    let (mut session, client) = testing::attached(24, 80)?;
    if row.context == CliExited {
        testing::stack_as(&mut session, None, true, "opts str /bin/cat program run");
        let pane = *session.panes.keys().last().ok_or("no pane")?;
        session.exited(pane, crate::process::Ended::Exited(3));
    }
    let client = (row.context == Key).then_some(client);
    let left = testing::stack_as(&mut session, client, row.context != Key, row.input);
    Ok(left.last().cloned().unwrap_or_default())
}

#[test]
fn each_failure_has_its_code_and_status() -> Result<(), String> {
    let print = std::env::var_os("FAILS_PRINT").is_some();
    let mut wrong = Vec::new();
    for row in ROWS {
        if print {
            eprintln!("running {:?}", row.input);
        }
        let got = failed(row)?;
        if print {
            eprintln!("{} {:?} {:?} => {got:?}", row.code, row.context, row.input);
        }
        if got != row.with {
            wrong.push(format!(
                "{:?} `{}`: {got:?}, not {:?}",
                row.context, row.input, row.with
            ));
        }
    }
    match wrong.is_empty() {
        true => Ok(()),
        false => Err(wrong.join("\n")),
    }
}

#[test]
fn every_code_has_a_row_or_a_reason() -> Result<(), String> {
    for code in Code::all() {
        let name = code.name();
        let rowed = ROWS.iter().any(|r| r.code == name);
        let apart = ELSEWHERE.iter().any(|(c, _)| *c == name);
        if !rowed && !apart {
            return Err(format!("{name}: no row, and no reason it has none"));
        }
        if rowed && apart {
            return Err(format!("{name}: both a row and a reason"));
        }
    }
    for row in ROWS {
        if !row.with.starts_with(&format!("fails {} ", row.code)) {
            return Err(format!(
                "row for {} fails otherwise: {}",
                row.code, row.with
            ));
        }
    }
    Ok(())
}
