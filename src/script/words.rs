//! fux's words, one table of them (`words!`): each its family, name, what
//! it does to the stack in the names of the kinds it takes and gives
//! (`KINDS`, `NAMES`), a line of help, an example and the options it
//! takes; and what each does, run by the program's [`Host`].
use super::{Ask, Bound, Fux, Host, Local, On, Opts, Shell};
use crate::code::{Code, Raise};
use crate::command::{
    AnyRef, Buffer, Button, ClientAction, Command, Extent, Input, Kind, Members, Mouse, MoveTo,
    PanePick, PaneResize, Percent, Pick, Request, Resized, Sibling, Since, Source, SwapWith, To,
    Usage, Wait, Wheel, WsRef,
};
use crate::help;
use crate::id::{ClientId, PaneId};
use crate::keys::Direction;
use crate::layout::Axis;
use crate::screens::Open;
use fuxth::{Done, Failure, Value};
use std::num::NonZeroU32;
use std::ops::RangeInclusive;

/// `words! { Heading…; Act { Heading Variant "name" "effect" "about"
/// "example" [After: Opt…]; … } Family { … "example"; … } … }` expands to
/// `Group`, the headings of `help` in its order, each named as its
/// variant; an enum of each family's words (the family is how the word
/// runs: `Opt` builds options or a request, `Screen` opens a client's
/// screen, `Keys` changes its keys, `Act` acts); `Word`, a word of any
/// family; `Word::ALL`, every word, family by family, the acting words
/// last, so that in `help` a heading's options come before the words that
/// take them (each word's id in the vocabulary is its place here); `Word::row`, its
/// facts: its name, stack effect, help line, heading and an example; and
/// `Act::takes`, the options an acting word takes and what they go after,
/// which each of its rows says (`[]` for none).
macro_rules! words {
    (@takes) => {
        None
    };
    (@takes $after:ident: $($opt:ident)+) => {
        Some(Takes {
            after: After::$after,
            options: &[$(Opt::$opt),*],
        })
    };
    (@family $family:ident $($variant:ident)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $family {
            $($variant,)*
        }

        impl $family {
            pub const ALL: &[$family] = &[$($family::$variant),*];
        }

        impl std::fmt::Display for $family {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(Word::$family(*self).row().name)
            }
        }
    };
    ($($heading:ident)*;
    Act {
        $($a_group:ident $act:ident $a_name:literal $a_effect:literal $a_about:literal $a_example:literal
            [$($takes:tt)*];)*
    }
    $($family:ident {
        $($group:ident $variant:ident $name:literal $effect:literal $about:literal $example:literal;)*
    })*) => {
        /// A word's heading in `help`.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub enum Group {
            $($heading,)*
        }

        impl Group {
            pub fn name(self) -> &'static str {
                match self {
                    $(Group::$heading => stringify!($heading),)*
                }
            }
        }

        $(words!(@family $family $($variant)*);)*
        words!(@family Act $($act)*);

        /// A word of fux's vocabulary.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Word {
            $($family($family),)*
            Act(Act),
        }

        impl Word {
            pub const ALL: &[Word] = &[
                $($(Word::$family($family::$variant),)*)*
                $(Word::Act(Act::$act),)*
            ];

            pub fn row(self) -> &'static Row {
                match self {
                    $($(Word::$family($family::$variant) => &Row {
                        name: $name,
                        effect: $effect,
                        about: $about,
                        group: Group::$group,
                        example: $example,
                    },)*)*
                    $(Word::Act(Act::$act) => &Row {
                        name: $a_name,
                        effect: $a_effect,
                        about: $a_about,
                        group: Group::$a_group,
                        example: $a_example,
                    },)*
                }
            }
        }

        impl Act {
            /// The options it takes, if it takes any: it refuses others.
            pub fn takes(self) -> Option<&'static Takes> {
                match self {
                    $(Act::$act => words!(@takes $($takes)*).as_ref(),)*
                }
            }
        }
    };
}

/// What a word is.
pub struct Row {
    pub name: &'static str,
    /// What it does to the stack, in the names of `KINDS` and `NAMES`:
    /// `x secs -- x`. fuxth reads it, for how many values the word takes
    /// and gives, and their kinds.
    pub effect: &'static str,
    pub about: &'static str,
    pub group: Group,
    /// It in use, as a command line (or a config's line).
    pub example: &'static str,
}

/// The options a word takes, and what they go after.
pub struct Takes {
    pub after: After,
    pub options: &'static [Opt],
}

/// What a word's options go after: `opts`, for a word that makes its own
/// pane; else the pane (or tab, workspace or client) it acts on; or
/// either, for one that acts on a pane, tab or workspace, or on everything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    Opts,
    Pane,
    Either,
}

impl Word {
    /// The word so named.
    pub fn named(name: &str) -> Option<Word> {
        Word::ALL.iter().copied().find(|w| w.row().name == name)
    }
}

impl Act {
    /// The options it takes: none, for one that takes none.
    pub fn options(self) -> &'static [Opt] {
        self.takes().map_or(&[], |t| t.options)
    }
}

impl Opt {
    /// The words that take it.
    pub fn takers(self) -> Vec<Act> {
        let takes = |act: &Act| act.options().contains(&self);
        Act::ALL.iter().copied().filter(takes).collect()
    }
}

/// The words that start from opts, as the table says: `run, new-tab,
/// new-workspace, events-of and attach`, `last` before the last.
pub fn from_opts(last: &str) -> String {
    let opts = |act: &&Act| (act.takes()).is_some_and(|t| t.after != After::Pane);
    let names: Vec<&str> = (Act::ALL.iter().filter(opts))
        .map(|act| Word::Act(*act).row().name)
        .collect();
    crate::words::listed(&names, last)
}

/// fux's kinds of value, each as its values' `name` says it, as an error
/// says it, and, for one that must be used, what one left over is told.
#[rustfmt::skip]
pub const KINDS: [(&str, &str, Option<&str>); 19] = [
    ("pane", "a pane", None), ("tab", "a tab", None), ("workspace", "a workspace", None),
    ("client", "a client", None), ("size", "a size (from percent)", None),
    ("answer", "an action's answer", None),
    ("closed-answer", "a closed pane's answer", None),
    ("client-capture", "a client's capture", None),
    ("answers", "an answer of a tab's or workspace's panes", None), ("capture", "a capture", None),
    ("listing", "a listing", None), ("json", "a JSON value", None),
    ("opts", "options (from opts, for run, new-tab, new-workspace, events-of and attach; a pane's words take theirs on the pane: `%2 all-history capture`)", Some("nothing takes these options")),
    ("pane-request", "a pane request", Some("nothing acts on this request")),
    ("tab-request", "a tab request", Some("nothing acts on this request")),
    ("workspace-request", "a workspace request", Some("nothing acts on this request")),
    ("client-request", "a client request", Some("nothing acts on this request")),
    ("changed-request", "a changed request", Some("nothing acts on this request")),
    ("expected-request", "an expected request", Some("nothing acts on this request")),
];

/// Kinds taken where an effect names another: an action's answer is its
/// pane, and a workspace may be given by its name.
pub const TAKEN_AS: [(&str, &str); 3] = [
    ("pane", "answer"),
    ("pane", "closed-answer"),
    ("workspace", "string"),
];

/// The names the words' effects use beyond the kinds' own, each with the
/// kinds it stands for (`from>to`: what a kind is once the word passes it
/// through), and how an error says them; then the names of a string, and
/// of an integer.
#[rustfmt::skip]
pub const NAMES: &[(&str, &str, &str)] = &[
    ("closed-outcome", "closed-answer answers", "a closed pane or collection answer"),
    ("outcome", "answer answers", "an action's or wait's answer: a pane's, or a tab's or workspace's panes'"),
    ("x", "opts pane>pane-request answer>pane-request closed-answer>pane-request tab>tab-request workspace>workspace-request client>client-request pane-request tab-request workspace-request client-request changed-request expected-request",
        "the pane it goes on, under its own values (or a tab, workspace, client, a request on one, or opts)"),
    ("target", "pane>answer answer closed-answer>answer pane-request>answer expected-request>answer tab>answers tab-request>answers workspace>answers workspace-request>answers client>answers client-request>answers",
        "a pane, tab, workspace or client, or a request on one"),
    ("subject", "pane answer closed-answer pane-request", "a pane, or a request on one"),
    ("place", "pane>answer answer closed-answer>answer pane-request>answer changed-request>answer tab>answers tab-request>answers workspace>answers workspace-request>answers",
        "a pane, tab or workspace, or a request on one"),
    ("handle", "pane tab workspace answer closed-answer", "a pane, tab or workspace"),
    ("watched", "opts pane tab workspace answer closed-answer pane-request tab-request workspace-request", "a pane, tab or workspace, a request on one, or opts (everything)"),
    ("ref", "string pane tab workspace answer closed-answer", "a name, or a pane, tab or workspace"),
    ("text", "string integer boolean", "a string, a number or a boolean"),
    ("new", "integer boolean list", "a setting's value: a number, true or false, or a list (fux help set)"),
    ("var", "string integer boolean pane tab workspace answer closed-answer", "a string, a number, a boolean, or a pane, tab or workspace"),
    ("value", "integer boolean string list pane tab workspace client size answer closed-answer answers capture client-capture listing json",
        "a value (options and requests are none: what acts on them gives one)"),
    ("seen", "string answer capture", "a version: an action's or wait's answer, a pane's capture, or its text"),
    ("record", "listing", "a listing"),
    ("width", "integer size", "cells, or a percent"),
    ("height", "integer size", "cells, or a percent"),
    ("where", "tab workspace string", "a tab, a workspace, new-tab or new-workspace"),
];
pub const STRINGS: &str = "name dir key path line id file mode field setting socket way action code keys word hex version screen cursor";
pub const INTEGERS: &str = "cols rows secs n row col lines from to cells";

// Every word of fux's, by family; in help, under its heading.
words! {
    Finding Starting Waiting Input Reading Changing Clients Server Shell Config Help;
    Act {
        Finding Pane "pane" "ref -- pane" "the pane ref names: its given name, %N, a pane, an answer or a version" "fux str web pane capture" [];
        Finding Tab "tab" "ref -- tab" "the tab ref names: its name, @N, or a pane's tab" "fux %3 tab str logs rename" [];
        Finding Workspace "workspace" "ref -- workspace" "the workspace ref names: its name, +N, or a pane's or tab's workspace" "fux str agent workspace kill" [];
        Finding Here "here" "-- pane" "the pane this runs in (FUX_PANE)" "fux here str done notify" [];
        Finding Client "client" "name -- client" "a client by its id, c2" "fux str c1 client str left focus" [];
        Finding Me "me" "-- client" "the client whose key or prompt runs this" "str z str 'me zoom' bind" [];
        Finding Panes "panes" "handle -- list" "the panes of a pane (itself), a tab or a workspace, in layout order" "fux str agent workspace panes each capture print end" [];
        Finding Tabs "tabs" "handle -- list" "the tabs of a pane or tab (its own) or a workspace, in the workspace's order" "fux str agent workspace tabs each str name get print end" [];
        Finding Focused "focused" "client -- pane" "the pane a client focuses" "fux str c1 client focused capture" [];
        Starting Opts "opts" "-- opts" "empty options, for run, new-tab, new-workspace, events-of and attach (the other words take them on the pane)" "fux opts str agent in 80 24 size str ./app program run" [];
        Starting Run "run" "opts -- answer" "start a program in a tab of its own; in is required, name names its pane; its pane stays once it exits" "fux agent-opts str web name 120 40 size str ./app program str --flag arg run" [Opts: Name Size Cwd Env Program Arg In];
        Starting Split "split" "subject -- answer" "split a pane: a shell, or the program given" "fux %3 vertical split" [Pane: Horizontal Vertical Shares Cwd Env Program Arg];
        Starting NewTab "new-tab" "opts -- answer" "a tab with a shell" "fux opts str logs name 80 24 size str agent in new-tab" [Opts: Name Size Cwd Env In];
        Starting NewWorkspace "new-workspace" "opts -- answer" "a workspace with a tab and a shell" "fux opts str work name 80 24 size new-workspace" [Opts: Name Size Cwd Env];
        Starting Respawn "respawn" "subject -- answer" "start again in its pane the program it was given (run, split), or the one given now" "fux %3 kill-first respawn" [Pane: KillFirst Env Program Arg];
        Waiting Wait "wait" "place -- place" "wait until the pane's version differs from the one changed gives, else an action's answer's (%2 str y text send-keys wait), else now's, or the pane closes; a tab or workspace: any of the panes there as it begins" "A=$(fux %3 str y text send-keys); fux str \"$A\" changed 60 timeout wait" [Pane: Changed Timeout];
        Input SendKeys "send-keys" "target -- target" "send the keys on it to a pane, every pane of a tab, or a client as typed at its terminal; 900 KiB at most in all" "fux %3 str 'ls -l' text str Enter key send-keys" [Pane: Text Key Paste Bytes Expect];
        Input SendMouse "send-mouse" "subject -- answer" "send the mouse event on it" "fux %3 str click 2 10 mouse send-mouse" [Pane: Mouse];
        Input Signal "signal" "subject name -- answer" "TERM KILL INT HUP QUIT USR1 USR2 STOP CONT TSTP WINCH ILL ABRT FPE SEGV PIPE ALRM BUS TRAP SYS XCPU XFSZ VTALRM PROF TTIN TTOU URG CHLD, to the pane's foreground" "fux %3 str TERM signal" [];
        Reading Capture "capture" "subject -- capture" "the screen, as the words on the pane say" "fux %3 100 history capture" [Pane: History AllHistory Wrapped Full Styles Marks MainScreen AltScreen LastOutput Rows Cols];
        Reading Ls "ls" "-- listing" "workspaces, tabs, panes and clients" "fux ls" [];
        Reading Info "info" "handle -- record" "a pane, tab or workspace, all of it" "fux %3 info json" [];
        Reading Get "get" "handle field -- value" "one field of a pane, tab or workspace: version, exited, last_status, idle_ms, cwd" "fux %3 str exited get" [];
        Reading Json "json" "value -- string" "value as JSON (an answer as its JSON); a failed wait's answer is the message try's handler gets" "fux ls json" [];
        Reading Events "events" "--" "what happens, a JSON line each, as it happens" "fux events" [];
        Reading EventsOf "events-of" "watched --" "what happens in a pane, tab or workspace, or on opts everywhere, a JSON line each" "fux str agent workspace events-of" [Either: From];
        Reading Dump "dump" "-- text" "everything, as a program that makes it again (shells new, nothing typed into them)" "fux dump > all.fux; fux start && fux --file all.fux" [];
        Reading DumpOf "dump-of" "handle -- text" "a tab or workspace, as a program that makes it again (shells new, nothing typed into them)" "fux str work workspace dump-of > work.fux; fux --file work.fux" [];
        Changing Kill "kill" "handle --" "close a pane, tab or workspace, without asking" "fux %3 kill" [];
        Changing Rename "rename" "handle text --" "rename a pane, tab or workspace" "fux %3 str editor rename" [];
        Changing Notify "notify" "handle text --" "a notice on the clients showing it" "fux %3 str 'tests passed' notify" [];
        Changing NotifyAll "notify-all" "text --" "a notice on every client" "fux str 'back soon' notify-all" [];
        Changing Resize "resize" "handle cols rows --" "what a tab (or a pane's) is laid out on while no client shows it, 1 to 4,096 each" "fux @2 120 40 resize" [];
        Changing ResizeAuto "resize-auto" "handle --" "a tab laid out on a client's screen again (at its size until one shows it)" "fux @2 resize-auto" [];
        Changing ResizeX "resize-x" "pane width --" "a pane's width: cells, or a percent" "fux %3 30 percent resize-x" [];
        Changing ResizeY "resize-y" "pane height --" "a pane's height: cells, or a percent" "fux %3 10 resize-y" [];
        Changing Percent "percent" "n -- size" "n percent of the tab, for resize-x and resize-y" "fux %3 30 percent resize-x" [];
        Changing MoveBorder "move-border" "pane way cells --" "move the pane's nearest border: left right up down" "fux %3 str left 5 move-border" [];
        Changing MoveTo "move-to" "pane where --" "move a pane to a tab, a workspace, new-tab or new-workspace" "fux %3 str new-tab move-to" [];
        Changing MoveBeside "move-beside" "pane way --" "move a pane beside its neighbour: left right up down" "fux %3 str right move-beside" [];
        Changing SwapPanes "swap-panes" "pane pane --" "swap two panes" "fux %3 %4 swap-panes" [];
        Changing SwapToward "swap-toward" "pane way --" "swap a pane with its neighbour" "fux %3 str up swap-toward" [];
        Changing Reorder "reorder" "handle way --" "move one place: next or previous" "fux @2 str next reorder" [];
        Changing ClearHistory "clear-history" "pane --" "drop the pane's history" "fux %3 clear-history" [];
        Changing Buffers "buffers" "-- json" "paste buffer metadata, newest first: id, bytes and preview" "fux buffers" [];
        Changing Buffer "buffer" "id -- text" "exact paste buffer text by id or newest" "fux newest buffer" [];
        Changing PasteBuffer "paste-buffer" "pane id -- answer" "paste a buffer by id or newest into a pane; answers as send-keys does" "fux %3 newest paste-buffer" [];
        Clients Attach "attach" "opts --" "show the server in this terminal (bare fux is opts attach); with a size, be a client of no terminal instead" "fux opts str work in attach" [Opts: In Size Watch];
        Clients Focus "focus" "client way --" "left right up down next previous or last pane" "fux str c1 client str left focus" [];
        Clients FocusPane "focus-pane" "client pane --" "focus a pane" "fux str c1 client %3 focus-pane" [];
        Clients ShowTab "show-tab" "client tab --" "show a tab" "fux str c1 client @2 show-tab" [];
        Clients StepTab "step-tab" "client way --" "show the next or previous tab" "fux str c1 client str next step-tab" [];
        Clients ShowWorkspace "show-workspace" "client workspace --" "show a workspace" "fux str c1 client str work show-workspace" [];
        Clients StepWorkspace "step-workspace" "client way --" "show the next or previous workspace" "fux str c1 client str previous step-workspace" [];
        Clients Zoom "zoom" "client --" "zoom the focused pane, or restore" "fux str c1 client zoom" [];
        Clients Detach "detach" "client --" "detach a client" "fux str c1 client detach" [];
        Clients CaptureClient "capture-client" "client -- client-capture" "what a client's terminal shows" "fux str c1 client capture-client" [];
        Clients ResizeClient "resize-client" "client cols rows --" "a headless client's size, 1 to 4,096 each" "fux str c1 client 120 40 resize-client" [];
        Server Server "server" "name --" "the server named so (NAME.sock in fux's directory), for what follows" "fux str test server start agent-opts 80 24 size str ./app program run" [];
        Server Socket "socket" "path --" "the server on this socket, for what follows" "fux str ./test.sock socket status" [];
        Server Start "start" "--" "start the server in the background, if none answers" "fux start" [];
        Server Status "status" "-- record" "the server that answers, and its version; with none, fails no_server" "fux status || fux start" [];
        Server KillServer "kill-server" "--" "stop the server: not from its own panes, nor with people attached" "fux str test server kill-server" [];
        Server ForceKillServer "force-kill-server" "--" "stop it anyway, whatever fux version it is" "fux str test server force-kill-server" [];
        Server Set "set" "setting new --" "a setting of the server's, for every client: `fux help set` lists them" "fux str history-bytes 16777216 set" [];
        Server Version "version" "-- text" "this fux's version (a pane's version is `str version get` on the pane)" "fux version" [];
        Shell Args "args" "-- list" "the strings after fux --file FILE (none for any other program)" "fux args len" [];
        Shell Input "input" "-- string" "the rest of stdin, as UTF-8 text, 16 MiB at most; not when the program came from stdin" "ls | fux input lines len" [];
        Shell ReadFile "read-file" "path -- string" "a file's text, as UTF-8, 16 MiB at most" "fux str notes.txt read-file lines len" [];
        Shell Getenv "getenv" "name -- string" "an environment variable's value; one not set fails unset_var" "fux str HOME getenv" [];
        Shell Warn "warn" "value --" "print a value on stderr, a line" "fux str 'no tests found' warn 2 exit" [];
        Shell Exit "exit" "n --" "end the program now, what is left printed, with exit status n, 0 to 255" "fux str 'no tests found' warn 2 exit" [];
        Config ListKeys "list-keys" "--" "this client's keys (the bundled keys from the CLI), as config lines that make them" "fux list-keys" [];
        Config KeyNames "key-names" "--" "the names keys go by" "fux key-names" [];
        Help Help "help" "--" "the words, under their headings" "fux help" [];
        Help Explain "explain" "word --" "one word at length (json's says what json gives); or the guide to driving programs from a script or an agent: agents, its first screen, enough for most work; a topic of it (recipes keys read shells programs servers); guide, all of it; or limits, the bounds fux keeps" "fux str capture explain" [];
        Help Check "check" "file mode --" "check a file as program (compile only) or config (also run dry)" "fux str config.fux str config check" [];
    }
    Opt {
        Starting Name "name" "x name -- x" "the given name: run's pane, new-tab's tab, or new-workspace's workspace" "fux agent-opts str web name 80 24 size str ./app program run";
        Starting Size "size" "x cols rows -- x" "what its tab is laid out on while no client shows it, 1 to 4,096 each; needed from outside fux, else the caller's own screen or tab, as fux makes no size up; attach: be a client with no terminal, of this size, which prints its id and runs until signalled" "fux agent-opts 80 24 size str ./app program run";
        Starting Cwd "cwd" "x dir -- x" "where it starts" "fux agent-opts str /srv cwd 80 24 size str ./app program run";
        Starting Env "env" "x key var -- x" "a variable for it: text, or a pane, tab or workspace as its id (%3; an action's answer, its pane)" "fux agent-opts str PORT 8080 env 80 24 size str ./app program run";
        Starting Program "program" "x path -- x" "the program to start, in place of a shell" "fux agent-opts 80 24 size str ./app program run";
        Starting Arg "arg" "x text -- x" "an argument for the program" "fux agent-opts 80 24 size str ./app program str -v arg run";
        Starting Horizontal "horizontal" "x -- x" "split side by side" "fux %3 horizontal split";
        Starting Vertical "vertical" "x -- x" "split stacked" "fux %3 vertical split";
        Starting Shares "shares" "x n n -- x" "split: how the pane and the new one divide the pane's room, A to B (default: halved)" "fux %3 horizontal 2 1 shares split";
        Starting In "in" "x workspace -- x" "the workspace for run, new-tab or attach; run and new-tab create a missing name, or a fresh unnamed workspace with str new-workspace in" "fux opts str agent in 80 24 size new-tab";
        Starting Watch "watch" "x -- x" "attach without typing: keys do nothing but the prefix, then d, which detaches" "fux opts watch attach";
        Starting KillFirst "kill-first" "x -- x" "respawn: end the program first if it still runs" "fux %3 kill-first respawn";
        Waiting Changed "changed" "seen -- changed-request" "a wait from this version (%2:0.57.0.3.0:RUN, as an action, a wait or `str version get` gives it; or an answer or capture, its version) of the pane it names, not from now" "A=$(fux %3 str y text send-keys); fux str \"$A\" changed 60 timeout wait";
        Waiting Timeout "timeout" "x secs -- x" "how long a wait waits for a change before it answers timeout, else the server's wait-seconds" "fux %3 300 timeout wait";
        Input Expect "expect" "seen -- expected-request" "keys for the pane this version names, which send-keys sends only if the pane is still at it as they are queued, else sends none and fails stale; a version as changed takes it: a capture's is its pane's as it was read" "fux %3 capture let seen seen string str '[y/N]' contains if seen expect str y text send-keys drop then";
        Input Text "text" "x text -- x" "text, typed" "fux %3 str 'ls -l' text str Enter key send-keys";
        Input Key "key" "x key -- x" "a key by its name: Enter, C-c, Up, F5" "fux %3 str C-c key send-keys";
        Input Paste "paste" "x text -- x" "text as a paste, bracketed if the program turned that on (an end mark, ESC [201~, in it then dropped); else as it is" "fux %3 str 'a long prompt' paste str Enter key send-keys";
        Input Bytes "bytes" "x hex -- x" "bytes as they are, in hex (1b5b41, c3): part of a key's sequence or of a character" "fux %3 str 1b5b41 bytes send-keys";
        Input Mouse "mouse" "x action row col -- x" "click right middle press release wheel-up wheel-down or move, at a cell from 0" "fux %3 str click 2 10 mouse send-mouse";
        Reading From "from" "x cursor -- x" "events-of: from this cursor (SEQ:RUN, as ls and a stream's ready give it), not from now: what came after it, or how many of those the journal let go (missed)" "C=$(fux ls json | jq -r .events); fux opts str \"$C\" from events-of";
        Reading History "history" "x lines -- x" "capture: lines of history first" "fux %3 100 history capture";
        Reading AllHistory "all-history" "x -- x" "capture: all the history first" "fux %3 all-history capture";
        Reading Wrapped "wrapped" "x -- x" "capture: each screen row a line, as wrapped at the pane's width (the cells a mouse event counts), not joined into the lines printed" "fux %3 wrapped 0 2 rows capture";
        Reading Full "full" "x -- x" "capture: every row and cell, untrimmed" "fux %3 full capture";
        Reading Styles "styles" "x -- x" "capture: colours and attributes" "fux %3 styles capture";
        Reading Marks "marks" "x -- x" "capture: highlighted text (reverse video, or a background colour) as ⟦this⟧" "fux %3 marks capture";
        Reading MainScreen "main-screen" "x -- x" "capture: what was printed before a program took the screen" "fux %3 main-screen capture";
        Reading AltScreen "alt-screen" "x -- x" "capture: the full screen a program drew last" "fux %3 alt-screen capture";
        Reading LastOutput "last-output" "x -- x" "capture: the last shell command's output, or the running one's so far (a full-screen one's: its screen to the cursor); fails no_marks where no command was marked (OSC 133); one of main-screen, alt-screen and last-output at most" "fux %3 last-output capture";
        Reading Rows "rows" "x from to -- x" "capture: those rows only, from 0, each line they touch whole (with wrapped, those rows alone)" "fux %3 0 4 rows capture";
        Reading Cols "cols" "x from to -- x" "capture: those columns only, from 0" "fux %3 0 39 cols capture";
    }
    Screen {
        Clients ChooseTab "choose-tab" "client --" "a list of tabs to go to" "fux str c1 client choose-tab";
        Clients ChooseWorkspace "choose-workspace" "client --" "a list of workspaces to go to" "fux str c1 client choose-workspace";
        Clients ChooseTabFor "choose-tab-for" "client pane --" "a list of tabs to move the pane to" "fux str c1 client %3 choose-tab-for";
        Clients ChooseWorkspaceFor "choose-workspace-for" "client pane --" "a list of workspaces to move the pane to" "fux str c1 client %3 choose-workspace-for";
        Clients ChoosePane "choose-pane" "client pane --" "a list of panes to swap the pane with" "fux str c1 client %3 choose-pane";
        Clients Prompt "prompt" "client name text code --" "a line to type, titled name and starting as text: its submit key (Enter) runs code, the line on its stack" "fux str c1 client str 'rename tab' str logs str '@2 swap rename' prompt";
        Clients Confirm "confirm" "client text code --" "a question: its yes key (y) runs code" "fux str c1 client str 'close %3?' str '%3 kill' confirm";
        Clients CopyMode "copy-mode" "client --" "copy and select in the focused pane" "fux str c1 client copy-mode";
        Clients CommandPrompt "command-prompt" "client --" "a prompt for fuxth" "fux str c1 client command-prompt";
    }
    Keys {
        Config Prefix "prefix" "key --" "this client's prefix (config)" "str C-a prefix";
        Config Bind "bind" "keys code --" "keys after the prefix run fuxth (config)" "str 'g v' str 'me focused horizontal split drop' bind";
        Config BindRoot "bind-root" "key code --" "a key without the prefix runs fuxth (config)" "str M-n str 'me str next step-tab' bind-root";
        Config Label "label" "text --" "what the column says the last binding does, or a screen's hints the last key bound in it (keys of one label shown together)" "str 'g v' str 'me focused horizontal split drop' bind str 'split right' label";
        Config Group "group" "name --" "the column's heading for the last binding" "str 'g v' str 'me focused horizontal split drop' bind str panes group";
        Config Repeat "repeat" "--" "the last binding's layer repeats until Esc" "str H str 'me focused str left 2 move-border' bind repeat";
        Config Unbind "unbind" "keys --" "keys after the prefix run nothing" "str 'g v' unbind";
        Config UnbindRoot "unbind-root" "key --" "a key without the prefix runs nothing" "str M-n unbind-root";
        Config BindIn "bind-in" "screen key action --" "in one of fux's own screens (copy list line confirm), a key does one of its actions, as the default keys bind each" "str copy str g str top bind-in str top label";
        Config UnbindIn "unbind-in" "screen key --" "in that screen, the key does what it would unbound: in a line, a character types itself; a named key with modifiers does what it does alone" "str copy str g unbind-in";
        Config UnbindAll "unbind-all" "--" "no bindings at all, nor keys in fux's screens (Escape still leaves them)" "unbind-all";
    }
}

/// A failure of a word's own: its code and why.
fn fail(code: Raise, message: impl Into<String>) -> Failure {
    Failure::new(Code::from(code).into(), message)
}

/// How a word of fux's is used, for a mistake in using it: what goes
/// before it, what it leaves, and an example.
pub fn usage(word: &str) -> Option<String> {
    let docs = super::docs();
    let library = || Some(&*docs.iter().find(|doc| doc.name == word)?.effect);
    let core = || fuxth::Builtin::named(word).map(fuxth::Builtin::effect);
    // Fux's, the prelude's or library's, or the core's; else the
    // language's own syntax, said as it is written.
    let fuxs = Word::named(word);
    let effect = match fuxs {
        Some(w) => w.row().effect,
        None => match library().or_else(core) {
            Some(effect) => effect,
            None => {
                let (shape, _) = fuxth::Parsing::named(word)?.syntax();
                return Some(format!("use: {shape}"));
            }
        },
    };
    let (takes, leaves) = effect.split_once("--").unwrap_or((effect, ""));
    // What a request's word goes on: the pane, opts, or either, as the
    // words that take it do.
    let takes = takes.trim().to_uppercase();
    let request = takes.strip_prefix('X');
    let takers = match fuxs {
        Some(Word::Opt(opt)) => opt.takers(),
        Some(Word::Act(_) | Word::Screen(_) | Word::Keys(_)) | None => Vec::new(),
    };
    // A word whose options go on either counts for both.
    let has = |after| {
        let on = |t: &Takes| t.after == after || t.after == After::Either;
        (takers.iter()).any(|act| act.takes().is_some_and(on))
    };
    let (on, left) = match (has(After::Pane), has(After::Opts)) {
        (true, false) => ("PANE", "a request"),
        (false, true) => ("OPTS", "opts"),
        (true, true) | (false, false) => ("PANE-OR-OPTS", "a request (or opts)"),
    };
    let takes = match request.filter(|r| r.is_empty() || r.starts_with(' ')) {
        Some(rest) => format!("{on}{rest}"),
        None => takes,
    };
    let leaves = match leaves.trim() {
        "" => String::new(),
        "x" => format!(", which leaves {left} for the word that acts"),
        leaves => format!(", which leaves {leaves}"),
    };
    let example = fuxs.map_or_else(String::new, |w| format!("; e.g. {}", w.row().example));
    // A capture's option says so: it goes between opts and capture.
    let kind = match takers == [Act::Capture] {
        true => "a capture option, ",
        false => "",
    };
    Some(format!("use: {kind}{takes} {word}{leaves}{example}"))
}

fn wrong(wanted: &str, got: &Value<Fux>) -> Failure {
    fail(Raise::Type, format!("takes {wanted}, not {}", got.kind()))
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
/// JSON (`Answer::json`), anything else of fux's as its text.
fn json_of(value: &Value<Fux>) -> crate::json::Json {
    use crate::json::Json;
    match value {
        Value::Int(n) => Json::Int(*n),
        Value::Bool(b) => Json::Bool(*b),
        Value::Str(s) => Json::str(s.to_string()),
        // As deep as lists nest, at most fuxth's limit.
        Value::List(list) => Json::Array(list.items().iter().map(json_of).collect()),
        Value::App(Fux::Answer(answer, _)) => answer.json(),
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

/// A share of a split (`shares`): 1 or more, in a u32.
fn share(value: &Value<Fux>) -> Result<NonZeroU32, Failure> {
    let n = within(value, 1..=i64::from(u32::MAX))?;
    NonZeroU32::new(n).ok_or_else(|| fail(Raise::Range, "a share is 1 or more"))
}

/// An integer within `range`, as the type it goes in.
fn within<T: TryFrom<i64>>(value: &Value<Fux>, range: RangeInclusive<i64>) -> Result<T, Failure> {
    let n = int(value)?;
    let fits = T::try_from(n).ok().filter(|_| range.contains(&n));
    fits.ok_or_else(|| {
        let said = match (range.start(), range.end()) {
            (from, &i64::MAX) => format!("from {from}"),
            (from, to) => format!("{from} to {to}"),
        };
        fail(Raise::Range, format!("{n} is out of range: {said}"))
    })
}

fn buffer(value: &Value<Fux>) -> Result<Buffer, Failure> {
    let text = string(value)?;
    if text == "newest" {
        return Ok(Buffer::Newest);
    }
    text.parse().map(Buffer::Id).map_err(|()| {
        fail(
            Raise::BadValue,
            format!("{text:?} is not a buffer id (bN:RUN) or newest"),
        )
    })
}

/// A way one place along: next or previous.
fn sibling(value: &Value<Fux>) -> Result<Sibling, Failure> {
    one_of(
        value,
        &[("next", Sibling::Next), ("previous", Sibling::Previous)],
    )
}

/// One of a closed set, by its name; refused, naming each, if none.
fn one_of<T: Copy>(value: &Value<Fux>, all: &[(&str, T)]) -> Result<T, Failure> {
    let given = string(value)?;
    let found = all.iter().find(|(name, _)| *name == given);
    found.map(|(_, t)| *t).ok_or_else(|| {
        let names: Vec<&str> = all.iter().map(|(name, _)| *name).collect();
        let names = crate::words::listed(&names, "or");
        fail(Raise::BadValue, format!("{given:?} is not {names}"))
    })
}

/// A size: columns and rows each 1 to `view::MOST`, as a client's screen,
/// and so a tab that fits clients, may be.
fn laid_out(cols: &Value<Fux>, rows: &Value<Fux>) -> Result<crate::view::Screen, Failure> {
    let cells = 1..=i64::from(crate::view::MOST);
    let (cols, rows) = (within(cols, cells.clone())?, within(rows, cells)?);
    crate::view::Screen::new(rows, cols).map_err(|e| fail(Raise::Range, e))
}

/// The pane a value is: a pane, or an action's or wait's answer of one
/// (a closed one's too, whose words then find no pane).
fn as_pane(value: &Value<Fux>) -> Option<PaneId> {
    if let Value::App(Fux::Pane(p, _)) = value {
        return Some(*p);
    }
    if let Value::App(Fux::Answer(answer, _)) = value
        && let super::Answer::Pane(answer) = &**answer
    {
        return Some(answer.pane());
    }
    None
}

fn pane(value: &Value<Fux>) -> Result<PaneId, Failure> {
    as_pane(value).ok_or_else(|| wrong("a pane (from pane, run or split)", value))
}

fn client(value: &Value<Fux>) -> Result<ClientId, Failure> {
    let Value::App(Fux::Client(c, _)) = value else {
        return Err(wrong("a client (from me or client)", value));
    };
    Ok(*c)
}

/// A pane, tab or workspace.
fn target(value: &Value<Fux>) -> Result<AnyRef, Failure> {
    if let Some(p) = as_pane(value) {
        Ok(AnyRef::Pane(p))
    } else if let Value::App(Fux::Tab(t, _)) = value {
        Ok(AnyRef::Tab(*t))
    } else if let Value::App(Fux::Workspace(w, _)) = value {
        Ok(AnyRef::Workspace(*w))
    } else {
        Err(wrong("a pane, tab or workspace", value))
    }
}

/// A workspace, or its name: `+N` read as its id, as `parse_workspace` reads it.
fn workspace(value: &Value<Fux>) -> Result<WsRef, Failure> {
    if let Value::App(Fux::Workspace(w, _)) = value {
        return Ok(WsRef::Id(*w));
    }
    crate::command::parse_workspace(&string(value)?)
        .map_err(|e| fail(Raise::BadValue, e.to_string()))
}

fn direction(value: &Value<Fux>) -> Result<Direction, Failure> {
    one_of(value, &Direction::ALL.map(|way| (way.name(), way)))
}

/// Options a word would leave unused, refused rather than ignored: it
/// takes those `help` lists for it.
fn unused(word: Act, built: &Opts) -> Result<(), Failure> {
    let Some(option) = built.given.iter().find(|o| !word.options().contains(o)) else {
        return Ok(());
    };
    let instead = format!(
        "takes no {option}: {option} goes on {}",
        takers(Some(Word::Opt(*option)))
    );
    Err(fail(Raise::Unused, instead))
}

/// What a request is on: a pane, tab or workspace, a client, or the
/// version an action's or wait's answer holds, of its pane.
fn on(value: &Value<Fux>) -> Result<On, Failure> {
    if let Value::App(Fux::Client(c, _)) = value {
        return Ok(On::Client(*c));
    }
    if let Some(version) = held(value) {
        return Ok(On::Version(version));
    }
    target(value).map(On::Target)
}

/// The version an action's answer, or a wait's, holds: its pane's from
/// before what it did. A wait on it waits from there, so nothing that came
/// between is missed.
fn held(value: &Value<Fux>) -> Option<crate::command::Version> {
    match value {
        Value::App(Fux::Answer(answer, _)) => match &**answer {
            super::Answer::Pane(answer) => answer.version(),
            // A capture's is for `expect` and `changed` alone (`version`).
            super::Answer::Panes(_)
            | super::Answer::Capture(_)
            | super::Answer::Listing { .. }
            | super::Answer::Json(_)
            | super::Answer::Status(_) => None,
        },
        Value::Int(_) | Value::Bool(_) | Value::Str(_) | Value::List(_) | Value::App(_) => None,
    }
}

/// An explicit baseline is consumed only by its corresponding action.
const CHANGED: &str = "takes no request changed made: only wait uses its baseline";

const EXPECTED: &str = "takes no request expect made: only send-keys checks a version";

/// The version `value` holds (`Answer::version`), or is as text.
fn version(value: &Value<Fux>) -> Result<crate::command::Version, Failure> {
    let wanted = "a version: an action's or wait's answer, a pane's capture, or its text";
    let text = match value {
        Value::App(Fux::Answer(answer, _)) => {
            return answer.version().ok_or_else(|| wrong(wanted, value));
        }
        Value::Str(text) => text,
        other @ (Value::Int(_) | Value::Bool(_) | Value::List(_) | Value::App(_)) => {
            return Err(wrong(wanted, other));
        }
    };
    text.parse().map_err(|()| {
        let why = format!(
            "{text:?} is no version: PANE:RESPAWNS.SCREEN.BELLS.DONE.ENDED:RUN, as an action answers it"
        );
        fail(Raise::BadValue, why)
    })
}

/// The options or request `value` is: options stay options; what else it
/// goes on (a pane, tab, workspace or client, an action's answer) becomes
/// a request on that.
fn modifying(value: Value<Fux>) -> Result<Box<Opts>, Failure> {
    let built = match value {
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

/// The options or request a word of a request's (an option, a wait, keys,
/// a mouse event) makes of what it goes on, `next` giving the values it
/// takes: `changed` and `expect` make a request, on its version's pane.
fn modify(
    opt: Opt,
    next: &mut dyn FnMut() -> Result<Value<Fux>, Failure>,
) -> Result<Box<Opts>, Failure> {
    // `changed` and `expect` make a request; every other option goes on
    // one, or on options.
    let mut built = match matches!(opt, Opt::Changed | Opt::Expect) {
        true => Box::default(),
        false => modifying(next()?)?,
    };
    built.given.push(opt);
    match opt {
        Opt::Changed => built.on = Some(On::Changed(version(&next()?)?)),
        Opt::Expect => built.on = Some(On::Seen(version(&next()?)?)),
        Opt::From => {
            let text = string(&next()?)?;
            let Ok(cursor) = text.parse() else {
                let why =
                    format!("{text:?} is no cursor: SEQ:RUN, as ls and a stream's ready give it");
                return Err(fail(Raise::BadValue, why));
            };
            built.from = Some(cursor);
        }
        Opt::Name => built.name = Some(string(&next()?)?),
        Opt::Cwd => built.cwd = Some(string(&next()?)?.into()),
        Opt::In => built.place = Some(workspace(&next()?)?),
        Opt::History => built.history = Some(within(&next()?, COUNT)?),
        Opt::AllHistory => built.history = Some(usize::MAX),
        Opt::Size => built.size = Some(laid_out(&next()?, &next()?)?),
        Opt::Horizontal => built.axis = Some(Axis::Horizontal),
        Opt::Vertical => built.axis = Some(Axis::Vertical),
        Opt::Shares => built.shares = Some((share(&next()?)?, share(&next()?)?)),
        Opt::KillFirst => built.kill = true,
        Opt::Watch => built.watch = true,
        Opt::Wrapped => built.view.wrapped = true,
        Opt::Full => built.view.full = true,
        Opt::Styles => built.view.styles = true,
        Opt::Marks => built.view.marks = true,
        Opt::Timeout => {
            let secs = within(&next()?, COUNT)?;
            let limit = crate::config::Limit::read(secs).map_err(|why| fail(Raise::Range, why))?;
            built.timeout = Some(limit);
        }
        Opt::Text => built.inputs.push(Input::Text(text(&next()?)?)),
        Opt::Paste => built.inputs.push(Input::Paste(text(&next()?)?)),
        Opt::Bytes => built.inputs.push(Input::Bytes(hex(&string(&next()?)?)?)),
        Opt::Program => {
            let program = string(&next()?)?;
            match built.argv.first_mut() {
                Some(first) => *first = program,
                None => built.argv.push(program),
            }
        }
        Opt::Arg => {
            let arg = text(&next()?)?;
            if built.argv.is_empty() {
                return Err(fail(Raise::NoProgram, "comes after program"));
            }
            built.argv.push(arg);
        }
        Opt::Env => {
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
                .map_err(|why| fail(Raise::BadValue, why))?;
        }
        Opt::MainScreen => built.view.source = screen(&built, Source::Main)?,
        Opt::AltScreen => built.view.source = screen(&built, Source::Alternate)?,
        Opt::LastOutput => built.view.source = screen(&built, Source::LastOutput)?,
        Opt::Rows => built.view.rows = Some(span(opt, next)?),
        Opt::Cols => built.view.cols = Some(span(opt, next)?),
        Opt::Key => {
            let word = string(&next()?)?;
            let press = word.parse().map_err(|_| {
                fail(
                    Raise::NotAKey,
                    format!("{word:?} is no key's name (key-names lists them)"),
                )
            })?;
            built.inputs.push(Input::Key(press));
        }
        Opt::Mouse => {
            let mouse = one_of(&next()?, &MICE)?;
            built.mouse = Some((mouse, within(&next()?, CELL)?, within(&next()?, CELL)?));
        }
    }
    Ok(built)
}

/// The screen a capture reads, `source`: refused if one is given.
fn screen(built: &Opts, source: Source) -> Result<Source, Failure> {
    match built.view.source {
        Source::Shown => Ok(source),
        Source::Main | Source::Alternate | Source::LastOutput => {
            let why = "a capture reads one screen, and one is given";
            Err(fail(Raise::Unused, why))
        }
    }
}

/// The rows or columns a capture reads, from `next`: the first, then the
/// last, from 0.
fn span(
    opt: Opt,
    next: &mut dyn FnMut() -> Result<Value<Fux>, Failure>,
) -> Result<(u16, u16), Failure> {
    let (from, to) = (within(&next()?, CELL)?, within(&next()?, CELL)?);
    if from > to {
        let why = format!("{from} {to}: the first {opt} before the last");
        return Err(fail(Raise::Range, why));
    }
    Ok((from, to))
}

/// What an action acts on, and what it was given: a request; or a pane,
/// tab, workspace or client alone, given nothing. Refused if `word`
/// leaves something given unused.
fn request(word: Act, value: Value<Fux>) -> Result<(On, Opts), Failure> {
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
        let why = format!(
            "takes a pane, not options: what opts gives is for {}; the rest go on the pane \
             (`%2 all-history capture`, `%2 vertical split`)",
            from_opts("and")
        );
        return Err(fail(Raise::Type, why));
    };
    unused(word, &built)?;
    Ok((on, built))
}

/// The pane an action acts on, and what it was given.
fn request_pane(word: Act, value: Value<Fux>) -> Result<(PaneId, Opts), Failure> {
    match request(word, value)? {
        (On::Target(AnyRef::Pane(p)), built) => Ok((p, built)),
        (On::Version(version), built) => Ok((version.pane, built)),
        // Refused, not acted on unchecked (`unused` refuses it first today).
        (On::Seen(_), _) => Err(fail(Raise::Type, EXPECTED)),
        (On::Changed(_), _) => Err(fail(Raise::Type, CHANGED)),
        (On::Target(AnyRef::Tab(_) | AnyRef::Workspace(_)) | On::Client(_), _) => Err(fail(
            Raise::Type,
            "takes a pane (from pane, run or split), or a request on one",
        )),
    }
}

/// The options `value` holds, for `word`, which makes what they are for:
/// refused if it leaves one unused.
fn finished(word: Act, value: Value<Fux>) -> Result<Opts, Failure> {
    match value {
        Value::App(Fux::Opts(built)) if built.on.is_none() => {
            unused(word, &built)?;
            Ok(*built)
        }
        other @ (Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_)) => Err(wrong(
            &format!("options (from opts), as {word} takes"),
            &other,
        )),
    }
}

/// The words that take `word`'s options, listed: an option's takers;
/// every word that takes options, for any other word.
pub(super) fn takers(word: Option<Word>) -> String {
    let on = match word {
        Some(Word::Opt(opt)) => opt.takers(),
        Some(Word::Act(_) | Word::Screen(_) | Word::Keys(_)) | None => (Act::ALL.iter().copied())
            .filter(|act| act.takes().is_some())
            .collect(),
    };
    let names: Vec<&str> = on
        .into_iter()
        .map(|act| Word::Act(act).row().name)
        .collect();
    crate::words::listed(&names, "or")
}

/// The bytes `hex` spells, two digits each (`1b5b41`, `c3 a9`; blanks
/// between bytes ignored).
fn hex(hex: &str) -> Result<Vec<u8>, Failure> {
    let bad = || {
        fail(
            Raise::BadValue,
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
    (!bytes.is_empty()).then_some(bytes).ok_or_else(bad)
}

/// The most input one send-keys sends: a request holds it whole.
pub(crate) const INPUT_MAX: usize = 900 * 1024;
// A frame holds it, with room for the rest of its request; and an empty
// input queue its text and bytes, which go in as they are (`Pane::input`).
const _: () = assert!(INPUT_MAX < crate::protocol::MAX_PAYLOAD);
const _: () = assert!(INPUT_MAX <= crate::pane::INPUT_BYTES);

/// `inputs`, refused past what one send-keys sends. What they take is
/// what the request carries, which must fit in one frame: text and pastes
/// their bytes, keys their names' (`C-M-F12` 7), and each input after the
/// first 5 bytes more.
fn fits(inputs: &[Input]) -> Result<(), Failure> {
    let typed = (inputs.iter())
        .map(|input| match input {
            Input::Text(text) | Input::Paste(text) => text.len(),
            Input::Key(key) => key.to_string().len(),
            Input::Bytes(bytes) => bytes.len(),
        })
        .fold(0usize, usize::saturating_add)
        .saturating_add(inputs.len().saturating_sub(1).saturating_mul(5));
    match typed > INPUT_MAX {
        true => Err(fail(
            Raise::TooLong,
            format!(
                "{typed} bytes, more than {} KiB, the most it sends at once \
                 (text and pastes count their bytes, keys their names', each input \
                 after the first 5 more)",
                INPUT_MAX / 1024
            ),
        )),
        false => Ok(()),
    }
}

/// `resize-x` or `resize-y`: a pane's width or height, from `next`, cells
/// or a percent.
fn resize_to(
    axis: Axis,
    next: &mut dyn FnMut() -> Result<Value<Fux>, Failure>,
) -> Result<Done<Ask>, Failure> {
    let target = pane(&next()?)?;
    let extent = match next()? {
        Value::App(Fux::Extent(e)) => e,
        v @ Value::Int(_) => Extent::Cells(within(&v, CELL)?),
        other @ (Value::Bool(_) | Value::Str(_) | Value::List(_) | Value::App(_)) => {
            return Err(wrong("cells or a percent", &other));
        }
    };
    let by = PaneResize::To(axis, extent);
    tell(Command::ResizePane { target, by })
}

/// The pane, tab or workspace of `kind` that `text` names, looked for.
fn resolve(text: String, kind: Kind) -> Result<Done<Ask>, Failure> {
    Ok(Done::Later(ask(Command::Resolve { text, kind })))
}

/// What `pane`, `tab` or `workspace` finds of `kind`, named by `found`: a
/// handle (an action's answer is its pane) of the kind is itself; another
/// kind's is looked for by its id.
fn find(kind: Kind, found: Value<Fux>, out: &mut Vec<Value<Fux>>) -> Result<Done<Ask>, Failure> {
    if let Ok(id) = target(&found) {
        if id.kind() != kind {
            return resolve(id.to_string(), kind);
        }
        out.push(Value::App(super::handle(id, None)));
        return Ok(Done::Now);
    }
    match found {
        // An empty name names nothing: never the caller's own, which
        // `here` is, so that an empty variable closes nothing.
        Value::Str(s) if s.trim().is_empty() => Err(fail(
            Raise::NoTarget,
            format!(
                "an empty name names no {}: `here` is the pane this runs in",
                kind.name()
            ),
        )),
        Value::Str(s) => resolve(s.to_string(), kind),
        other @ (Value::Int(_) | Value::Bool(_) | Value::List(_) | Value::App(_)) => {
            Err(wrong("a name, or a pane, tab or workspace", &other))
        }
    }
}

/// A request for the panes or tabs of `target`, as a list of handles.
fn members_of(target: AnyRef, members: Members) -> Ask {
    ask(Command::Members { target, members })
}

fn ask(request: impl Into<Request>) -> Ask {
    let request = request.into();
    let of = if let Request::Do(
        Command::ShowBuffer {
            buffer: Buffer::Id(id),
        }
        | Command::PasteBuffer {
            buffer: Buffer::Id(id),
            ..
        },
    ) = &request
    {
        Some(id.run())
    } else {
        None
    };
    Ask::Server {
        request: Box::new(request),
        client: None,
        of,
    }
}

/// An order to the server that answers nothing.
fn tell(request: impl Into<Request>) -> Result<Done<Ask>, Failure> {
    Ok(Done::Later(ask(request)))
}

/// What this client does itself.
fn local(local: Local) -> Result<Done<Ask>, Failure> {
    Ok(Done::Later(Ask::Local(local)))
}

fn ask_as(client: ClientId, command: Command) -> Ask {
    Ask::Server {
        request: Box::new(command.into()),
        client: Some(client),
        of: None,
    }
}

fn on_client(client: ClientId, action: ClientAction) -> Ask {
    let command = Command::Client {
        client: Some(client),
        action,
    };
    ask_as(client, command)
}

/// An order to a client that answers nothing.
fn order(client: ClientId, action: ClientAction) -> Result<Done<Ask>, Failure> {
    Ok(Done::Later(on_client(client, action)))
}

impl fuxth::Host for Host<'_> {
    type Value = Fux;
    type Request = Ask;

    fn print(&mut self, line: &str) -> Result<(), Failure> {
        writeln!(self.out, "{line}").map_err(|e| fail(Raise::Output, e.to_string()))
    }

    fn call(
        &mut self,
        id: usize,
        args: fuxth::Args<'_, Fux>,
        out: &mut Vec<Value<Fux>>,
    ) -> Result<Done<Ask>, Failure> {
        let Some(&word) = Word::ALL.get(id) else {
            return Err(Failure::new(
                fuxth::Code::Contract,
                "no word of fux's has this id",
            ));
        };
        let mut of = self.context.of;
        let mut args = args;
        let mut next = || {
            let value = args.next().ok_or_else(|| {
                fail(
                    Raise::StackUnderflow,
                    format!("too few values: {}", word.row().effect),
                )
            })?;
            of = super::same_run(of, super::scope(&value))?;
            Ok(value)
        };
        let from = out.len();
        let result = self.dispatch(word, &mut next, out)?;
        for value in out.iter_mut().skip(from) {
            match value {
                Value::App(Fux::Opts(opts)) => {
                    let version = match opts.on {
                        Some(On::Version(v) | On::Changed(v) | On::Seen(v)) => Some(v.run),
                        _ => None,
                    };
                    opts.of = super::same_run(of, version)?;
                }
                Value::App(
                    Fux::Pane(_, run)
                    | Fux::Tab(_, run)
                    | Fux::Workspace(_, run)
                    | Fux::Client(_, run),
                ) => {
                    *run = super::same_run(*run, of)?;
                }
                Value::Int(_) | Value::Bool(_) | Value::Str(_) | Value::List(_) | Value::App(_) => {
                }
            }
        }
        match result {
            Done::Later(Ask::Server {
                request,
                client,
                of: used,
            }) => Ok(Done::Later(Ask::Server {
                request,
                client,
                of: super::same_run(of, used)?,
            })),
            Done::Later(Ask::Local(Local::Attach {
                workspace,
                read_only,
                size,
                ..
            })) => local(Local::Attach {
                of,
                workspace,
                read_only,
                size,
            }),
            result @ (Done::Now | Done::Later(_)) => Ok(result),
        }
    }
}

impl Host<'_> {
    fn dispatch(
        &mut self,
        word: Word,
        mut next: &mut dyn FnMut() -> Result<Value<Fux>, Failure>,
        out: &mut Vec<Value<Fux>>,
    ) -> Result<Done<Ask>, Failure> {
        let now = |out: &mut Vec<Value<Fux>>, value: Value<Fux>| {
            out.push(value);
            Ok(Done::Now)
        };
        let later = |ask: Ask| Ok(Done::Later(ask));
        let act = match word {
            Word::Act(act) => act,
            Word::Opt(opt) => return now(out, Value::App(Fux::Opts(modify(opt, &mut next)?))),
            Word::Screen(screen) => return self.open(screen, &mut next),
            Word::Keys(keys) => return self.configure(keys, &mut next),
        };
        let step_tab = |step| ClientAction::SelectTab(Pick::Step(step));
        let step_workspace = |step| ClientAction::SelectWorkspace(Pick::Step(step));
        match act {
            // ----------------------------------------------------- finding
            Act::Pane => find(Kind::Pane, next()?, out),
            Act::Tab => find(Kind::Tab, next()?, out),
            Act::Workspace => find(Kind::Workspace, next()?, out),
            Act::Here => resolve(String::new(), Kind::Pane),
            Act::Client => {
                let text = string(&next()?)?;
                let id = text
                    .parse()
                    .map_err(|e: Usage| fail(Raise::BadValue, e.to_string()))?;
                now(out, Value::App(Fux::Client(id, None)))
            }
            Act::Me => match self.context.client {
                Some(c) => now(out, Value::App(Fux::Client(c, self.context.of))),
                None => Err(fail(
                    Raise::NotAClient,
                    "the client whose key or prompt runs this: this runs from the command line",
                )),
            },
            Act::Focused => {
                let c = client(&next()?)?;
                later(ask_as(
                    c,
                    Command::Resolve {
                        text: String::new(),
                        kind: Kind::Pane,
                    },
                ))
            }
            // ---------------------------------------------------- starting
            Act::Opts => now(out, Value::App(Fux::Opts(Box::default()))),
            Act::Run => {
                let built = finished(act, next()?)?;
                if built.argv.is_empty() {
                    return Err(fail(
                        Raise::NoProgram,
                        "needs a program: agent-opts str ./app program run",
                    ));
                }
                let run = Command::Run {
                    workspace: built.place.ok_or_else(|| {
                        fail(
                            Raise::NoWorkspace,
                            "run needs a workspace: give opts str NAME in, or use agent-opts",
                        )
                    })?,
                    name: built.name,
                    size: built.size,
                    cwd: built.cwd,
                    env: built.env,
                    argv: built.argv,
                };
                later(ask(run))
            }
            Act::NewTab => {
                let built = finished(act, next()?)?;
                let tab = Command::NewTab {
                    target: built.place,
                    name: built.name,
                    size: built.size,
                    cwd: built.cwd,
                    env: built.env,
                };
                later(ask(tab))
            }
            Act::NewWorkspace => {
                let built = finished(act, next()?)?;
                let workspace = Command::NewWorkspace {
                    name: built.name,
                    size: built.size,
                    cwd: built.cwd,
                    env: built.env,
                };
                later(ask(workspace))
            }
            Act::Split => {
                let (p, built) = request_pane(act, next()?)?;
                let split = Command::Split {
                    axis: built.axis.unwrap_or(Axis::Horizontal),
                    target: p,
                    argv: built.argv,
                    cwd: built.cwd,
                    env: built.env,
                    shares: built.shares,
                };
                later(ask(split))
            }
            Act::Respawn => {
                let (p, built) = request_pane(act, next()?)?;
                let respawn = Command::Respawn {
                    target: p,
                    kill: built.kill,
                    env: built.env,
                    argv: built.argv,
                };
                later(ask(respawn))
            }
            // ----------------------------------------------------- waiting
            Act::Wait => {
                let (on, built) = request(act, next()?)?;
                // On a tab or workspace, in each of its panes.
                let since = match on {
                    On::Version(version) | On::Changed(version) => Since::Version(version),
                    On::Seen(_) => return Err(fail(Raise::Type, EXPECTED)),
                    On::Target(target) => Since::Now(target),
                    On::Client(_) => {
                        let why = "takes a pane, tab or workspace, not a client";
                        return Err(fail(Raise::Type, why));
                    }
                };
                let wait = Wait {
                    since,
                    timeout: built.timeout,
                };
                later(ask(Request::Wait(wait)))
            }
            // ------------------------------------------------------- input
            Act::SendKeys => {
                let (on, built) = request(act, next()?)?;
                if built.inputs.is_empty() {
                    return Err(fail(
                        Raise::NoKeys,
                        "needs keys: %2 str hello text send-keys",
                    ));
                }
                fits(&built.inputs)?;
                let to = match on {
                    On::Target(target) => To::Now(target),
                    On::Version(version) => To::Now(AnyRef::Pane(version.pane)),
                    On::Seen(version) => To::Seen(version),
                    On::Changed(_) => return Err(fail(Raise::Type, CHANGED)),
                    On::Client(c) => {
                        // Typed at its terminal: whatever pane they reach is
                        // the client's to choose, so there is none to wait on
                        // here.
                        if built.inputs.iter().any(|i| matches!(i, Input::Bytes(_))) {
                            return Err(fail(
                                Raise::Unused,
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
                        return later(ask(typed));
                    }
                };
                let keys = Command::SendKeys {
                    to,
                    inputs: built.inputs,
                };
                later(ask(keys))
            }
            Act::SendMouse => {
                let (p, built) = request_pane(act, next()?)?;
                let Some((mouse, row, col)) = built.mouse else {
                    return Err(fail(
                        Raise::NoKeys,
                        "needs a mouse event: %2 str click 3 10 mouse send-mouse",
                    ));
                };
                let mouse = Command::SendMouse {
                    target: p,
                    mouse,
                    row,
                    col,
                };
                later(ask(mouse))
            }
            Act::Signal => {
                let (p, _) = request_pane(act, next()?)?;
                let signals = fuxix::process::Signal::ALL.map(|signal| (signal.name(), signal));
                let signal = one_of(&next()?, &signals)?;
                let signal = Command::Signal { target: p, signal };
                later(ask(signal))
            }
            // ----------------------------------------------------- reading
            Act::Capture => {
                let (p, built) = request_pane(act, next()?)?;
                let capture = Command::CapturePane {
                    target: p,
                    history: built.history,
                    view: built.view,
                };
                later(ask(capture))
            }
            Act::Ls => {
                let ls = Command::Ls {
                    target: None,
                    get: None,
                };
                later(ask(ls))
            }
            Act::Info => {
                let info = Command::Ls {
                    target: Some(target(&next()?)?),
                    get: None,
                };
                later(ask(info))
            }
            Act::Panes => later(members_of(target(&next()?)?, Members::Panes)),
            Act::Tabs => later(members_of(target(&next()?)?, Members::Tabs)),
            Act::Get => {
                let target = Some(target(&next()?)?);
                let get = Some(string(&next()?)?);
                later(ask(Command::Ls { target, get }))
            }
            Act::Json => match next()? {
                // Options and requests are no value to convert: what acts
                // on them gives one.
                Value::App(Fux::Opts(_)) => Err(fail(
                    Raise::Unused,
                    "converts a value: it goes after what acts on the pane (%2 capture json)",
                )),
                other @ (Value::Int(_)
                | Value::Bool(_)
                | Value::Str(_)
                | Value::List(_)
                | Value::App(_)) => now(out, Value::str(json_of(&other).render())),
            },
            Act::Events => tell(Request::Events(None, None)),
            Act::EventsOf => {
                // Options alone stream everything; on a pane, tab or
                // workspace, what happens there.
                let (target, built) = match next()? {
                    Value::App(Fux::Opts(built)) if built.on.is_none() => {
                        unused(act, &built)?;
                        (None, *built)
                    }
                    other @ (Value::Int(_)
                    | Value::Bool(_)
                    | Value::Str(_)
                    | Value::List(_)
                    | Value::App(_)) => match request(act, other)? {
                        (On::Target(target), built) => (Some(target), built),
                        (On::Version(version), built) => (Some(AnyRef::Pane(version.pane)), built),
                        (On::Seen(_), _) => return Err(fail(Raise::Type, EXPECTED)),
                        (On::Changed(_), _) => return Err(fail(Raise::Type, CHANGED)),
                        (On::Client(_), _) => {
                            let why = "takes a pane, tab or workspace, or opts, not a client";
                            return Err(fail(Raise::Type, why));
                        }
                    },
                };
                tell(Request::Events(target, built.from))
            }
            Act::Dump => later(ask(Command::DumpLayout { target: None })),
            Act::DumpOf => {
                let target = Some(target(&next()?)?);
                later(ask(Command::DumpLayout { target }))
            }
            // ---------------------------------------------------- changing
            Act::Kill => tell(Command::Kill {
                target: target(&next()?)?,
            }),
            Act::Rename => {
                let target = target(&next()?)?;
                let name = text(&next()?)?;
                tell(Command::Rename { target, name })
            }
            Act::Notify => {
                let target = Some(target(&next()?)?);
                let text = text(&next()?)?;
                tell(Command::Notify { target, text })
            }
            Act::NotifyAll => tell(Command::Notify {
                target: None,
                text: text(&next()?)?,
            }),
            Act::Resize => {
                let target = target(&next()?)?;
                let size = laid_out(&next()?, &next()?)?;
                tell(Command::Resize(Resized::Tabs(target, Some(size))))
            }
            Act::ResizeAuto => tell(Command::Resize(Resized::Tabs(target(&next()?)?, None))),
            Act::ResizeClient => {
                let c = client(&next()?)?;
                let size = laid_out(&next()?, &next()?)?;
                tell(Command::Resize(Resized::Client(c, size)))
            }
            Act::ResizeX => resize_to(Axis::Horizontal, &mut next),
            Act::ResizeY => resize_to(Axis::Vertical, &mut next),
            Act::Percent => {
                let n = within(&next()?, Percent::SHARES)?;
                let share = Percent::new(n).ok_or_else(|| fail(Raise::Range, "no share"))?;
                now(out, Value::App(Fux::Extent(Extent::Percent(share))))
            }
            Act::MoveBorder => {
                let p = pane(&next()?)?;
                let way = direction(&next()?)?;
                let cells = within(&next()?, CELL)?;
                tell(Command::ResizePane {
                    target: p,
                    by: PaneResize::Border(way, cells),
                })
            }
            Act::MoveBeside => {
                let (target, way) = (pane(&next()?)?, direction(&next()?)?);
                let to = MoveTo::Beside(way);
                tell(Command::MovePane { target, to })
            }
            Act::SwapToward => {
                let (target, way) = (pane(&next()?)?, direction(&next()?)?);
                let with = SwapWith::Toward(way);
                tell(Command::SwapPane { target, with })
            }
            Act::MoveTo => {
                let p = pane(&next()?)?;
                let to = match next()? {
                    Value::App(Fux::Tab(t, _)) => MoveTo::Tab(t),
                    Value::App(Fux::Workspace(w, _)) => MoveTo::Workspace(WsRef::Id(w)),
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
            Act::SwapPanes => {
                let (a, b) = (pane(&next()?)?, pane(&next()?)?);
                tell(Command::SwapPane {
                    target: a,
                    with: SwapWith::Pane(b),
                })
            }
            Act::Reorder => {
                let t = target(&next()?)?;
                let toward = sibling(&next()?)?;
                tell(Command::Reorder { target: t, toward })
            }
            Act::ClearHistory => tell(Command::ClearHistory {
                target: pane(&next()?)?,
            }),
            Act::Buffers => later(ask(Command::ListBuffers)),
            Act::Buffer => later(ask(Command::ShowBuffer {
                buffer: buffer(&next()?)?,
            })),
            Act::PasteBuffer => {
                let p = pane(&next()?)?;
                let buffer = buffer(&next()?)?;
                later(ask(Command::PasteBuffer { buffer, target: p }))
            }
            // ----------------------------------------------------- clients
            Act::Attach => {
                let built = finished(act, next()?)?;
                local(Local::Attach {
                    of: None,
                    workspace: built.place,
                    read_only: built.watch,
                    size: built.size.map(|screen| screen.size()),
                })
            }
            Act::Focus => {
                let c = client(&next()?)?;
                let ways = Direction::ALL.map(|way| (way.name(), PanePick::Toward(way)));
                let steps = [
                    ("next", PanePick::Step(Sibling::Next)),
                    ("previous", PanePick::Step(Sibling::Previous)),
                    ("last", PanePick::Last),
                ];
                let pick = one_of(&next()?, &[ways.as_slice(), &steps].concat())?;
                order(c, ClientAction::SelectPane(pick))
            }
            Act::FocusPane => {
                let c = client(&next()?)?;
                let p = pane(&next()?)?;
                order(c, ClientAction::SelectPane(PanePick::Id(p)))
            }
            Act::ShowTab => {
                let c = client(&next()?)?;
                let t = next()?;
                let Value::App(Fux::Tab(tab, _)) = &t else {
                    return Err(wrong("a tab", &t));
                };
                order(c, ClientAction::SelectTab(Pick::Id(*tab)))
            }
            Act::ShowWorkspace => {
                let c = client(&next()?)?;
                let w = workspace(&next()?)?;
                order(c, ClientAction::SelectWorkspace(Pick::Id(w)))
            }
            Act::StepTab => order(client(&next()?)?, step_tab(sibling(&next()?)?)),
            Act::StepWorkspace => order(client(&next()?)?, step_workspace(sibling(&next()?)?)),
            Act::Zoom => order(client(&next()?)?, ClientAction::Zoom),
            Act::Detach => order(client(&next()?)?, ClientAction::Detach),
            Act::CaptureClient => {
                let c = client(&next()?)?;
                later(on_client(c, ClientAction::Capture))
            }
            // ------------------------------------------------------ server
            Act::Server => local(Local::Server(crate::socket::Choice::Name(
                string(&next()?)?,
            ))),
            Act::Socket => local(Local::Server(crate::socket::Choice::Path(
                string(&next()?)?.into(),
            ))),
            Act::Start => local(Local::Start),
            Act::Status => local(Local::Status),
            Act::KillServer => tell(Command::KillServer),
            Act::ForceKillServer => local(Local::ForceKill),
            Act::Set => {
                let name = string(&next()?)?;
                let setting = crate::config::Setting::read(&name, &next()?);
                tell(Command::Set(vec![
                    setting.map_err(|why| fail(Raise::Config, why))?,
                ]))
            }
            Act::Version => now(out, Value::str(env!("CARGO_PKG_VERSION"))),
            // ------------------------------------------------------- shell
            Act::Args => local(Local::Shell(Shell::Args)),
            Act::Input => local(Local::Shell(Shell::Input)),
            Act::ReadFile => local(Local::Shell(Shell::ReadFile(string(&next()?)?))),
            Act::Getenv => {
                let name = string(&next()?)?.parse();
                let name = name.map_err(|why: String| fail(Raise::BadValue, why))?;
                local(Local::Shell(Shell::Getenv(name)))
            }
            Act::Warn => local(Local::Shell(Shell::Warn(next()?.text()))),
            Act::Exit => local(Local::Shell(Shell::Exit(within(&next()?, 0..=255)?))),
            // ------------------------------------------------------ config
            // A client's own; else the bundled keys, without a user config.
            Act::ListKeys => {
                let text = match &self.keymap {
                    Some(keymap) => keymap.describe(),
                    None => super::default_keys()
                        .map_err(|e| fail(Raise::Config, e.to_string()))?
                        .describe(),
                };
                self.show(&text)
            }
            Act::KeyNames => self.show(&format!("{}\n", crate::keys::all_names().join(" "))),
            // -------------------------------------------------------- help
            Act::Help => self.show(&help::overview()),
            Act::Explain => {
                let word = string(&next()?)?;
                let text = help::explain(&word)
                    .ok_or_else(|| fail(Raise::UnknownWord, help::unknown(&word)))?;
                self.show(&text)
            }
            Act::Check => {
                let path = string(&next()?)?;
                let config = one_of(&next()?, &[("program", false), ("config", true)])?;
                let text =
                    super::read_file(path.as_ref()).map_err(|f| fail(Raise::Read, f.located()))?;
                let checked = |failed: super::Failed| fail(Raise::Check, failed.located());
                if config {
                    super::run_config(&path, &text, None, None).map_err(checked)?;
                } else {
                    let tokens = fuxth::token::source(&path, &text)
                        .map_err(|e| checked(super::Failed::from(e)))?;
                    super::compile(&tokens).map_err(checked)?;
                }
                self.show(&format!("{path}: ok\n"))
            }
        }
    }
}

/// `job` compiled as its keys or screen will run it, with the config's
/// definitions: its mistake said as `what`'s, of `code`.
fn checked(
    keymap: &crate::keymap::Keymap,
    job: &crate::screens::Job,
    code: Raise,
    what: &str,
) -> Result<(), Failure> {
    let mistake = |e: fuxth::Error| {
        let mut failed = fail(code, format!("{}, in {what}", e.message));
        failed.word = e.word;
        failed
    };
    let tokens = job.tokens().map_err(mistake)?;
    super::compiled(keymap.config.as_ref(), &tokens)
        .map(drop)
        .map_err(mistake)
}

/// The binding just made, `bound`, which `label` and `group` change.
fn just_made<'k>(
    keymap: &'k mut crate::keymap::Keymap,
    bound: Option<&Bound>,
) -> Result<&'k mut crate::keymap::Binding, Failure> {
    let refused = |why: &str| fail(Raise::BadBinding, why);
    let binding = match bound.ok_or_else(|| refused("changes the binding just made: bind first"))? {
        Bound::Root(key) => (keymap.root.iter_mut())
            .find(|(k, _)| k == key)
            .map(|(_, b)| b),
        Bound::Prefix(keys) => keymap.binding_mut(keys),
        Bound::In(..) => return Err(refused("a screen's key has a label only")),
    };
    binding.ok_or_else(|| refused("the binding just made is gone"))
}

impl Host<'_> {
    /// Writes `text` out.
    fn show(&mut self, text: &str) -> Result<Done<Ask>, Failure> {
        write!(self.out, "{text}").map_err(|e| fail(Raise::Output, e.to_string()))?;
        Ok(Done::Now)
    }

    /// A client's own screen, which it opens itself: from its keys or
    /// prompt, here; asked from elsewhere, relayed for it to run.
    fn open(
        &mut self,
        screen: Screen,
        next: &mut dyn FnMut() -> Result<Value<Fux>, Failure>,
    ) -> Result<Done<Ask>, Failure> {
        let c = client(&next()?)?;
        let here = self.context.client == Some(c);
        let open = match screen {
            Screen::ChooseTab => Open::ChooseTab(None),
            Screen::ChooseWorkspace => Open::ChooseWorkspace(None),
            Screen::CopyMode => Open::Copy,
            Screen::CommandPrompt => Open::Prompt(crate::screens::Prompt {
                title: ":".to_owned(),
                line: crate::view::Line::new(String::new()),
                action: None,
            }),
            Screen::ChooseTabFor => Open::ChooseTab(Some(pane(&next()?)?)),
            Screen::ChooseWorkspaceFor => Open::ChooseWorkspace(Some(pane(&next()?)?)),
            Screen::ChoosePane => Open::ChoosePane(pane(&next()?)?),
            Screen::Prompt => {
                let title = text(&next()?)?;
                // Its start typed, so the line holds no more, nor any other,
                // than one typed: text alone, refused whole if it is not.
                let start = text(&next()?)?;
                let mut line = crate::view::Line::new(String::new());
                let typed = match start.contains(char::is_control) {
                    true => Err("a line holds text alone, no control characters".to_owned()),
                    false => line.edit(&crate::act::Edit::Insert(start)),
                };
                typed.map_err(|why| fail(Raise::BadValue, why))?;
                let action = Some(self.action(&next()?, true, here)?);
                Open::Prompt(crate::screens::Prompt {
                    title,
                    line,
                    action,
                })
            }
            Screen::Confirm => {
                let question = text(&next()?)?;
                let action = self.action(&next()?, false, here)?;
                Open::Confirm(crate::screens::Confirm { question, action })
            }
        };
        match here {
            true => local(Local::Open(open)),
            false => order(c, ClientAction::Run(open.code())),
        }
    }

    /// The fuxth a prompt (`given`: the line typed on its stack) or a
    /// question runs: within `ACTION_MAX`, as the client holds it, outside
    /// any frame; and compiled now if it opens `here`, in the client's own
    /// program (one relayed is checked by the client it opens on, with its
    /// own config, as it runs the word again).
    fn action(&self, value: &Value<Fux>, given: bool, here: bool) -> Result<String, Failure> {
        let code = string(value)?;
        let (len, most) = (code.len(), crate::screens::ACTION_MAX);
        if len > most {
            return Err(fail(
                Raise::TooLong,
                format!("an action of {len} bytes is past {most}"),
            ));
        }
        let job = crate::screens::Job {
            code,
            given: given.then(String::new),
        };
        if let Some(keymap) = self.keymap.as_deref().filter(|_| here) {
            checked(keymap, &job, Raise::BadValue, "the action")?;
        }
        Ok(job.code)
    }

    /// A config's word: it changes the client's keymap.
    fn configure(
        &mut self,
        keys: Keys,
        next: &mut dyn FnMut() -> Result<Value<Fux>, Failure>,
    ) -> Result<Done<Ask>, Failure> {
        let Some(keymap) = self.keymap.as_deref_mut() else {
            return Err(fail(
                Raise::NotConfig,
                "changes a client's keys: in its config, or typed at its prompt",
            ));
        };
        let refused = |why: String| fail(Raise::BadBinding, why);
        match keys {
            Keys::Prefix => {
                let key = crate::keymap::key(&string(&next()?)?).map_err(refused)?;
                keymap.set_prefix(key).map_err(refused)?;
            }
            Keys::Bind | Keys::BindRoot => {
                let keys_text = string(&next()?)?;
                let code = string(&next()?)?;
                // Checked now, rather than each time its keys are typed.
                let job = code.as_str().into();
                let what = format!("the binding for {keys_text}");
                checked(keymap, &job, Raise::BadBinding, &what)?;
                let binding = crate::keymap::Binding::new(code, keys == Keys::BindRoot);
                let pressed = crate::keymap::keys(&keys_text).map_err(refused)?;
                match (keys == Keys::BindRoot, pressed.as_slice()) {
                    (true, [key]) => {
                        keymap.bind_root(*key, binding).map_err(refused)?;
                        self.last_bound = Some(Bound::Root(*key));
                    }
                    (true, _) => return Err(refused("takes one key".to_owned())),
                    (false, _) => {
                        keymap.bind(&pressed, binding, false).map_err(refused)?;
                        self.last_bound = Some(Bound::Prefix(pressed));
                    }
                }
            }
            Keys::Unbind => {
                let pressed = crate::keymap::keys(&string(&next()?)?).map_err(refused)?;
                keymap.unbind(&pressed).map_err(refused)?;
            }
            Keys::UnbindRoot => {
                let key = crate::keymap::key(&string(&next()?)?).map_err(refused)?;
                keymap.unbind_root(key).map_err(refused)?;
            }
            Keys::BindIn | Keys::UnbindIn => {
                let modal = one_of(&next()?, crate::keymap::Modal::ALL)?;
                let key = crate::keymap::key(&string(&next()?)?).map_err(refused)?;
                let table = keymap.modes.table(modal);
                match keys == Keys::BindIn {
                    true => table.bind(key, &string(&next()?)?).map_err(refused)?,
                    false => table.unbind(key).map_err(refused)?,
                }
                self.last_bound = Some(Bound::In(modal, key));
            }
            Keys::UnbindAll => keymap.unbind_all(),
            Keys::Label => {
                let label = text(&next()?)?;
                if let Some(Bound::In(modal, key)) = self.last_bound {
                    let gone = || refused("the key just bound is gone".to_owned());
                    *keymap.modes.table(modal).label(key).ok_or_else(gone)? = Some(label);
                    return Ok(Done::Now);
                }
                just_made(keymap, self.last_bound.as_ref())?.label = label;
            }
            Keys::Group => {
                let binding = just_made(keymap, self.last_bound.as_ref())?;
                binding.group = string(&next()?)?;
            }
            Keys::Repeat => {
                just_made(keymap, self.last_bound.as_ref())?;
                let Some(Bound::Prefix(keys)) = &self.last_bound else {
                    let why = "a binding without the prefix does not repeat";
                    return Err(refused(why.to_owned()));
                };
                keymap.set_repeat(keys).map_err(refused)?;
            }
        }
        Ok(Done::Now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fuxth::AppValue;

    /// No word's name needs quoting in a shell. (That no two share one,
    /// nor any the language's, the vocabulary checks as it is made.)
    #[test]
    fn every_name_is_a_shell_word() {
        for name in Word::ALL.iter().map(|w| w.row().name) {
            assert!(
                !name.chars().any(|c| ";|&<>()$`*?[{!~#'\"\\".contains(c)),
                "{name}: a character a shell treats specially"
            );
        }
    }

    /// `signal`'s help names every signal it takes.
    #[test]
    fn every_signal_is_told_of() {
        let about = Word::Act(Act::Signal).row().about;
        let (named, _) = about.split_once(',').unwrap_or_default();
        let mut named: Vec<&str> = named.split_whitespace().collect();
        let mut all = fuxix::process::Signal::ALL
            .map(fuxix::process::Signal::name)
            .to_vec();
        named.sort_unstable();
        all.sort_unstable();
        assert_eq!(named, all);
    }

    /// `send-keys`'s help, and the guide, state its bound as it has it.
    #[test]
    fn the_input_bound_is_stated_as_checked() {
        let input = format!("{} KiB at most", INPUT_MAX / 1024);
        let about = Word::Act(Act::SendKeys).row().about;
        assert!(about.contains(&input), "{input}");
        assert!(crate::help::AGENTS.contains(&input), "{input}");
    }

    /// Each word's effect is in the names of fux's kinds, strictly: the
    /// vocabulary is made, no name in any effect unknown.
    #[test]
    fn every_effect_names_known_kinds() {
        let made = super::super::vocabulary().map(drop);
        assert_eq!(made, Ok(()));
    }

    /// A kind taken as another is so where a word's effect names it, never
    /// inside a name: a string is a workspace to `in`, not a handle.
    #[test]
    fn a_string_is_a_workspace_only_where_a_word_says() {
        let compiles = |code: &str| {
            let tokens = fuxth::token::source("t", code).map_err(|e| e.to_string());
            tokens.and_then(|t| {
                super::super::compile(&t)
                    .map(drop)
                    .map_err(|e| e.to_string())
            })
        };
        assert_eq!(compiles("opts str agent in new-tab"), Ok(()));
        assert_eq!(compiles("opts 80 24 size new-tab clear-history"), Ok(()));
        for wrong in [
            "str nope kill",
            "str nope horizontal split",
            "str nope info",
        ] {
            let refused = compiles(wrong).err().unwrap_or_default();
            assert!(refused.starts_with("type: "), "{wrong}: {refused}");
        }
    }

    /// A tab's answer is a kind of its own, which a word that wants a
    /// pane refuses before anything runs; a pane's answer is its pane.
    #[test]
    fn a_tabs_answer_is_no_pane() {
        let checked = |code: &str| {
            let tokens = fuxth::token::source("t", code).unwrap_or_default();
            super::super::compile(&tokens).map(drop)
        };
        for wrong in [
            "@1 wait capture",
            "@1 0 timeout wait capture",
            "@1 str x text send-keys capture",
            "+1 wait str y text send-keys",
            "+1 0 timeout wait str y text send-keys",
        ] {
            assert!(checked(wrong).is_err(), "{wrong}");
        }
        assert_eq!(checked("%1 str x text send-keys wait capture"), Ok(()));
    }

    /// A tab's or workspace's answer is an array, however many panes
    /// answered.
    #[test]
    fn an_answer_of_several_panes_is_an_array() -> Result<(), ()> {
        let version = "%1:0.0.0.0.0:1-1".parse()?;
        let one = super::super::Answer::Panes(vec![crate::answer::PaneAnswer::Acted(version)]);
        let json = json_of(&Value::App(Fux::Answer(Box::new(one), None))).render();
        assert_eq!(
            json,
            r#"[{"pane":"%1","version":"%1:0.0.0.0.0:1-1","status":0}]"#
        );
        Ok(())
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
                &mut super::super::Settings::default(),
            )
            .map_err(|failed| failed.code)
        };
        let settings: String = std::iter::repeat_n("str clipboard false set\n", 200).collect();
        assert_eq!(run(&settings), Ok(()));
        let forever = "begin false until";
        assert_eq!(run(forever), Err(Code::Steps));
    }

    #[test]
    fn a_default_capture_view_is_plain() {
        assert_eq!(crate::command::CaptureView::default(), Opts::default().view);
    }

    /// Every ordinary modifier preserves the target and baseline, both in
    /// the checker and in the constructed value. The only kind changes are
    /// bare handles becoming their corresponding request.
    #[test]
    fn modifiers_keep_the_target_and_baseline() -> Result<(), Box<dyn std::error::Error>> {
        let version: crate::command::Version = "%2:0.0.0.0.0:48211-1867a3f9c2b8e7d6"
            .parse()
            .map_err(|()| "version")?;
        let pane = Fux::Pane(version.pane, Some(version.run));
        let tab = Fux::Tab(crate::id::TabId::of(1), Some(version.run));
        let workspace = Fux::Workspace(crate::id::WsId::of(1), Some(version.run));
        let client = Fux::Client(crate::id::ClientId::of(1), Some(version.run));
        let on = [
            None,
            Some(On::Target(AnyRef::Pane(version.pane))),
            Some(On::Target(AnyRef::Tab(crate::id::TabId::of(1)))),
            Some(On::Target(AnyRef::Workspace(crate::id::WsId::of(1)))),
            Some(On::Client(crate::id::ClientId::of(1))),
            Some(On::Version(version)),
            Some(On::Changed(version)),
            Some(On::Seen(version)),
        ];
        let mut targets = vec![
            pane,
            tab,
            workspace,
            client,
            Fux::Answer(
                Box::new(super::super::Answer::Pane(
                    crate::answer::PaneAnswer::Acted(version),
                )),
                Some(version.run),
            ),
            Fux::Answer(
                Box::new(super::super::Answer::Pane(
                    crate::answer::PaneAnswer::Closed(version.pane),
                )),
                Some(version.run),
            ),
        ];
        targets.extend(on.map(|on| {
            Fux::Opts(Box::new(Opts {
                on,
                ..Opts::default()
            }))
        }));
        for target in targets {
            let expected = modifying(Value::App(target.clone())).map_err(|f| f.message)?;
            let kind = match target.name() {
                "pane" | "answer" | "closed-answer" => "pane-request",
                "tab" => "tab-request",
                "workspace" => "workspace-request",
                "client" => "client-request",
                request => request,
            };
            for &opt in Opt::ALL
                .iter()
                .filter(|o| !matches!(o, Opt::Changed | Opt::Expect))
            {
                let args = match opt {
                    Opt::Name | Opt::Cwd | Opt::Program | Opt::Arg | Opt::Text | Opt::Paste => {
                        vec![Value::str("x")]
                    }
                    Opt::Size | Opt::Shares => vec![Value::Int(1), Value::Int(1)],
                    Opt::Rows | Opt::Cols => vec![Value::Int(0), Value::Int(1)],
                    Opt::Env => vec![Value::str("X"), Value::Int(1)],
                    Opt::In => vec![Value::str("+1")],
                    Opt::Timeout | Opt::History => vec![Value::Int(0)],
                    Opt::Key => vec![Value::str("Enter")],
                    Opt::Bytes => vec![Value::str("61")],
                    Opt::Mouse => vec![Value::str("click"), Value::Int(0), Value::Int(0)],
                    Opt::From => vec![Value::str("0:48211-1867a3f9c2b8e7d6")],
                    Opt::Horizontal
                    | Opt::Vertical
                    | Opt::Watch
                    | Opt::KillFirst
                    | Opt::AllHistory
                    | Opt::Wrapped
                    | Opt::Full
                    | Opt::Styles
                    | Opt::Marks
                    | Opt::MainScreen
                    | Opt::AltScreen
                    | Opt::LastOutput
                    | Opt::Changed
                    | Opt::Expect => vec![],
                };
                // `arg` requires a program first, which preserves the same kind.
                let initial = if opt == Opt::Arg {
                    let mut built = expected.clone();
                    built.argv.push("sh".into());
                    Fux::Opts(built)
                } else {
                    target.clone()
                };
                let mut values = std::iter::once(Value::App(initial)).chain(args.clone());
                let actual = modify(opt, &mut || {
                    values
                        .next()
                        .ok_or_else(|| fail(Raise::StackUnderflow, "test argument"))
                })
                .map_err(|f| f.message)?;
                assert_eq!(actual.on, expected.on, "{target:?} {opt}");
                assert_eq!(Fux::Opts(actual).name(), kind, "{target:?} {opt}");
                let args = args
                    .iter()
                    .map(|v| match v {
                        Value::Str(s) => format!("str {s}"),
                        Value::Int(_) | Value::Bool(_) | Value::List(_) | Value::App(_) => v.text(),
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                for (candidate, ..) in KINDS {
                    let mut vocabulary = super::super::vocabulary()?;
                    vocabulary.add("held", &format!("-- {}", target.name()))?;
                    vocabulary.add("take-kind", &format!("{candidate} --"))?;
                    let mut compiler = fuxth::Compiler::<Fux>::new(vocabulary);
                    let source = format!("held {args} {opt} take-kind");
                    let checked = compiler
                        .add(&fuxth::token::source("modifier", &source)?)
                        .and_then(|()| compiler.finish());
                    // pane accepts answers directly, but none of these outputs is an answer.
                    assert_eq!(
                        checked.is_ok(),
                        candidate == kind,
                        "{source}: {} -> {candidate}: {checked:?}",
                        target.name()
                    );
                }
            }
        }
        Ok(())
    }
}
