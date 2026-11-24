//! A pane's files as a program reads them that is not a person with `cat`: for now `changes`, in which the
//! holder says what its rounds of writing changed. It is for a reader that has the folder as an earlier
//! round left it (a carrier that keeps a copy of it elsewhere, as koh does) and wants what differs, without
//! comparing the two. The README's "A pane's files" is where every file's form is written.
//!
//! ```text
//! 0
//! 2 fifo in
//! 2 put cursor
//! 2 folder main
//! 2 link shown main
//! 48 append history 391204 4194391
//! 48 move history history.1
//! 48 put history
//! 56 append history 1024 1024
//! 62 append history 1024 2190
//! 62 put main/screen
//! ```
//!
//! The first line is a round's number: the lines say what every round after it did, each with its round's
//! number first (the even `generation` that round ended at), in the order it was done. A reader that has
//! the folder as a round before that one left it is not told, and reads the folder whole.

/// The most lines `changes` has after its first: past them the oldest rounds go, and the first line says which.
pub const CHANGES_MAX: usize = 1024;

/// What a round did to a name, as a line of `changes` says it after the round's number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// `put NAME`: a file made or replaced. None of the bytes a reader has under that name are its bytes.
    Put(String),
    /// `append NAME AT LEN`: a file added to at its end, or cut back, or both: its first AT bytes are as they
    /// were, and it is LEN bytes long. With AT the same as LEN it was cut back to there and nothing added.
    Append(String, u64, u64),
    /// `move FROM TO`: a file given another name (what had that name is gone).
    Move(String, String),
    /// `folder NAME`: a folder made.
    Folder(String),
    /// `link NAME TO`: a link made or turned, and the name beside it that it leads to.
    Link(String, String),
    /// `fifo NAME`: a FIFO made.
    Fifo(String),
    /// `gone NAME`: gone, a folder with all that was in it.
    Gone(String),
}

impl Change {
    /// The name it is about; of a move, the name the file took.
    pub fn name(&self) -> &str {
        match self {
            Change::Put(name) | Change::Append(name, ..) | Change::Folder(name) | Change::Fifo(name) => name,
            Change::Link(name, _) | Change::Gone(name) | Change::Move(_, name) => name,
        }
    }

    /// Whether a holder could have written it: its names are fux's names (a file in a screen's folder
    /// is two, with a `/` between), a link leads to one name, and no more bytes are kept than there are.
    fn sound(&self) -> bool {
        let named = |name: &str| name.split('/').all(crate::is_name);
        match self {
            Change::Append(name, at, len) => named(name) && at <= len,
            Change::Move(from, to) => named(from) && named(to) && from != to,
            Change::Link(name, to) => named(name) && crate::is_name(to),
            Change::Put(name) | Change::Folder(name) | Change::Fifo(name) | Change::Gone(name) => named(name),
        }
    }
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Change::Put(name) => write!(f, "put {name}"),
            Change::Append(name, at, len) => write!(f, "append {name} {at} {len}"),
            Change::Move(from, to) => write!(f, "move {from} {to}"),
            Change::Folder(name) => write!(f, "folder {name}"),
            Change::Link(name, to) => write!(f, "link {name} {to}"),
            Change::Fifo(name) => write!(f, "fifo {name}"),
            Change::Gone(name) => write!(f, "gone {name}"),
        }
    }
}

/// What a pane's `changes` says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// The round the file begins with: what every round after it did is in the lines.
    pub since: u64,
    /// What they did, in order, each with its round's number: even, and above `since`.
    pub list: Vec<(u64, Change)>,
}

impl Changes {
    /// What the text of a `changes` says. None if it is not what a holder writes, to the byte (a line
    /// that is none of the seven, a name that is no name of fux's, more bytes kept than there are, a
    /// number that is odd or out of order, more lines than the bound): that is no `changes` to a reader,
    /// which then reads the folder whole.
    pub fn parse(text: &str) -> Option<Changes> {
        let mut lines = text.lines();
        let mut changes = Changes { since: lines.next()?.parse().ok()?, list: Vec::new() };
        for line in lines {
            let words: Vec<&str> = line.split(' ').collect();
            let change = match words.get(1..)? {
                ["put", name] => Change::Put(name.to_string()),
                ["append", name, at, len] => Change::Append(name.to_string(), at.parse().ok()?, len.parse().ok()?),
                ["move", from, to] => Change::Move(from.to_string(), to.to_string()),
                ["folder", name] => Change::Folder(name.to_string()),
                ["link", name, to] => Change::Link(name.to_string(), to.to_string()),
                ["fifo", name] => Change::Fifo(name.to_string()),
                ["gone", name] => Change::Gone(name.to_string()),
                _ => return None,
            };
            changes.list.push((words.first()?.parse().ok()?, change));
        }
        let even = |number: u64| number.is_multiple_of(2);
        let sound = |(number, change): &(u64, Change)| even(*number) && *number > changes.since && change.sound();
        let sound = even(changes.since) && changes.list.len() <= CHANGES_MAX && changes.list.iter().all(sound);
        let ordered = changes.list.is_sorted_by_key(|(number, _)| *number);
        // (Written again it is the text: no other spelling of a number, no blank more, every line ended.)
        (sound && ordered && changes.to_string() == text).then_some(changes)
    }

    /// What the rounds after `number` did, for a reader that has the folder as that round left it. None
    /// when the file does not say (that round is before the one it begins with): read the folder whole.
    pub fn after(&self, number: u64) -> Option<&[(u64, Change)]> {
        let from = self.list.partition_point(|(round, _)| *round <= number);
        (number >= self.since).then(|| self.list.get(from..).unwrap_or_default())
    }

    /// Puts what the round `number` did after what the rounds before did. What it makes of no account
    /// goes from the lines before it, so that a reader behind by any number of rounds reads only what
    /// differs from what it has: a file replaced again is one `put`, a link turned again one `link`,
    /// what is gone is one `gone` (a folder's with all that was said of what was in it). Of the `append`s
    /// of a file the last stays, and before it each cut (AT the same as LEN) to less than the file is
    /// long now: a reader takes the least AT of those after its round. Nothing goes that was said
    /// before the name was last in a `move`: that is of the file that has the other name now.
    /// Past `CHANGES_MAX` lines the oldest rounds go, each whole, and `since` is the last of them.
    pub fn fold(&mut self, number: u64, change: Change) {
        let name = change.name();
        let under = |other: &str| other.strip_prefix(name).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
        let goes = |before: &Change| match (&change, before) {
            (Change::Gone(_), before) => under(before.name()),
            (Change::Put(_), Change::Put(other) | Change::Append(other, ..)) => other == name,
            (Change::Append(_, _, len), Change::Append(other, at, was)) => other == name && !(at == was && at < len),
            (Change::Link(..), Change::Link(other, _)) => other == name,
            _ => false,
        };
        let moved = |(_, before): &(u64, Change)| matches!(before, Change::Move(from, to) if under(from) || under(to));
        let (kept, mut at) = (self.list.iter().rposition(moved).map_or(0, |moved| moved + 1), 0);
        self.list.retain(|(_, before)| {
            at += 1;
            at <= kept || !goes(before)
        });
        self.list.push((number, change));
        let oldest = self.list.len().checked_sub(CHANGES_MAX + 1).and_then(|over| self.list.get(over));
        if let Some(oldest) = oldest.map(|(number, _)| *number) {
            self.since = oldest;
            self.list.retain(|(number, _)| *number > oldest);
        }
    }
}

impl std::fmt::Display for Changes {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "{}", self.since)?;
        self.list.iter().try_for_each(|(number, change)| writeln!(f, "{number} {change}"))
    }
}
