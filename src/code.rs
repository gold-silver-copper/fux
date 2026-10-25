//! What kind of failure a failure is: its code, a name that stays the same,
//! for a script to tell failures apart by (`fux: CODE: MESSAGE`). Every
//! code fux gives is here, the language's own among them as fuxth names
//! them; `docs/json.md` lists them all.
use std::num::NonZeroU8;

/// The exit statuses, a meaning each, besides 0: what was asked came.
/// Any failure but these.
const FAILED: NonZeroU8 = NonZeroU8::MIN;
/// The pane waited on closed, or everything an event stream watched.
const CLOSED: NonZeroU8 = NonZeroU8::MIN.saturating_add(3);
/// The time limit came first: as `timeout(1)`.
const TIMED_OUT: NonZeroU8 = NonZeroU8::MIN.saturating_add(123);

/// The language's codes that fuxth finds only compiling or at a program's
/// end, which fux's words raise as they run too, for a `try` to catch:
/// `unused`, options a word would leave unused; `unknown_word`, `explain`'s
/// word that is none; `bad_name`, a name a pane, tab or workspace cannot
/// have.
pub const RAISED: [fuxth::Code; 3] = [
    fuxth::Code::Unused,
    fuxth::Code::UnknownWord,
    fuxth::Code::BadName,
];

/// `codes! { /// doc  Variant "name", Closed "closed" CLOSED, … }` expands
/// to the enum `Code` of those variants and `Lang`, the language's own
/// codes; `Code::ALL`, fux's own in order; `Code::status`, each one's exit
/// status: the one its row names (`Code::Closed => CLOSED`), else `FAILED`,
/// as for the language's; `Code::raised`, whether a word a program runs
/// can fail with it, so a `try` may catch it: not for a row marked
/// `[run_loop]`, which only the loop running the program gives; a
/// language's code if fuxth's words raise it, or fux's (`RAISED`); and
/// `fuxth::Code::from`, each one as the language takes it (one of fux's as
/// `Host`, by its name).
macro_rules! codes {
    (@status) => {
        FAILED
    };
    (@status $status:ident) => {
        $status
    };
    (@raised) => {
        true
    };
    (@raised run_loop) => {
        false
    };
    ($($(#[$doc:meta])* $variant:ident $name:literal $($status:ident)? $([$by:ident])?,)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Code {
            $($(#[$doc])* $variant,)*
            /// One of the language's own (fuxth's), as fuxth names it.
            Lang(LangCode),
        }

        impl Code {
            pub const ALL: &[Code] = &[$(Code::$variant),*];

            /// The exit status a failure, or an answer, of this kind ends
            /// with.
            pub fn status(self) -> NonZeroU8 {
                match self {
                    $(Code::$variant => codes!(@status $($status)?),)*
                    Code::Lang(_) => FAILED,
                }
            }

            /// Whether a word a program runs can fail with it.
            pub fn raised(self) -> bool {
                match self {
                    $(Code::$variant => codes!(@raised $($by)?),)*
                    Code::Lang(LangCode(code)) => code.raised() || RAISED.contains(&code),
                }
            }
        }

        impl From<Code> for fuxth::Code {
            fn from(code: Code) -> fuxth::Code {
                match code {
                    $(Code::$variant => fuxth::Code::Host($name),)*
                    Code::Lang(LangCode(code)) => code,
                }
            }
        }
    };
}

codes! {
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
    /// `FUX_PANE` is another server's pane, or comes with no `FUX_SERVER` to say whose.
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
    /// A program that exited or closed its terminal: nothing takes input
    /// there.
    NotRunning "not_running",
    /// A program still running, which a respawn would not end.
    StillRunning "still_running",
    /// A respawn of a pane whose program `run` did not start.
    NotRun "not_run",
    /// The pane only has its shell, which `signal` does not signal.
    OnlyShell "only_shell",
    /// A pane's input queue is full: its terminal took none of it since.
    InputFull "input_full",
    /// A pane where no shell command was marked (OSC 133), for what needs
    /// one (`last-output`).
    NoMarks "no_marks",
    /// A program that could not start.
    Start "start",
    /// A size no pane or tab can have: a client's screen past 4096
    /// (`view::MOST`) a side (`attach`, its terminal resized), or with
    /// no room for a pane below its bar as it starts a server's first
    /// shell; a new tab or workspace no client sizes, given none; a pane's
    /// `resize-x` or `resize-y` the layout cannot give; a tab resized, a
    /// split or a pane moved where its tab's panes would not all show.
    Size "size",
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
    /// Everything an event stream watched closed.
    Gone "gone" CLOSED,
    // A program of words.
    /// A run of more steps than allowed.
    Steps "steps" [run_loop],
    /// A run the prefix, then Esc, stopped.
    Cancelled "cancelled" [run_loop],
    /// A word the client runs itself (attach, server, start, status), which
    /// runs from the command line only: not a key's or a config's.
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
    /// Options with no program, where one is needed.
    NoProgram "no_program",
    /// `send-keys` with no keys.
    NoKeys "no_keys",
    /// A value that is not a client.
    NotAClient "not_a_client",
}

impl Code {
    /// Every code: fux's own, then the language's.
    pub fn all() -> impl Iterator<Item = Code> {
        let lang = fuxth::Code::ALL.iter().map(|c| Code::Lang(LangCode(*c)));
        Code::ALL.iter().copied().chain(lang)
    }

    pub fn name(self) -> &'static str {
        fuxth::Code::from(self).name()
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
pub struct LangCode(fuxth::Code);

/// The language's code as fux names it, found by its name among them all:
/// a host's (every host code is one of fux's own, given by
/// `fuxth::Code::from`; one it does not know is the program's failure,
/// `fail`), the language's own as it is.
impl From<fuxth::Code> for Code {
    fn from(code: fuxth::Code) -> Code {
        (code.name().parse()).unwrap_or(Code::Lang(LangCode(fuxth::Code::Fail)))
    }
}

#[cfg(test)]
mod tests {
    use super::Code;

    /// A host's code fux does not know is no code of the language's: it is
    /// the program's failure.
    #[test]
    fn an_unknown_host_code_is_the_programs_failure() {
        assert_eq!(Code::from(fuxth::Code::Host("nosuch")).name(), "fail");
        assert_eq!(Code::from(fuxth::Code::Host("closed")), Code::Closed);
    }

    /// Every code has a name of its own, and docs/json.md's list of
    /// errors names each, and no other.
    #[test]
    fn every_code_has_its_own_name_and_the_docs_list_it() {
        let docs = crate::help::JSON;
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
            assert_eq!(Code::from(fuxth::Code::from(code)), code);
            let named = Code::all().skip(i + 1).any(|c| c.name() == code.name());
            assert!(!named, "{code:?} shares its name");
            names.insert(code.name());
        }
        assert_eq!(listed, names, "docs/json.md's Errors and fux's codes");
    }
}
