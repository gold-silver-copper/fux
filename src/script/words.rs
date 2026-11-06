//! fux's words, one table of them (`words!`): each its family, name, what
//! it takes and gives in the names of the kinds it takes and gives
//! (`KINDS`, `NAMES`), a line of help, an example and the options it
//! takes; and what each does, run by the program's [`Host`].
use super::settings::Reads;
use super::{Ask, Bound, Fux, Host, Local, On, Opts, Shell};
use super::{Done, Failure, Value};
use crate::help;
use fux_vt::keys::Direction;
use fux_wire::callback::Open;
use fux_wire::code::Code;
use fux_wire::command::{
    AnyRef, Beside, Button, ClientAction, ClientRead, Command, Extent, HistoryBytes, Input, Kind,
    Limit, Members, Mouse, MoveTo, PaneResize, Percent, Program, Request, Resized, Sibling, Since,
    Source, SwapWith, To, Usage, Wait, Way, Wheel, WsRef,
};
use fux_wire::id::{ClientId, PaneId};
use fux_wire::layout::Axis;
use std::num::NonZeroU32;
use std::ops::RangeInclusive;

/// `words! { Heading…; Act { Heading Variant "name" "effect" raises(Code…
/// asks) "about" "example" [After: Opt…]; … } Family { … "example"; … } …
/// }` expands to
/// `Group`, the headings of `help` in its order, each named as its
/// variant; an enum of each family's words (the family is how the word
/// runs: `Opt` builds options or a request, `Screen` opens a client's
/// screen, `Keys` changes its keys, `Act` acts); `Word`, a word of any
/// family; `Word::ALL`, every word, family by family, the acting words
/// last, so that in `help` a heading's options come before the words that
/// take them (each word's id in the vocabulary is its place here); `Word::row`, its
/// facts: its name, effect, the codes it fails with (`asks`, those
/// asking a request does), help line, heading and an example; and
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
    (@raises [$($code:ident)*] $asks:literal) => {
        Raises {
            codes: &[$(Code::$code),*],
            asks: $asks,
        }
    };
    (@raises [$($code:ident)*] $asks:literal asks $($rest:ident)*) => {
        words!(@raises [$($code)*] true $($rest)*)
    };
    (@raises [$($code:ident)*] $asks:literal $next:ident $($rest:ident)*) => {
        words!(@raises [$($code)* $next] $asks $($rest)*)
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
        $($a_group:ident $act:ident $a_name:literal $a_effect:literal raises($($a_raise:ident)*)
            $a_about:literal $a_example:literal [$($takes:tt)*];)*
    }
    $($family:ident {
        $($group:ident $variant:ident $name:literal $effect:literal raises($($raise:ident)*)
            $about:literal $example:literal;)*
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
            /// Each word's name and the host function grift calls it by.
            pub(super) fn host_functions<'a, 'b, A: super::Asks + ?Sized>(
            ) -> Vec<(&'static str, grift::HostFunction<super::bridge::Runtime<'a, 'b, A>>)> {
                vec![
                    $($(($name, |g, state, args| {
                        super::bridge::request(g, state, Word::$family($family::$variant), args)
                    }),)*)*
                    $(($a_name, |g, state, args| {
                        super::bridge::request(g, state, Word::Act(Act::$act), args)
                    }),)*
                ]
            }

            pub const ALL: &[Word] = &[
                $($(Word::$family($family::$variant),)*)*
                $(Word::Act(Act::$act),)*
            ];

            pub fn row(self) -> &'static Row {
                match self {
                    $($(Word::$family($family::$variant) => &Row {
                        name: $name,
                        effect: $effect,
                        raises: words!(@raises [] false $($raise)*),
                        about: $about,
                        group: Group::$group,
                        example: $example,
                    },)*)*
                    $(Word::Act(Act::$act) => &Row {
                        name: $a_name,
                        effect: $a_effect,
                        raises: words!(@raises [] false $($a_raise)*),
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
    /// What it takes and gives, in the names of `KINDS` and `NAMES`:
    /// `x secs -- x`. bridge.rs reads it, for how many values the word takes
    /// and gives, and their kinds.
    pub effect: &'static str,
    pub raises: Raises,
    pub about: &'static str,
    pub group: Group,
    /// It in use, as a command line (or a config's line).
    pub example: &'static str,
}

/// The codes a word fails with as it runs: its own, and, if it `asks`,
/// those asking a request fails with (`Code::asked`); one that takes values
/// fails `stack_underflow` given too few, and `bad_value` given handles of
/// different server runs, besides.
pub struct Raises {
    pub codes: &'static [Code],
    pub asks: bool,
}

impl Word {
    /// The codes it fails with, as `Raises` says, each once.
    pub fn codes(self) -> Vec<Code> {
        let row = self.row();
        let takes = row
            .effect
            .split_once("--")
            .is_some_and(|(t, _)| !t.trim().is_empty());
        let taking = [Code::StackUnderflow, Code::BadValue]
            .into_iter()
            .filter(|_| takes);
        let own = (row.raises.codes.iter().copied()).chain(taking);
        let asked = Code::all().filter(|code| row.raises.asks && code.asked());
        let mut codes = Vec::new();
        for code in own.chain(asked) {
            if !codes.contains(&code) {
                codes.push(code);
            }
        }
        codes
    }
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

/// The words that start from opts, as the table says: `new-tab, attach
/// and attach-defaults`, `last` before the last.
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
pub const KINDS: [(&str, &str, Option<&str>); 21] = [
    ("pane", "a pane", None), ("tab", "a tab", None), ("workspace", "a workspace", None),
    ("all", "the whole session", None),
    ("client", "a client", None), ("size", "a size (from percent)", None),
    ("answer", "an action's answer", None),
    ("closed-answer", "a closed pane's answer", None),
    ("client-capture", "a client's capture", None),
    ("answers", "an answer of a tab's or workspace's panes", None), ("capture", "a capture", None),
    ("record", "a record", None), ("none", "none", None),
    ("code", "a code value another client sent, as its words, which execute compiles here", None),
    ("options", "options (from options, for new-tab, attach and attach-defaults; a pane's words take theirs on the pane: `(capture (all-history (pane 2)))`)", Some("nothing takes these options")),
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
    ("callback", "(code! (lambda! (...) ...)), or [captured arguments,(code! (lambda! (...) ...))]"),
    ("code", "(code! (lambda! (...) ...)), a code value"),
    ("revision", "a run-qualified layout revision"),
    ("tree", "a flat layout node list"),
    ("closed-outcome", "a closed pane or collection answer"),
    ("outcome", "an action's or wait's answer: a pane's, or a tab's or workspace's panes'"),
    ("x", "the pane it goes on, under its own values (or a tab, workspace, client, a request on one, or options)"),
    ("target", "a pane, tab, workspace or client, or a request on one"),
    ("subject", "a pane, or a request on one"),
    ("place", "a pane, tab or workspace, or a request on one"),
    ("handle", "a pane, tab or workspace"),
    ("attachable", "a pane, tab, workspace, a request on one, or options"),
    ("event-scope", "a pane, tab, workspace, client or all"),
    ("scope", "a pane, tab, workspace or all"),
    ("capture-source", "a pane, a request on one, or a client"),
    ("pane-builder", "a pane or a request on one"),
    ("ref", "a name, or a pane, tab or workspace"),
    ("text", "a string, a number or a boolean"),
    ("new", "a setting's value: a number, true or false, or a list (fux '(explain \"set\")')"),
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
        Finding Pane "pane" "ref -- pane" raises(Type NoTarget asks) "the pane ref names: its given name, %N, a pane, an answer or a version" "fux '(capture (pane \"web\"))'" [];
        Finding Tab "tab" "ref -- tab" raises(Type NoTarget asks) "the tab ref names: its name, @N, or a pane's tab" "fux '(sequence! (rename (tab (pane 3)) \"logs\") #inert)'" [];
        Finding Workspace "workspace" "ref -- workspace" raises(Type NoTarget asks) "the workspace ref names: its name, +N, or a pane's or tab's workspace" "fux '(sequence! (kill (workspace \"agent\")) #inert)'" [];
        Finding Here "here" "-- pane" raises(asks) "the pane this runs in (FUX_PANE)" "fux '(sequence! (notify (here) \"done\") #inert)'" [];
        Finding Client "client" "name -- client" raises(Type) "a client by its id, c2" "fux '(sequence! (detach (client \"c1\")) #inert)'" [];
        Finding Me "me" "-- client" raises(NotHere) "the client whose key or prompt runs this" "(sequence! (bind-key \"C-b z\" (code! (lambda! () (sequence! (zoom (focused (me))) #inert)))) #inert)" [];
        Finding Panes "panes" "scope -- list" raises(Type asks) "the panes in a pane (itself), a tab, a workspace or all, in layout order" "fux '(sequence! (fold (lambda! (accumulator-30 item-26) (let! ((() accumulator-30)) (sequence! (print (capture item-26)) (list)))) (list) (panes (workspace \"agent\"))) #inert)'" [];
        Finding Tabs "tabs" "scope -- list" raises(Type asks) "the tabs in a tab (itself), a workspace or all, in order; a pane holds none" "fux '(sequence! (fold (lambda! (accumulator-41 item-36) (let! ((() accumulator-41)) (sequence! \"name\" (print (get item-36 \"name\")) (list)))) (list) (tabs (workspace \"agent\"))) #inert)'" [];
        Finding All "all" "-- all" raises() "the whole session, for the words that take a scope: information, panes, tabs, kill, notify, events and dump" "fux '(information (all))'" [];
        Finding Focused "focused" "client -- pane" raises(Type asks) "the pane a client focuses: its tab's zoomed one while it has one" "fux '(capture (focused (client \"c1\")))'" [];
        Starting Opts "options" "-- options" raises() "empty options, for new-tab, attach and attach-defaults (the other words take them on the pane)" "fux '(new-tab (program (size (in (options) \"agent\") 80 24) \"./app\"))'" [];
        Starting Split "split" "subject -- answer" raises(Type Unused Usage Config asks) "split a pane along its required axis: a shell, or the program given" "fux '(split (axis (pane 3) \"vertical\"))'" [Pane: Axis Shares Cwd HistoryBytes Reflow AlternateScrollLines PictureBytes ProgramClipboard Program ShellProgram Arg];
        Starting Stack "stack" "subject -- answer" raises(Type Unused Config asks) "an alternative to a pane on its rectangle, in the stack it is a child of, else a new one: a shell, or the program given; each client is shown one child of a stack (step, show)" "fux '(stack (program (pane 3) \"/bin/sh\"))'" [Pane: Cwd HistoryBytes Reflow AlternateScrollLines PictureBytes ProgramClipboard Program ShellProgram Arg];
        Starting NewTab "new-tab" "options -- answer" raises(Type Unused Config asks) "a tab with the client's shell or explicit launch, in the workspace in names (made if it is not there; (in builder \"new-workspace\") makes an unnamed one), else the caller's" "fux '(new-tab (in (size (name (options) \"logs\") 80 24) \"agent\"))'" [Opts: Name Size Cwd HistoryBytes Reflow AlternateScrollLines PictureBytes ProgramClipboard Program ShellProgram Arg In];
        Starting Respawn "respawn" "subject -- answer" raises(Type Unused asks) "start again in its pane the program it was given (new-tab, split), or the one given now" "fux '(respawn (kill-first (pane 3)))'" [Pane: KillFirst Program Arg];
        Waiting Wait "wait" "place -- place" raises(Type Unused asks) "wait until the pane's version differs from the one changed gives, else an action's answer's (wait (line (pane 2) \"y\")), else now's, or the pane closes; a tab or workspace: any of the panes there as it begins" "A=$(fux '(line (pane 3) \"y\")'); fux '(wait (timeout (changed (first (arguments))) 60))' \"$A\"" [Pane: Changed Timeout];
        Input SendKeys "send-keys" "target -- target" raises(Type Unused Usage asks) "send the keys on it to a pane, every pane of a tab, or a client as typed at its terminal" "fux '(send-keys (key (text (pane 3) \"ls -l\") \"Enter\"))'" [Pane: Text Key Paste Bytes Expect];
        Input SendMouse "send-mouse" "pane action row col -- answer" raises(Type Range asks) "click right middle press release wheel-up wheel-down or move, at a cell from 0" "fux '(send-mouse (pane 3) \"click\" 2 10)'" [];
        Input Signal "signal" "subject name -- answer" raises(Type Unused asks) "TERM KILL INT HUP QUIT USR1 USR2 STOP CONT TSTP WINCH ILL ABRT FPE SEGV PIPE ALRM BUS TRAP SYS XCPU XFSZ VTALRM PROF TTIN TTOU URG CHLD, to the pane's foreground" "fux '(signal (pane 3) \"TERM\")'" [];
        Reading Actions "actions" "-- list" raises() "the native actions this program completed, in order" "fux '(catch! (sequence! (start) (raise (error (quote! fail)))) failure (actions))'" [];
        Reading DataText "data-text" "value -- string" raises(Type Unused) "serialize a value as readable grift data; retained captures stream one row at a time" "fux '(data-text (capture (pane 1)))'" [];
        Reading Datum "datum" "value -- data" raises(Type Unused) "expose a retained result as grift data, without reading it again" "fux '(datum (capture (pane 1)))'" [];
        Reading Capture "capture" "capture-source -- capture-source" raises(Type Unused asks) "a pane's observation (its facts, as information gives them) with its screen, read with its options; or what a client terminal shows (without pane options or a version)" "fux '(capture (history (pane 3) 100))'" [Pane: History Wrapped Full Styles Marks MainScreen AltScreen LastOutput Rows Cols Since];
        Reading Snapshot "snapshot" "snapshot-scope projection -- record" raises(Type asks) "one bounded observation: facts, topology or recipe" "fux '(data-text (snapshot (all) \"topology\"))'" [];
        Reading Get "get" "readable field -- value" raises(Type NoField Range TooLong asks) "a held answer, capture, record or failure field, locally; or a live pane, tab or workspace field" "fux '(sequence! (define! value-106 (pane 3)) \"program\" (define! value-108 (get value-106 \"program\")) \"exited\" (get value-108 \"exited\"))'" [];
        Reading NextEvents "next-events" "event-scope cursor secs -- record" raises(Type Range asks) "one bounded journal batch, with exact next cursor, missed count and status; empty cursor starts now" "fux '(data-text (next-events (all) \"\" 0))'" [];
        Changing Kill "kill" "scope --" raises(Type asks) "close a pane, tab or workspace, or all of them, without asking" "fux '(sequence! (kill (pane 3)) #inert)'" [];
        Changing Rename "rename" "handle text --" raises(Type asks) "rename a pane, tab or workspace, to a name no other one has" "fux '(sequence! (rename (pane 3) \"editor\") #inert)'" [];
        Changing Notify "notify" "scope text --" raises(Type asks) "a notice on the clients showing it, or every client with all" "fux '(sequence! (notify (all) \"done\") #inert)'" [];
        Changing Resize "resize" "sized columns rows --" raises(Type Size asks) "a tab layout or headless client size, each at least 1, as large as a screen's grid holds; a pane selects its tab" "fux '(sequence! (resize (tab 2) 120 40) #inert)'" [];
        Changing ResizeAuto "resize-auto" "handle --" raises(Type asks) "a tab laid out on the screens of the clients showing it again (at its size until one shows it)" "fux '(sequence! (resize-auto (tab 2)) #inert)'" [];
        Changing ResizePane "resize-pane" "pane extent way --" raises(Type Range asks) "a pane's horizontal or vertical extent: cells, or a percent" "fux '(sequence! (resize-pane (pane 3) (percent 30) \"horizontal\") #inert)'" [];
        Changing Percent "percent" "n -- size" raises(Type Range) "n percent of the tab, for resize-pane" "fux '(sequence! (resize-pane (pane 3) (percent 30) \"horizontal\") #inert)'" [];
        Changing Arrange "arrange" "tab revision tree --" raises(Type Range asks) "replace the whole layout at a run-qualified structural revision" "fux '(sequence! (arrange (tab 2) \"REVISION\" (list (pane 3))) #inert)'" [];
        Changing MoveBorder "move-border" "pane way cells --" raises(Type Range asks) "move the pane's nearest border: left right up down" "fux '(sequence! (move-border (pane 3) \"left\" 5) #inert)'" [];
        Changing Move "move" "subject where --" raises(Type asks) "move a pane to a tab, workspace, new-tab, new-workspace, or beside its neighbour: left right up down; or a workspace into another, after what it holds, or to the top with all" "fux '(sequence! (move (workspace \"agent1\") (workspace \"proj\")) #inert)'" [];
        Changing SwapWith "swap-with" "pane counterpart --" raises(Type asks) "swap a pane with another pane or its neighbour: left right up down" "fux '(sequence! (swap-with (pane 3) (pane 4)) #inert)'" [];
        Changing Reorder "reorder" "handle way --" raises(Type asks) "move one place: next or previous" "fux '(sequence! (reorder (tab 2) \"next\") #inert)'" [];
        Changing HistoryBudget "history-budget" "pane integer --" raises(Type Range asks) "change this pane's history budget in bytes; lowering evicts old rows" "fux '(sequence! (history-budget (pane 3) 4194304) #inert)'" [];
        Changing ClearHistory "clear-history" "pane --" raises(Type asks) "drop the pane's history" "fux '(sequence! (clear-history (pane 3)) #inert)'" [];
        Changing Zoom "zoom" "pane --" raises(Type asks) "fill its tab with the pane on every screen showing it, or restore the tab if it does" "fux '(sequence! (zoom (pane 3)) #inert)'" [];
        Clients Attach "attach" "attachable --" raises(Type Unused Nested NotHere asks) "show the server in this terminal (bare fux runs (attach (options))); with a size, be a client of no terminal instead; explicit targets and watch refuse creation options" "fux '(attach (tab 2))'" [Either: In Size Watch Cwd HistoryBytes Reflow AlternateScrollLines PictureBytes ProgramClipboard Program ShellProgram Arg Name];
        Clients Show "show" "client handle --" raises(Type asks) "show a client a pane, tab or workspace, and each that holds it, a pane's stacks too" "fux '(sequence! (show (client \"c1\") (pane 3)) #inert)'" [];
        Clients Step "step" "client handle way --" raises(Type asks) "show a client the next, previous or last shown of the handle's siblings (a pane's: the children of the stack nearest it, else its tab's panes shown), or a pane's neighbour left right up or down; a pane other than a zoomed one ends the zoom" "fux '(sequence! (step (client \"c1\") (tab 2) \"next\") #inert)'" [];
        Clients Detach "detach" "client --" raises(Type asks) "detach a client" "fux '(sequence! (detach (client \"c1\")) #inert)'" [];
        Clients Keymap "keys" "client -- record" raises(Type asks) "the client's keys, as it answers: at, the keys it has typed into the map it is in; keymap, its keys from the root, each one that runs a binding (kind binding: key, code, label, group) or holds a map of more (kind map: key, repeat, keymap), at any depth; list and line, its screens' keys (key, action or code, label). Each code is a code value of its words, which reading compiles nothing of: execute compiles them in this program's dictionary, as a callback's are, and runs them, failing there for a word it lacks; a callback or bind takes it as it came" "fux '(sequence! (define! value-195 (keys (client \"c1\"))) \"keymap\" (data-text (first (get value-195 \"keymap\"))))'" [];
        Clients Preferences "preferences" "client -- list" raises(Type asks) "each of the client's preferences, as it answers: its name, its value as set takes it, and its type" "fux '(data-text (preferences (client \"c1\")))'" [];
        Server Start "start" "--" raises(NotHere asks) "start the server in the background, if none answers" "fux '(sequence! (start) #inert)'" [];
        Server Status "status" "-- record" raises(NotHere asks) "the server that answers, and its version; with none, fails no_server" "fux '(status)' || fux '(start)'" [];
        Server KillServer "kill-server" "--" raises(asks) "stop the server: not from its own panes, nor with people attached" "FUX_SOCKET=$PWD/test.sock fux '(kill-server)'" [];
        Config AttachDefaults "attach-defaults" "attachable --" raises(Type Unused Config NotConfig) "replace this client's preferred attach place; target-only retains the first name, name-only or empty clears the target; target and name conflict" "(sequence! (attach-defaults (name (options) \"main\")) #inert)" [Either: In Name];
        Config Set "set" "setting new --" raises(Type Config NotConfig) "a preference for this client only: `fux '(explain \"set\")'` lists their effects" "fux '(sequence! (set \"history-bytes\" 16777216) #inert)'" [];
        Reading Version "version" "pane-or-seen -- string" raises(Type asks) "a pane's current version, or the version an answer, pane capture or canonical text holds" "fux '(version (capture (pane 3)))'" [];
        Shell Args "arguments" "-- list" raises() "the strings after the program on its command line, or after fux --file FILE (none for a key or a config)" "fux '(length (arguments))'" [];
        Shell Input "input" "-- string" raises(Read NotHere) "the rest of stdin, as UTF-8 text; not when the program came from stdin" "printf 'text\\n' | fux '(string-byte-length (input))'" [];
        Shell ReadFile "read-file" "path -- string" raises(Type Read NotHere) "a file's text, as UTF-8" "fux '(length (string-split (read-file \"notes.txt\") \"\\n\"))'" [];
        Shell Getenv "environment-variable" "name -- string" raises(Type UnsetVar NotHere) "an environment variable's value; one not set fails unset-variable" "fux '(environment-variable \"HOME\")'" [];
        Shell Warn "warn" "value --" raises(Output NotHere) "print a value on stderr, a line" "fux '(sequence! (warn \"no tests found\") (exit 2) #inert)'" [];
        Shell Exit "exit" "n --" raises(Type Range NotHere) "end the program now, what is left printed, with exit status n, 0 to 255" "fux '(sequence! (warn \"no tests found\") (exit 2) #inert)'" [];
        Config ListKeys "list-keys" "--" raises(Config Output) "this client's keys (the bundled keys from the CLI), as config lines that make them" "fux '(sequence! (list-keys) #inert)'" [];
        Config KeyNames "key-names" "--" raises(Output) "the names keys go by" "fux '(sequence! (key-names) #inert)'" [];
        Help Help "help" "--" raises(Output) "the words, under their headings" "fux '(sequence! (help) #inert)'" [];
        Help Word "word" "word -- record" raises(Type UnknownWord) "the dictionary's record of the word named so now: its name, provenance (core, fux, library, or run for this program's own), effect, documentation (all help says of it), definition (its words as read, or native) and codes (what it can fail with as it runs, or null where unknown)" "fux '(sequence! (define! value-241 (word \"dup\")) \"definition\" (get value-241 \"definition\"))'" [];
        Help Dictionary "dictionary" "-- list" raises() "a record of each word in this program's dictionary now, as word gives it, sorted by name" "fux '(map (lambda! (entry) (get entry \"name\")) (dictionary))'" [];
        Help Explain "explain" "word --" raises(Type UnknownWord Output) "one operation at length; data describes printed data and errors; agents is the first screen of the agent guide; guide prints it whole" "fux '(sequence! (explain \"capture\") #inert)'" [];
    }
    Opt {
        Starting Name "name" "x name -- x" raises(Type Unused) "the given name: new-tab's tab" "fux '(new-tab (program (size (name (in (options) \"agent\") \"web\") 80 24) \"./app\"))'";
        Starting Size "size" "x columns rows -- x" raises(Type Unused Size) "what its tab is always laid out on, each at least 1, as large as a screen's grid holds; needed from outside fux, else the caller's own screen or tab, as fux makes no size up; attach: be a client with no terminal, of this size, which prints its id and runs until signalled" "fux '(new-tab (program (size (in (options) \"agent\") 80 24) \"./app\"))'";
        Starting Cwd "directory" "x dir -- x" raises(Type Unused) "where it starts" "fux '(new-tab (program (size (directory (in (options) \"agent\") \"/srv\") 80 24) \"./app\"))'";
        Starting Program "program" "x path -- x" raises(Type Unused) "the program to start, in place of a shell; its pane stays once it exits" "fux '(new-tab (program (size (in (options) \"agent\") 80 24) \"./app\"))'";
        Starting HistoryBytes "history-bytes" "x integer -- x" raises(Type Unused Range) "history budget for the new pane, in bytes" "fux '(new-tab (program (history-bytes (in (options) \"agent\") 0) \"/bin/cat\"))'";
        Starting Reflow "reflow" "x boolean -- x" raises(Type Unused) "whether the new pane rewraps primary-screen lines on resize" "fux '(new-tab (program (reflow (in (options) \"agent\") #false) \"/bin/cat\"))'";
        Starting AlternateScrollLines "alternate-scroll-lines" "x integer -- x" raises(Type Unused Range) "arrows per alternate-screen wheel action (0 to 32767)" "fux '(new-tab (program (alternate-scroll-lines (in (options) \"agent\") 3) \"/bin/cat\"))'";
        Starting PictureBytes "picture-bytes" "x integer -- x" raises(Type Unused Range) "picture budget for the new pane (0 to 96 MiB); zero disables pictures" "fux '(new-tab (program (picture-bytes (in (options) \"agent\") 0) \"/bin/cat\"))'";
        Starting ProgramClipboard "program-clipboard" "x boolean -- x" raises(Type Unused) "whether the new pane's program may copy to the attached terminals' clipboards (OSC 52)" "fux '(new-tab (program (program-clipboard (in (options) \"agent\") #false) \"/bin/cat\"))'";
        Starting ShellProgram "shell-program" "x list -- x" raises(Type Unused) "the checked argv list to start with shell role; its exit closes the pane" "fux '(new-tab (shell-program (size (options) 80 24) (list \"/bin/sh\")))'";
        Starting Arg "argument" "x text -- x" raises(Type Unused Usage) "an argument for the program" "fux '(new-tab (argument (program (size (in (options) \"agent\") 80 24) \"./app\") \"-v\"))'";
        Starting Axis "axis" "x way -- x" raises(Type Unused) "horizontal (side by side) or vertical (stacked); split requires it" "fux '(split (axis (pane 3) \"horizontal\"))'";
        Starting Shares "shares" "x n n -- x" raises(Type Unused Range) "split: how the pane and the new one divide the pane's room, A to B (default: halved)" "fux '(split (shares (axis (pane 3) \"horizontal\") 2 1))'";
        Starting In "in" "x workspace -- x" raises(Type Unused) "the workspace for new-tab, attach or attach-defaults; again, one in the one before, for new-tab: it makes each from the first not there (a missing name, or a fresh unnamed one with (in builder \"new-workspace\")), the first at the top" "fux '(new-tab (size (in (in (options) \"proj\") \"agent1\") 80 24))'";
        Starting Watch "watch" "x -- x" raises(Type Unused) "attach without typing: keys do nothing but the prefix, then d, which detaches" "fux '(sequence! (attach (watch (options))) #inert)'";
        Starting KillFirst "kill-first" "x -- x" raises(Type Unused) "respawn: end the program first if it still runs" "fux '(respawn (kill-first (pane 3)))'";
        Waiting Changed "changed" "seen -- changed-request" raises(Type Unused) "a wait from this version (%2:0.57.0.3.0:RUN, as an action, a wait or `(get value \"version\")` gives it; or an answer or capture, its version) of the pane it names, not from now" "A=$(fux '(line (pane 3) \"y\")'); fux '(wait (timeout (changed (first (arguments))) 60))' \"$A\"";
        Waiting Timeout "timeout" "x secs -- x" raises(Type Unused Range) "how long a wait waits for a change before it answers timeout; with none, until something changes" "fux '(wait (timeout (pane 3) 300))'";
        Input Expect "expect" "seen -- expected-request" raises(Type Unused) "keys for the pane this version names, which send-keys sends only if the pane is still at it as they are queued, else sends none and fails stale; a version as changed takes it: a capture's is its pane's as it was read" "fux '(sequence! (define! variables-372 (context)) (bind variables-372 (quote! seen) (capture (pane 3))) (if! (not? (empty-list? (string-search (string (evaluate (quote! seen) variables-372)) \"[y/N]\" 0))) (sequence! (send-keys (text (expect (evaluate (quote! seen) variables-372)) \"y\")) (list)) (list)) #inert)'";
        Input Text "text" "x text -- x" raises(Type Unused) "text, typed" "fux '(send-keys (key (text (pane 3) \"ls -l\") \"Enter\"))'";
        Input Key "key" "x key -- x" raises(Type Unused NotAKey) "a key by its name: Enter, C-c, Up, F5" "fux '(send-keys (key (pane 3) \"C-c\"))'";
        Input Paste "paste" "x text -- x" raises(Type Unused) "text as a paste, bracketed if the program turned that on (an end mark, ESC [201~, in it then dropped); else as it is" "fux '(send-keys (key (paste (pane 3) \"a long prompt\") \"Enter\"))'";
        Input Bytes "bytes" "x bytes -- x" raises(Type Unused) "immutable bytes as they are, including non-UTF-8 input" "fux '(send-keys (bytes (pane 3) #bytes(27 91 65)))'";
        Reading History "history" "pane-builder lines -- pane-builder" raises(Type Unused Range) "capture: lines of history first (all-history: all of it)" "fux '(capture (history (pane 3) 100))'";
        Reading Wrapped "wrapped" "pane-builder -- pane-builder" raises(Type Unused) "capture: each screen row a line, as wrapped at the pane's width (the cells a mouse event counts), not joined into the lines printed" "fux '(capture (rows (wrapped (pane 3)) 0 2))'";
        Reading Full "full" "pane-builder -- pane-builder" raises(Type Unused) "capture: every row and cell, untrimmed" "fux '(capture (full (pane 3)))'";
        Reading Styles "styles" "pane-builder -- pane-builder" raises(Type Unused) "capture: colours and attributes" "fux '(capture (styles (pane 3)))'";
        Reading Marks "marks" "pane-builder -- pane-builder" raises(Type Unused) "capture: what only styling tells, as text: highlighted (reverse video, or a background colour) as ⟦this⟧, and rendered faint (SGR 2) as ⟨this⟩" "fux '(capture (marks (pane 3)))'";
        Reading MainScreen "main-screen" "pane-builder -- pane-builder" raises(Type Unused) "capture: what was printed before a program took the screen" "fux '(capture (main-screen (pane 3)))'";
        Reading AltScreen "alternate-screen" "pane-builder -- pane-builder" raises(Type Unused) "capture: the full screen a program drew last" "fux '(capture (alternate-screen (pane 3)))'";
        Reading LastOutput "last-output" "pane-builder -- pane-builder" raises(Type Unused) "capture: the last shell command's output, or the running one's so far (a full-screen one's: its screen to the cursor); fails no-marks where no command was marked (OSC 133); one of main-screen, alt-screen and last-output at most" "fux '(capture (last-output (pane 3)))'";
        Reading Rows "rows" "pane-builder from to -- pane-builder" raises(Type Unused Range) "capture: half-open from..to, clipped to available rows; joined lines touched whole (with wrapped, those rows alone)" "fux '(capture (rows (pane 3) 0 4))'";
        Reading Cols "columns" "pane-builder from to -- pane-builder" raises(Type Unused Range) "capture: half-open from..to columns, clipped to available cells" "fux '(capture (columns (pane 3) 0 40))'";
        Reading Since "since" "pane-builder seen -- pane-builder" raises(Type Unused) "capture: only the lines with a row changed since this version of the pane (a capture's, or as changed takes it), each after its row as rows counts it (3:text; data: changed-rows); fails stale where fux can't say which rows changed: rows moved since (a scroll, resize, reset, switch of screens or change to history), the program respawned, or it was read mid-repeat (REP)" "V=$(fux '(version (capture (pane 3)))'); fux '(capture (since (pane 3) (first (arguments))))' \"$V\"";
    }
    Screen {
        Clients Choose "choose" "client title list list list callback --" raises(Type TooLong NotHere asks) "a list of [payload,label] rows; current and selected are [] or [index]; callback receives captured arguments then client payload and leaves nothing" "fux '(sequence! (choose (client \"c1\") \"hosts\" (list (list \"host\" \"Host\")) () (list 0) (code! (lambda! (argument-467 argument-468) #inert))) #inert)'";
        Clients Prompt "prompt" "client name text callback --" raises(Type TooLong NotHere asks) "a line to type, titled name and starting as text: its submit key (Enter) runs code, the line its argument" "fux '(sequence! (prompt (client \"c1\") \"rename tab\" \"logs\" (code! (lambda! (argument-478) (sequence! (rename (tab 2) argument-478) #inert)))) #inert)'";
        Clients CommandPrompt "command-prompt" "client --" raises(Type NotHere asks) "a prompt for grift" "fux '(sequence! (command-prompt (client \"c1\")) #inert)'";
    }
    Keys {
        Config Bind "bind-key" "keys code --" raises(Type NotConfig BadBinding) "keys run code: each key but the last holds a map of more keys, made as needed, the first in the map a client is in while it holds none (config)" "(sequence! (bind-key \"C-b g v\" (code! (lambda! () (sequence! (split (axis (focused (me)) \"horizontal\")) #inert)))) #inert)";
        Config MoveKeys "move-keys" "from to --" raises(Type NotConfig BadBinding) "what keys run or hold, others do instead, and they nothing (config)" "(sequence! (move-keys \"C-b\" \"C-a\") #inert)";
        Config Label "label" "text --" raises(Type NotConfig BadBinding) "what the column says the last binding does, or a screen's hints the last key bound in it (keys of one label shown together)" "(sequence! (bind-key \"C-b g v\" (code! (lambda! () (sequence! (split (axis (focused (me)) \"horizontal\")) #inert)))) (label \"split right\") #inert)";
        Config Group "group" "name --" raises(Type NotConfig BadBinding) "the column's heading for the last binding" "(sequence! (bind-key \"C-b g v\" (code! (lambda! () (sequence! (split (axis (focused (me)) \"horizontal\")) #inert)))) (group \"panes\") #inert)";
        Config Repeat "repeat" "keys --" raises(Type NotConfig BadBinding) "the map the keys hold repeats: it stays after a key of it runs, until Esc (config)" "(sequence! (bind-key \"C-b g h\" (code! (lambda! () (sequence! (move-border (focused (me)) \"left\" 2) #inert)))) (repeat \"C-b g\") #inert)";
        Config Unbind "unbind" "keys --" raises(Type NotConfig BadBinding) "keys run or hold nothing, and a map they leave empty goes" "(sequence! (unbind \"C-b g v\") #inert)";
        Config BindAction "bind-action" "key code --" raises(Type NotConfig BadBinding) "a list's key runs code, receiving client payload; Escape cannot be bound" "(sequence! (bind-action \"r\" (code! (lambda! (argument-71 argument-72) (sequence! (rename-prompt argument-71 argument-72) #inert)))) (label \"rename\") #inert)";
        Config BindIn "bind-in" "screen key action --" raises(Type NotConfig BadBinding) "in one of fux's own screens (list line), a key does one of its actions, as the default keys bind each" "(sequence! (bind-in \"list\" \"g\" \"first\") (label \"first\") #inert)";
        Config UnbindIn "unbind-in" "screen key --" raises(Type NotConfig BadBinding) "in that screen, the key does what it would unbound: in a line, a character types itself; a named key with modifiers does what it does alone" "(sequence! (unbind-in \"list\" \"g\") #inert)";
        Config UnbindAll "unbind-all" "--" raises(NotConfig) "no bindings at all, nor keys in fux's screens (Escape still leaves them)" "unbind-all";
    }
}

/// A failure of a word's own: its code and why.
fn fail(code: Code, message: impl Into<String>) -> Failure {
    Failure::new(code, message)
}

/// How a word of fux's is used, for a mistake in using it: what goes
/// before it, what it leaves, and an example.
pub fn usage(word: &str) -> Option<String> {
    let row = Word::named(word)?.row();
    Some(format!(
        "({word} {})",
        row.effect.split_once("--")?.0.trim()
    ))
}

/// `failure`, a value of a kind the word doesn't take, told as the word's
/// effect names what it takes in that place (`taken`: the place and the
/// value's kind), as help says it. Each word checks a value before it takes
/// the next, so the value taken last is the one refused. A refusal of what
/// a value holds never comes here: the word checks it through `Host::held`.
fn described(word: Word, taken: Option<(usize, &'static str)>, mut failure: Failure) -> Failure {
    let Some((at, kind)) = taken else {
        return failure;
    };
    let effect = word
        .row()
        .effect
        .split_once("--")
        .map_or("", |(takes, _)| takes);
    let name = effect.split_whitespace().nth(at).unwrap_or_default();
    let said = match name {
        "integer" => Some("an integer"),
        "string" => Some("a string"),
        "boolean" => Some("a boolean"),
        "list" => Some("a list"),
        name => (NAMES.iter().map(|(n, said)| (*n, *said)))
            .chain(KINDS.iter().map(|(n, said, _)| (*n, *said)))
            .find(|(n, _)| *n == name)
            .map(|(_, said)| said),
    };
    let refused = failure.code == Code::Type
        && failure.message.starts_with("takes ")
        && failure.message.ends_with(&format!(", not {kind}"));
    if let Some(said) = said.filter(|_| refused) {
        failure.message = format!("takes {said}, not {kind}");
    }
    failure
}

/// A value read as `T`: one of a kind `T` doesn't take fails `type`, and one
/// of its kind that `T` refuses fails `code`.
fn read<T: Reads>(value: &Value<Fux>, code: Code) -> Result<T, Failure> {
    T::read(value).map_err(|unread| refused(unread, code))
}

/// A reader's refusal: `type` for a value of another kind, else `code`.
fn refused(unread: super::settings::Unread, code: Code) -> Failure {
    match unread {
        super::settings::Unread::Kind(why) => fail(Code::Type, why),
        super::settings::Unread::Value(why) => fail(code, why),
    }
}

fn wrong(wanted: &str, got: &Value<Fux>) -> Failure {
    fail(Code::Type, format!("takes {wanted}, not {}", got.kind()))
}

fn string(value: &Value<Fux>) -> Result<String, Failure> {
    let (Value::Str(s) | Value::Symbol(s)) = value else {
        return Err(wrong("a string", value));
    };
    Ok(s.to_string())
}

/// A value as words that take text read it: a string, or a number.
fn text(value: &Value<Fux>) -> Result<String, Failure> {
    match value {
        Value::Str(_) | Value::Symbol(_) => string(value),
        Value::Int(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        other @ (Value::Float(_) | Value::Bytes(_) | Value::List(_) | Value::App(_)) => {
            Err(wrong("a string, a number or a boolean", other))
        }
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
    NonZeroU32::new(n).ok_or_else(|| fail(Code::Range, "a share is 1 or more"))
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
        fail(Code::Range, format!("{n} is out of range: {said}"))
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
fn one_of<T: Clone>(value: &Value<Fux>, all: &[(&str, T)]) -> Result<T, Failure> {
    let given = string(value)?;
    let found = all.iter().find(|(name, _)| *name == given);
    found.map(|(_, t)| t.clone()).ok_or_else(|| {
        let names: Vec<&str> = all.iter().map(|(name, _)| *name).collect();
        let names = fux_wire::words::listed(&names, "or");
        fail(Code::BadValue, format!("{given:?} is not {names}"))
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
    if let Value::App(Fux::Record(record)) = value
        && matches!(record.held_kind(), Some(("answer" | "closed-answer", _)))
        && let Ok(Value::App(Fux::Pane(p, _))) = record.get("pane")
    {
        return Some(p);
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
        .map_err(|e| fail(Code::BadValue, e.to_string()))
}

fn scope(value: &Value<Fux>) -> Result<Option<AnyRef>, Failure> {
    match value {
        Value::App(Fux::All) => Ok(None),
        value @ (Value::Float(_)
        | Value::Symbol(_)
        | Value::Bytes(_)
        | Value::Int(_)
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
            Code::BadValue,
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
    Err(fail(Code::Unused, instead))
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
        // A capture's is for `expect` and `changed` alone (`version`).
        Value::App(Fux::Record(record))
            if matches!(record.held_kind(), Some(("answer" | "closed-answer", _))) =>
        {
            record.version()
        }
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

/// An explicit baseline is consumed only by its corresponding action.
const CHANGED: &str = "takes no request changed made: only wait uses its baseline";

const EXPECTED: &str = "takes no request expect made: only send-keys checks a version";

/// The version `value` holds (`Answer::version`), or is as text.
fn version(value: &Value<Fux>) -> Result<fux_wire::command::Version, Failure> {
    let wanted = "a version: an action's or wait's answer, a pane's capture, or its text";
    let text: &str = match value {
        Value::App(Fux::Record(record)) if record.is_held() => {
            return record.version().ok_or_else(|| wrong(wanted, value));
        }
        Value::Str(text) => text,
        other @ (Value::Float(_)
        | Value::Symbol(_)
        | Value::Bytes(_)
        | Value::Int(_)
        | Value::Bool(_)
        | Value::List(_)
        | Value::App(_)) => {
            return Err(wrong(wanted, other));
        }
    };
    text.parse().map_err(|()| {
        let why = format!(
            "{:?} is no version: PANE:RESPAWNS.SCREEN.BELLS.DONE.ENDED:RUN, as an action answers it",
            text
        );
        fail(Code::BadValue, why)
    })
}

/// The options or request `value` is: options stay options; what else it
/// goes on (a pane, tab, workspace or client, an action's answer) becomes
/// a request on that.
fn modifying(value: Value<Fux>) -> Result<std::rc::Rc<Opts>, Failure> {
    let built = match value {
        Value::App(Fux::Opts(built)) => built,
        other @ (Value::Float(_)
        | Value::Symbol(_)
        | Value::Bytes(_)
        | Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_)) => {
            let on = on(&other)
                .map_err(|_| wrong("a pane, tab, workspace or client, or options", &other))?;
            std::rc::Rc::new(Opts {
                on: Some(on),
                subject: Some(other),
                ..Opts::default()
            })
        }
    };
    Ok(built)
}

/// The request or options `steps` make of `subject`, as grift data says
/// them: each option as it ran, so what it checked is checked again.
pub(super) fn replayed(
    subject: Option<Value<Fux>>,
    steps: Vec<(Opt, Vec<Value<Fux>>)>,
) -> Result<Value<Fux>, Failure> {
    let mut run = subject
        .as_ref()
        .map(super::checked_scope)
        .transpose()?
        .flatten();
    let mut built = match subject {
        Some(subject) => Value::App(Fux::Opts(modifying(subject)?)),
        None => Value::App(Fux::Opts(std::rc::Rc::default())),
    };
    for (opt, arguments) in steps {
        let mut given = std::iter::once(built.clone()).chain(arguments);
        if matches!(opt, Opt::Changed | Opt::Expect) {
            given.next();
        }
        let mut next = || {
            let value = given.next().ok_or_else(|| {
                fail(
                    Code::StackUnderflow,
                    format!("too few values: {}", Word::Opt(opt).row().effect),
                )
            })?;
            run = super::same_run(run, super::checked_scope(&value)?)?;
            Ok(value)
        };
        built = Value::App(Fux::Opts(modify(opt, &mut next)?));
        if given.next().is_some() {
            let why = format!("too many values: {}", Word::Opt(opt).row().effect);
            return Err(fail(Code::StackUnderflow, why));
        }
    }
    if let Value::App(Fux::Opts(opts)) = &mut built {
        let version = match opts.on {
            Some(On::Version(v) | On::Changed(v) | On::Seen(v)) => Some(v.run),
            _ => None,
        };
        std::rc::Rc::make_mut(opts).of = super::same_run(run, version)?;
    }
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

/// A directory: its text, its bytes, or as a saved session holds one, a
/// record of its `encoding` and `text`, so one that isn't text comes back
/// byte for byte.
fn directory(value: &Value<Fux>) -> Result<std::path::PathBuf, Failure> {
    let bytes = match value {
        Value::Bytes(bytes) => (**bytes).clone(),
        Value::List(place) => {
            let field = |name: &str| {
                let found = place.items().iter().find_map(|entry| {
                    let Value::List(entry) = entry else {
                        return None;
                    };
                    let [Value::Symbol(key), value] = entry.items() else {
                        return None;
                    };
                    (**key == name).then_some(value)
                });
                found
                    .ok_or_else(|| wrong("a directory", value))
                    .and_then(string)
            };
            let text = field("text")?;
            if field("encoding")? == "utf8" {
                return Ok(text.into());
            }
            let digit = |c: u8| {
                char::from(c)
                    .to_digit(16)
                    .and_then(|d| u8::try_from(d).ok())
            };
            let (pairs, odd) = text.as_bytes().as_chunks::<2>();
            let byte = |[high, low]: &[u8; 2]| {
                Some(
                    digit(*high)?
                        .saturating_mul(16)
                        .saturating_add(digit(*low)?),
                )
            };
            let bytes = pairs.iter().map(byte).collect::<Option<_>>();
            bytes
                .filter(|_| odd.is_empty())
                .ok_or_else(|| fail(Code::BadValue, format!("{text:?}: no directory's hex")))?
        }
        other @ (Value::Float(_)
        | Value::Symbol(_)
        | Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::App(_)) => return Ok(string(other)?.into()),
    };
    path_of_bytes(bytes)
}

#[cfg(unix)]
fn path_of_bytes(bytes: Vec<u8>) -> Result<std::path::PathBuf, Failure> {
    use std::os::unix::ffi::OsStringExt;
    Ok(std::ffi::OsString::from_vec(bytes).into())
}

/// A Windows path's WTF-8 bytes (UTF-8, a lone surrogate as its three
/// bytes), made again as the UTF-16 units they are.
#[cfg(windows)]
fn path_of_bytes(bytes: Vec<u8>) -> Result<std::path::PathBuf, Failure> {
    use std::os::windows::ffi::OsStringExt;
    let bad = || fail(Code::BadValue, "a directory's bytes are no WTF-8");
    let mut units = Vec::with_capacity(bytes.len());
    let mut rest = bytes.as_slice();
    while !rest.is_empty() {
        let text = match std::str::from_utf8(rest) {
            Ok(text) => text,
            Err(error) => rest
                .split_at_checked(error.valid_up_to())
                .and_then(|(valid, _)| std::str::from_utf8(valid).ok())
                .ok_or_else(bad)?,
        };
        units.extend(text.encode_utf16());
        rest = rest.get(text.len()..).ok_or_else(bad)?;
        if let [0xED, high @ 0xA0..=0xBF, low @ 0x80..=0xBF, after @ ..] = rest {
            units.push(0xD000 | (u16::from(*high) & 0x3F) << 6 | u16::from(*low) & 0x3F);
            rest = after;
        } else if !rest.is_empty() {
            return Err(bad());
        }
    }
    Ok(std::ffi::OsString::from_wide(&units).into())
}

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
        Opt::Arg | Opt::Text | Opt::Key | Opt::Paste | Opt::Bytes | Opt::In
    ) && built.given.iter().any(|given| given.slot() == opt.slot())
    {
        return Err(fail(
            Code::Unused,
            format!("{opt}: {} is already given", opt.slot()),
        ));
    }
    built.given.push(opt);
    let mut arguments = Vec::new();
    let mut taken = || {
        let value = next()?;
        arguments.push(value.clone());
        Ok(value)
    };
    apply(opt, &mut built, &mut taken)?;
    built.steps.push((opt, arguments));
    Ok(std::rc::Rc::new(built))
}

/// `opt`, its values from `next`, on `built`.
fn apply(
    opt: Opt,
    built: &mut Opts,
    next: &mut dyn FnMut() -> Result<Value<Fux>, Failure>,
) -> Result<(), Failure> {
    match opt {
        Opt::Changed => built.on = Some(On::Changed(version(&next()?)?)),
        Opt::Expect => built.on = Some(On::Seen(version(&next()?)?)),
        Opt::Name => built.name = Some(string(&next()?)?),
        Opt::Cwd => built.cwd = Some(directory(&next()?)?),
        Opt::In => built.place.push(workspace(&next()?)?),
        Opt::HistoryBytes => {
            built.history_bytes = Some(read::<HistoryBytes>(&next()?, Code::Range)?);
        }
        Opt::Reflow => built.reflow = Some(read::<bool>(&next()?, Code::BadValue)?),
        Opt::AlternateScrollLines => {
            built.alternate_scroll_lines = Some(read::<fux_wire::command::ScrollLines>(
                &next()?,
                Code::Range,
            )?)
        }
        Opt::PictureBytes => {
            built.picture_bytes = Some(read::<fux_wire::command::PictureBytes>(
                &next()?,
                Code::Range,
            )?)
        }
        Opt::ProgramClipboard => {
            built.program_clipboard = Some(read::<bool>(&next()?, Code::BadValue)?);
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
            let limit = Limit::read(secs).map_err(|why| fail(Code::Range, why))?;
            built.timeout = Some(limit);
        }
        Opt::Text => built.inputs.push(Input::Text(text(&next()?)?)),
        Opt::Paste => built.inputs.push(Input::Paste(text(&next()?)?)),
        Opt::Bytes => {
            let v = next()?;
            let Value::Bytes(bytes) = v else {
                return Err(wrong("bytes", &v));
            };
            built.inputs.push(Input::Bytes((*bytes).clone()));
        }
        Opt::Program | Opt::ShellProgram => {
            let shell = opt == Opt::ShellProgram;
            built.shell = Some(shell);
            if shell {
                let program = read::<Program>(&next()?, Code::BadValue)?;
                built.argv = program.into_argv();
                return Ok(());
            }
            built.argv.push(string(&next()?)?);
        }
        Opt::Arg => {
            let arg = text(&next()?)?;
            if built.argv.is_empty() {
                return Err(fail(Code::Usage, "comes after program"));
            }
            built.argv.push(arg);
        }
        Opt::MainScreen => built.view.source = Source::Main,
        Opt::AltScreen => built.view.source = Source::Alternate,
        Opt::LastOutput => built.view.source = Source::LastOutput,
        Opt::Rows => built.view.rows = Some(span(opt, next)?),
        Opt::Cols => built.view.cols = Some(span(opt, next)?),
        Opt::Since => built.view.since = Some(version(&next()?)?),
        Opt::Key => {
            let word = string(&next()?)?;
            let press = word.parse().map_err(|_| {
                fail(
                    Code::NotAKey,
                    format!("{word:?} is no key's name (key-names lists them)"),
                )
            })?;
            built.inputs.push(Input::Key(press));
        }
    }
    Ok(())
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
        return Err(fail(Code::Range, why));
    }
    Ok((from, to))
}

/// What an action acts on, and what it was given: a request; or a pane,
/// tab, workspace or client alone, given nothing. Refused if `word`
/// leaves something given unused.
fn request(word: Act, value: Value<Fux>) -> Result<(On, Opts), Failure> {
    let mut built = match value {
        Value::App(Fux::Opts(built)) => owned(built),
        other @ (Value::Float(_)
        | Value::Symbol(_)
        | Value::Bytes(_)
        | Value::Int(_)
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
            "takes a pane, not options: what options gives is for {}; the rest go on the pane (`(capture (all-history (pane 2)))`, `(split (axis (pane 2) \"vertical\"))`)",
            from_opts("and")
        );
        return Err(fail(Code::Type, why));
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
        (On::Seen(_), _) => Err(fail(Code::Type, EXPECTED)),
        (On::Changed(_), _) => Err(fail(Code::Type, CHANGED)),
        (On::Target(AnyRef::Tab(_) | AnyRef::Workspace(_)) | On::Client(_), _) => Err(fail(
            Code::Type,
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
        other @ (Value::Float(_)
        | Value::Symbol(_)
        | Value::Bytes(_)
        | Value::Int(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_)) => Err(wrong(
            &format!("options (from options), as {word} takes"),
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
    let target = match (&built.on, built.place.as_slice()) {
        (_, [_, _, ..]) => {
            return Err(fail(Code::BadValue, "attach takes one workspace in"));
        }
        (None, place) => place.first().cloned().map(T::Workspace),
        (Some(On::Target(target)), []) => Some(match target {
            AnyRef::Workspace(ws) => T::Workspace(WsRef::Id(*ws)),
            AnyRef::Tab(tab) => T::Tab(*tab),
            AnyRef::Pane(pane) => T::Pane(*pane),
        }),
        (Some(On::Version(version)), []) => Some(T::Pane(version.pane)),
        (Some(_), [_]) => {
            return Err(fail(Code::BadValue, "attach takes no in with a subject"));
        }
        (Some(_), []) => {
            return Err(fail(
                Code::Type,
                "attach needs a workspace, tab, pane or options",
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
            Code::Unused,
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

/// `resize-pane`: an explicit axis and extent, cells or a percent.
fn resize_to(next: &mut dyn FnMut() -> Result<Value<Fux>, Failure>) -> Result<Done<Ask>, Failure> {
    let target = pane(&next()?)?;
    let extent = match next()? {
        Value::App(Fux::Extent(e)) => e,
        v @ Value::Int(_) => Extent::Cells(within(&v, CELL)?),
        other @ (Value::Float(_)
        | Value::Symbol(_)
        | Value::Bytes(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::App(_)) => {
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
        Value::Str(s) | Value::Symbol(s) => {
            let text: &str = &s;
            // An empty name never names the caller's pane, which `here` is.
            if text.trim().is_empty() {
                return Err(fail(
                    Code::NoTarget,
                    format!(
                        "an empty name names no {}: `here` is the pane this runs in",
                        kind.name()
                    ),
                ));
            }
            resolve(text.to_owned(), kind)
        }
        other @ (Value::Float(_)
        | Value::Bytes(_)
        | Value::Int(_)
        | Value::Bool(_)
        | Value::List(_)
        | Value::App(_)) => Err(wrong("a name, or a pane, tab or workspace", &other)),
    }
}

/// A request for the panes or tabs of `target`, as a list of handles.
fn members_of(target: Option<AnyRef>, members: Members) -> Ask {
    ask(Command::Members { target, members })
}

fn ask(request: impl Into<Request>) -> Ask {
    Ask::Server {
        request: Box::new(request.into()),
        client: None,
        of: None,
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

/// What only `client` knows, asked of it (`Request::Read`).
fn read_client(client: ClientId, read: ClientRead) -> Result<Done<Ask>, Failure> {
    let request = Box::new(Request::Read { client, read });
    Ok(Done::Later(Ask::Server {
        request,
        client: Some(client),
        of: None,
    }))
}

/// An order to a client that answers nothing.
fn order(client: ClientId, action: ClientAction) -> Result<Done<Ask>, Failure> {
    Ok(Done::Later(on_client(client, action)))
}

impl Host<'_> {
    pub(super) fn call(
        &mut self,
        word: Word,
        args: Vec<Value<Fux>>,
        out: &mut Vec<Value<Fux>>,
    ) -> Result<Done<Ask>, Failure> {
        let mut of = self.context.of;
        let mut args = args.into_iter();
        // The place in the effect and the kind of the value taken last.
        let mut taken: Option<(usize, &'static str)> = None;
        let mut next = || {
            let value = args.next().ok_or_else(|| {
                fail(
                    Code::StackUnderflow,
                    format!("too few values: {}", word.row().effect),
                )
            })?;
            of = super::same_run(of, super::checked_scope(&value)?)?;
            taken = Some((taken.map_or(0, |(i, _)| i.saturating_add(1)), value.kind()));
            Ok(value)
        };
        let from = out.len();
        let result = self.dispatch(word, &mut next, out);
        let held = std::mem::take(&mut self.held);
        let result = result.map_err(|failure| match held {
            true => failure,
            false => described(word, taken, failure),
        })?;
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
                Value::Float(_)
                | Value::Symbol(_)
                | Value::Bytes(_)
                | Value::Int(_)
                | Value::Bool(_)
                | Value::Str(_)
                | Value::List(_)
                | Value::App(_) => {}
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
                configured,
                ..
            })) => local(Local::Attach {
                of,
                target,
                read_only,
                size,
                creation,
                configured,
            }),
            result @ (Done::Now | Done::Later(_)) => Ok(result),
        }
    }
}

impl Host<'_> {
    fn launch(
        &mut self,
        built: &Opts,
    ) -> Result<(fux_wire::command::Start, fux_wire::command::PaneOptions), Failure> {
        self.prepare(false)
            .map_err(|why| fail(Code::Config, why.to_string()))?;
        let preferences = &self
            .state
            .as_deref()
            .ok_or_else(|| fail(Code::Config, "launch needs client defaults"))?
            .preferences;
        let options = built.pane_options(
            preferences
                .pane_options()
                .map_err(|why| fail(Code::Config, why))?,
        );
        built
            .launch(preferences.shell().map_err(|why| fail(Code::Config, why))?)
            .map(|start| (start, options))
            .map_err(|why| fail(Code::BadValue, why))
    }
}

impl Host<'_> {
    /// `checked`, a check of what a value holds (a list's elements): its
    /// failure is told as it is, not as what the word takes in that place.
    fn held<T>(&mut self, checked: Result<T, Failure>) -> Result<T, Failure> {
        self.held |= checked.is_err();
        checked
    }

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
                    .map_err(|e: Usage| fail(Code::BadValue, e.to_string()))?;
                now(out, Value::App(Fux::Client(id, None)))
            }
            Act::Me => match self.context.client {
                Some(c) => now(out, Value::App(Fux::Client(c, self.context.of))),
                None => Err(fail(
                    Code::NotHere,
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
                let (start, options) = self.launch(&built)?;
                let tab = Command::NewTab {
                    target: built.place,
                    name: built.name,
                    size: built.size,
                    cwd: built.cwd,
                    options,
                    start,
                };
                later(ask(tab))
            }
            Act::Split => {
                let (p, built) = request_pane(act, next()?)?;
                let axis = built.axis.ok_or_else(|| {
                    fail(
                        Code::Usage,
                        "split requires a direction: (split (axis (pane 2) \"horizontal\"))",
                    )
                })?;
                let (start, options) = self.launch(&built)?;
                let split = Command::Split {
                    beside: Beside::Split(axis, built.shares),
                    target: p,
                    start,
                    cwd: built.cwd,
                    options,
                };
                later(ask(split))
            }
            Act::Stack => {
                let (p, built) = request_pane(act, next()?)?;
                let (start, options) = self.launch(&built)?;
                let stack = Command::Split {
                    beside: Beside::Stack,
                    target: p,
                    start,
                    cwd: built.cwd,
                    options,
                };
                later(ask(stack))
            }
            Act::Respawn => {
                let (p, mut built) = request_pane(act, next()?)?;
                let respawn = Command::Respawn {
                    target: p,
                    kill: built.kill,
                    argv: if built.argv.is_empty() {
                        None
                    } else {
                        Some(built.program().map_err(|why| fail(Code::BadValue, why))?)
                    },
                };
                later(ask(respawn))
            }
            // ----------------------------------------------------- waiting
            Act::Wait => {
                let (on, built) = request(act, next()?)?;
                // On a tab or workspace, in each of its panes.
                let since = match on {
                    On::Version(version) | On::Changed(version) => Since::Version(version),
                    On::Seen(_) => return Err(fail(Code::Type, EXPECTED)),
                    On::Target(target) => Since::Now(target),
                    On::Client(_) => {
                        let why = "takes a pane, tab or workspace, not a client";
                        return Err(fail(Code::Type, why));
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
                        Code::Usage,
                        "needs keys: (send-keys (text (pane 2) \"hello\"))",
                    ));
                }
                let to = match on {
                    On::Target(target) => To::Now(target),
                    On::Version(version) => To::Now(AnyRef::Pane(version.pane)),
                    On::Seen(version) => To::Seen(version),
                    On::Changed(_) => return Err(fail(Code::Type, CHANGED)),
                    On::Client(c) => {
                        // Typed at its terminal: whatever pane they reach is
                        // the client's to choose, so there is none to wait on
                        // here.
                        if built.inputs.iter().any(|i| matches!(i, Input::Bytes(_))) {
                            return Err(fail(
                                Code::Unused,
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
            Act::Actions => now(
                out,
                Value::List(std::rc::Rc::new(super::List::new(
                    self.actions.iter().map(Value::str).collect(),
                ))),
            ),
            Act::Datum => {
                let value = next()?;
                if matches!(value, Value::App(Fux::Opts(_))) {
                    let why = "act on the request before reading its data";
                    return Err(Failure::new(Code::Unused, why));
                }
                now(out, super::record::plain(value))
            }
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
            Act::Panes => later(members_of(scope(&next()?)?, Members::Panes)),
            Act::Tabs => later(members_of(scope(&next()?)?, Members::Tabs)),
            Act::Get => {
                let value = next()?;
                // A live field's handle, checked before the field is taken.
                let live = match &value {
                    Value::App(Fux::Record(_)) => None,
                    value @ (Value::Float(_)
                    | Value::Symbol(_)
                    | Value::Bytes(_)
                    | Value::Int(_)
                    | Value::Bool(_)
                    | Value::Str(_)
                    | Value::List(_)
                    | Value::App(_)) => Some(target(value)?),
                };
                let field = string(&next()?)?;
                if let Some(target) = live {
                    return later(ask(Command::Inspect {
                        scope: fux_wire::inspect::Scope::Target(target),
                        projection: fux_wire::inspect::Projection::Field(super::native_field(
                            &field,
                        )),
                    }));
                }
                match value {
                    Value::App(Fux::Record(record)) => now(out, record.get(&field)?),
                    other @ (Value::Float(_)
                    | Value::Symbol(_)
                    | Value::Bytes(_)
                    | Value::Int(_)
                    | Value::Bool(_)
                    | Value::Str(_)
                    | Value::List(_)
                    | Value::App(_)) => Err(wrong("a handle or held observation", &other)),
                }
            }
            Act::NextEvents => {
                let scope = observation_scope(&next()?)?;
                let text = string(&next()?)?;
                let from = if text.is_empty() {
                    None
                } else {
                    Some(cursor(&Value::str(text))?)
                };
                let timeout = Limit::read(within(&next()?, 0..=i64::from(u32::MAX))?)
                    .map_err(|why| fail(Code::Range, why))?;
                tell(Request::NextEvents {
                    scope,
                    from,
                    timeout,
                })
            }
            // ---------------------------------------------------- changing
            Act::Kill => tell(Command::Kill {
                target: scope(&next()?)?,
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
                // Checked before the size is taken.
                let sized = match next()? {
                    Value::App(Fux::Client(c, _)) => Err(c),
                    value @ (Value::Float(_)
                    | Value::Symbol(_)
                    | Value::Bytes(_)
                    | Value::Int(_)
                    | Value::Bool(_)
                    | Value::Str(_)
                    | Value::List(_)
                    | Value::App(_)) => Ok(target(&value)?),
                };
                let size = laid_out(&next()?, &next()?)?;
                let resized = match sized {
                    Err(c) => Resized::Client(c, size),
                    Ok(target) => Resized::Tabs(target, Some(size)),
                };
                tell(Command::Resize(resized))
            }
            Act::ResizeAuto => tell(Command::Resize(Resized::Tabs(target(&next()?)?, None))),
            Act::ResizePane => resize_to(&mut next),
            Act::Percent => {
                let n = within(&next()?, Percent::SHARES)?;
                let share = Percent::new(n).ok_or_else(|| fail(Code::Range, "no share"))?;
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
                    .map_err(|()| fail(Code::BadValue, "a layout revision is TAB:REVISION:RUN"))?;
                let tree = next()?;
                let Value::List(list) = &tree else {
                    return Err(wrong("a flat layout list", &tree));
                };
                fux_wire::layout::admit_nodes(list.items().len())?;
                let mut nodes = Vec::with_capacity(list.items().len());
                let mut of = None;
                let read = (|| {
                    for node in list.items() {
                        let node = if let Some(pane) = as_pane(node) {
                            of = super::same_run(of, super::checked_scope(node)?)?;
                            fux_wire::layout::Node::Pane(pane)
                        } else if let Value::List(fields) = node
                            && let [Value::Str(kind), Value::List(children)] = fields.items()
                            && &***kind == "stack"
                        {
                            let index = |c| within(c, 0..=i64::from(u32::MAX));
                            let children = children.items().iter().map(index);
                            fux_wire::layout::Node::Stack {
                                children: children.collect::<Result<_, _>>()?,
                            }
                        } else if let Value::List(fields) = node {
                            let [axis, first_share, second_share, first, second] = fields.items()
                            else {
                                return Err(fail(
                                    Code::BadValue,
                                    "a split is (list AXIS SHARE SHARE FIRST SECOND); a stack is (list \"stack\" (list CHILD ...))",
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
                                        Code::BadValue,
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
                            return Err(wrong("a pane, split or stack list", node));
                        };
                        nodes.push(node);
                    }
                    Ok(())
                })();
                self.held(read)?;
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
                let subject = next()?;
                let target = if let Value::App(Fux::Workspace(w, _)) = subject {
                    AnyRef::Workspace(w)
                } else {
                    AnyRef::Pane(pane(&subject)?)
                };
                let to = match next()? {
                    Value::App(Fux::All) => MoveTo::Top,
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
                            fail(Code::BadValue, why)
                        })?)
                    }
                    value @ (Value::Float(_)
                    | Value::Symbol(_)
                    | Value::Bytes(_)
                    | Value::Int(_)
                    | Value::Bool(_)
                    | Value::List(_)
                    | Value::App(_)) => {
                        return Err(wrong(
                            "a tab, workspace, all, new-tab, new-workspace or direction",
                            &value,
                        ));
                    }
                };
                tell(Command::Move { target, to })
            }
            Act::SwapWith => {
                let target = pane(&next()?)?;
                let value = next()?;
                let with = match value {
                    Value::Str(_) => SwapWith::Toward(direction(&value)?),
                    value @ (Value::Float(_)
                    | Value::Symbol(_)
                    | Value::Bytes(_)
                    | Value::Int(_)
                    | Value::Bool(_)
                    | Value::List(_)
                    | Value::App(_)) => SwapWith::Pane(pane(&value)?),
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
                let bytes = read::<HistoryBytes>(&next()?, Code::Range)?;
                tell(Command::HistoryBudget { target, bytes })
            }
            Act::ClearHistory => tell(Command::ClearHistory {
                target: pane(&next()?)?,
            }),
            Act::Zoom => tell(Command::Zoom {
                target: pane(&next()?)?,
            }),
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
                    configured: None,
                })
            }
            Act::Show => {
                let c = client(&next()?)?;
                order(c, ClientAction::Show(target(&next()?)?))
            }
            Act::Step => {
                let c = client(&next()?)?;
                let from = target(&next()?)?;
                let ways = Direction::ALL.map(|way| (way.name(), Way::Toward(way)));
                let steps = [
                    ("next", Way::Step(Sibling::Next)),
                    ("previous", Way::Step(Sibling::Previous)),
                    ("last", Way::Last),
                ];
                let way = one_of(&next()?, &[ways.as_slice(), &steps].concat())?;
                order(c, ClientAction::Step { from, way })
            }
            Act::Detach => order(client(&next()?)?, ClientAction::Detach),
            Act::Keymap => read_client(client(&next()?)?, ClientRead::Keys),
            Act::Preferences => read_client(client(&next()?)?, ClientRead::Preferences),
            // ------------------------------------------------------ server
            Act::Start => {
                self.prepare(false)
                    .map_err(|why| fail(Code::Config, why.to_string()))?;
                local(Local::Start)
            }
            Act::Status => local(Local::Status),
            Act::KillServer => tell(Command::KillServer),
            Act::AttachDefaults => {
                self.prepare(false)
                    .map_err(|why| fail(Code::Config, why.to_string()))?;
                let value = next()?;
                let of = super::checked_scope(&value)?;
                let (target, built) = attach_options(act, value)?;
                if target.is_some() && built.given.contains(&Opt::Name) {
                    return Err(fail(
                        Code::BadValue,
                        "attach-defaults takes no name with a target",
                    ));
                }
                let state = self.state.as_deref_mut().ok_or_else(|| {
                    fail(
                        Code::NotConfig,
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
                self.prepare(false)
                    .map_err(|failed| fail(Code::Config, failed.to_string()))?;
                let name = string(&next()?)?;
                let value = next()?;
                let state = self
                    .state
                    .as_deref_mut()
                    .ok_or_else(|| fail(Code::NotConfig, "set needs this client's local state"))?;
                state
                    .preferences
                    .set(&name, &value)
                    .map_err(|why| refused(why, Code::Config))?;
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
                let name = name.map_err(|why: String| fail(Code::BadValue, why))?;
                local(Local::Shell(Shell::Getenv(name)))
            }
            Act::Warn => local(Local::Shell(Shell::Warn(next()?.text()?))),
            Act::Exit => local(Local::Shell(Shell::Exit(within(&next()?, 0..=255)?))),
            // ------------------------------------------------------ config
            // A client's own; else the bundled keys, without a user config.
            Act::ListKeys => {
                self.prepare(false)
                    .map_err(|failed| fail(Code::Config, failed.to_string()))?;
                let text = match &self.state {
                    Some(state) => state.keys.describe(),
                    None => super::default_state()
                        .map_err(|e| fail(Code::Config, e.to_string()))?
                        .keys
                        .describe(),
                };
                self.show(&text)
            }
            Act::KeyNames => self.show(&format!("{}\n", fux_vt::keys::all_names().join(" "))),
            // -------------------------------------------------------- help
            Act::Help => self.show(&help::overview()),
            // grift's own: `bridge` answers them, as they read any value.
            Act::DataText | Act::Word | Act::Dictionary => Ok(Done::Now),
            Act::Explain => {
                let word = string(&next()?)?;
                let text = help::explain(&word)
                    .ok_or_else(|| fail(Code::UnknownWord, help::unknown(&word)))?;
                self.show(&text)
            }
        }
    }
}

/// The binding just made, `bound`, which `label` and `group` change.
fn just_made<'k>(
    keymap: &'k mut crate::keymap::Keymap,
    bound: Option<&Bound>,
) -> Result<&'k mut crate::keymap::Binding, Failure> {
    let refused = |why: &str| fail(Code::BadBinding, why);
    let binding = match bound.ok_or_else(|| refused("changes the binding just made: bind first"))? {
        Bound::Keys(keys) => keymap.binding_mut(keys),
        Bound::In(..) => return Err(refused("a screen's key has a label only")),
    };
    binding.ok_or_else(|| refused("the binding just made is gone"))
}

impl Host<'_> {
    /// Writes `text` out.
    fn show(&mut self, text: &str) -> Result<Done<Ask>, Failure> {
        write!(self.out, "{text}").map_err(|e| fail(Code::Output, e.to_string()))?;
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
                    Value::List(list) => self.held(match list.items() {
                        [title, subject] => (|| {
                            let title = string(title)?;
                            Ok((
                                title,
                                Some((target(subject)?, super::checked_scope(subject)?)),
                            ))
                        })(),
                        _ => Err(fail(
                            Code::BadValue,
                            "title is text or [text,subject handle]",
                        )),
                    })?,
                    Value::Float(_)
                    | Value::Symbol(_)
                    | Value::Bytes(_)
                    | Value::Int(_)
                    | Value::Bool(_)
                    | Value::App(_) => {
                        return Err(wrong("a title or [text,subject handle]", &heading));
                    }
                };
                let rows = next()?;
                let Value::List(rows) = rows else {
                    return Err(wrong("rows", &rows));
                };
                let read = (|| {
                    let mut items = Vec::with_capacity(rows.items().len());
                    for row in rows.items() {
                        let Value::List(row) = row else {
                            return Err(wrong("[payload,label]", row));
                        };
                        let [payload, label] = row.items() else {
                            return Err(fail(Code::BadValue, "a row is [payload,label]"));
                        };
                        let payload = super::callback::data(payload)?;
                        let label = string(label)?;
                        items.push(fux_wire::callback::Item { payload, label });
                    }
                    Ok(items)
                })();
                let items = self.held(read)?;
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
                                fail(Code::BadValue, "index is outside the chooser rows")
                            }),
                        _ => Err(fail(
                            Code::BadValue,
                            "an index is [] or [nonnegative integer]",
                        )),
                    }
                };
                let current = index(next()?)?;
                let selected = index(next()?)?;
                if selected.is_none() && !items.is_empty() {
                    return Err(fail(Code::BadValue, "nonempty rows need a selected index"));
                }
                let action = self.action(&next()?)?;
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
                typed.map_err(|why| fail(Code::BadValue, why))?;
                let action = Some(self.action(&next()?)?);
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

    /// A callback: a code value, or one with the values it is given first
    /// (`[captured arguments, code … end]`), never a string.
    fn action(&mut self, value: &Value<Fux>) -> Result<fux_wire::callback::Callback, Failure> {
        let (code, args) = match value {
            Value::App(Fux::Code(_)) => (super::callback::code(value)?, Vec::new()),
            // What the pair holds, checked as such: its code's own
            // requirement is told.
            Value::List(v) => self.held(match v.items() {
                [Value::List(args), code] => (|| {
                    let mut out = Vec::with_capacity(args.items().len());
                    for arg in args.items() {
                        out.push(super::callback::data(arg)?);
                    }
                    Ok((super::callback::code(code)?, out))
                })(),
                _ => Err(fail(
                    Code::BadValue,
                    "a callback is (code! (lambda! (...) ...)) or [captured arguments,(code! (lambda! (...) ...))]",
                )),
            })?,
            Value::Float(_)
            | Value::Symbol(_)
            | Value::Bytes(_)
            | Value::Int(_)
            | Value::Bool(_)
            | Value::Str(_)
            | Value::App(_) => {
                return Err(wrong(
                    "(code! (lambda! (...) ...)) or [captured arguments,(code! (lambda! (...) ...))]",
                    value,
                ));
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
                Code::NotConfig,
                "changes a client's keys: in its config, or typed at its prompt",
            )
        };
        if self.cli {
            return Err(refused_context());
        }
        let state = self.state.as_deref_mut().ok_or_else(refused_context)?;
        state.rebound = true;
        let refused = |why: String| fail(Code::BadBinding, why);
        let keymap = &mut state.keys;
        let mut pressed = || crate::keymap::keys(&string(&next()?)?).map_err(refused);
        match keys {
            Keys::Bind => {
                let keys_text = string(&next()?)?;
                let code = super::callback::code(&next()?)?;
                let binding = crate::keymap::Binding::new(code);
                let pressed = crate::keymap::keys(&keys_text).map_err(refused)?;
                keymap.bind(&pressed, binding).map_err(refused)?;
                self.last_bound = Some(Bound::Keys(pressed));
            }
            Keys::MoveKeys => {
                let (from, to) = (pressed()?, pressed()?);
                keymap.move_keys(&from, &to).map_err(refused)?;
            }
            Keys::Unbind => keymap.unbind(&pressed()?).map_err(refused)?,
            Keys::Repeat => keymap.set_repeat(&pressed()?).map_err(refused)?,
            Keys::BindAction => {
                let key = crate::keymap::key(&string(&next()?)?).map_err(refused)?;
                let action = super::callback::code(&next()?)?.into();
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
        }
        Ok(Done::Now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TestResult;
    use std::rc::Rc;
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
                    .ok_or_else(|| fail(Code::StackUnderflow, "subject"))
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
    #[test]
    fn closed_observations_have_no_version() -> TestResult {
        let version: fux_wire::command::Version =
            "%1:0.0.0.0.0:1-1".parse().map_err(|()| "version")?;
        let mut capture = fux_wire::answer::Captured::default_styled(version);
        capture.of = fux_wire::answer::Of::Client {
            client: fux_wire::id::ClientId::of(1),
            rows: 40,
            cols: 120,
        };
        for answer in [
            super::super::Answer::Pane(fux_wire::answer::PaneAnswer::Closed(version.pane)),
            super::super::Answer::Capture(Box::new(capture)),
        ] {
            let value = answer.in_run(Some(version.run))?;
            let refused = super::version(&value).err().map(|f| f.code);
            assert_eq!(refused, Some(Code::Type), "{}", value.kind());
        }
        Ok(())
    }
    /// A Windows directory holding a lone surrogate comes back unit for unit.
    #[cfg(windows)]
    #[test]
    fn a_lone_surrogates_directory_is_made_again() -> TestResult {
        use std::os::windows::ffi::OsStrExt;
        let path = super::path_of_bytes(b"C:\\x\xed\xa0\x80y".to_vec()).map_err(|f| f.message)?;
        let units: Vec<u16> = path.as_os_str().encode_wide().collect();
        assert_eq!(units, [0x43, 0x3a, 0x5c, 0x78, 0xd800, 0x79]);
        Ok(())
    }
    #[test]
    fn accumulating_options_preserve_order() -> TestResult {
        let mut value = Value::App(Fux::Opts(std::rc::Rc::default()));
        for (opt, values) in [
            (Opt::Program, vec![Value::str("cat")]),
            (Opt::Arg, vec![Value::str("a")]),
            (Opt::Arg, vec![Value::str("b")]),
            (Opt::Text, vec![Value::str("a")]),
            (Opt::Key, vec![Value::str("Enter")]),
            (Opt::Paste, vec![Value::str("b")]),
            (Opt::Bytes, vec![Value::Bytes(Rc::new(vec![99]))]),
        ] {
            let mut args = std::iter::once(value).chain(values);
            value = Value::App(Fux::Opts(
                modify(opt, &mut || {
                    args.next()
                        .ok_or_else(|| fail(Code::StackUnderflow, "test argument"))
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
        Ok(())
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
        let mut targets = vec![pane, tab, workspace, client];
        for answer in [
            fux_wire::answer::PaneAnswer::Acted(version),
            fux_wire::answer::PaneAnswer::Closed(version.pane),
        ] {
            if let Value::App(answer) =
                super::super::Answer::Pane(answer).in_run(Some(version.run))?
            {
                targets.push(answer);
            }
        }
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
                    Opt::Name | Opt::Cwd | Opt::Program | Opt::Arg | Opt::Text | Opt::Paste => {
                        vec![Value::str("x")]
                    }
                    Opt::ShellProgram => {
                        vec![Value::List(std::rc::Rc::new(super::super::List::new(
                            vec![Value::str("sh")],
                        )))]
                    }
                    Opt::Size | Opt::Shares => vec![Value::Int(1), Value::Int(1)],
                    Opt::Rows | Opt::Cols => vec![Value::Int(0), Value::Int(1)],
                    Opt::Since => vec![Value::str("%1:0.0.0.0.0:1-1")],
                    Opt::In => vec![Value::str("+1")],
                    Opt::Timeout | Opt::History => vec![Value::Int(0)],
                    Opt::Key => vec![Value::str("Enter")],
                    Opt::Bytes => vec![Value::Bytes(Rc::new(vec![97]))],
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
                        .ok_or_else(|| fail(Code::StackUnderflow, "test argument"))
                })
                .map_err(|f| f.message)?;
                assert_eq!(actual.on, expected.on, "{target:?} {opt}");
                assert_eq!(Fux::Opts(actual).name(), kind, "{target:?} {opt}");
            }
        }
        Ok(())
    }
    #[test]
    fn every_word_fails_only_with_the_codes_it_declares() -> TestResult {
        use fux_wire::id::{ClientId, TabId, WsId};
        let opts = std::rc::Rc::new(Opts::default());
        let on_pane = Opts {
            on: Some(On::Target(AnyRef::Pane(PaneId::of(1)))),
            ..Opts::default()
        };
        let values = [
            Value::Int(0),
            Value::Int(-1),
            Value::Int(80),
            Value::str(""),
            Value::str("x"),
            Value::str("next"),
            Value::Bool(true),
            Value::List(Rc::new(crate::script::List::new(Vec::new()))),
            Value::List(Rc::new(crate::script::List::new(vec![Value::str("a")]))),
            Value::App(Fux::Pane(PaneId::of(1), None)),
            Value::App(Fux::Tab(TabId::of(1), None)),
            Value::App(Fux::Workspace(WsId::of(1), None)),
            Value::App(Fux::Client(ClientId::of(1), None)),
            Value::App(Fux::All),
            Value::App(Fux::Null),
            Value::App(Fux::Opts(opts)),
            Value::App(Fux::Opts(Rc::new(on_pane))),
            Value::App(Fux::Extent(Extent::Cells(3))),
        ];
        let mut draft = crate::script::default_state()?.draft();
        let mut seed = 1u64;
        let mut wrong = Vec::new();
        for &word in Word::ALL {
            let codes = word.codes();
            let takes = word
                .row()
                .effect
                .split_once("--")
                .map_or(0, |(t, _)| t.split_whitespace().count());
            for _ in 0..500 {
                let mut stack = Vec::new();
                for _ in 0..takes {
                    seed = seed
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    let at = usize::try_from(seed >> 33).unwrap_or(0) % values.len();
                    stack.extend(values.get(at).cloned());
                }
                let mut sink = Vec::new();
                let mut host = Host::new(
                    fux_wire::protocol::Context::default(),
                    Some(&mut draft),
                    &mut sink,
                );
                let mut stack = stack.into_iter();
                let mut next = || (stack.next()).ok_or_else(|| fail(Code::StackUnderflow, "test"));
                if let Err(failure) = host.dispatch(word, &mut next, &mut Vec::new())
                    && !codes.contains(&failure.code)
                {
                    wrong.push(format!(
                        "{word:?}: {}: {}",
                        failure.code.name(),
                        failure.message
                    ));
                }
            }
        }
        wrong.dedup();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        Ok(())
    }
    struct Recorder(usize);
    impl crate::script::Asks for Recorder {
        fn ask(
            &mut self,
            asked: &fux_wire::protocol::Asked,
        ) -> Result<fux_wire::protocol::Reply, crate::script::Failed> {
            if let Some(found) = crate::script::testing::found(asked) {
                return Ok(found);
            }
            self.0 = self.0.saturating_add(1);
            Err(crate::script::Failed::new(
                Code::Client,
                "unexpected request",
            ))
        }
        fn local(
            &mut self,
            _: &Local,
            _: Option<&mut crate::client_state::Draft>,
            _: &mut dyn std::io::Write,
        ) -> Result<Vec<Value<Fux>>, crate::script::Failed> {
            self.0 = self.0.saturating_add(1);
            Err(crate::script::Failed::new(
                Code::Client,
                "unexpected local action",
            ))
        }
    }
    fn offline(
        program: &str,
    ) -> Result<(Option<crate::script::Failed>, usize, String), crate::script::Failed> {
        let mut state = crate::script::default_state()?.draft();
        let mut out = Vec::new();
        let mut recorder = Recorder(0);
        let result =
            crate::script::testing::evaluate(program, Some(&mut state), &mut recorder, &mut out);
        Ok((
            result.err(),
            recorder.0,
            String::from_utf8_lossy(&out).into_owned(),
        ))
    }
    #[test]
    fn a_wrong_operand_fails_its_word_as_it_runs_before_it_asks() -> TestResult {
        let version = r#""%2:0.0.0.0.0:48211-1867a3f9c2b8e7d6""#;
        let mut cases = vec![
            (Code::Type, "(zoom 3)".to_owned()),
            (
                Code::Unused,
                r#"(new-tab (in (timeout (options) 1) "work"))"#.to_owned(),
            ),
            (Code::Type, r#"(information (client "c1"))"#.to_owned()),
            (Code::Type, format!("(all-history (changed {version}))")),
            (Code::Type, "(version (tab 1))".to_owned()),
            (
                Code::Type,
                r#"(resize-pane (axis (pane 1) "horizontal") 30 "horizontal")"#.to_owned(),
            ),
            (
                Code::Type,
                r#"(send-mouse (axis (pane 1) "horizontal") "click" 0 0)"#.to_owned(),
            ),
            (Code::Type, r#"(wait (timeout (client "c1") 0))"#.to_owned()),
            (Code::Type, r#"(kill "x")"#.to_owned()),
            (Code::Type, r#"(split (axis "nope" "horizontal"))"#.to_owned()),
            (Code::Unused, format!("(wait (timeout (expect {version}) 0))")),
            (
                Code::Unused,
                format!(r#"(send-keys (text (changed {version}) "x"))"#),
            ),
            (
                Code::Type,
                format!(r#"(next-events (changed {version}) "" 0)"#),
            ),
            (Code::Type, r#"(information "nope")"#.to_owned()),
            (Code::Type, "(dump 1)".to_owned()),
            (Code::Type, r#"(dump (client "c1"))"#.to_owned()),
            // An element of a list of the wrong kind is a list it refuses.
            (
                Code::BadValue,
                "(shell-program (options) (list 1))".to_owned(),
            ),
        ];
        // all-history's guard, whatever comes after it.
        for target in ["(options)", "(tab 1)", "(workspace 1)", r#"(client "c1")"#, "1"] {
            cases.push((Code::Type, format!("(all-history {target})")));
        }
        for call in [
            "(capture (all))",
            "(wait (all))",
            r#"(send-keys (text (all) "x"))"#,
            "(resize (all) 80 24)",
            "(attach (all))",
            r#"(rename (all) "x")"#,
        ] {
            cases.push((Code::Type, call.to_owned()));
        }
        for call in ["(full C)", "(rows C 0 1)", "(main-screen C)"] {
            let call = call.replace('C', r#"(client "c1")"#);
            cases.push((Code::Type, format!("(capture {call})")));
        }
        let root = env!("CARGO_MANIFEST_DIR").replace('\\', "/");
        let layouts = format!(r#"(require! "{root}/recipes/layouts.grift")"#);
        for recipe in ["even-h", "even-v", "main-h", "main-v", "tiled"] {
            for target in [
                "1",
                "(pane 1)",
                "(workspace 1)",
                "(all)",
                r#""tab""#,
                r#"(client "c1")"#,
            ] {
                cases.push((Code::Type, format!("{layouts} ({recipe} {target})")));
            }
        }
        let mut wrong = Vec::new();
        for (code, expression) in cases {
            let program = format!(r#"(print "before") {expression} (print "after")"#);
            let (failed, asked, text) = offline(&program)?;
            if failed.as_ref().map(|failure| failure.code) != Some(code)
                || asked != 0
                || text != "before\n"
            {
                let failed = failed.map(|f| f.to_string()).unwrap_or_default();
                wrong.push(format!("{expression}: {failed}, {asked} asked, printed {text:?}"));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        Ok(())
    }
    #[test]
    fn a_refusal_names_all_the_word_takes() -> TestResult {
        for (expression, about) in [
            (r#"(notify 5 "done")"#, "all"),
            (r#"(send-keys "x")"#, "client"),
            ("(swap-with (pane 1) 5)", "direction"),
            (r#"(resize "x" 80 24)"#, "headless client"),
            (r#"(get 5 "name")"#, "held observation"),
            (r#"(snapshot 5 "facts")"#, "scope"),
            (r#"(prompt (client "c1") "T" "s" 5)"#, "code!"),
        ] {
            let (failed, asked, _) = offline(expression)?;
            let failed = failed.ok_or("the wrong operand was accepted")?;
            assert_eq!(failed.code, Code::Type, "{expression}: {failed}");
            assert!(failed.message.contains(about), "{expression}: {failed}");
            assert_eq!(asked, 0, "{expression}");
        }
        for expression in [
            r#"(prompt (client "c1") "T" "s" "(kill (all))")"#,
            r#"(choose (client "c1") "T" (list (list 7 ())) () (list 0) (code! (lambda! (#ignore #ignore) #inert)))"#,
        ] {
            let (failed, asked, _) = offline(expression)?;
            assert_eq!(failed.map(|f| f.code), Some(Code::Type));
            assert_eq!(asked, 0);
        }
        Ok(())
    }
    #[test]
    fn a_reader_given_another_kind_fails_type() -> TestResult {
        for expression in [
            r#"(history-bytes (options) "nope")"#,
            "(alternate-scroll-lines (options) #true)",
            r#"(picture-bytes (options) "nope")"#,
            r#"(reflow (options) "nope")"#,
            "(program-clipboard (options) 1)",
            r#"(shell-program (options) "sh")"#,
            r#"(history-budget (pane 1) "nope")"#,
            r#"(set "bell" "off")"#,
        ] {
            let (failed, asked, text) = offline(&format!(
                "(catch-kind! {expression} type-error failure (print \"caught\"))"
            ))?;
            assert!(failed.is_none(), "{expression}: {failed:?}");
            assert_eq!((asked, text.as_str()), (0, "caught\n"));
        }
        for expression in [
            r#"(set "styles" (list 1))"#,
            r#"(set "styles" (list (list 5 1 2)))"#,
            r#"(set "bar" (list 1))"#,
            r#"(set "bar-slots" (list 1))"#,
            r#"(set "shell" (list 1))"#,
        ] {
            let (failed, _, text) = offline(&format!(
                "(catch-kind! {expression} config failure (print \"caught\"))"
            ))?;
            assert!(failed.is_none(), "{expression}: {failed:?}");
            assert_eq!(text, "caught\n");
        }
        for (expression, code) in [
            ("(history-bytes (options) -1)", Code::Range),
            ("(shell-program (options) ())", Code::BadValue),
        ] {
            assert_eq!(
                offline(expression)?.0.map(|f| f.code),
                Some(code),
                "{expression}"
            );
        }
        Ok(())
    }
    #[test]
    fn only_an_option_word_goes_on_a_request() {
        assert!(
            usage("add").is_none(),
            "grift's arithmetic is not an option builder"
        );
        let name = usage("name").unwrap_or_default();
        assert!(name.starts_with("(name ") && name.ends_with(')'), "{name}");
    }
    #[test]
    fn targeted_and_watch_attach_refuse_explicit_creation_before_any_request() -> TestResult {
        for selector in [r#"(in (options) "main")"#, "(watch (options))"] {
            for option in [
                r#"(name B "x")"#,
                r#"(directory B "/")"#,
                "(history-bytes B 0)",
                "(program-clipboard B #false)",
                r#"(program B "")"#,
                r#"(shell-program B (list "/bin/sh"))"#,
                r#"(argument (program B "/bin/sh") "-l")"#,
            ] {
                let program = format!(
                    "(set \"bell\" #false) (attach (size {} 80 24))",
                    option.replace('B', selector)
                );
                let (failure, asked, _) = offline(&program)?;
                assert_eq!(failure.map(|f| f.code), Some(Code::Unused), "{program}");
                assert_eq!(asked, 0, "{program}");
            }
        }
        Ok(())
    }
}
