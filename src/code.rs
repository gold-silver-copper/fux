//! What kind of failure a failure is: its code, a name that stays the same,
//! for a script to tell failures apart by (`fux: CODE: MESSAGE`). Every
//! code fux gives is here, the language's own among them as fuxth names
//! them; `docs/json.md` lists them all.
use std::num::NonZeroU8;

/// The exit statuses, a meaning each, besides 0: what was asked came.
/// Any failure but these.
const FAILED: NonZeroU8 = NonZeroU8::MIN;
/// A command used wrongly.
const USAGE: NonZeroU8 = NonZeroU8::MIN.saturating_add(1);
/// The program exited, or was started again, before the wait was met.
const EXITED: NonZeroU8 = NonZeroU8::MIN.saturating_add(2);
/// The pane waited on closed, or everything an event stream watched.
const CLOSED: NonZeroU8 = NonZeroU8::MIN.saturating_add(3);
/// The shell command a wait lasted for ended before the wait was met.
const ENDED: NonZeroU8 = NonZeroU8::MIN.saturating_add(4);
/// The program reads its terminal, and the text waited for has not come
/// (or gone): fux judges it cannot before the agent types.
const READING: NonZeroU8 = NonZeroU8::MIN.saturating_add(6);
/// The time limit came first, the program still at work: as `timeout(1)`.
const TIMED_OUT: NonZeroU8 = NonZeroU8::MIN.saturating_add(123);

/// `codes! { /// doc  Variant "name", Closed "closed" CLOSED, … }` expands
/// to the enum `Code` of those variants and `Lang`, the language's own
/// codes; `Code::ALL`, fux's own in order; `Code::status`, each one's exit
/// status: the one its row names (`Code::Closed => CLOSED`), else `FAILED`,
/// as for the language's; and `fuxth::Code::from`, each one as the language
/// takes it (one of fux's as `Host`, by its name).
macro_rules! codes {
    (@status) => {
        FAILED
    };
    (@status $status:ident) => {
        $status
    };
    ($($(#[$doc:meta])* $variant:ident $name:literal $($status:ident)?,)*) => {
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
    /// A socket `grant` made, revoked, or reaching only its workspace.
    NotGranted "not_granted",
    /// A grant that cannot be made or revoked.
    Grant "grant",
    // What a command names.
    /// A pane named is not there.
    NoPane "no_pane",
    /// A tab named is not there.
    NoTab "no_tab",
    /// A workspace named is not there.
    NoWorkspace "no_workspace",
    /// A client named is not there.
    NoClient "no_client",
    /// A paste buffer named is not there.
    NoBuffer "no_buffer",
    /// Nothing to act on was named, and the caller has none.
    NoTarget "no_target",
    /// `FUX_PANE` is another server's pane.
    StrayPane "stray_pane",
    /// A field `get` names that the object has not.
    NoField "no_field",
    /// No pane, tab or workspace that way, or before or after it.
    NothingThere "nothing_there",
    /// A copy-mode selection with no rows, or too large.
    Selection "selection",
    /// A name taken by another pane, tab or workspace.
    NameTaken "name_taken",
    /// An id asked for that was given before.
    Id "id",
    /// Every id of a kind has been given.
    IdsExhausted "ids_exhausted",
    /// A value out of what a word takes.
    BadValue "bad_value",
    /// A command used wrongly.
    Usage "usage" USAGE,
    // What a command meets.
    /// A program that exited, which takes no input.
    NotRunning "not_running",
    /// A program still running, which a respawn would not end.
    StillRunning "still_running",
    /// A respawn of a pane whose program `run` did not start.
    NotRun "not_run",
    /// The pane only has its shell, which `signal` does not signal.
    OnlyShell "only_shell",
    /// A pane's program is not reading its terminal.
    NotReading "not_reading",
    /// A program that could not start.
    Start "start",
    /// A size no pane or tab can have.
    Size "size",
    /// A mouse event the pane's program does not ask for.
    Mouse "mouse",
    /// A signal that could not be sent.
    Signal "signal",
    /// A log file that could not be opened.
    Log "log",
    /// A directory that is not one.
    NotADirectory "not_a_directory",
    /// A client with a terminal, which sets its own size.
    NotHeadless "not_headless",
    /// `kill-server` from one of the server's own panes.
    OwnServer "own_server",
    /// `kill-server` with people attached, not forced.
    PeopleAttached "people_attached",
    // How a wait ends.
    /// The program exited, or was started again, before the wait was met.
    Exited "exited" EXITED,
    /// The shell command a wait lasted for (`wait-done`) ended first.
    Ended "ended" ENDED,
    /// A wait's limit came first.
    Timeout "timeout" TIMED_OUT,
    /// The pane waited on closed.
    Closed "closed" CLOSED,
    /// Everything an event stream watched closed.
    Gone "gone" CLOSED,
    /// The reader of an event stream fell too far behind.
    Lost "lost",
    /// The program reads its terminal and the text has not come.
    Stopped "stopped" READING,
    // A program of words.
    /// A run of more steps than allowed.
    Steps "steps",
    /// A run the prefix, then Esc, stopped.
    Cancelled "cancelled",
    /// A word the client runs itself (attach, server, start, status), which
    /// runs from the command line only: not a key's, a config's or a saved
    /// session's.
    NotHere "not_here",
    /// A word a saved session may not hold.
    NotLayout "not_layout",
    /// A word a config file may not hold.
    NotConfig "not_config",
    /// The config file or a setting refused.
    Config "config",
    /// A key binding that cannot be made.
    BadBinding "bad_binding",
    /// A program that did not check (`check`).
    Check "check",
    /// A file that could not be read.
    Read "read",
    /// Output that could not be written.
    Output "output",
    /// A word that may not run inside another's run.
    Nested "nested",
    /// An option given to a word that takes none of it.
    Unused "unused",
    /// A key name no key has.
    NotAKey "not_a_key",
    /// Options with no program, where one is needed.
    NoProgram "no_program",
    /// `send-keys` with no keys.
    NoKeys "no_keys",
    /// A value that is not a client.
    NotAClient "not_a_client",
    /// A paste that could not be read.
    Paste "paste",
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

    #[test]
    fn every_code_has_its_own_name_and_the_docs_list_it() {
        let docs = crate::help::JSON;
        let errors = docs.split("## Errors").nth(1).unwrap_or_default();
        let errors = errors.split("\n## ").next().unwrap_or_default();
        for (i, code) in Code::all().enumerate() {
            assert_eq!(code.name().parse(), Ok(code), "{code:?}");
            assert_eq!(Code::from(fuxth::Code::from(code)), code);
            let named = Code::all().skip(i + 1).any(|c| c.name() == code.name());
            assert!(!named, "{code:?} shares its name");
            let listed = errors.contains(&format!("`{}`", code.name()));
            assert!(
                listed,
                "docs/json.md's Errors does not list {}",
                code.name()
            );
        }
    }
}
