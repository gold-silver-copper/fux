//! fux's words, one table of them (`words!`): each its family, name, what
//! it does to the stack in the names of the kinds it takes and gives
//! (`KINDS`, `NAMES`), a line of help, an example and the options it
//! takes; and what each does, run by the program's [`Host`].
use super::settings::Reads;
use super::{Ask, Bound, Fux, Host, Local, On, Opts, Shell};
use crate::help;
use fux_vt::keys::Direction;
use fux_wire::callback::Open;
use fux_wire::code::{Code, Raise};
use fux_wire::command::{
    AnyRef, Buffer, Button, ClientAction, Command, EnvName, Extent, HistoryBytes, Input, Kind,
    Limit, Members, Mouse, MoveTo, PanePick, PaneResize, Percent, Pick, Program, Request, Resized,
    Sibling, Since, Source, SwapWith, To, Usage, Wait, Wheel, WsRef,
};
use fux_wire::id::{ClientId, PaneId};
use fux_wire::layout::Axis;
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
    fn slot(self) -> Self {
        if matches!(self, Opt::AltScreen | Opt::LastOutput) {
            Opt::MainScreen
        } else if self == Opt::ShellProgram {
            Opt::Program
        } else {
            self
        }
    }

    /// The words that take it.
    pub fn takers(self) -> Vec<Act> {
        let takes = |act: &Act| act.options().contains(&self);
        Act::ALL.iter().copied().filter(takes).collect()
    }
}

/// The words that start from opts, as the table says: `new-tab,
/// new-workspace, attach and attach-defaults`, `last` before the last.
pub fn from_opts(last: &str) -> String {
    let opts = |act: &&Act| (act.takes()).is_some_and(|t| t.after != After::Pane);
    let names: Vec<&str> = (Act::ALL.iter().filter(opts))
        .map(|act| Word::Act(*act).row().name)
        .collect();
    fux_wire::words::listed(&names, last)
}

/// fux's kinds of value, each as its values' `name` says it, as an error
/// says it, and, for one that must be used, what one left over is told.
#[rustfmt::skip]
pub const KINDS: [(&str, &str, Option<&str>); 20] = [
    ("pane", "a pane", None), ("tab", "a tab", None), ("workspace", "a workspace", None),
    ("all", "the whole session", None),
    ("client", "a client", None), ("size", "a size (from percent)", None),
    ("answer", "an action's answer", None),
    ("closed-answer", "a closed pane's answer", None),
    ("client-capture", "a client's capture", None),
    ("answers", "an answer of a tab's or workspace's panes", None), ("capture", "a capture", None),
    ("record", "a record", None), ("null", "null", None),
    ("opts", "options (from opts, for new-tab, new-workspace, attach and attach-defaults; a pane's words take theirs on the pane: `%2 all-history capture`)", Some("nothing takes these options")),
    ("pane-request", "a pane request", Some("nothing acts on this request")),
    ("tab-request", "a tab request", Some("nothing acts on this request")),
    ("workspace-request", "a workspace request", Some("nothing acts on this request")),
    ("client-request", "a client request", Some("nothing acts on this request")),
    ("changed-request", "a changed request", Some("nothing acts on this request")),
    ("expected-request", "an expected request", Some("nothing acts on this request")),
];

/// The names the words' effects use beyond the kinds' own, each with what
/// it stands for, as help says it.
#[rustfmt::skip]
pub const NAMES: &[(&str, &str)] = &[
    ("title", "a title or [title,subject handle]"),
    ("callback", "source text or [captured arguments,source text]"),
    ("revision", "a run-qualified layout revision"),
    ("tree", "a flat layout node list"),
    ("closed-outcome", "a closed pane or collection answer"),
    ("outcome", "an action's or wait's answer: a pane's, or a tab's or workspace's panes'"),
    ("x", "the pane it goes on, under its own values (or a tab, workspace, client, a request on one, or opts)"),
    ("target", "a pane, tab, workspace or client, or a request on one"),
    ("subject", "a pane, or a request on one"),
    ("place", "a pane, tab or workspace, or a request on one"),
    ("handle", "a pane, tab or workspace"),
    ("attachable", "a pane, tab, workspace, a request on one, or opts"),
    ("event-scope", "a pane, tab, workspace, client or all"),
    ("scope", "a pane, tab, workspace or all"),
    ("capture-source", "a pane, a request on one, or a client"),
    ("pane-builder", "a pane or a request on one"),
    ("ref", "a name, or a pane, tab or workspace"),
    ("text", "a string, a number or a boolean"),
    ("new", "a setting's value: a number, true or false, or a list (fux help set)"),
    ("var", "a string, a number, a boolean, or a pane, tab or workspace"),
    ("value", "a value (options and requests are none: what acts on them gives one)"),
    ("seen", "a version: an action's or wait's answer, a pane's capture, or its text"),
    ("readable", "a handle or held observation"),
    ("snapshot-scope", "a snapshot scope"),
    ("extent", "cells, or a percent"),
    ("pane-or-seen", "a pane, an action's or wait's answer, a pane capture, or version text"),
    ("counterpart", "a pane or a direction"),
    ("sized", "a pane, tab, workspace or headless client"),
    ("where", "a tab, a workspace, new-tab, new-workspace or a direction"),
];

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
        Finding All "all" "-- all" "the whole session, for info, notify, events and dump" "fux all info" [];
        Finding Focused "focused" "client -- pane" "the pane a client focuses" "fux str c1 client focused capture" [];
        Starting Opts "opts" "-- opts" "empty options, for new-tab, new-workspace, attach and attach-defaults (the other words take them on the pane)" "fux opts str agent in 80 24 size str ./app program new-tab" [];
        Starting Split "split" "subject -- answer" "split a pane along its required axis: a shell, or the program given" "fux %3 str vertical axis split" [Pane: Axis Shares Cwd Env UnsetEnv CopyEnvironment HistoryBytes Reflow AlternateScrollLines PictureBytes ProgramClipboard Program ShellProgram Arg];
        Starting NewTab "new-tab" "opts -- answer" "a tab with the client's shell or explicit launch" "fux opts str logs name 80 24 size str agent in new-tab" [Opts: Name Size Cwd Env UnsetEnv CopyEnvironment HistoryBytes Reflow AlternateScrollLines PictureBytes ProgramClipboard Program ShellProgram Arg In];
        Starting NewWorkspace "new-workspace" "opts -- answer" "a workspace with a tab and the client's shell or explicit launch" "fux opts str work name 80 24 size new-workspace" [Opts: Name Size Cwd Env UnsetEnv CopyEnvironment HistoryBytes Reflow AlternateScrollLines PictureBytes ProgramClipboard Program ShellProgram Arg];
        Starting Respawn "respawn" "subject -- answer" "start again in its pane the program it was given (new-tab, new-workspace, split), or the one given now" "fux %3 kill-first respawn" [Pane: KillFirst Env UnsetEnv Program Arg];
        Waiting Wait "wait" "place -- place" "wait until the pane's version differs from the one changed gives, else an action's answer's (%2 str y text send-keys wait), else now's, or the pane closes; a tab or workspace: any of the panes there as it begins" "A=$(fux %3 str y text send-keys); fux str \"$A\" changed 60 timeout wait" [Pane: Changed Timeout];
        Input SendKeys "send-keys" "target -- target" "send the keys on it to a pane, every pane of a tab, or a client as typed at its terminal; as much as one request (1 MiB) carries" "fux %3 str 'ls -l' text str Enter key send-keys" [Pane: Text Key Paste Bytes Expect];
        Input SendMouse "send-mouse" "pane action row col -- answer" "click right middle press release wheel-up wheel-down or move, at a cell from 0" "fux %3 str click 2 10 send-mouse" [];
        Input Signal "signal" "subject name -- answer" "TERM KILL INT HUP QUIT USR1 USR2 STOP CONT TSTP WINCH ILL ABRT FPE SEGV PIPE ALRM BUS TRAP SYS XCPU XFSZ VTALRM PROF TTIN TTOU URG CHLD, to the pane's foreground" "fux %3 str TERM signal" [];
        Reading Capture "capture" "capture-source -- capture-source" "a pane screen with its options, or what a client terminal shows (without pane options or a version)" "fux %3 100 history capture" [Pane: History Wrapped Full Styles Marks MainScreen AltScreen LastOutput Rows Cols];
        Reading Snapshot "snapshot" "snapshot-scope projection -- record" "one bounded observation: facts, topology or recipe" "fux all str topology snapshot json" [];
        Reading Get "get" "readable field -- value" "a held answer, capture or record field, locally; or a live pane, tab or workspace field" "fux %3 str exited get" [];
        Reading Json "json" "value -- string" "value as JSON (an answer as its JSON); a failed wait's answer is the message try's handler gets" "fux ls json" [];
        Reading NextEvents "next-events" "event-scope cursor secs -- record" "one bounded journal batch, with exact next cursor, missed count and status; empty cursor starts now" "fux all str '' 0 next-events json" [];
        Changing Kill "kill" "handle --" "close a pane, tab or workspace, without asking" "fux %3 kill" [];
        Changing Rename "rename" "handle text --" "rename a pane, tab or workspace" "fux %3 str editor rename" [];
        Changing Notify "notify" "scope text --" "a notice on the clients showing it, or every client with all" "fux all str done notify" [];
        Changing Resize "resize" "sized cols rows --" "a tab layout or headless client size, each at least 1, as large as a screen's grid holds; a pane selects its tab" "fux @2 120 40 resize" [];
        Changing ResizeAuto "resize-auto" "handle --" "a tab laid out on a client's screen again (at its size until one shows it)" "fux @2 resize-auto" [];
        Changing ResizePane "resize-pane" "pane extent way --" "a pane's horizontal or vertical extent: cells, or a percent" "fux %3 30 percent str horizontal resize-pane" [];
        Changing Percent "percent" "n -- size" "n percent of the tab, for resize-pane" "fux %3 30 percent str horizontal resize-pane" [];
        Changing Arrange "arrange" "tab revision tree --" "replace the whole layout at a run-qualified structural revision" "fux @2 str REVISION list %3 end arrange" [];
        Changing MoveBorder "move-border" "pane way cells --" "move the pane's nearest border: left right up down" "fux %3 str left 5 move-border" [];
        Changing Move "move" "pane where --" "move a pane to a tab, workspace, new-tab, new-workspace, or beside its neighbour: left right up down" "fux %3 str new-tab move" [];
        Changing SwapWith "swap-with" "pane counterpart --" "swap a pane with another pane or its neighbour: left right up down" "fux %3 %4 swap-with" [];
        Changing Reorder "reorder" "handle way --" "move one place: next or previous" "fux @2 str next reorder" [];
        Changing HistoryBudget "history-budget" "pane integer --" "change this pane's history budget in bytes (0 to 1 GiB); lowering evicts old rows" "fux %3 4194304 history-budget" [];
        Changing ClearHistory "clear-history" "pane --" "drop the pane's history" "fux %3 clear-history" [];
        Changing Buffers "buffers" "-- list" "paste buffer metadata, newest first: id, bytes and preview" "fux buffers" [];
        Changing Buffer "buffer" "id -- text" "exact paste buffer text by id or newest" "fux newest buffer" [];
        Changing PasteBuffer "paste-buffer" "pane id -- answer" "paste a buffer by id or newest into a pane; answers as send-keys does" "fux %3 newest paste-buffer" [];
        Clients Attach "attach" "attachable --" "show the server in this terminal (bare fux uses default-attach); with a size, be a client of no terminal instead; explicit targets and watch refuse creation options" "fux @2 attach" [Either: In Size Watch Cwd Env UnsetEnv CopyEnvironment HistoryBytes Reflow AlternateScrollLines PictureBytes ProgramClipboard Program ShellProgram Arg Name];
        Clients Focus "focus" "client way --" "left right up down next previous or last pane" "fux str c1 client str left focus" [];
        Clients Show "show" "client handle --" "show and follow a pane, tab or workspace" "fux str c1 client %3 show" [];
        Clients StepTab "step-tab" "client way --" "show the next, previous or last visited tab" "fux str c1 client str next step-tab" [];
        Clients StepWorkspace "step-workspace" "client way --" "show the next, previous or last visited workspace" "fux str c1 client str previous step-workspace" [];
        Clients Zoom "zoom" "client --" "zoom the focused pane, or restore" "fux str c1 client zoom" [];
        Clients Detach "detach" "client --" "detach a client" "fux str c1 client detach" [];
        Server Server "server" "name --" "the server named so (NAME.sock in fux's directory), for what follows" "fux str test server start agent-opts 80 24 size str ./app program new-tab" [];
        Server Socket "socket" "path --" "the server on this socket, for what follows" "fux str ./test.sock socket status" [];
        Server Start "start" "--" "start the server in the background, if none answers" "fux start" [];
        Server Status "status" "-- record" "the server that answers, and its version; with none, fails no_server" "fux status || fux start" [];
        Server KillServer "kill-server" "--" "stop the server: not from its own panes, nor with people attached" "fux str test server kill-server" [];
        Server ForceKillServer "force-kill-server" "--" "stop it anyway, whatever fux version it is" "fux str test server force-kill-server" [];
        Config AttachDefaults "attach-defaults" "attachable --" "replace this client's preferred attach place; target-only retains the first name, name-only or empty clears the target; target and name conflict" "opts str main name attach-defaults" [Either: In Name];
        Config Set "set" "setting new --" "a preference for this client only: `fux help set` lists their effects" "fux str history-bytes 16777216 set" [];
        Reading Version "version" "pane-or-seen -- string" "a pane's current version, or the version an answer, pane capture or canonical text holds" "fux %3 capture version" [];
        Shell Args "args" "-- list" "the strings after fux --file FILE (none for any other program)" "fux args len" [];
        Shell Input "input" "-- string" "the rest of stdin, as UTF-8 text; not when the program came from stdin" "ls | fux input lines len" [];
        Shell ReadFile "read-file" "path -- string" "a file's text, as UTF-8" "fux str notes.txt read-file lines len" [];
        Shell Getenv "getenv" "name -- string" "an environment variable's value; one not set fails unset_var" "fux str HOME getenv" [];
        Shell Warn "warn" "value --" "print a value on stderr, a line" "fux str 'no tests found' warn 2 exit" [];
        Shell Exit "exit" "n --" "end the program now, what is left printed, with exit status n, 0 to 255" "fux str 'no tests found' warn 2 exit" [];
        Config ListKeys "list-keys" "--" "this client's keys (the bundled keys from the CLI), as config lines that make them" "fux list-keys" [];
        Config KeyNames "key-names" "--" "the names keys go by" "fux key-names" [];
        Help Help "help" "--" "the words, under their headings" "fux help" [];
        Help Explain "explain" "word --" "one word at length (json's says what json gives); or the guide to driving programs from a script or an agent: agents, its first screen, enough for most work; a topic of it (recipes keys read shells programs servers); or guide, all of it" "fux str capture explain" [];
    }
    Opt {
        Starting Name "name" "x name -- x" "the given name: new-tab's tab, or new-workspace's workspace" "fux agent-opts str web name 80 24 size str ./app program new-tab";
        Starting Size "size" "x cols rows -- x" "what its tab is laid out on while no client shows it, each at least 1, as large as a screen's grid holds; needed from outside fux, else the caller's own screen or tab, as fux makes no size up; attach: be a client with no terminal, of this size, which prints its id and runs until signalled" "fux agent-opts 80 24 size str ./app program new-tab";
        Starting Cwd "cwd" "x dir -- x" "where it starts" "fux agent-opts str /srv cwd 80 24 size str ./app program new-tab";
        Starting Env "env" "x key var -- x" "a variable for it: text, or a pane, tab or workspace as its id (%3; an action's answer, its pane)" "fux agent-opts str PORT 8080 env 80 24 size str ./app program new-tab";
        Starting Program "program" "x path -- x" "the program to start, in place of a shell; its pane stays once it exits" "fux agent-opts 80 24 size str ./app program new-tab";
        Starting HistoryBytes "history-bytes" "x integer -- x" "history budget for the new pane, in bytes (0 to 1 GiB)" "fux agent-opts 0 history-bytes str /bin/cat program new-tab";
        Starting Reflow "reflow" "x boolean -- x" "whether the new pane rewraps primary-screen lines on resize" "fux agent-opts false reflow str /bin/cat program new-tab";
        Starting AlternateScrollLines "alternate-scroll-lines" "x integer -- x" "arrows per alternate-screen wheel action (0 to 32767)" "fux agent-opts 3 alternate-scroll-lines str /bin/cat program new-tab";
        Starting PictureBytes "picture-bytes" "x integer -- x" "picture budget for the new pane (0 to 96 MiB); zero disables pictures" "fux agent-opts 0 picture-bytes str /bin/cat program new-tab";
        Starting ProgramClipboard "program-clipboard" "x boolean -- x" "whether the new pane's program may copy to paste buffers" "fux agent-opts false program-clipboard str /bin/cat program new-tab";
        Starting ShellProgram "shell-program" "x list -- x" "the checked argv list to start with shell role; its exit closes the pane" "fux opts 80 24 size list str /bin/sh end shell-program new-tab";
        Starting CopyEnvironment "copy-environment" "x list -- x" "replace the client selection for this creation; empty selects nothing; selected values are saved and exported by dump" "fux agent-opts list str LANG end copy-environment str /bin/cat program new-tab";
        Starting UnsetEnv "unset-env" "x name -- x" "explicitly remove this environment name from the child" "fux agent-opts str OLD unset-env str /bin/cat program new-tab";
        Starting Arg "arg" "x text -- x" "an argument for the program" "fux agent-opts 80 24 size str ./app program str -v arg new-tab";
        Starting Axis "axis" "x way -- x" "horizontal (side by side) or vertical (stacked); split requires it" "fux %3 str horizontal axis split";
        Starting Shares "shares" "x n n -- x" "split: how the pane and the new one divide the pane's room, A to B (default: halved)" "fux %3 str horizontal axis 2 1 shares split";
        Starting In "in" "x workspace -- x" "the workspace for new-tab, attach or attach-defaults; new-tab creates a missing name, or a fresh unnamed workspace with str new-workspace in" "fux opts str agent in 80 24 size new-tab";
        Starting Watch "watch" "x -- x" "attach without typing: keys do nothing but the prefix, then d, which detaches" "fux opts watch attach";
        Starting KillFirst "kill-first" "x -- x" "respawn: end the program first if it still runs" "fux %3 kill-first respawn";
        Waiting Changed "changed" "seen -- changed-request" "a wait from this version (%2:0.57.0.3.0:RUN, as an action, a wait or `str version get` gives it; or an answer or capture, its version) of the pane it names, not from now" "A=$(fux %3 str y text send-keys); fux str \"$A\" changed 60 timeout wait";
        Waiting Timeout "timeout" "x secs -- x" "how long a wait waits for a change before it answers timeout, else this client's wait-seconds" "fux %3 300 timeout wait";
        Input Expect "expect" "seen -- expected-request" "keys for the pane this version names, which send-keys sends only if the pane is still at it as they are queued, else sends none and fails stale; a version as changed takes it: a capture's is its pane's as it was read" "fux %3 capture let seen seen string str '[y/N]' contains if seen expect str y text send-keys drop then";
        Input Text "text" "x text -- x" "text, typed" "fux %3 str 'ls -l' text str Enter key send-keys";
        Input Key "key" "x key -- x" "a key by its name: Enter, C-c, Up, F5" "fux %3 str C-c key send-keys";
        Input Paste "paste" "x text -- x" "text as a paste, bracketed if the program turned that on (an end mark, ESC [201~, in it then dropped); else as it is" "fux %3 str 'a long prompt' paste str Enter key send-keys";
        Input Bytes "bytes" "x hex -- x" "bytes as they are, in hex (1b5b41, c3): part of a key's sequence or of a character" "fux %3 str 1b5b41 bytes send-keys";
        Reading History "history" "pane-builder lines -- pane-builder" "capture: lines of history first (all-history: all of it)" "fux %3 100 history capture";
        Reading Wrapped "wrapped" "pane-builder -- pane-builder" "capture: each screen row a line, as wrapped at the pane's width (the cells a mouse event counts), not joined into the lines printed" "fux %3 wrapped 0 2 rows capture";
        Reading Full "full" "pane-builder -- pane-builder" "capture: every row and cell, untrimmed" "fux %3 full capture";
        Reading Styles "styles" "pane-builder -- pane-builder" "capture: colours and attributes" "fux %3 styles capture";
        Reading Marks "marks" "pane-builder -- pane-builder" "capture: highlighted text (reverse video, or a background colour) as ⟦this⟧" "fux %3 marks capture";
        Reading MainScreen "main-screen" "pane-builder -- pane-builder" "capture: what was printed before a program took the screen" "fux %3 main-screen capture";
        Reading AltScreen "alt-screen" "pane-builder -- pane-builder" "capture: the full screen a program drew last" "fux %3 alt-screen capture";
        Reading LastOutput "last-output" "pane-builder -- pane-builder" "capture: the last shell command's output, or the running one's so far (a full-screen one's: its screen to the cursor); fails no_marks where no command was marked (OSC 133); one of main-screen, alt-screen and last-output at most" "fux %3 last-output capture";
        Reading Rows "rows" "pane-builder from to -- pane-builder" "capture: half-open from..to, clipped to available rows; joined lines touched whole (with wrapped, those rows alone)" "fux %3 0 4 rows capture";
        Reading Cols "cols" "pane-builder from to -- pane-builder" "capture: half-open from..to columns, clipped to available cells" "fux %3 0 40 cols capture";
    }
    Screen {
        Clients Choose "choose" "client title list list list callback --" "a list of [payload,label] rows; current and selected are [] or [index]; callback receives captured arguments then client payload and leaves nothing" "fux str c1 client str hosts list list str host str Host end end list end list 0 end str 'drop drop' choose";
        Clients Prompt "prompt" "client name text callback --" "a line to type, titled name and starting as text: its submit key (Enter) runs code, the line on its stack" "fux str c1 client str 'rename tab' str logs str '@2 swap rename' prompt";
        Clients CopyMode "copy-mode" "client --" "copy and select in the focused pane" "fux str c1 client copy-mode";
        Clients CommandPrompt "command-prompt" "client --" "a prompt for fuxth" "fux str c1 client command-prompt";
    }
    Keys {
        Config Prefix "prefix" "key --" "this client's prefix (config)" "str C-a prefix";
        Config Bind "bind" "keys code --" "keys after the prefix run fuxth (config)" "str 'g v' str 'me focused str horizontal axis split drop' bind";
        Config BindRoot "bind-root" "key code --" "a key without the prefix runs fuxth (config)" "str M-n str 'me str next step-tab' bind-root";
        Config Label "label" "text --" "what the column says the last binding does, or a screen's hints the last key bound in it (keys of one label shown together)" "str 'g v' str 'me focused str horizontal axis split drop' bind str 'split right' label";
        Config Group "group" "name --" "the column's heading for the last binding" "str 'g v' str 'me focused str horizontal axis split drop' bind str panes group";
        Config Repeat "repeat" "--" "the last binding's layer repeats until Esc" "str H str 'me focused str left 2 move-border' bind repeat";
        Config Unbind "unbind" "keys --" "keys after the prefix run nothing" "str 'g v' unbind";
        Config UnbindRoot "unbind-root" "key --" "a key without the prefix runs nothing" "str M-n unbind-root";
        Config BindAction "bind-action" "key code --" "an explicit executable list callback, receiving client payload; Escape cannot be bound" "str r str rename-prompt bind-action str rename label";
        Config BindIn "bind-in" "screen key action --" "in one of fux's own screens (copy list line), a key does one of its actions, as the default keys bind each" "str copy str g str top bind-in str top label";
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
    // What an option's word goes on: the pane, opts, or either, as the
    // words that take it do. Any other word's `x` is any value.
    let takes = takes.trim().to_uppercase();
    let (option, takers) = match fuxs {
        Some(Word::Opt(opt)) => (true, opt.takers()),
        Some(Word::Act(_) | Word::Screen(_) | Word::Keys(_)) | None => (false, Vec::new()),
    };
    let request = (takes.strip_prefix("PANE-BUILDER"))
        .or_else(|| takes.strip_prefix('X'))
        .filter(|_| option);
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
        "x" | "pane-builder" if option => {
            format!(", which leaves {left} for the word that acts")
        }
        leaves => format!(", which leaves {leaves}"),
    };
    let example = fuxs.map_or_else(String::new, |w| format!("; e.g. {}", w.row().example));
    // A capture's option says so: it goes between its pane and capture.
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
        Value::Str(_) => string(value),
        Value::Int(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        other @ (Value::List(_) | Value::App(_)) => {
            Err(wrong("a string, a number or a boolean", other))
        }
    }
}

/// A value's JSON, as a string.
fn json_text(value: &Value<Fux>) -> Value<Fux> {
    let mut text = String::new();
    // Writing to a `String` can't fail.
    _ = write_json_value(value, &mut text);
    Value::str(text)
}

/// `value` as JSON: a list's items walked with a stack of the lists under
/// way, not as a recursion, so lists nested however deep are written.
pub(super) fn write_json_value(
    value: &Value<Fux>,
    out: &mut dyn std::fmt::Write,
) -> std::fmt::Result {
    let mut lists: Vec<(std::slice::Iter<'_, Value<Fux>>, bool)> = Vec::new();
    let mut value = value;
    loop {
        match value {
            Value::Int(n) => write!(out, "{n}")?,
            Value::Bool(b) => write!(out, "{b}")?,
            Value::Str(s) => fux_wire::json::write_str(out, s)?,
            Value::List(list) => {
                out.write_char('[')?;
                lists.push((list.items().iter(), false));
            }
            Value::App(app) => match app {
                Fux::Null => out.write_str("null"),
                Fux::All => out.write_str("\"all\""),
                Fux::Record(r) => r.write(out),
                Fux::Answer(answer, _) => answer.write_json(out),
                Fux::Pane(pane, _) => write!(out, "\"{pane}\""),
                Fux::Tab(tab, _) => write!(out, "\"{tab}\""),
                Fux::Workspace(workspace, _) => write!(out, "\"{workspace}\""),
                Fux::Client(client, _) => write!(out, "\"{client}\""),
                Fux::Extent(Extent::Cells(n)) => write!(out, "\"{n}\""),
                Fux::Extent(Extent::Percent(n)) => write!(out, "\"{}%\"", n.get()),
                Fux::Opts(_) => write!(out, "\"({})\"", fuxth::AppValue::kind(app)),
            }?,
        }
        // The next item of the innermost list under way.
        value = loop {
            let Some((items, started)) = lists.last_mut() else {
                return Ok(());
            };
            if let Some(item) = items.next() {
                if std::mem::replace(started, true) {
                    out.write_char(',')?;
                }
                break item;
            }
            lists.pop();
            out.write_char(']')?;
        };
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

/// A relative tab or workspace selection, including retained history.
fn pick<T>(value: &Value<Fux>) -> Result<Pick<T>, Failure> {
    one_of(
        value,
        &[
            ("next", Some(Sibling::Next)),
            ("previous", Some(Sibling::Previous)),
            ("last", None),
        ],
    )
    .map(|step| step.map_or(Pick::Last, Pick::Step))
}

/// One of a closed set, by its name; refused, naming each, if none.
fn one_of<T: Clone>(value: &Value<Fux>, all: &[(&str, T)]) -> Result<T, Failure> {
    let given = string(value)?;
    let found = all.iter().find(|(name, _)| *name == given);
    found.map(|(_, t)| t.clone()).ok_or_else(|| {
        let names: Vec<&str> = all.iter().map(|(name, _)| *name).collect();
        let names = fux_wire::words::listed(&names, "or");
        fail(Raise::BadValue, format!("{given:?} is not {names}"))
    })
}

/// A size: columns and rows each 1 to `command::MOST`, as a client's screen,
/// and so a tab that fits clients, may be.
fn laid_out(cols: &Value<Fux>, rows: &Value<Fux>) -> Result<fux_wire::command::Screen, Failure> {
    Ok(fux_wire::command::Screen::new(int(rows)?, int(cols)?)?)
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
    as_pane(value).ok_or_else(|| wrong("a pane (from pane, new-tab or split)", value))
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
    fux_wire::command::parse_workspace(&string(value)?)
        .map_err(|e| fail(Raise::BadValue, e.to_string()))
}

fn scope(value: &Value<Fux>) -> Result<Option<AnyRef>, Failure> {
    match value {
        Value::App(Fux::All) => Ok(None),
        value @ (Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_)) => target(value).map(Some),
    }
}

/// A snapshot or journal scope: the session, a client, or a target.
fn observation_scope(value: &Value<Fux>) -> Result<fux_wire::inspect::Scope, Failure> {
    if let Value::App(Fux::Client(c, _)) = value {
        return Ok(fux_wire::inspect::Scope::Client(*c));
    }
    scope(value).map(|target| {
        target.map_or(
            fux_wire::inspect::Scope::All,
            fux_wire::inspect::Scope::Target,
        )
    })
}

fn cursor(value: &Value<Fux>) -> Result<fux_wire::events::Cursor, Failure> {
    let text = string(value)?;
    text.parse().map_err(|()| {
        fail(
            Raise::BadValue,
            format!("{text:?} is no cursor: SEQ:RUN, as snapshot and next-events give it"),
        )
    })
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
fn held(value: &Value<Fux>) -> Option<fux_wire::command::Version> {
    match value {
        Value::App(Fux::Answer(answer, _)) => match &**answer {
            super::Answer::Pane(answer) => answer.version(),
            // A capture's is for `expect` and `changed` alone (`version`).
            super::Answer::Panes(_) | super::Answer::Capture(_) => None,
        },
        Value::Int(_) | Value::Bool(_) | Value::Str(_) | Value::List(_) | Value::App(_) => None,
    }
}

/// An explicit baseline is consumed only by its corresponding action.
const CHANGED: &str = "takes no request changed made: only wait uses its baseline";

const EXPECTED: &str = "takes no request expect made: only send-keys checks a version";

/// The version `value` holds (`Answer::version`), or is as text.
fn version(value: &Value<Fux>) -> Result<fux_wire::command::Version, Failure> {
    let wanted = "a version: an action's or wait's answer, a pane's capture, or its text";
    let text: &str = match value {
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
            "{:?} is no version: PANE:RESPAWNS.SCREEN.BELLS.DONE.ENDED:RUN, as an action answers it",
            text
        );
        fail(Raise::BadValue, why)
    })
}

/// The options or request `value` is: options stay options; what else it
/// goes on (a pane, tab, workspace or client, an action's answer) becomes
/// a request on that.
fn modifying(value: Value<Fux>) -> Result<std::rc::Rc<Opts>, Failure> {
    let built = match value {
        Value::App(Fux::Opts(built)) => built,
        other @ (Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_)) => {
            let on = on(&other)
                .map_err(|_| wrong("a pane, tab, workspace or client, or options", &other))?;
            std::rc::Rc::new(Opts {
                on: Some(on),
                ..Opts::default()
            })
        }
    };
    Ok(built)
}

/// The closed choice of split or resize axis.
fn axis(value: &Value<Fux>) -> Result<Axis, Failure> {
    one_of(
        value,
        &[
            ("horizontal", Axis::Horizontal),
            ("vertical", Axis::Vertical),
        ],
    )
}

/// What `send-mouse` takes, by name.
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

/// The options or request an option makes of what it goes on, `next`
/// giving its values: `changed` and `expect` make a request on a version's
/// pane.
fn modify(
    opt: Opt,
    next: &mut dyn FnMut() -> Result<Value<Fux>, Failure>,
) -> Result<std::rc::Rc<Opts>, Failure> {
    // `changed` and `expect` make a request; every other option goes on
    // one, or on options.
    let mut built = match matches!(opt, Opt::Changed | Opt::Expect) {
        true => Opts::default(),
        false => owned(modifying(next()?)?),
    };
    if !matches!(
        opt,
        Opt::Arg | Opt::Env | Opt::UnsetEnv | Opt::Text | Opt::Key | Opt::Paste | Opt::Bytes
    ) && built.given.iter().any(|given| given.slot() == opt.slot())
    {
        return Err(fail(
            Raise::Unused,
            format!("{opt}: {} is already given", opt.slot()),
        ));
    }
    built.given.push(opt);
    match opt {
        Opt::Changed => built.on = Some(On::Changed(version(&next()?)?)),
        Opt::Expect => built.on = Some(On::Seen(version(&next()?)?)),
        Opt::Name => built.name = Some(string(&next()?)?),
        Opt::Cwd => built.cwd = Some(string(&next()?)?.into()),
        Opt::In => built.place = Some(workspace(&next()?)?),
        Opt::HistoryBytes => {
            built.history_bytes =
                Some(HistoryBytes::read(&next()?).map_err(|why| fail(Raise::Range, why))?);
        }
        Opt::Reflow => {
            built.reflow = Some(bool::read(&next()?).map_err(|why| fail(Raise::BadValue, why))?)
        }
        Opt::AlternateScrollLines => {
            built.alternate_scroll_lines = Some(
                fux_wire::command::ScrollLines::read(&next()?)
                    .map_err(|why| fail(Raise::Range, why))?,
            )
        }
        Opt::PictureBytes => {
            built.picture_bytes = Some(
                fux_wire::command::PictureBytes::read(&next()?)
                    .map_err(|why| fail(Raise::Range, why))?,
            )
        }
        Opt::ProgramClipboard => {
            built.program_clipboard =
                Some(bool::read(&next()?).map_err(|why| fail(Raise::BadValue, why))?);
        }
        Opt::History => built.history = Some(within(&next()?, COUNT)?),
        Opt::Size => built.size = Some(laid_out(&next()?, &next()?)?),
        Opt::Axis => built.axis = Some(axis(&next()?)?),
        Opt::Shares => built.shares = Some((share(&next()?)?, share(&next()?)?)),
        Opt::KillFirst => built.kill = true,
        Opt::Watch => built.watch = true,
        Opt::Wrapped => built.view.wrapped = true,
        Opt::Full => built.view.full = true,
        Opt::Styles => built.view.styles = true,
        Opt::Marks => built.view.marks = true,
        Opt::Timeout => {
            let secs = within(&next()?, COUNT)?;
            let limit = Limit::read(secs).map_err(|why| fail(Raise::Range, why))?;
            built.timeout = Some(limit);
        }
        Opt::Text => built.inputs.push(Input::Text(text(&next()?)?)),
        Opt::Paste => built.inputs.push(Input::Paste(text(&next()?)?)),
        Opt::Bytes => built.inputs.push(Input::Bytes(hex(&string(&next()?)?)?)),
        Opt::Program | Opt::ShellProgram => {
            let shell = opt == Opt::ShellProgram;
            built.shell = Some(shell);
            if shell {
                let program = Program::read(&next()?).map_err(|why| fail(Raise::BadValue, why))?;
                built.argv = program.into_argv();
                return Ok(std::rc::Rc::new(built));
            }
            built.argv.push(string(&next()?)?);
        }
        Opt::Arg => {
            let arg = text(&next()?)?;
            if built.argv.is_empty() {
                return Err(fail(Raise::Usage, "comes after program"));
            }
            built.argv.push(arg);
        }
        Opt::Env => {
            let (key, value) = (string(&next()?)?, next()?);
            // A pane, tab or workspace as fux writes it (`%3`).
            let value = match (target(&value), text(&value)) {
                (Ok(handle), _) => handle.to_string(),
                (Err(_), Ok(text)) => text,
                (Err(_), Err(failure)) => {
                    if failure.code != fuxth::Code::Type {
                        return Err(failure);
                    }
                    let what = "a string, a number, a boolean, or a pane, tab or workspace";
                    return Err(wrong(what, &value));
                }
            };
            built
                .env
                .push(key, Some(value.into()))
                .map_err(|why| fail(Raise::BadValue, why))?;
        }
        Opt::UnsetEnv => built
            .env
            .push(string(&next()?)?, None)
            .map_err(|why| fail(Raise::BadValue, why))?,
        Opt::CopyEnvironment => {
            built.copy_environment =
                Some(Vec::<EnvName>::read(&next()?).map_err(|why| fail(Raise::BadValue, why))?);
        }
        Opt::MainScreen => built.view.source = Source::Main,
        Opt::AltScreen => built.view.source = Source::Alternate,
        Opt::LastOutput => built.view.source = Source::LastOutput,
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
    }
    Ok(std::rc::Rc::new(built))
}

/// The options `shared` holds, owned: moved out when nothing else holds
/// them, else copied.
fn owned(shared: std::rc::Rc<Opts>) -> Opts {
    std::rc::Rc::unwrap_or_clone(shared)
}

/// The rows or columns a capture reads, from `next`: the first, then the
/// exclusive end, from 0.
fn span(
    opt: Opt,
    next: &mut dyn FnMut() -> Result<Value<Fux>, Failure>,
) -> Result<(u16, u16), Failure> {
    let (from, to) = (within(&next()?, CELL)?, within(&next()?, CELL)?);
    if from > to {
        let why = format!("{from} {to}: the first {opt} must not exceed the exclusive end");
        return Err(fail(Raise::Range, why));
    }
    Ok((from, to))
}

/// What an action acts on, and what it was given: a request; or a pane,
/// tab, workspace or client alone, given nothing. Refused if `word`
/// leaves something given unused.
fn request(word: Act, value: Value<Fux>) -> Result<(On, Opts), Failure> {
    let mut built = match value {
        Value::App(Fux::Opts(built)) => owned(built),
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
             (`%2 all-history capture`, `%2 str vertical axis split`)",
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
            "takes a pane (from pane, new-tab or split), or a request on one",
        )),
    }
}

/// The options `value` holds, for `word`, which makes what they are for:
/// refused if it leaves one unused.
fn finished(word: Act, value: Value<Fux>) -> Result<Opts, Failure> {
    match value {
        Value::App(Fux::Opts(built)) if built.on.is_none() => {
            unused(word, &built)?;
            Ok(owned(built))
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

/// Attach alone accepts typed places through the ordinary request subject.
fn attach_options(
    word: Act,
    value: Value<Fux>,
) -> Result<(Option<fux_wire::command::AttachTarget>, Opts), Failure> {
    use fux_wire::command::AttachTarget as T;
    let built = owned(modifying(value)?);
    unused(word, &built)?;
    let target = match (&built.on, &built.place) {
        (None, place) => place.clone().map(T::Workspace),
        (Some(On::Target(target)), None) => Some(match target {
            AnyRef::Workspace(ws) => T::Workspace(WsRef::Id(*ws)),
            AnyRef::Tab(tab) => T::Tab(*tab),
            AnyRef::Pane(pane) => T::Pane(*pane),
        }),
        (Some(On::Version(version)), None) => Some(T::Pane(version.pane)),
        (Some(_), Some(_)) => {
            return Err(fail(Raise::BadValue, "attach takes no in with a subject"));
        }
        (Some(_), None) => {
            return Err(fail(
                Raise::Type,
                "attach needs a workspace, tab, pane or opts",
            ));
        }
    };
    Ok((target, built))
}

pub(super) fn check_attach_creation(
    built: &Opts,
    targeted: bool,
    watch: bool,
) -> Result<(), Failure> {
    if (targeted || watch)
        && let Some(option) = built.given.iter().find(|option| {
            matches!(
                option,
                Opt::Name
                    | Opt::Cwd
                    | Opt::Env
                    | Opt::UnsetEnv
                    | Opt::CopyEnvironment
                    | Opt::HistoryBytes
                    | Opt::Reflow
                    | Opt::AlternateScrollLines
                    | Opt::PictureBytes
                    | Opt::ProgramClipboard
                    | Opt::Program
                    | Opt::ShellProgram
                    | Opt::Arg
            )
        })
    {
        return Err(fail(
            Raise::Unused,
            format!("takes no {option} with a target or watch"),
        ));
    }
    Ok(())
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
    fux_wire::words::listed(&names, "or")
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

/// `resize-pane`: an explicit axis and extent, cells or a percent.
fn resize_to(next: &mut dyn FnMut() -> Result<Value<Fux>, Failure>) -> Result<Done<Ask>, Failure> {
    let target = pane(&next()?)?;
    let extent = match next()? {
        Value::App(Fux::Extent(e)) => e,
        v @ Value::Int(_) => Extent::Cells(within(&v, CELL)?),
        other @ (Value::Bool(_) | Value::Str(_) | Value::List(_) | Value::App(_)) => {
            return Err(wrong("cells or a percent", &other));
        }
    };
    let by = PaneResize::To(axis(&next()?)?, extent);
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
        Value::Str(s) => {
            let text: &str = &s;
            // An empty name never names the caller's pane, which `here` is.
            if text.trim().is_empty() {
                return Err(fail(
                    Raise::NoTarget,
                    format!(
                        "an empty name names no {}: `here` is the pane this runs in",
                        kind.name()
                    ),
                ));
            }
            resolve(text.to_owned(), kind)
        }
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
                    std::rc::Rc::make_mut(opts).of = super::same_run(of, version)?;
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
                target,
                read_only,
                size,
                creation,
                ..
            })) => local(Local::Attach {
                of,
                target,
                read_only,
                size,
                creation,
            }),
            result @ (Done::Now | Done::Later(_)) => Ok(result),
        }
    }
}

impl Host<'_> {
    fn launch(
        &mut self,
        built: &Opts,
    ) -> Result<
        (
            fux_wire::command::Start,
            fux_wire::command::EnvPatch,
            fux_wire::command::PaneOptions,
        ),
        Failure,
    > {
        self.prepare()
            .map_err(|why| fail(Raise::Config, why.to_string()))?;
        let preferences = &self
            .state
            .as_deref()
            .ok_or_else(|| fail(Raise::Config, "launch needs client defaults"))?
            .preferences;
        let options = built.pane_options(
            preferences
                .pane_options()
                .map_err(|why| fail(Raise::Config, why))?,
        );
        built
            .launch(
                preferences
                    .shell()
                    .map_err(|why| fail(Raise::Config, why))?,
                preferences
                    .copy_environment()
                    .map_err(|why| fail(Raise::Config, why))?,
            )
            .map(|(start, env)| (start, env, options))
            .map_err(|why| fail(Raise::BadValue, why))
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
            Word::Opt(opt) => {
                return now(out, Value::App(Fux::Opts(modify(opt, &mut next)?)));
            }
            Word::Screen(screen) => return self.open(screen, &mut next),
            Word::Keys(keys) => return self.configure(keys, &mut next),
        };
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
                    Raise::NotHere,
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
            Act::All => now(out, Value::App(Fux::All)),
            Act::Opts => now(out, Value::App(Fux::Opts(std::rc::Rc::default()))),
            Act::NewTab => {
                let built = finished(act, next()?)?;
                let (start, env, options) = self.launch(&built)?;
                let tab = Command::NewTab {
                    target: built.place,
                    name: built.name,
                    size: built.size,
                    cwd: built.cwd,
                    env,
                    options,
                    start,
                };
                later(ask(tab))
            }
            Act::NewWorkspace => {
                let built = finished(act, next()?)?;
                let (start, env, options) = self.launch(&built)?;
                let workspace = Command::NewWorkspace {
                    name: built.name,
                    size: built.size,
                    cwd: built.cwd,
                    env,
                    options,
                    start,
                };
                later(ask(workspace))
            }
            Act::Split => {
                let (p, built) = request_pane(act, next()?)?;
                let axis = built.axis.ok_or_else(|| {
                    fail(
                        Raise::Usage,
                        "split requires a direction: %2 str horizontal axis split",
                    )
                })?;
                let (start, env, options) = self.launch(&built)?;
                let split = Command::Split {
                    axis,
                    target: p,
                    start,
                    cwd: built.cwd,
                    env,
                    options,
                    shares: built.shares,
                };
                later(ask(split))
            }
            Act::Respawn => {
                let (p, mut built) = request_pane(act, next()?)?;
                let respawn = Command::Respawn {
                    target: p,
                    kill: built.kill,
                    argv: if built.argv.is_empty() {
                        None
                    } else {
                        Some(built.program().map_err(|why| fail(Raise::BadValue, why))?)
                    },
                    env: built.env,
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
                    timeout: match built.timeout {
                        Some(limit) => limit,
                        None => {
                            self.prepare()
                                .map_err(|why| fail(Raise::Config, why.to_string()))?;
                            self.state
                                .as_deref()
                                .ok_or_else(|| fail(Raise::Config, "wait needs client defaults"))?
                                .preferences
                                .wait()
                                .map_err(|why| fail(Raise::Config, why))?
                        }
                    },
                };
                later(ask(Request::Wait(wait)))
            }
            // ------------------------------------------------------- input
            Act::SendKeys => {
                let (on, built) = request(act, next()?)?;
                if built.inputs.is_empty() {
                    return Err(fail(
                        Raise::Usage,
                        "needs keys: %2 str hello text send-keys",
                    ));
                }
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
                let p = pane(&next()?)?;
                let mouse = one_of(&next()?, &MICE)?;
                let row = within(&next()?, CELL)?;
                let col = within(&next()?, CELL)?;
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
                let value = next()?;
                if let Value::App(Fux::Client(c, _)) = value {
                    return later(on_client(c, ClientAction::Capture));
                }
                let (p, built) = request_pane(act, value)?;
                later(ask(Command::CapturePane {
                    target: p,
                    history: built.history,
                    view: built.view,
                }))
            }
            Act::Snapshot => {
                let scope = observation_scope(&next()?)?;
                let projection = one_of(
                    &next()?,
                    &[
                        ("facts", fux_wire::inspect::Projection::Facts),
                        ("topology", fux_wire::inspect::Projection::Topology),
                        ("recipe", fux_wire::inspect::Projection::Recipe),
                    ],
                )?;
                later(ask(Command::Inspect { scope, projection }))
            }
            Act::Panes => later(members_of(target(&next()?)?, Members::Panes)),
            Act::Tabs => later(members_of(target(&next()?)?, Members::Tabs)),
            Act::Get => {
                let value = next()?;
                let field = string(&next()?)?;
                match value {
                    Value::App(Fux::Record(record)) => now(out, record.get(&field)?),
                    Value::App(Fux::Answer(answer, run)) => {
                        now(out, super::record::answer_field(&answer, run, &field)?)
                    }
                    value @ (Value::Int(_)
                    | Value::Bool(_)
                    | Value::Str(_)
                    | Value::List(_)
                    | Value::App(_)) => later(ask(Command::Inspect {
                        scope: fux_wire::inspect::Scope::Target(target(&value)?),
                        projection: fux_wire::inspect::Projection::Field(field),
                    })),
                }
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
                | Value::App(_)) => now(out, json_text(&other)),
            },
            Act::NextEvents => {
                let scope = observation_scope(&next()?)?;
                let text = string(&next()?)?;
                let from = if text.is_empty() {
                    None
                } else {
                    Some(cursor(&Value::str(text))?)
                };
                let timeout = Limit::read(within(&next()?, 0..=i64::from(u32::MAX))?)
                    .map_err(|why| fail(Raise::Range, why))?;
                tell(Request::NextEvents {
                    scope,
                    from,
                    timeout,
                })
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
                let target = scope(&next()?)?;
                let text = text(&next()?)?;
                tell(Command::Notify { target, text })
            }
            Act::Resize => {
                let value = next()?;
                let size = laid_out(&next()?, &next()?)?;
                let resized = match value {
                    Value::App(Fux::Client(c, _)) => Resized::Client(c, size),
                    value @ (Value::Int(_)
                    | Value::Bool(_)
                    | Value::Str(_)
                    | Value::List(_)
                    | Value::App(_)) => Resized::Tabs(target(&value)?, Some(size)),
                };
                tell(Command::Resize(resized))
            }
            Act::ResizeAuto => tell(Command::Resize(Resized::Tabs(target(&next()?)?, None))),
            Act::ResizePane => resize_to(&mut next),
            Act::Percent => {
                let n = within(&next()?, Percent::SHARES)?;
                let share = Percent::new(n).ok_or_else(|| fail(Raise::Range, "no share"))?;
                now(out, Value::App(Fux::Extent(Extent::Percent(share))))
            }
            Act::Arrange => {
                let tab = next()?;
                let Value::App(Fux::Tab(target, _)) = tab else {
                    return Err(wrong("a tab", &tab));
                };
                let revision = next()?;
                let Value::Str(revision) = &revision else {
                    return Err(wrong("a layout revision string", &revision));
                };
                let revision = revision
                    .parse()
                    .map_err(|()| fail(Raise::BadValue, "a layout revision is TAB:REVISION:RUN"))?;
                let tree = next()?;
                let Value::List(list) = &tree else {
                    return Err(wrong("a flat layout list", &tree));
                };
                fux_wire::layout::admit_nodes(list.items().len())?;
                let mut nodes = Vec::with_capacity(list.items().len());
                let mut of = None;
                for node in list.items() {
                    let node = if let Some(pane) = as_pane(node) {
                        of = super::same_run(of, super::scope(node))?;
                        fux_wire::layout::Node::Pane(pane)
                    } else if let Value::List(fields) = node {
                        let [axis, first_share, second_share, first, second] = fields.items()
                        else {
                            return Err(fail(
                                Raise::BadValue,
                                "a split is list AXIS SHARE SHARE FIRST SECOND end",
                            ));
                        };
                        let Value::Str(axis) = axis else {
                            return Err(wrong("a string", axis));
                        };
                        let axis = match &***axis {
                            "horizontal" => Axis::Horizontal,
                            "vertical" => Axis::Vertical,
                            _ => {
                                return Err(fail(
                                    Raise::BadValue,
                                    "layout axis must be horizontal or vertical",
                                ));
                            }
                        };
                        fux_wire::layout::Node::Split {
                            axis,
                            first_share: share(first_share)?,
                            second_share: share(second_share)?,
                            first: within(first, 0..=i64::from(u32::MAX))?,
                            second: within(second, 0..=i64::from(u32::MAX))?,
                        }
                    } else {
                        return Err(wrong("a pane or split list", node));
                    };
                    nodes.push(node);
                }
                later(Ask::Server {
                    request: Box::new(
                        Command::Arrange {
                            target,
                            revision,
                            nodes,
                        }
                        .into(),
                    ),
                    client: None,
                    of,
                })
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
            Act::Move => {
                let target = pane(&next()?)?;
                let to = match next()? {
                    Value::App(Fux::Tab(t, _)) => MoveTo::Tab(t),
                    Value::App(Fux::Workspace(w, _)) => MoveTo::Workspace(WsRef::Id(w)),
                    Value::Str(s) if **s == *"new-tab" => MoveTo::NewTab,
                    Value::Str(s) if **s == *"new-workspace" => MoveTo::NewWorkspace,
                    value @ Value::Str(_) => {
                        let given = string(&value)?;
                        MoveTo::Beside(direction(&value).map_err(|_| {
                            let why = format!(
                                "{given:?} is not new-tab, new-workspace, left, right, up or down"
                            );
                            fail(Raise::BadValue, why)
                        })?)
                    }
                    value @ (Value::Int(_) | Value::Bool(_) | Value::List(_) | Value::App(_)) => {
                        return Err(wrong(
                            "a tab, workspace, new-tab, new-workspace or direction",
                            &value,
                        ));
                    }
                };
                tell(Command::MovePane { target, to })
            }
            Act::SwapWith => {
                let target = pane(&next()?)?;
                let value = next()?;
                let with = match value {
                    Value::Str(_) => SwapWith::Toward(direction(&value)?),
                    value @ (Value::Int(_) | Value::Bool(_) | Value::List(_) | Value::App(_)) => {
                        SwapWith::Pane(pane(&value)?)
                    }
                };
                tell(Command::SwapPane { target, with })
            }
            Act::Reorder => {
                let t = target(&next()?)?;
                let toward = sibling(&next()?)?;
                tell(Command::Reorder { target: t, toward })
            }
            Act::HistoryBudget => {
                let target = pane(&next()?)?;
                let bytes = HistoryBytes::read(&next()?).map_err(|why| fail(Raise::Range, why))?;
                tell(Command::HistoryBudget { target, bytes })
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
                let (target, built) = attach_options(act, next()?)?;
                check_attach_creation(&built, target.is_some(), built.watch)?;
                local(Local::Attach {
                    of: None,
                    target,
                    read_only: built.watch,
                    size: built.size.map(|screen| screen.size()),
                    creation: Box::new(built),
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
            Act::Show => {
                let c = client(&next()?)?;
                let action = match target(&next()?)? {
                    AnyRef::Pane(p) => ClientAction::SelectPane(PanePick::Id(p)),
                    AnyRef::Tab(t) => ClientAction::SelectTab(Pick::Id(t)),
                    AnyRef::Workspace(w) => ClientAction::SelectWorkspace(Pick::Id(WsRef::Id(w))),
                };
                order(c, action)
            }
            Act::StepTab => order(client(&next()?)?, ClientAction::SelectTab(pick(&next()?)?)),
            Act::StepWorkspace => order(
                client(&next()?)?,
                ClientAction::SelectWorkspace(pick(&next()?)?),
            ),
            Act::Zoom => order(client(&next()?)?, ClientAction::Zoom),
            Act::Detach => order(client(&next()?)?, ClientAction::Detach),
            // ------------------------------------------------------ server
            Act::Server => local(Local::Server(fux_wire::protocol::Choice::Name(string(
                &next()?,
            )?))),
            Act::Socket => local(Local::Server(fux_wire::protocol::Choice::Path(
                string(&next()?)?.into(),
            ))),
            Act::Start => local(Local::Start),
            Act::Status => local(Local::Status),
            Act::KillServer => tell(Command::KillServer),
            Act::ForceKillServer => local(Local::ForceKill),
            Act::AttachDefaults => {
                self.prepare()
                    .map_err(|why| fail(Raise::Config, why.to_string()))?;
                let value = next()?;
                let of = super::scope(&value);
                let (target, built) = attach_options(act, value)?;
                if target.is_some() && built.given.contains(&Opt::Name) {
                    return Err(fail(
                        Raise::BadValue,
                        "attach-defaults takes no name with a target",
                    ));
                }
                let state = self.state.as_deref_mut().ok_or_else(|| {
                    fail(
                        Raise::NotConfig,
                        "attach-defaults needs this client's local state",
                    )
                })?;
                let name = if target.is_some() {
                    state.attach.as_ref().and_then(|a| a.name.clone())
                } else {
                    built.name
                };
                state.attach = Some(crate::client_state::AttachDefaults { of, target, name });
                Ok(Done::Now)
            }
            Act::Set => {
                self.prepare()
                    .map_err(|failed| fail(Raise::Config, failed.to_string()))?;
                let name = string(&next()?)?;
                let value = next()?;
                let state = self
                    .state
                    .as_deref_mut()
                    .ok_or_else(|| fail(Raise::NotConfig, "set needs this client's local state"))?;
                state
                    .preferences
                    .set(&name, &value)
                    .map_err(|why| fail(Raise::Config, why))?;
                Ok(Done::Now)
            }
            Act::Version => {
                let value = next()?;
                if let Value::App(Fux::Pane(p, _)) = value {
                    later(ask(Command::Inspect {
                        scope: fux_wire::inspect::Scope::Target(AnyRef::Pane(p)),
                        projection: fux_wire::inspect::Projection::Field("version".into()),
                    }))
                } else {
                    now(out, Value::str(version(&value)?.to_string()))
                }
            }
            // ------------------------------------------------------- shell
            Act::Args => local(Local::Shell(Shell::Args)),
            Act::Input => local(Local::Shell(Shell::Input)),
            Act::ReadFile => local(Local::Shell(Shell::ReadFile(string(&next()?)?))),
            Act::Getenv => {
                let name = string(&next()?)?.parse();
                let name = name.map_err(|why: String| fail(Raise::BadValue, why))?;
                local(Local::Shell(Shell::Getenv(name)))
            }
            Act::Warn => local(Local::Shell(Shell::Warn(next()?.text()?))),
            Act::Exit => local(Local::Shell(Shell::Exit(within(&next()?, 0..=255)?))),
            // ------------------------------------------------------ config
            // A client's own; else the bundled keys, without a user config.
            Act::ListKeys => {
                self.prepare()
                    .map_err(|failed| fail(Raise::Config, failed.to_string()))?;
                let text = match &self.state {
                    Some(state) => state.keys.describe(),
                    None => super::default_state()
                        .map_err(|e| fail(Raise::Config, e.to_string()))?
                        .keys
                        .describe(),
                };
                self.show(&text)
            }
            Act::KeyNames => self.show(&format!("{}\n", fux_vt::keys::all_names().join(" "))),
            // -------------------------------------------------------- help
            Act::Help => self.show(&help::overview()),
            Act::Explain => {
                let word = string(&next()?)?;
                let text = help::explain(&word)
                    .ok_or_else(|| fail(Raise::UnknownWord, help::unknown(&word)))?;
                self.show(&text)
            }
        }
    }
}

/// `job` compiled as its keys or screen will run it, with the config's
/// definitions: its mistake said as `what`'s, of `code`.
fn checked(
    state: &crate::client_state::Draft,
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
    super::compiled(state.config.as_ref(), &tokens)
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
            Screen::Choose => {
                let heading = next()?;
                let (title, about) = match &heading {
                    Value::Str(_) => (string(&heading)?, None),
                    Value::List(list) => match list.items() {
                        [title, subject] => (
                            string(title)?,
                            Some((target(subject)?, super::scope(subject))),
                        ),
                        _ => {
                            return Err(fail(
                                Raise::BadValue,
                                "title is text or [text,subject handle]",
                            ));
                        }
                    },
                    Value::Int(_) | Value::Bool(_) | Value::App(_) => {
                        return Err(wrong("a title or [text,subject handle]", &heading));
                    }
                };
                let rows = next()?;
                let Value::List(rows) = rows else {
                    return Err(wrong("rows", &rows));
                };
                let mut items = Vec::with_capacity(rows.items().len());
                for row in rows.items() {
                    let Value::List(row) = row else {
                        return Err(wrong("[payload,label]", row));
                    };
                    let [payload, label] = row.items() else {
                        return Err(fail(Raise::BadValue, "a row is [payload,label]"));
                    };
                    let payload = super::callback::data(payload)?;
                    let label = string(label)?;
                    items.push(fux_wire::callback::Item { payload, label });
                }
                let index = |v: Value<Fux>| -> Result<Option<usize>, Failure> {
                    let Value::List(list) = &v else {
                        return Err(wrong("[] or [index]", &v));
                    };
                    match list.items() {
                        [] => Ok(None),
                        [Value::Int(n)] => usize::try_from(*n)
                            .ok()
                            .filter(|n| *n < items.len())
                            .map(Some)
                            .ok_or_else(|| {
                                fail(Raise::BadValue, "index is outside the chooser rows")
                            }),
                        _ => Err(fail(
                            Raise::BadValue,
                            "an index is [] or [nonnegative integer]",
                        )),
                    }
                };
                let current = index(next()?)?;
                let selected = index(next()?)?;
                if selected.is_none() && !items.is_empty() {
                    return Err(fail(Raise::BadValue, "nonempty rows need a selected index"));
                }
                let action = Self::action(&next()?)?;
                Open::List(fux_wire::callback::ListScreen {
                    title,
                    items,
                    current,
                    selected: selected.unwrap_or(0),
                    about,
                    action,
                    client: fux_wire::callback::Data::Client(c, self.context.of),
                })
            }
            Screen::CopyMode => Open::Copy,
            Screen::CommandPrompt => Open::Prompt(fux_wire::callback::Prompt {
                title: ":".to_owned(),
                line: fux_wire::act::Line::new(String::new()),
                action: None,
            }),
            Screen::Prompt => {
                let title = text(&next()?)?;
                // Its start typed, so the line holds no more, nor any other,
                // than one typed: text alone, refused whole if it is not.
                let start = text(&next()?)?;
                let mut line = fux_wire::act::Line::new(String::new());
                let typed = match start.contains(char::is_control) {
                    true => Err("a line holds text alone, no control characters".to_owned()),
                    false => line.edit(&fux_wire::act::Edit::Insert(start)),
                };
                typed.map_err(|why| fail(Raise::BadValue, why))?;
                let action = Some(Self::action(&next()?)?);
                Open::Prompt(fux_wire::callback::Prompt {
                    title,
                    line,
                    action,
                })
            }
        };
        match here {
            true => local(Local::Open(open)),
            false => order(c, ClientAction::Open(open)),
        }
    }

    /// Source is explicit; captured values never pass through a tokenizer.
    fn action(value: &Value<Fux>) -> Result<fux_wire::callback::Callback, Failure> {
        let (code, args) = match value {
            Value::Str(_) => (string(value)?, Vec::new()),
            Value::List(v) => match v.items() {
                [Value::List(args), code] => {
                    let mut out = Vec::with_capacity(args.items().len());
                    for arg in args.items() {
                        out.push(super::callback::data(arg)?);
                    }
                    (string(code)?, out)
                }
                _ => {
                    return Err(fail(
                        Raise::BadValue,
                        "a callback is source text or [captured arguments,source text]",
                    ));
                }
            },
            Value::Int(_) | Value::Bool(_) | Value::App(_) => {
                return Err(wrong("a callback", value));
            }
        };
        Ok(fux_wire::callback::Callback { code, args })
    }

    /// A config's word: it changes the client's keymap.
    fn configure(
        &mut self,
        keys: Keys,
        next: &mut dyn FnMut() -> Result<Value<Fux>, Failure>,
    ) -> Result<Done<Ask>, Failure> {
        let refused_context = || {
            fail(
                Raise::NotConfig,
                "changes a client's keys: in its config, or typed at its prompt",
            )
        };
        if self.cli {
            return Err(refused_context());
        }
        let state = self.state.as_deref_mut().ok_or_else(refused_context)?;
        let refused = |why: String| fail(Raise::BadBinding, why);
        let keymap = &mut state.keys;
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
                checked(state, &job, Raise::BadBinding, &what)?;
                let keymap = &mut state.keys;
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
            Keys::BindAction => {
                let key = crate::keymap::key(&string(&next()?)?).map_err(refused)?;
                let code = string(&next()?)?;
                let action = Self::action(&Value::str(code))?;
                let tokens = fuxth::token::source("list action", &action.code)
                    .map_err(|e| refused(e.message))?;
                super::callback_source(state.config.as_ref(), &tokens)
                    .map_err(|e| refused(e.to_string()))?;
                let keymap = &mut state.keys;
                keymap
                    .modes
                    .list
                    .bind_action(key, action)
                    .map_err(refused)?;
                self.last_bound = Some(Bound::In(crate::keymap::Modal::List, key));
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
    use crate::TestResult;
    use crate::script::{Asks, Failed, Host, Local};
    use std::rc::Rc;

    /// Finds what an id names, and counts anything else a program asks of
    /// the server, or locally, answering each with a failure.
    struct Recorder(usize);

    impl Asks for Recorder {
        fn ask(
            &mut self,
            asked: &fux_wire::protocol::Asked,
        ) -> Result<fux_wire::protocol::Reply, Failed> {
            if let fux_wire::command::Request::Do(Command::Resolve { text, .. }) = &asked.request
                && let Ok(id) = text.parse::<AnyRef>()
            {
                return Ok(fux_wire::protocol::Body::Found(id).into());
            }
            self.0 = self.0.saturating_add(1);
            Err(Failed::new(
                fux_wire::code::Code::Client,
                "unexpected request",
            ))
        }
        fn local(
            &mut self,
            _: &Local,
            _: Option<&mut crate::client_state::Draft>,
            _: &mut dyn std::io::Write,
        ) -> Result<Vec<fuxth::Value<crate::script::Fux>>, Failed> {
            self.0 = self.0.saturating_add(1);
            Err(Failed::new(
                fux_wire::code::Code::Client,
                "unexpected local operation",
            ))
        }
    }

    /// Runs `code` with no server: its failure, how many requests it made,
    /// and what it printed.
    fn offline(code: &str) -> Result<(Option<Failed>, usize, String), Failed> {
        let tokens = fuxth::token::source("offline", code)?;
        let mut machine = crate::script::compile(&tokens)?;
        let mut state = crate::script::default_state()?.draft();
        let mut out = Vec::new();
        let mut host = Host::new(
            fux_wire::protocol::Context::default(),
            Some(&mut state),
            &mut out,
        );
        let mut recorder = Recorder(0);
        let ran = crate::script::run(&mut machine, &mut host, &mut recorder, None);
        drop(host);
        let printed = String::from_utf8_lossy(&out).into_owned();
        Ok((ran.err(), recorder.0, printed))
    }

    #[test]
    fn targeted_and_watch_attach_refuse_explicit_creation_before_any_request() -> TestResult {
        for selector in ["opts str main in", "opts watch"] {
            for option in [
                "str x name",
                "str / cwd",
                "str X str value env",
                "str X unset-env",
                "list end copy-environment",
                "0 history-bytes",
                "false program-clipboard",
                "str '' program",
                "list str /bin/sh end shell-program",
                "str /bin/sh program str -l arg",
            ] {
                let code = format!("str bell false set {selector} {option} 80 24 size attach");
                let tokens = fuxth::token::source("attach", &code)?;
                let mut machine = crate::script::compile(&tokens).map_err(|f| f.to_string())?;
                let mut state = crate::script::default_state()
                    .map_err(|f| f.to_string())?
                    .draft();
                let mut sink = Vec::new();
                let mut host = Host::new(
                    fux_wire::protocol::Context::default(),
                    Some(&mut state),
                    &mut sink,
                );
                let mut recorder = Recorder(0);
                let result = crate::script::run(&mut machine, &mut host, &mut recorder, None);
                assert!(
                    result.is_err_and(
                        |f| f.code == fux_wire::code::Code::from(fux_wire::code::Raise::Unused)
                    ),
                    "{code}"
                );
                assert_eq!(recorder.0, 0, "{code}");
            }
        }
        Ok(())
    }

    use super::*;
    use fuxth::AppValue;

    #[test]
    fn attach_subjects_and_defaults_retain_their_typed_target_and_run() -> TestResult {
        use super::*;
        use fux_wire::command::AttachTarget;
        let run = "1-2".parse().map_err(|()| "run")?;
        let mut draft = crate::script::default_state()?.draft();
        for (value, target) in [
            (
                Fux::Tab(fux_wire::id::TabId::of(2), Some(run)),
                AttachTarget::Tab(fux_wire::id::TabId::of(2)),
            ),
            (
                Fux::Pane(PaneId::of(3), Some(run)),
                AttachTarget::Pane(PaneId::of(3)),
            ),
        ] {
            let mut sink = Vec::new();
            let mut host = Host::new(
                fux_wire::protocol::Context::default(),
                Some(&mut draft),
                &mut sink,
            );
            let mut values = vec![Value::App(value.clone())].into_iter();
            let mut next = || {
                values
                    .next()
                    .ok_or_else(|| fail(Raise::StackUnderflow, "subject"))
            };
            assert!(matches!(
                host.dispatch(Word::Act(Act::AttachDefaults), &mut next, &mut Vec::new())
                    .map_err(|e| e.message)?,
                Done::Now
            ));
            let (attached, _) =
                attach_options(Act::Attach, Value::App(value)).map_err(|e| e.message)?;
            assert_eq!(attached, Some(target.clone()));
            let defaults = draft.attach.as_ref().ok_or("defaults")?;
            assert_eq!(defaults.target, Some(target));
            assert_eq!(defaults.of, Some(run));
            assert_eq!(defaults.name.as_deref(), Some("main"));
        }
        Ok(())
    }

    /// Each word checks its operands as it runs: given one of a kind it
    /// doesn't take, it fails there, after what came before it took effect
    /// (a line printed), and before it asks the server anything. The
    /// acting word refuses an option it takes none of (`unused`); a library
    /// word's guard says what it takes (`fail`).
    #[test]
    fn a_wrong_operand_fails_its_word_as_it_runs_before_it_asks() -> TestResult {
        let version = "%2:0.0.0.0.0:48211-1867a3f9c2b8e7d6";
        let mut cases = vec![
            ("type", "@1 version".to_owned()),
            (
                "type",
                "%1 str horizontal axis 30 str horizontal resize-pane".to_owned(),
            ),
            (
                "type",
                "%1 str horizontal axis str click 0 0 send-mouse".to_owned(),
            ),
            ("type", "str c1 client 0 timeout wait".to_owned()),
            ("type", "str x kill".to_owned()),
            ("type", "str nope str horizontal axis split".to_owned()),
            ("unused", format!("str {version} expect 0 timeout wait")),
            (
                "unused",
                format!("str {version} changed str x text send-keys"),
            ),
            (
                "unused",
                format!("str {version} changed all-history capture"),
            ),
            (
                "type",
                format!("str {version} changed str '' 0 next-events"),
            ),
            ("fail", "str nope info".to_owned()),
            ("fail", "str c1 client info".to_owned()),
            ("fail", "1 dump".to_owned()),
            ("fail", "str c1 client dump".to_owned()),
        ];
        for suffix in [
            "kill",
            "capture",
            "wait",
            "str x text send-keys",
            "panes",
            "tabs",
            "80 24 resize",
            "attach",
            "str x rename",
        ] {
            cases.push(("type", format!("all {suffix}")));
        }
        for suffix in [
            "full capture drop",
            "all-history capture drop",
            "0 1 rows capture drop",
            "main-screen capture drop",
        ] {
            cases.push(("type", format!("str c1 client {suffix}")));
        }
        for recipe in [
            "layout-current",
            "even-h",
            "even-v",
            "main-h",
            "main-v",
            "tiled",
        ] {
            for target in ["1", "%1", "+1", "all", "str tab", "str c1 client"] {
                cases.push(("fail", format!("{target} {recipe}")));
            }
        }
        let mut wrong = Vec::new();
        for (code, program) in cases {
            let source = format!("str SIDE-EFFECT print {program}");
            let (failed, asked, printed) = offline(&source)?;
            let failed = failed.map(|f| f.to_string()).unwrap_or_default();
            if !failed.starts_with(&format!("{code}:")) || asked != 0 || printed != "SIDE-EFFECT\n"
            {
                wrong.push(format!(
                    "{source}: {failed}, {asked} asked, printed {printed:?}"
                ));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        Ok(())
    }

    /// A closed pane's answer and a client's capture have no version:
    /// `changed`, `expect` and `version` refuse them as they run.
    #[test]
    fn closed_observations_have_no_version() -> TestResult {
        let version: fux_wire::command::Version =
            "%1:0.0.0.0.0:1-1".parse().map_err(|()| "version")?;
        let mut capture = fux_wire::answer::Captured::default_styled(version);
        capture.of = fux_wire::answer::Of::Client(fux_wire::id::ClientId::of(1));
        for answer in [
            super::super::Answer::Pane(fux_wire::answer::PaneAnswer::Closed(version.pane)),
            super::super::Answer::Capture(Box::new(capture)),
        ] {
            let value = Value::App(Fux::Answer(Rc::new(answer), Some(version.run)));
            let refused = super::version(&value).err().map(|f| f.code);
            assert_eq!(refused, Some(fuxth::Code::Type), "{}", value.kind());
        }
        Ok(())
    }

    #[test]
    fn options_past_a_launch_frame_are_refused_before_they_are_copied() -> TestResult {
        let allocator = fux_vt_peak::ThreadPeak;
        let list = |item: &str, n| {
            let list = fuxth::List::new(vec![Value::str(item); n]);
            Ok::<_, String>(Value::List(std::rc::Rc::new(list)))
        };
        let big = std::iter::repeat_n('x', 1 << 20).collect::<String>();
        // 8 MiB of options on a pane; a list of the same 8 MiB, past a
        // launch frame.
        let request_of = |argv| Opts {
            on: Some(On::Target(AnyRef::Pane(fux_wire::id::PaneId::of(1)))),
            argv,
            ..Opts::default()
        };
        let unique = || {
            Value::App(Fux::Opts(std::rc::Rc::new(request_of(vec![
                big.clone();
                8
            ]))))
        };
        let fresh = || Value::App(Fux::Opts(std::rc::Rc::default()));
        let bad = fail(Raise::BadValue, "").code;
        // An option on what `values` give, or an action consuming them.
        for (step, values, code) in [
            (Ok(Opt::ShellProgram), vec![fresh(), list(&big, 8)?], bad),
            (Ok(Opt::CopyEnvironment), vec![fresh(), list(&big, 8)?], bad),
            (Err(Act::Respawn), vec![unique()], bad),
        ] {
            let mut values = values.into_iter();
            let mut next = || {
                values
                    .next()
                    .ok_or_else(|| fail(Raise::StackUnderflow, "test argument"))
            };
            let mut sink = Vec::new();
            let mut host = Host::new(fux_wire::protocol::Context::default(), None, &mut sink);
            let base = allocator.live();
            allocator.reset_peak();
            let refused = match step {
                Ok(opt) => modify(opt, &mut next).err(),
                Err(act) => {
                    let word = Word::Act(act);
                    host.dispatch(word, &mut next, &mut Vec::new()).err()
                }
            };
            assert_eq!(refused.map(|f| f.code), Some(code), "{step:?}");
            assert!(allocator.peak().saturating_sub(base) < 64 << 10, "{step:?}");
        }
        Ok(())
    }

    #[test]
    fn accumulating_options_preserve_order() -> TestResult {
        let mut value = Value::App(Fux::Opts(std::rc::Rc::default()));
        for (opt, values) in [
            (Opt::Program, vec![Value::str("cat")]),
            (Opt::Arg, vec![Value::str("a")]),
            (Opt::Arg, vec![Value::str("b")]),
            (Opt::Env, vec![Value::str("X"), Value::str("first")]),
            (Opt::UnsetEnv, vec![Value::str("Y")]),
            (Opt::Env, vec![Value::str("Z"), Value::str("last")]),
            (Opt::Text, vec![Value::str("a")]),
            (Opt::Key, vec![Value::str("Enter")]),
            (Opt::Paste, vec![Value::str("b")]),
            (Opt::Bytes, vec![Value::str("63")]),
        ] {
            let mut args = std::iter::once(value).chain(values);
            value = Value::App(Fux::Opts(
                modify(opt, &mut || {
                    args.next()
                        .ok_or_else(|| fail(Raise::StackUnderflow, "test argument"))
                })
                .map_err(|f| f.message)?,
            ));
        }
        let built = modifying(value).map_err(|f| f.message)?;
        assert_eq!(built.argv, ["cat", "a", "b"]);
        assert_eq!(
            built.inputs,
            [
                Input::Text("a".into()),
                Input::Key("Enter".parse()?),
                Input::Paste("b".into()),
                Input::Bytes(vec![b'c'])
            ]
        );
        let env = built
            .env
            .as_slice()
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_ref().and_then(|v| v.to_str())))
            .collect::<Vec<_>>();
        assert_eq!(
            env,
            [("X", Some("first")), ("Y", None), ("Z", Some("last"))]
        );
        Ok(())
    }

    #[test]
    fn every_name_is_a_shell_word() {
        for name in Word::ALL.iter().map(|w| w.row().name) {
            assert!(
                !name.chars().any(|c| ";|&<>()$`*?[{!~#'\"\\".contains(c)),
                "{name}: a character a shell treats specially"
            );
        }
    }

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

    #[test]
    fn only_an_option_word_goes_on_a_request() {
        let pad = usage("pad");
        assert_eq!(
            pad.as_deref(),
            Some("use: X WIDTH FILL pad, which leaves s")
        );
        let name = usage("name").unwrap_or_default();
        assert!(name.starts_with("use: PANE-OR-OPTS NAME name"), "{name}");
    }

    #[test]
    fn modifiers_keep_the_target_and_baseline() -> TestResult {
        let version: fux_wire::command::Version = "%2:0.0.0.0.0:48211-1867a3f9c2b8e7d6"
            .parse()
            .map_err(|()| "version")?;
        let pane = Fux::Pane(version.pane, Some(version.run));
        let tab = Fux::Tab(fux_wire::id::TabId::of(1), Some(version.run));
        let workspace = Fux::Workspace(fux_wire::id::WsId::of(1), Some(version.run));
        let client = Fux::Client(fux_wire::id::ClientId::of(1), Some(version.run));
        let on = [
            None,
            Some(On::Target(AnyRef::Pane(version.pane))),
            Some(On::Target(AnyRef::Tab(fux_wire::id::TabId::of(1)))),
            Some(On::Target(AnyRef::Workspace(fux_wire::id::WsId::of(1)))),
            Some(On::Client(fux_wire::id::ClientId::of(1))),
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
                Rc::new(super::super::Answer::Pane(
                    fux_wire::answer::PaneAnswer::Acted(version),
                )),
                Some(version.run),
            ),
            Fux::Answer(
                Rc::new(super::super::Answer::Pane(
                    fux_wire::answer::PaneAnswer::Closed(version.pane),
                )),
                Some(version.run),
            ),
        ];
        targets.extend(on.map(|on| {
            Fux::Opts(std::rc::Rc::new(Opts {
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
                let effect = Word::Opt(opt).row().effect;
                let pane_only = effect.starts_with("pane-builder");
                if pane_only && !matches!(kind, "pane-request") {
                    continue;
                }
                let args = match opt {
                    Opt::HistoryBytes => vec![Value::Int(0)],
                    Opt::Reflow => vec![Value::Bool(false)],
                    Opt::AlternateScrollLines | Opt::PictureBytes => vec![Value::Int(0)],
                    Opt::ProgramClipboard => vec![Value::Bool(false)],
                    Opt::Name
                    | Opt::Cwd
                    | Opt::Program
                    | Opt::Arg
                    | Opt::Text
                    | Opt::Paste
                    | Opt::UnsetEnv => {
                        vec![Value::str("x")]
                    }
                    Opt::ShellProgram => {
                        vec![Value::List(std::rc::Rc::new(fuxth::List::new(vec![
                            Value::str("sh"),
                        ])))]
                    }
                    Opt::CopyEnvironment => {
                        vec![Value::List(std::rc::Rc::new(fuxth::List::new(vec![])))]
                    }
                    Opt::Size | Opt::Shares => vec![Value::Int(1), Value::Int(1)],
                    Opt::Rows | Opt::Cols => vec![Value::Int(0), Value::Int(1)],
                    Opt::Env => vec![Value::str("X"), Value::Int(1)],
                    Opt::In => vec![Value::str("+1")],
                    Opt::Timeout | Opt::History => vec![Value::Int(0)],
                    Opt::Key => vec![Value::str("Enter")],
                    Opt::Bytes => vec![Value::str("61")],
                    Opt::Axis => vec![Value::str("horizontal")],
                    Opt::Watch
                    | Opt::KillFirst
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
                    std::rc::Rc::make_mut(&mut built).argv.push("sh".into());
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
            }
        }
        Ok(())
    }
}
