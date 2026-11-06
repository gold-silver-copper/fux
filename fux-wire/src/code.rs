//! Failures shared by the native server and its grift clients.
use crate::command::{AnyRef, Kind};
use crate::id::{ClientId, PaneId, TabId, WsId};
use fux_vt::keys::Direction;
use std::num::NonZeroU8;
const FAILED: NonZeroU8 = NonZeroU8::MIN;
const PROGRAM: NonZeroU8 = NonZeroU8::MIN.saturating_add(1);
const CLOSED: NonZeroU8 = NonZeroU8::MIN.saturating_add(3);
const TIMED_OUT: NonZeroU8 = NonZeroU8::MIN.saturating_add(123);
macro_rules! codes {
    (@status) => { FAILED };
    (@status $status:ident) => { $status };
    (@asked) => { false };
    (@asked asked) => { true };
    ($($variant:ident $name:literal $([$asked:ident])? $($status:ident)?,)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Code { $($variant,)* }
        impl Code {
            pub const ALL: &[Code] = &[$(Code::$variant,)*];
            pub fn name(self) -> &'static str { match self { $(Self::$variant => $name,)* } }
            pub fn status(self) -> NonZeroU8 { match self { $(Self::$variant => codes!(@status $($status)?),)* } }
            pub fn asked(self) -> bool { match self { $(Self::$variant => codes!(@asked $($asked)?),)* } }
            pub fn raised(self) -> bool { self != Self::Cancelled }
        }
    };
}
codes! {
    Cancelled "cancelled",
    NoServer "no-server" [asked],
    Mismatch "mismatch" [asked],
    Client "client" [asked],
    NoPane "no-pane" [asked],
    NoTab "no-tab" [asked],
    NoWorkspace "no-workspace" [asked],
    NoClient "no-client" [asked],
    NoTarget "no-target" [asked],
    StrayPane "stray-pane" [asked],
    NoField "no-field" [asked],
    NothingThere "nothing-there" [asked],
    NameTaken "name-taken" [asked],
    BadValue "bad-value" [asked],
    Usage "usage" [asked] PROGRAM,
    NotRunning "not-running" [asked],
    StillRunning "still-running" [asked],
    NotRun "not-run" [asked],
    OnlyShell "only-shell" [asked],
    Stale "stale" [asked],
    NoMarks "no-marks" [asked],
    Start "start" [asked],
    Size "size" [asked],
    Mouse "mouse" [asked],
    Signal "signal" [asked],
    Indeterminate "indeterminate" [asked],
    NotHeadless "not-headless" [asked],
    OwnServer "own-server" [asked],
    PeopleAttached "people-attached" [asked],
    Timeout "timeout" [asked] TIMED_OUT,
    Closed "closed" [asked] CLOSED,
    NotHere "not-here" [asked],
    NotConfig "not-config" [asked],
    Config "config" [asked],
    BadBinding "bad-binding",
    Read "read",
    UnsetVar "unset-variable",
    Output "output" [asked],
    Nested "nested",
    NotAKey "not-a-key",
    Type "type-error" PROGRAM,
    Range "range-error" [asked],
    StackUnderflow "arity-error" PROGRAM,
    TooLong "too-long" [asked],
    Unused "unused",
    UnknownWord "unbound-symbol" PROGRAM,
    BadName "bad-name" [asked],
    Memory "out-of-memory" [asked],
    IdsExhausted "ids-exhausted" [asked],
    Fail "fail" [asked],
    Require "read-error" PROGRAM,
    Contract "contract-error",
    Parse "parse-error" PROGRAM,
}
impl Code {
    pub fn all() -> impl Iterator<Item = Code> {
        Self::ALL.iter().copied()
    }
}
impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}
impl std::str::FromStr for Code {
    type Err = ();
    fn from_str(name: &str) -> Result<Self, ()> {
        Self::all().find(|c| c.name() == name).ok_or(())
    }
}
/// Why a command, or attaching a client, failed: its code says its exit
/// status.
#[derive(Debug)]
pub enum Error {
    // What a command names, or its context gives, that is not there.
    NoWorkspace(WsId),
    NoPaneGiven,
    NoPane(PaneId),
    NoTab(TabId),
    NoWorkspaceGiven,
    NoClient(ClientId),
    /// A pane, tab or workspace alone of its kind, with nothing to move to.
    OnlyOne(Kind),
    AtEnd(Kind),
    NoNeighbor {
        from: PaneId,
        direction: Direction,
    },
    /// A name that cannot be given: the pane, tab or workspace that has it.
    NameTaken(String, AnyRef),
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
pub fn fail(code: Code, message: impl Into<String>) -> Error {
    Error::Failed {
        code,
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
            Self::OnlyOne(_) | Self::AtEnd(_) | Self::NoNeighbor { .. } => Code::NothingThere,
            Self::NameReadAsTarget(_) => Code::BadName,
            Self::NameTaken(..) => Code::NameTaken,
            Self::Failed { code, .. } => *code,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoWorkspace(id) => write!(f, "no workspace {id}"),
            Self::NoPaneGiven => f.write_str("no pane given: name one (`(pane N)`)"),
            Self::NoPane(id) => write!(f, "no pane {id}"),
            Self::NoTab(id) => write!(f, "no tab {id}"),
            Self::NoWorkspaceGiven => f.write_str(
                "no workspace given: name one (`(workspace NAME)`; for new-tab, the \
                     option `(in (opts) NAME)`)",
            ),
            Self::NoClient(id) => write!(f, "no client {id}"),
            Self::OnlyOne(kind) => write!(f, "only one {}", kind.name()),
            Self::AtEnd(kind) => write!(f, "the {} is already at that end", kind.name()),
            Self::NoNeighbor { from, direction } => {
                write!(f, "no pane {} of {from}", direction.name())
            }
            Self::NameTaken(name, holder) => write!(
                f,
                "{holder}, a {}, is named {name:?} already: choose another name",
                holder.kind().name()
            ),
            Self::NameReadAsTarget(name) => write!(
                f,
                "{name:?} cannot be a name: it is reserved for target syntax"
            ),
            Self::Failed { message, .. } => f.write_str(message),
        }
    }
}
impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::Code;

    /// Every code has a name of its own that reads back as it, and
    /// docs/records.md's list of failures names each, and no other.
    #[test]
    fn every_code_has_its_own_name_and_the_docs_list_it() {
        let docs = include_str!("../../docs/records.md");
        let errors = docs.split("## Failures").nth(1).unwrap_or_default();
        // The list: from its first item to the blank line after it.
        let list = errors.split("\n- ").skip(1).collect::<Vec<_>>().join(" ");
        let list = list.split("\n\n").next().unwrap_or_default();
        // What is quoted: every other piece between backquotes.
        let quoted = list.split('`').enumerate().filter(|(i, _)| i % 2 == 1);
        let listed: std::collections::BTreeSet<&str> = quoted.map(|(_, name)| name).collect();
        let mut names = std::collections::BTreeSet::new();
        for (i, code) in Code::all().enumerate() {
            assert_eq!(code.name().parse(), Ok(code), "{code:?}");
            let named = Code::all().skip(i + 1).any(|c| c.name() == code.name());
            assert!(!named, "{code:?} shares its name");
            names.insert(code.name());
        }
        assert_eq!(listed, names, "docs/records.md's Failures and fux's codes");
    }
}
