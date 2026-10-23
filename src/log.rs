//! A pane's journal: what happens in it, in the order it happens, each
//! entry numbered: bells, notifications, titles, shell commands' ends and
//! resizes. `events` streams it, and a wait reads what came after it began.
use std::collections::VecDeque;

/// A number from a pane's journal: the order things happened in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Seq(pub u64);

/// What a log entry records.
#[derive(Clone, Debug)]
pub enum Event {
    /// The program rang the bell.
    Bell,
    /// The program sent a notification (OSC 9, 777): its text.
    Notification(String),
    /// The program set its title.
    Title(String),
    /// A shell command ended (OSC 133 `D`): its status, if the shell said.
    Done(Option<i32>),
    /// The screen took a new size.
    Resize(fux_vt::Size),
}

/// One thing that happened, and its number.
#[derive(Clone, Debug)]
pub struct Entry {
    pub seq: Seq,
    pub event: Event,
}

/// How many entries a pane's journal keeps: what came since a reader last
/// looked, which it does after every round of output.
const KEPT: usize = 1024;

/// A pane's journal: its last [`KEPT`] entries, in order. A reader asking
/// for what came after one that went is told so.
#[derive(Debug, Default)]
pub struct Log {
    entries: VecDeque<Entry>,
    /// The number of the last entry recorded.
    last: Seq,
}

impl Log {
    /// Records `event`, numbered after the last.
    pub fn push(&mut self, event: Event) {
        self.last = Seq(self.last.0.saturating_add(1));
        if self.entries.len() >= KEPT {
            self.entries.pop_front();
        }
        self.entries.push_back(Entry {
            seq: self.last,
            event,
        });
    }

    /// The entries after `seq`, oldest first.
    pub fn after(&self, seq: Seq) -> impl Iterator<Item = &Entry> {
        let from = self.entries.partition_point(|e| e.seq <= seq);
        self.entries.iter().skip(from)
    }

    /// The number of the last entry recorded: 0 if none was.
    pub fn last(&self) -> Seq {
        self.last
    }

    /// Whether something that came after `seq` was let go.
    pub fn lost_after(&self, seq: Seq) -> bool {
        self.entries
            .front()
            .is_some_and(|first| first.seq.0 > seq.0.saturating_add(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Past what it keeps, the journal lets the oldest entries go, and says
    /// what came after one that went.
    #[test]
    fn the_oldest_entries_go_first_and_are_told() {
        let mut log = Log::default();
        for _ in 0..=KEPT {
            log.push(Event::Bell);
        }
        assert!(log.lost_after(Seq(0)) && !log.lost_after(Seq(1)));
        assert_eq!(log.after(Seq(0)).count(), KEPT);
        assert_eq!(log.after(log.last()).count(), 0);
    }
}
