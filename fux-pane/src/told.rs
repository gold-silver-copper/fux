//! What a pane tells of itself and its program, in order, for its server
//! to journal (`Told`), and how its methods tell it (`Teller`).
use crate::pane::{End, Left, Said};
use fux_wire::command::Version;
use fux_wire::events::Actor;
use fux_wire::id::PaneId;

/// What a pane tells of itself, in order, each with who caused it: what
/// a pane's methods that change what events tell write to, at once,
/// through a `pane::PaneMut`. Its process sends them on to the server
/// (`host`), which journals them at the pane's place.
pub struct Teller<'a> {
    pane: PaneId,
    generation: u64,
    /// Who causes what the pane does for a request.
    actor: Actor,
    told: &'a mut Vec<(Told, Actor)>,
}

impl<'a> Teller<'a> {
    pub fn new(
        pane: PaneId,
        generation: u64,
        actor: Actor,
        told: &'a mut Vec<(Told, Actor)>,
    ) -> Self {
        Teller {
            pane,
            generation,
            actor,
            told,
        }
    }

    /// The pane tells `what`.
    pub fn tell(&mut self, what: Told) {
        let actor = match what {
            Told::Bell
            | Told::Notification(_)
            | Told::Title(_)
            | Told::CommandStarted
            | Told::Done(_)
            | Told::HungUp
            | Told::ReadFailed(_)
            | Told::Exited(_) => Actor::Program(self.pane, self.generation),
            Told::Renamed(_)
            | Told::Respawned { .. }
            | Told::Resized(_)
            | Told::HistoryBudget(_)
            | Told::Input { .. }
            | Told::Signalled { .. } => self.actor,
        };
        self.told.push((what, actor));
    }
}

/// What a pane tells of itself and its program (`Teller::tell`).
#[derive(Clone, Debug, PartialEq)]
pub enum Told {
    /// Named otherwise: its new name.
    Renamed(String),
    /// Its program started again (`respawn`), as generation `generation`,
    /// over one that ran: what that one left, and what was started, which
    /// its server's copy takes (the journal keeps the event without it).
    Respawned {
        left: Left,
        generation: u64,
        argv: Option<Box<fux_wire::command::Program>>,
    },
    Bell,
    Notification(Said),
    Title(Said),
    /// A shell command's output began (OSC 133 `C`).
    CommandStarted,
    /// A shell command ended, with its status if it is known.
    Done(Option<i32>),
    Resized(fux_vt::Size),
    /// The pane's retained history budget changed.
    HistoryBudget(usize),
    /// Its program closed its terminal and runs on.
    HungUp,
    /// Reading its program's terminal failed unexpectedly, so; or its
    /// pane's own process ended, so (`Session::pane_lost`).
    ReadFailed(String),
    /// A request's input was queued for its program `generation`: how it
    /// came, how many bytes, and the version `expect` admitted it at, if
    /// it did; never what it says.
    Input {
        kind: Sent,
        bytes: usize,
        // Boxed, so the rarer guard grows no entry of the journal.
        expected: Option<Box<Version>>,
        generation: u64,
    },
    /// A request sent `signal` to what runs in the foreground of its
    /// program `generation`'s terminal.
    Signalled {
        signal: fuxix::process::Signal,
        generation: u64,
    },
    /// Its program ended.
    Exited(End),
}

/// How a request's input came: `send-keys`, a paste among its inputs or
/// not, or `send-mouse`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sent {
    Keys,
    Paste,
    Mouse,
}

wired!(Told {
    Renamed(name),
    Respawned { left, generation, argv },
    Bell,
    Notification(said),
    Title(said),
    CommandStarted,
    Done(status),
    Resized(size),
    HistoryBudget(bytes),
    HungUp,
    ReadFailed(error),
    Input { kind, bytes, expected, generation },
    Signalled { signal, generation },
    Exited(end),
});
wired!(Sent { Keys, Paste, Mouse });
