//! What kind of failure a failure is: its code, a name that stays the same,
//! for a script to tell failures apart by (`fux: CODE: MESSAGE`). Every
//! code fux gives is here, the language's own among them as fuxth names
//! them; `docs/json.md` lists them all.
use std::num::NonZeroU8;

/// `codes! { /// doc  Variant "name", … }` expands to the enum `Code` of
/// those variants and `Lang`, the language's own codes; `Code::ALL`, fux's
/// own in order; and `fuxth::Code::from`, each one as the language takes it
/// (one of fux's as `Host`, by its name).
macro_rules! codes {
    ($($(#[$doc:meta])* $variant:ident $name:literal,)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Code {
            $($(#[$doc])* $variant,)*
            /// One of the language's own (fuxth's), as fuxth names it.
            Lang(fuxth::Code),
        }

        impl Code {
            pub const ALL: &[Code] = &[$(Code::$variant),*];
        }

        impl From<Code> for fuxth::Code {
            fn from(code: Code) -> fuxth::Code {
                match code {
                    $(Code::$variant => fuxth::Code::Host($name),)*
                    Code::Lang(code) => code,
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
    /// A command used wrongly: exit status 2.
    Usage "usage",
    // What a command meets.
    /// A program that exited: it takes no input, or exited first.
    Exited "exited",
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
    /// A wait's limit came first: exit status 124.
    Timeout "timeout",
    /// The pane waited on closed: exit status 4.
    Closed "closed",
    /// Everything an event stream watched closed: exit status 4.
    Gone "gone",
    /// The reader of an event stream fell too far behind.
    Lost "lost",
    /// The program reads its terminal and the text has not come: exit
    /// status 7. A run the prefix then Esc stopped is named so too, with
    /// status 1.
    Stopped "stopped",
    // A program of words.
    /// A run of more steps than allowed.
    Steps "steps",
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
    /// The exit status a failure of this kind ends with: a usage error's 2,
    /// any other's 1 (a wait says its own: 3, 4, 7, 124).
    pub fn status(self) -> NonZeroU8 {
        let usage = u8::from(self == Code::Usage);
        NonZeroU8::MIN.saturating_add(usage)
    }

    /// Every code: fux's own, then the language's.
    pub fn all() -> impl Iterator<Item = Code> {
        let lang = fuxth::Code::ALL.iter().map(|code| Code::Lang(*code));
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

/// The language's code as fux names it: a host's by its name (every host
/// code is one of fux's own, given by `fuxth::Code::from`), the language's
/// own as it is.
impl From<fuxth::Code> for Code {
    fn from(code: fuxth::Code) -> Code {
        code.name().parse().unwrap_or(Code::Lang(code))
    }
}

#[cfg(test)]
mod tests {
    use super::Code;

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
