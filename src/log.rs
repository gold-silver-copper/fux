//! What happens in a pane, recorded as it happens, on one counter: the
//! output fux reads, each piece numbered as the screen took it, and where
//! it came from; each piece of input fux writes; bells. Every question an
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
use std::time::Instant;

/// A number from a pane's counter: the order things happened in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Seq(pub u64);

/// The number of a piece of input fux wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InputSeq(pub Seq);

/// The number of a piece of output the screen took.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutputSeq(pub Seq);

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

/// How output that is no echo is known so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A full-screen program's: everything it draws is output.
    Screen,
    /// The shell's marks: a command's output, or what followed a line
    /// submitted.
    Marks,
    /// The kernel echoes the keys and nothing else: the rest is output.
    Kernel,
    /// It followed input that was no edit, or none at all.
    Order,
}

/// Where a piece of output came from. Each kind says how it is known:
/// the kernel's echo for certain, output by its source, and what followed
/// an edit by its time and place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The program's own output.
    Output { by: Source },
    /// The kernel's echo of input (`ICANON`, `ECHO`), by its fixed rules,
    /// wherever it lands: certain.
    Echo { of: InputSeq },
    /// What the program drew after an edit, before the next input: the
    /// echo on the line typed on (`line`, its first row), output elsewhere;
    /// `shell`, a shell reading its command line, as its marks say.
    Edited {
        of: InputSeq,
        line: Option<fux_vt::RowId>,
        shell: bool,
    },
}

/// What a log entry records.
#[derive(Clone, Debug)]
pub enum Event {
    /// A piece of output and where it came from. Its bytes are kept while
    /// the log's budget allows ([`LOG_BYTES`]), for a replay.
    Output { origin: Origin, bytes: Box<[u8]> },
    /// A piece of input written.
    Input { unit: Unit, bytes: Box<[u8]> },
    /// An action's input began: what came since is what it brought.
    Action,
    /// The program rang the bell, or sent a notification.
    Bell,
    /// The screen took a new size.
    Resize(fux_vt::Size),
}

/// One thing that happened, and its number.
#[derive(Clone, Debug)]
pub struct Entry {
    pub seq: Seq,
    pub at: Instant,
    pub event: Event,
}

/// The most bytes of output and input a pane's log keeps for a replay;
/// older entries keep their place and origin, without their bytes.
pub const LOG_BYTES: usize = 1 << 20;
/// The most entries a pane's log keeps.
pub const LOG_ENTRIES: usize = 1 << 16;

/// A pane's log.
#[derive(Debug, Default)]
pub struct Log {
    entries: VecDeque<Entry>,
    /// Bytes held in the entries.
    bytes: usize,
    /// The first entry whose bytes are still held: a replay starts no
    /// earlier.
    whole_from: usize,
}

impl Log {
    /// Records `event` with number `seq`, at `at`.
    pub fn push(&mut self, seq: Seq, at: Instant, event: Event) {
        self.bytes = self.bytes.saturating_add(held(&event));
        self.entries.push_back(Entry { seq, at, event });
        while self.bytes > LOG_BYTES {
            let Some(entry) = self.entries.get_mut(self.whole_from) else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(held(&entry.event));
            if let Event::Output { bytes, .. } | Event::Input { bytes, .. } = &mut entry.event {
                *bytes = Box::default();
            }
            self.whole_from = self.whole_from.saturating_add(1);
        }
        while self.entries.len() > LOG_ENTRIES {
            self.entries.pop_front();
            self.whole_from = self.whole_from.saturating_sub(1);
        }
    }

    /// Where the output numbered `seq` came from, if the log still holds
    /// it.
    pub fn origin(&self, seq: OutputSeq) -> Option<Origin> {
        let at = self.entries.binary_search_by_key(&seq.0, |e| e.seq).ok()?;
        match self.entries.get(at).map(|e| &e.event) {
            Some(Event::Output { origin, .. }) => Some(*origin),
            _ => None,
        }
    }

    /// The first bell rung after `seq`, if one was.
    pub fn bell_after(&self, seq: Seq) -> Option<Seq> {
        self.after(seq)
            .find(|e| matches!(e.event, Event::Bell))
            .map(|e| e.seq)
    }

    /// The entries after `seq`, oldest first.
    pub fn after(&self, seq: Seq) -> impl Iterator<Item = &Entry> {
        let from = self.entries.partition_point(|e| e.seq <= seq);
        self.entries.iter().skip(from)
    }

    /// The entries whose bytes are still held, for a replay: from the
    /// first, if none has lost them.
    pub fn whole(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().skip(self.whole_from)
    }

    /// Whether the log holds every entry since the pane began, bytes too:
    /// a replay rebuilds its screen.
    pub fn complete(&self) -> bool {
        self.whole_from == 0 && self.entries.front().is_none_or(|e| e.seq.0 <= 1)
    }
}

/// The bytes an event holds.
fn held(event: &Event) -> usize {
    match event {
        Event::Output { bytes, .. } | Event::Input { bytes, .. } => bytes.len(),
        Event::Action | Event::Bell | Event::Resize(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Past its budget, the log lets go of the oldest bytes first, and
    /// keeps every entry's place and origin.
    #[test]
    fn the_oldest_bytes_go_first_and_origins_stay() {
        let mut log = Log::default();
        let now = Instant::now();
        let origin = Origin::Output { by: Source::Order };
        for n in 1..=3u64 {
            let bytes = vec![b'x'; LOG_BYTES / 2].into_boxed_slice();
            log.push(Seq(n), now, Event::Output { origin, bytes });
        }
        log.push(Seq(4), now, Event::Bell);
        assert!(!log.complete());
        assert_eq!(log.whole().count(), 3);
        assert_eq!(log.origin(OutputSeq(Seq(1))), Some(origin));
        assert_eq!(log.bell_after(Seq(2)), Some(Seq(4)));
        assert_eq!(log.after(Seq(2)).count(), 2);
    }
}
