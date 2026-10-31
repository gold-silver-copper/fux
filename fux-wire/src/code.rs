//! What kind of failure a failure is: its code, a name that stays the same,
//! for a script to tell failures apart by (`fux: CODE: MESSAGE`). Every
//! code fux gives is here, the language's own among them as fuxth names
//! them; `docs/json.md` lists them all.
use crate::command::{self, AnyRef, Kind};
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::words::joined;
use fux_vt::keys::Direction;
use std::num::NonZeroU8;

/// The exit statuses, a meaning each, besides 0: what was asked came.
/// Any failure but these.
const FAILED: NonZeroU8 = NonZeroU8::MIN;
/// The pane waited on closed.
const CLOSED: NonZeroU8 = NonZeroU8::MIN.saturating_add(3);
/// The time limit came first: as `timeout(1)`.
const TIMED_OUT: NonZeroU8 = NonZeroU8::MIN.saturating_add(123);

/// `codes! { raises [Lang…]; run_loop [Variant "name", …]; /// doc
/// Variant "name", Closed "closed" CLOSED, … }` expands to the enum `Code`
/// of those variants and `Lang`, the language's own codes; `Code::ALL`,
/// fux's own in order; `Code::status`, each one's exit status: the one its
/// row names (`Code::Closed => CLOSED`), else `FAILED`, as for the
/// language's; `Code::raised`, whether a word a program runs can fail with
/// it, so a `try` may catch it: not for a `run_loop` one, which only the
/// loop running the program gives; a language's code if fuxth's words
/// raise it, or fux's (`raises`); `Raise`, what a word's `fail` takes: the
/// rows' codes and `raises`'s, never a `run_loop` one; and
/// `fuxth_core::Code::from`, each one as the language takes it (one of fux's as
/// `Host`, by its name).
macro_rules! codes {
    (@status) => {
        FAILED
    };
    (@status $status:ident) => {
        $status
    };
    (raises [$($lang:ident),*];
    run_loop [$($(#[$loop_doc:meta])* $looped:ident $loop_name:literal,)*];
    $($(#[$doc:meta])* $variant:ident $name:literal $($status:ident)?,)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Code {
            $($(#[$doc])* $variant,)*
            $($(#[$loop_doc])* $looped,)*
            /// One of the language's own (fuxth's), as fuxth names it.
            Lang(LangCode),
        }

        impl Code {
            pub const ALL: &[Code] = &[$(Code::$variant,)* $(Code::$looped),*];

            /// The exit status a failure, or an answer, of this kind ends
            /// with.
            pub fn status(self) -> NonZeroU8 {
                match self {
                    $(Code::$variant => codes!(@status $($status)?),)*
                    $(Code::$looped)|* | Code::Lang(_) => FAILED,
                }
            }

            /// Whether a word a program runs can fail with it.
            pub fn raised(self) -> bool {
                match self {
                    $(Code::$variant)|* => true,
                    $(Code::$looped)|* => false,
                    Code::Lang(LangCode(code)) => code.raised() || Raise::LANG.contains(&code),
                }
            }
        }

        /// A code a word's `fail` (or the session's) takes, each one a
        /// `try` may catch: one of fux's own but the run loop's, or one of
        /// the language's that fux's words raise (`raises`), which the
        /// vocabulary declares. A code of the language's that only
        /// compiling finds (`Code::of`) is none. A failure a word's request
        /// comes back with (`script::Failed`) holds any `Code`.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Raise {
            $($(#[$doc])* $variant,)*
            $($lang,)*
        }

        impl Raise {
            /// The language's codes fux's words raise.
            pub const LANG: &[fuxth_core::Code] = &[$(fuxth_core::Code::$lang),*];
        }

        impl From<Raise> for Code {
            fn from(code: Raise) -> Code {
                match code {
                    $(Raise::$variant => Code::$variant,)*
                    $(Raise::$lang => Code::Lang(LangCode(fuxth_core::Code::$lang)),)*
                }
            }
        }

        impl From<Code> for fuxth_core::Code {
            fn from(code: Code) -> fuxth_core::Code {
                match code {
                    $(Code::$variant => fuxth_core::Code::Host($name),)*
                    $(Code::$looped => fuxth_core::Code::Host($loop_name),)*
                    Code::Lang(LangCode(code)) => code,
                }
            }
        }
    };
}

codes! {
    // The language's codes fux's words raise as they run: those fuxth's
    // own words raise too, and those fuxth finds only compiling or at a
    // program's end: `unused`, options a word would leave unused;
    // `unknown_word`, `explain`'s word that is none; `bad_name`, a name a
    // pane, tab or workspace cannot have.
    raises [Type, Range, StackUnderflow, TooLong, Unused, UnknownWord, BadName];
    // What only the loop running a program gives, never a word: no `try`
    // catches it.
    run_loop [
        /// A run of more steps than allowed.
        Steps "steps",
        /// A run the prefix, then Esc, stopped.
        Cancelled "cancelled",
    ];
    // Where a client finds its server.
    /// No server runs at the socket.
    NoServer "no_server",
    /// The client and the server speak different protocols.
    Mismatch "mismatch",
    /// The client could not do its part: its socket, its terminal.
    Client "client",
    // What a command names.
    /// A pane named is not there.
    NoPane "no_pane",
    /// A tab named is not there.
    NoTab "no_tab",
    /// A workspace named is not there; or a client that only watches
    /// attaches to a server that holds none.
    NoWorkspace "no_workspace",
    /// A client named is not there.
    NoClient "no_client",
    /// A paste buffer named is not there.
    NoBuffer "no_buffer",
    /// Nothing to act on was named, and the caller has none.
    NoTarget "no_target",
    /// `FUX_PANE` is another run's, has no `FUX_SERVER`, or names an old program generation.
    StrayPane "stray_pane",
    /// A field `get` names that the object has not.
    NoField "no_field",
    /// No pane, tab or workspace that way, or before or after it.
    NothingThere "nothing_there",
    /// A copy-mode selection with no rows, or too large.
    Selection "selection",
    /// A name taken by another pane, tab or workspace.
    NameTaken "name_taken",
    /// Every id of a kind has been given.
    IdsExhausted "ids_exhausted",
    /// A value out of what a word takes.
    BadValue "bad_value",
    /// A command used wrongly.
    Usage "usage",
    // What a command meets.
    /// A program that exited or closed its terminal, or none the pane's
    /// while it respawns; or its terminal reads failed: nothing takes input there.
    NotRunning "not_running",
    /// A program still running, which a respawn would not end; or a
    /// respawn of the pane still under way.
    StillRunning "still_running",
    /// A respawn of a pane whose program `run` did not start.
    NotRun "not_run",
    /// The pane only has its shell, which `signal` does not signal.
    OnlyShell "only_shell",
    /// A pane's input queue is full: its terminal took none of it since.
    InputFull "input_full",
    /// A pane or tab no longer at the version or layout revision given:
    /// it changed since it was seen, and nothing was sent or arranged.
    Stale "stale",
    /// A pane where no shell command was marked (OSC 133), for what needs
    /// one (`last-output`).
    NoMarks "no_marks",
    /// A program that could not start.
    Start "start",
    /// A size no pane or tab can have: a screen past 4096
    /// (`command::MOST`) a side (`attach`, its terminal resized, the word
    /// `size`), or with
    /// no room for a pane below its bar as it starts a server's first
    /// shell; a new tab or workspace given none, whose caller has no
    /// screen that sizes panes and no tab; a pane's
    /// `resize-pane` the layout cannot give; a tab resized, a
    /// split or a pane moved where its tab's panes would not all show.
    Size "size",
    /// Memory for a client capture's render grid was refused.
    Memory "memory",
    /// A mouse event the pane's program does not ask for.
    Mouse "mouse",
    /// A signal that could not be sent.
    Signal "signal",
    /// A client with a terminal, which sets its own size.
    NotHeadless "not_headless",
    /// `kill-server` from one of the server's own panes.
    OwnServer "own_server",
    /// `kill-server` with people attached, not forced.
    PeopleAttached "people_attached",
    // How a wait ends.
    /// A wait's limit came first.
    Timeout "timeout" TIMED_OUT,
    /// The pane waited on closed.
    Closed "closed" CLOSED,
    // A program of words.
    /// A word run where its context is missing: a command-line word in a
    /// key or config, or `me` outside a key or prompt.
    NotHere "not_here",
    /// A word a config file may not hold.
    NotConfig "not_config",
    /// The config file or a setting refused.
    Config "config",
    /// A key binding that cannot be made.
    BadBinding "bad_binding",
    /// A program that did not check (`check`).
    Check "check",
    /// A file that could not be read, or stdin.
    Read "read",
    /// An environment variable `getenv` names that is not set.
    UnsetVar "unset_var",
    /// Output that could not be written.
    Output "output",
    /// A word that may not run inside another's run.
    Nested "nested",
    /// A key name no key has.
    NotAKey "not_a_key",
}

impl Code {
    /// Every code: fux's own, then the language's.
    pub fn all() -> impl Iterator<Item = Code> {
        let lang = fuxth_core::Code::ALL
            .iter()
            .map(|c| Code::Lang(LangCode(*c)));
        Code::ALL.iter().copied().chain(lang)
    }

    pub fn name(self) -> &'static str {
        fuxth_core::Code::from(self).name()
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for Code {
    type Err = ();
    fn from_str(name: &str) -> Result<Code, ()> {
        Code::all().find(|c| c.name() == name).ok_or(())
    }
}

/// A code of the language's own, never a host's (a host's is one of fux's
/// own): so a code's name has one meaning, and one status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LangCode(fuxth_core::Code);

impl Code {
    /// The language's code as fux names it, found by its name among them
    /// all: a host's (every host code is one of fux's own, given by
    /// `fuxth_core::Code::from`; one it does not know is the program's failure,
    /// `fail`), the language's own as it is. Not a `From`, so that a
    /// word's `fail` (and the session's), which take a `Raise`, never
    /// name a code of the language's that only compiling finds.
    pub fn of(code: fuxth_core::Code) -> Code {
        (code.name().parse()).unwrap_or(Code::Lang(LangCode(fuxth_core::Code::Fail)))
    }
}

/// Why a command, or attaching a client, failed: exit status 1.
#[derive(Debug)]
pub enum Error {
    // What a command names, or its context gives, that is not there.
    NoWorkspace(WsId),
    NoPaneGiven,
    NoPane(PaneId),
    NoTab(TabId),
    NoWorkspaceGiven,
    NoClient(ClientId),
    NoBuffer(command::Buffer),
    /// A pane, tab or workspace alone of its kind, with nothing to move to.
    OnlyOne(Kind),
    AtEnd(Kind),
    NoNeighbor {
        from: PaneId,
        direction: Direction,
    },
    // What copy mode cannot do.
    NoRows,
    // A name that cannot be given.
    NameTaken(String),
    /// A tab has the name: those tabs, then their panes.
    TabNameTaken(String, Vec<TabId>, Vec<PaneId>),
    /// A name reserved for target syntax.
    NameReadAsTarget(String),
    /// Any other failure, made in one place and told apart by its code
    /// alone: its code, and why.
    Failed {
        code: Code,
        message: String,
    },
}

/// A failure of kind `code` (see [`Error::Failed`]).
pub fn fail(code: Raise, message: impl Into<String>) -> Error {
    Error::Failed {
        code: code.into(),
        message: message.into(),
    }
}

impl Error {
    /// What kind of failure it is, its code: stable, for a
    /// script to tell failures apart by.
    pub fn code(&self) -> Code {
        match self {
            Self::NoWorkspace(_) => Code::NoWorkspace,
            Self::NoPane(_) => Code::NoPane,
            Self::NoTab(_) => Code::NoTab,
            Self::NoPaneGiven | Self::NoWorkspaceGiven => Code::NoTarget,
            Self::NoClient(_) => Code::NoClient,
            Self::NoBuffer(_) => Code::NoBuffer,
            Self::OnlyOne(_) | Self::AtEnd(_) | Self::NoNeighbor { .. } => Code::NothingThere,
            Self::NoRows => Code::Selection,
            Self::NameReadAsTarget(_) => Raise::BadName.into(),
            Self::NameTaken(_) | Self::TabNameTaken(..) => Code::NameTaken,
            Self::Failed { code, .. } => *code,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoWorkspace(id) => write!(f, "no workspace {id}"),
            Self::NoPaneGiven => f.write_str("no pane given: name one (`str %N pane`)"),
            Self::NoPane(id) => write!(f, "no pane {id}"),
            Self::NoTab(id) => write!(f, "no tab {id}"),
            Self::NoWorkspaceGiven => f.write_str(
                "no workspace given: name one (`str NAME workspace`; for new-tab, the \
                     option `str NAME in`)",
            ),
            Self::NoClient(id) => write!(f, "no client {id}"),
            Self::NoBuffer(index) => write!(f, "no buffer {index}"),
            Self::OnlyOne(kind) => write!(f, "only one {}", kind.name()),
            Self::AtEnd(kind) => write!(f, "the {} is already at that end", kind.name()),
            Self::NoNeighbor { from, direction } => {
                write!(f, "no pane {} of {from}", direction.name())
            }
            Self::NoRows => f.write_str("the pane has no rows"),
            Self::NameTaken(name) => write!(f, "a workspace is named {name:?}"),
            Self::TabNameTaken(name, tabs, panes) => {
                let ids = (tabs.iter().map(|t| AnyRef::Tab(*t)))
                    .chain(panes.iter().map(|p| AnyRef::Pane(*p)));
                let ids = joined(ids, " ");
                let kills = joined(tabs, " tab kill`, `fux str ");
                write!(
                    f,
                    "a tab is named {name:?} already ({ids}); close it (`fux str {kills} tab kill`) or choose another name"
                )
            }
            Self::NameReadAsTarget(name) => write!(
                f,
                "{name:?} cannot be a name: it is reserved for target syntax"
            ),
            Self::Failed { message, .. } => f.write_str(message),
        }
    }
}
impl std::error::Error for Error {}

impl From<Error> for fuxth_core::Failure {
    fn from(error: Error) -> Self {
        Self::new(error.code().into(), error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::Code;

    /// A host's code fux does not know is no code of the language's: it is
    /// the program's failure.
    #[test]
    fn an_unknown_host_code_is_the_programs_failure() {
        assert_eq!(Code::of(fuxth_core::Code::Host("nosuch")).name(), "fail");
        assert_eq!(Code::of(fuxth_core::Code::Host("closed")), Code::Closed);
    }

    /// Every code has a name of its own, and docs/json.md's list of
    /// errors names each, and no other.
    #[test]
    fn every_code_has_its_own_name_and_the_docs_list_it() {
        let docs = include_str!("../../docs/json.md");
        let errors = docs.split("## Errors").nth(1).unwrap_or_default();
        // The list: from its first item to the blank line after it.
        let list = errors.split("\n- ").skip(1).collect::<Vec<_>>().join(" ");
        let list = list.split("\n\n").next().unwrap_or_default();
        // What is quoted: every other piece between backquotes.
        let quoted = list.split('`').enumerate().filter(|(i, _)| i % 2 == 1);
        let listed: std::collections::BTreeSet<&str> = quoted.map(|(_, name)| name).collect();
        let mut names = std::collections::BTreeSet::new();
        for (i, code) in Code::all().enumerate() {
            assert_eq!(code.name().parse(), Ok(code), "{code:?}");
            assert_eq!(Code::of(fuxth_core::Code::from(code)), code);
            let named = Code::all().skip(i + 1).any(|c| c.name() == code.name());
            assert!(!named, "{code:?} shares its name");
            names.insert(code.name());
        }
        assert_eq!(listed, names, "docs/json.md's Errors and fux's codes");
    }
}
