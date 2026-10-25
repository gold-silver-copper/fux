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
//! and when its time is up (`Waiting::due`).
use crate::after;
use crate::code::Code;
use crate::command::{AnyRef, Counts, Run, Version};
use crate::id::PaneId;
use crate::json::Json;
use crate::pane::Pane;
use crate::process::Ended;
use crate::protocol::{End, Reply};
use crate::session::Session;
use std::time::{Duration, Instant};

/// A wait under way: each pane it watches, and the counts it waits for
/// that pane's to differ from, in the server run `run`.
pub struct Waiting {
    watches: Vec<(PaneId, Counts)>,
    run: Run,
    deadline: Instant,
    /// How long it waits at most: its own timeout, else the server's.
    limit: Duration,
}

impl Waiting {
    /// A wait from `now`, for `limit` at most, until a pane's counts
    /// differ from those `watches` gives it.
    pub fn new(watches: Vec<(PaneId, Counts)>, run: Run, now: Instant, limit: Duration) -> Waiting {
        Waiting {
            watches,
            run,
            deadline: after(now, limit),
            limit,
        }
    }

    /// When to ask it again at the latest: when its time is up.
    pub fn due(&self) -> Instant {
        self.deadline
    }

    /// Its answer, if it has one at `now`: a line for each pane that
    /// changed, exited or closed, the first failing status naming the
    /// whole; at its limit, each pane as it stands. A pane part way
    /// through a frame has changed, though it answers only once the frame
    /// is whole, the rest of which is read already: so it holds the
    /// answer past the limit until then, rather than say it is unchanged.
    pub fn answer(&self, session: &Session, now: Instant) -> Option<Reply> {
        let pane = |id| session.panes.get(id);
        let told: Vec<Reply> = (self.watches.iter())
            .filter_map(|(id, since)| told(*id, pane(id), *since, self.run))
            .collect();
        let framing = (self.watches.iter()).any(|(id, _)| pane(id).is_some_and(Pane::mid_frame));
        let told = match told.is_empty() {
            false => told,
            true if now >= self.deadline && !framing => (self.watches.iter())
                .map(|(id, since)| timed_out(*id, *since, self.run, self.limit))
                .collect(),
            true => return None,
        };
        if let [one] = told.as_slice() {
            return Some(one.clone());
        }
        let why: String = (told.iter())
            .filter_map(|a| match &a.end {
                End::Failed { message, .. } => Some(format!("{}\n", message.trim_end())),
                End::Done => None,
            })
            .collect();
        let end = match told.iter().find_map(|a| a.end.code()) {
            None => End::Done,
            Some(code) => End::Failed { code, message: why },
        };
        Some(Reply {
            end,
            text: told.iter().map(|a| a.text.as_str()).collect(),
            json: told.iter().map(|a| a.json.as_str()).collect(),
            ids: told.iter().flat_map(|a| a.ids.iter().copied()).collect(),
            versions: told
                .iter()
                .flat_map(|a| a.versions.iter().copied())
                .collect(),
            ..Reply::default()
        })
    }
}

/// What pane `id` answers, if its counts differ from `since` or it
/// closed: `%2:0.57.1.3.0:RUN screen done`, and as JSON.
fn told(id: PaneId, pane: Option<&Pane>, since: Counts, run: Run) -> Option<Reply> {
    let Some(pane) = pane else {
        let end = End::Failed {
            code: Code::Closed,
            message: format!("{id}: the pane closed"),
        };
        let closed = Reply {
            ids: vec![AnyRef::Pane(id)],
            ..said(id, format!("{id} closed"), Vec::new(), end)
        };
        return Some(closed);
    };
    let now = pane.counts();
    // Part way through a frame (synchronized output, the rest of it already
    // read and kept for the next turn): its screen is one the program never
    // meant shown, so a wait answers once the frame is whole.
    if now == since || pane.mid_frame() {
        return None;
    }
    // A program started again is a new one: what its counts are now says
    // nothing of the last one's.
    let changed: Vec<&str> = match now.respawns == since.respawns {
        false => vec!["respawned"],
        true => [
            (now.screen != since.screen, "screen"),
            (now.bells != since.bells, "bell"),
            (now.done != since.done, "done"),
        ]
        .into_iter()
        .filter_map(|(changed, what)| changed.then_some(what))
        .collect(),
    };
    // Its end, if it ended since: a program started since is a new one,
    // whose end is news.
    let new = now.respawns != since.respawns || now.ended != since.ended;
    let ended = (pane.process.exit_status()).filter(|_| new);
    let ended_said = ended.map(crate::process::told);
    let version = pane.version(run);
    let line = std::iter::once(version.to_string())
        .chain(changed.iter().map(|c| (*c).to_owned()))
        .chain(ended_said)
        .collect::<Vec<_>>()
        .join(" ");
    let mut fields = vec![
        ("version", Json::str(version.to_string())),
        (
            "changed",
            Json::Array(changed.iter().map(|c| Json::str(*c)).collect()),
        ),
    ];
    fields.extend(ended.map(|ended| match ended {
        Ended::Exited(code) => ("exited", Json::int(code)),
        Ended::Signalled(signal) => ("signal", Json::int(signal)),
        Ended::Lost => ("exited", Json::Null),
    }));
    let changed = said(id, line, fields, End::Done);
    Some(Reply {
        versions: vec![version],
        ..changed
    })
}

/// What pane `id` answers when the time is up, its counts still `since`.
fn timed_out(id: PaneId, since: Counts, run: Run, limit: Duration) -> Reply {
    let since = Version {
        pane: id,
        counts: since,
        run,
    };
    let end = End::Failed {
        code: Code::Timeout,
        message: format!(
            "{id}: unchanged for {}s, still at version {since}",
            limit.as_secs_f64()
        ),
    };
    let fields = vec![("version", Json::str(since.to_string()))];
    Reply {
        versions: vec![since],
        ..said(id, format!("{since} timeout"), fields, end)
    }
}

/// A pane's answer, said two ways: as text, a line of what it says,
/// which names the pane; as JSON, the pane, `fields` and the status (a
/// failed one's is never shown: its failure ends the program, or a `try`
/// drops it).
fn said(id: PaneId, line: String, fields: Vec<(&'static str, Json<'static>)>, end: End) -> Reply {
    let pane = ("pane", Json::str(id.to_string()));
    let mut fields: Vec<_> = std::iter::once(pane).chain(fields).collect();
    fields.push(("status", Json::int(end.status())));
    Reply {
        end,
        text: format!("{line}\n"),
        json: format!("{}\n", Json::Object(fields).render()),
        ..Reply::default()
    }
}

#[cfg(test)]
mod tests {
    use crate::command::{Counts, Run, Version};
    use crate::id::PaneId;
    use crate::pane::{Pane, Runs};

    /// A wait answers no change part way through a frame, and the frame's
    /// whole screen once it is read.
    #[test]
    fn a_wait_answers_a_frame_whole() -> Result<(), Box<dyn std::error::Error>> {
        let size = fux_vt::Size::new(50, 200)?;
        let mut pane = Pane::new(PaneId::of(1), "sh".into(), Runs::Shell, size, 0)?;
        let (since, run) = (pane.counts(), Run::now().ok_or("a clock")?);
        let mut frame = b"\x1b[?2026h".to_vec();
        frame.extend(crate::pane::tests::spends_the_budget(50, 200));
        frame.extend_from_slice(b"DONE\x1b[?2026l");
        pane.output(&frame);
        assert!(pane.mid_frame());
        assert!(super::told(pane.id, Some(&pane), since, run).is_none());
        while pane.mid_frame() {
            pane.read_unread();
        }
        let told = super::told(pane.id, Some(&pane), since, run).ok_or("no answer")?;
        assert!(told.text.ends_with(" screen\n"), "{}", told.text);
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
