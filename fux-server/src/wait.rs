//! `wait`: a command line that answers once a pane's counts (`Counts`:
//! its program's respawns, its screen's revision, bells rung, shell commands
//! ended, its program's end) differ from those of the version the caller
//! holds, the pane closes, or the time is up. The caller holds a version
//! from before what it waits for (the one an action answers, from before
//! it acted, or a capture's), so nothing that comes between is missed; it
//! names its pane. Given none, a wait compares with each pane's counts as
//! it begins: on a tab or workspace it answers once any of its panes does. Which change means "done" is the
//! caller's to judge (`docs/agents.md`'s recipes).
//!
//! The server asks each wait again after every round (`Waiting::answer`),
//! and when its time is up (`Waiting::due`); so too a respawn under way,
//! answered once it is done.
use crate::after;
use crate::pane::Pane;
use crate::session::Session;
use fux_wire::answer::{Change, PaneAnswer, PaneResults};
use fux_wire::command::{Counts, Run, Version};
use fux_wire::id::PaneId;
use fux_wire::protocol::Reply;
use fux_wire::wire::Budget;
use std::time::{Duration, Instant};

/// What a wait watches, each pane with the counts it waits for that
/// pane's to differ from: one pane, whose answer is its own; or a tab's or
/// workspace's panes, which answer together.
pub enum Watches {
    Pane((PaneId, Counts)),
    Panes(Vec<(PaneId, Counts)>),
}

impl Watches {
    fn all(&self) -> &[(PaneId, Counts)] {
        match self {
            Watches::Pane(one) => std::slice::from_ref(one),
            Watches::Panes(all) => all,
        }
    }
}

/// A request waiting for its answer.
pub enum Waiting {
    Events(crate::journal::Read),
    /// A wait (`wait`, or an action given wait words).
    Change(Watch),
    /// A respawn whose pane's old program left output still read, in
    /// turns, before the new one takes over (`Session::respawn`): answered
    /// `reply` once its pane has been respawned `respawns` times, a failure
    /// if the pane closes first. Nothing else ends it: a respawn under way
    /// is the pane's, done whether or not the one who asked stays.
    Respawn {
        pane: PaneId,
        respawns: u64,
        reply: Reply,
    },
}

impl Waiting {
    /// A wait from `now`, timing out after `limit`, until a pane's counts
    /// differ from those `watches` gives it.
    pub fn new(watches: Watches, run: Run, now: Instant, limit: Duration) -> Waiting {
        Waiting::Change(Watch {
            watches,
            run,
            deadline: after(now, limit),
            limit,
        })
    }

    /// When to ask it again at the latest, if ever before a round changes
    /// what it waits for: when a wait's time is up.
    pub fn due(&self) -> Option<Instant> {
        match self {
            Waiting::Events(read) => Some(read.deadline),
            Waiting::Change(watch) => Some(watch.deadline),
            Waiting::Respawn { .. } => None,
        }
    }

    /// Its answer, if it has one at `now`, made in `budget`.
    pub fn answer(
        &mut self,
        session: &Session,
        budget: &mut Budget,
        now: Instant,
    ) -> Option<Reply> {
        match self {
            Waiting::Events(read) => read.answer(session, budget, now),
            Waiting::Change(watch) => watch.answer(session, budget, now),
            Waiting::Respawn {
                pane: id,
                respawns,
                reply,
            } => match session.parts.panes().get(id) {
                None => Some(PaneResults::One(PaneAnswer::Closed(*id)).into()),
                Some(pane) => (pane.respawns >= *respawns).then(|| reply.clone()),
            },
        }
    }
}

/// A wait under way: what it watches, in the server run `run`.
pub struct Watch {
    watches: Watches,
    run: Run,
    deadline: Instant,
    /// How long it waits for a change before it answers a timeout (later
    /// while a pane is mid-frame), supplied by the request.
    limit: Duration,
}

impl Watch {
    /// Its answer, if it has one at `now`: each pane that changed, exited
    /// or closed, the first failing one's code naming the whole, each
    /// failing one saying why; at its limit, each pane as it stands. A pane
    /// part way through a frame has changed, though it answers only once
    /// the frame is whole, the rest of which is read already: so it holds
    /// the answer past the limit until then, rather than say it is
    /// unchanged.
    pub fn answer(&self, session: &Session, budget: &mut Budget, now: Instant) -> Option<Reply> {
        let pane = |id| session.parts.panes().get(id);
        let framing =
            (self.watches.all().iter()).any(|(id, _)| pane(id).is_some_and(Pane::mid_frame));
        let panes = session.parts.panes();
        let changed = self.each(
            |id, since, b| told(id, panes.get(&id), (since, self.run), b),
            budget,
        );
        let told = match changed {
            Ok(told) if told.is_empty() && now >= self.deadline && !framing => {
                let timed_out = |id, since, _: &mut Budget| {
                    Ok(Some(PaneAnswer::TimedOut {
                        since: self.version(id, since),
                        limit: self.limit,
                    }))
                };
                self.each(timed_out, budget)
            }
            Ok(told) if told.is_empty() => return None,
            told => told,
        };
        let told = match told {
            Ok(told) => told,
            Err(error) => return Some(Reply::failed(error.code(), error.to_string())),
        };
        let answers = match (&self.watches, told.as_slice()) {
            (Watches::Pane(_), [one]) => PaneResults::One(one.clone()),
            _ => PaneResults::Many(told),
        };
        Some(answers.into())
    }

    /// What `answer` says of each pane it watches, if anything, in room
    /// for all of them charged to `budget` first.
    fn each(
        &self,
        answer: impl Fn(
            PaneId,
            Counts,
            &mut Budget,
        ) -> Result<Option<PaneAnswer>, fux_wire::code::Error>,
        budget: &mut Budget,
    ) -> Result<Vec<PaneAnswer>, fux_wire::code::Error> {
        let all = self.watches.all();
        let mut told = fuxix::Allocation::vector(budget, all.len())?;
        for (id, since) in all {
            told.extend(answer(*id, *since, budget)?);
        }
        Ok(told)
    }

    /// Pane `id`'s version in its run at `counts`.
    fn version(&self, id: PaneId, counts: Counts) -> Version {
        Version {
            pane: id,
            counts,
            run: self.run,
        }
    }
}

/// What pane `id` answers, if its counts differ from `since` or it
/// closed, its changes made in room charged to `budget` first.
fn told(
    id: PaneId,
    pane: Option<&Pane>,
    (since, run): (Counts, Run),
    budget: &mut Budget,
) -> Result<Option<PaneAnswer>, fux_wire::code::Error> {
    let Some(pane) = pane else {
        return Ok(Some(PaneAnswer::Closed(id)));
    };
    let now = pane.counts();
    // Part way through a frame (synchronized output, the rest of it already
    // read and kept for the next turn): its screen is one the program never
    // meant shown, so a wait answers once the frame is whole.
    if now == since || pane.mid_frame() {
        return Ok(None);
    }
    // A program started again is a new one: what its counts are now says
    // nothing of the last one's.
    let changes = match now.respawns == since.respawns {
        false => [
            (true, Change::Respawned),
            (false, Change::Screen),
            (false, Change::Bell),
        ],
        true => [
            (now.screen != since.screen, Change::Screen),
            (now.bells != since.bells, Change::Bell),
            (now.done != since.done, Change::Done),
        ],
    };
    let changes = changes
        .into_iter()
        .filter_map(|(changed, what)| changed.then_some(what));
    let mut changed = fuxix::Allocation::vector(budget, changes.clone().count())?;
    changed.extend(changes);
    // Its end, if it ended since: a program started since is a new one,
    // whose end is news.
    let new = now.respawns != since.respawns || now.ended != since.ended;
    Ok(Some(PaneAnswer::Changed {
        version: pane.version(run),
        changed,
        ended: (pane.process().exit_status()).filter(|_| new),
    }))
}

#[cfg(test)]
mod tests {
    use crate::pane::Pane;
    use crate::pane::tests::Lone;
    use fux_wire::command::{Counts, Run, Version};
    use fux_wire::id::PaneId;
    use std::time::Instant;

    /// A wait answers no change part way through a frame, and the frame's
    /// whole screen once it is read.
    #[test]
    fn a_wait_answers_a_frame_whole() -> Result<(), Box<dyn std::error::Error>> {
        let size = fux_vt::Size::new(50, 200)?;
        let mut pane = Lone::new(Pane::new(
            PaneId::of(1),
            None,
            crate::pane::tests::shell()?,
            size,
            crate::pane::tests::options(0)?,
            Instant::now(),
        )?);
        let (since, run) = (pane.counts(), Run::now().ok_or("a clock")?);
        let mut frame = b"\x1b[?2026h".to_vec();
        frame.extend(crate::pane::tests::spends_the_budget(50, 200));
        frame.extend_from_slice(b"DONE\x1b[?2026l");
        pane.output(&frame);
        assert!(pane.mid_frame());
        let budget = &mut fux_wire::wire::Budget::with_limit(1 << 10);
        assert!(super::told(pane.id(), Some(&pane.pane), (since, run), budget)?.is_none());
        while pane.mid_frame() {
            pane.read_unread();
        }
        let told = super::told(pane.id(), Some(&pane.pane), (since, run), budget)?;
        let told = told.ok_or("no answer")?;
        assert!(told.text().ends_with(" screen"), "{}", told.text());
        Ok(())
    }

    /// A version is written and read back as `PANE:COUNTS:RUN`, in one
    /// spelling only.
    #[test]
    fn a_version_reads_back_as_written() -> Result<(), ()> {
        let version = Version {
            pane: PaneId::of(2),
            counts: Counts {
                respawns: 1,
                screen: 57,
                bells: 0,
                done: 3,
                ended: true,
            },
            run: "48211-1867a3f9c2b8e7d6".parse()?,
        };
        let text = "%2:1.57.0.3.1:48211-1867a3f9c2b8e7d6";
        assert_eq!(version.to_string(), text);
        assert_eq!(text.parse(), Ok(version));
        for wrong in [
            "1.57.0.3.1",
            "%2:1.57.0.3:48211-1867a3f9c2b8e7d6",
            "%2:1.57.0.3.2:48211-1867a3f9c2b8e7d6",
            "%2:1.057.0.3.1:48211-1867a3f9c2b8e7d6",
            "%2:1.57.0.3.1:48211-1867A3F9C2B8E7D6",
            "%2:1.57.0.3.1:+48211-1867a3f9c2b8e7d6",
            "%2:1.57.0.3.1:48211-+1867a3f9c2b8e7d6",
            "%2:1.57.0.3.1:48211-1867a3f9c2b8e7d6:",
        ] {
            assert!(wrong.parse::<Version>().is_err(), "{wrong}");
        }
        Ok(())
    }
}
