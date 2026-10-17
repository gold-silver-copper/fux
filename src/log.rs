//! A pane's journal: what happens in it, recorded as it happens, on one
//! counter: the output fux reads, each piece numbered as the screen took
//! it, and what it printed; bells, notifications, titles, shell commands'
//! ends and resizes. Every question an
//! agent asks (what came since its input, which came first, is this text
//! the echo of what it typed) is answered from the record, never by
//! comparing screens afterwards.
//!
//! The counter is fux-vt's: each piece of output processed takes the next
//! number, and every row keeps the number of the piece that last changed
//! it ([`fux_vt::Row::version`]); what fux records between pieces takes a
//! number with [`fux_vt::Parser::tick`]. So a row's number and a log
//! entry's are one and the same, with nothing to map.
use std::collections::VecDeque;

/// A number from a pane's counter: the order things happened in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Seq(pub u64);

/// What a piece of input is to the program that reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// It edits the line being typed: text, a paste, BSpace, Delete, the
    /// cursor keys, C-u, C-w and the like. What the program draws in
    /// reply, on the line typed on, is its echo.
    Edit,
    /// Anything else: Enter, which submits the line, a control key, a
    /// function key, Escape, the mouse. What the program draws in reply is
    /// its output.
    Submit,
}

/// Where a piece of output came from. Each kind says how it is known:
/// the kernel's echo for certain, output by what drew it, and what followed
/// an edit by its time and place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A full-screen program's: everything it draws is output.
    Screen,
    /// The program's own output.
    Output,
    /// The kernel's echo of input (`ICANON`, `ECHO`), by its fixed rules,
    /// wherever it lands: certain.
    Echo,
    /// What the program drew after an edit, before the next input: the
    /// echo on the line typed on (`line`, its first row), output elsewhere;
    /// `shell`, a shell reading its command line, as its marks say.
    Edited {
        line: Option<fux_vt::RowId>,
        shell: bool,
    },
}

/// What a log entry records.
#[derive(Clone, Debug)]
pub enum Event {
    /// A piece of output, and what it printed.
    Output(Box<Printed>),
    /// The program rang the bell.
    Bell,
    /// The program sent a notification (OSC 9, 777): its text.
    Notification(String),
    /// The program set its title.
    Title(String),
    /// A shell command ended (OSC 133): its status, if the shell said.
    Done(Option<i32>),
    /// The screen took a new size.
    Resize(fux_vt::Size),
}

/// What a piece of output printed: its runs, what each may be the echo of
/// (`crate::classify`), which a wait never takes, and of each, the
/// characters it changed, those after the first and before the last it
/// printed as its row held them just before (a line drawn again, the same,
/// changes none: nothing new). No echoes kept: none is; no changes kept:
/// each run changed all it printed, as in a flood.
#[derive(Clone, Debug)]
pub struct Printed {
    pub prints: fux_vt::Prints,
    pub echo: Vec<crate::classify::Echo>,
    pub fresh: Vec<(usize, usize)>,
}

impl Printed {
    /// Each run, what it may be the echo of, and the characters it changed.
    pub fn runs(
        &self,
    ) -> impl Iterator<Item = (fux_vt::Print<'_>, &crate::classify::Echo, (usize, usize))> {
        (self.prints.iter().enumerate()).map(|(i, print)| {
            let echo = self.echo.get(i).unwrap_or(&crate::classify::Echo::Not);
            let fresh =
                (self.fresh.get(i).copied()).unwrap_or_else(|| (0, print.text.chars().count()));
            (print, echo, fresh)
        })
    }
}

/// One thing that happened, and its number.
#[derive(Clone, Debug)]
pub struct Entry {
    pub seq: Seq,
    pub event: Event,
}

/// How many bytes a pane's journal keeps by default (`journal-bytes`).
pub const JOURNAL_BYTES: usize = 32 << 20;
/// What an entry costs beside what it holds: its number and kind.
const ENTRY_COST: usize = std::mem::size_of::<Entry>();

/// A pane's journal: everything that happened in it, in order, under one
/// budget in bytes. Past it, the oldest entries go whole, and a reader
/// asking for what came after one that went is told so.
#[derive(Debug)]
pub struct Log {
    entries: VecDeque<Entry>,
    /// Bytes the entries take.
    bytes: usize,
    /// The bytes it keeps.
    pub budget: usize,
    /// The newest entry let go, if any was.
    dropped: Option<Seq>,
}

impl Log {
    /// An empty journal that keeps `budget` bytes.
    pub fn new(budget: usize) -> Log {
        Log {
            entries: VecDeque::new(),
            bytes: 0,
            budget,
            dropped: None,
        }
    }

    /// Records `event` with number `seq`.
    pub fn push(&mut self, seq: Seq, event: Event) {
        self.bytes = self.bytes.saturating_add(held(&event));
        self.entries.push_back(Entry { seq, event });
        self.keep(self.budget);
    }

    /// Lets the oldest entries go until it holds at most `bytes`.
    pub fn keep(&mut self, bytes: usize) {
        while self.bytes > bytes {
            let Some(entry) = self.entries.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(held(&entry.event));
            self.dropped = Some(entry.seq);
        }
    }

    /// Keeps `budget` bytes from now on.
    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        self.keep(budget);
    }

    /// How many bytes its entries take.
    pub fn held(&self) -> usize {
        self.bytes
    }

    /// The first bell rung, or notification sent, after `seq`, if one was:
    /// its number, and a notification's text.
    pub fn bell_after(&self, seq: Seq) -> Option<(Seq, Option<String>)> {
        self.after(seq).find_map(|e| match &e.event {
            Event::Bell => Some((e.seq, None)),
            Event::Notification(text) => Some((e.seq, Some(text.clone()))),
            Event::Output(_) | Event::Title(_) | Event::Done(_) | Event::Resize(_) => None,
        })
    }

    /// The entries after `seq`, oldest first.
    pub fn after(&self, seq: Seq) -> impl Iterator<Item = &Entry> {
        let from = self.entries.partition_point(|e| e.seq <= seq);
        self.entries.iter().skip(from)
    }

    /// The number of its newest entry, or of the last it let go: 0 if it
    /// held none.
    pub fn last(&self) -> Seq {
        let held = self.entries.back().map_or(Seq(0), |e| e.seq);
        held.max(self.dropped.unwrap_or(Seq(0)))
    }

    /// What each piece of output after `seq` printed, oldest first.
    pub fn printed_after(&self, seq: Seq) -> impl Iterator<Item = (Seq, &Printed)> {
        self.after(seq).filter_map(|e| {
            let Event::Output(p) = &e.event else {
                return None;
            };
            Some((e.seq, &**p))
        })
    }

    /// Whether something that came after `seq` was let go.
    pub fn lost_after(&self, seq: Seq) -> bool {
        self.dropped.is_some_and(|d| d > seq)
    }
}

/// The bytes an event takes.
fn held(event: &Event) -> usize {
    let own = match event {
        // What it takes on the heap: what it printed, each run's echo and
        // change, and the text an echo is of, shared by its runs, once.
        Event::Output(p) => {
            let echo = (p.echo.iter()).find_map(|e| match e {
                crate::classify::Echo::Of(text) => Some(text.len()),
                crate::classify::Echo::Not | crate::classify::Echo::Is => None,
            });
            let runs = std::mem::size_of::<crate::classify::Echo>()
                .saturating_mul(p.echo.capacity())
                .saturating_add(p.fresh.capacity().saturating_mul(16));
            (p.prints.heap())
                .saturating_add(runs)
                .saturating_add(echo.unwrap_or(0))
                .saturating_add(std::mem::size_of::<Printed>())
        }
        Event::Notification(text) | Event::Title(text) => text.len(),
        Event::Bell | Event::Done(_) | Event::Resize(_) => 0,
    };
    own.saturating_add(ENTRY_COST)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Past its budget, the journal lets go of the oldest entries whole,
    /// and says what came after one that went.
    #[test]
    fn the_oldest_entries_go_first_and_are_told() {
        let mut log = Log::new(4096);
        for n in 1..=3u64 {
            log.push(
                Seq(n),
                Event::Title(std::iter::repeat_n('x', 1500).collect()),
            );
        }
        log.push(Seq(4), Event::Bell);
        assert!(log.lost_after(Seq(0)) && !log.lost_after(Seq(1)));
        assert_eq!(log.bell_after(Seq(2)), Some((Seq(4), None)));
        assert_eq!(log.after(Seq(0)).count(), 3);
        log.keep(0);
        assert_eq!(log.after(Seq(0)).count(), 0);
        // Emptied, it still reads as told up to the last it let go.
        assert!(!log.lost_after(log.last()));
    }
}
