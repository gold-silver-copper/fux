//! `wait`: a command line that answers once a pane's version (`Version`:
//! its program's starts, its screen's revision, bells rung, shell commands
//! ended) differs from the one the caller holds, its program exits, the
//! pane closes, or the time is up. The caller holds a version from before
//! what it waits for (the one an action answers, from before it acted, or
//! a capture's), so nothing that comes between is missed; given none, a
//! wait compares with the version as it begins. On a tab or workspace it
//! answers once any of its panes does. Which change means "done" is the
//! caller's to judge (`docs/agents.md`'s recipes).
//!
//! The server asks each wait again after every round (`Waiting::answer`),
//! and when its time is up (`Waiting::due`).
use crate::after;
use crate::code::Code;
use crate::command::{AnyRef, Version};
use crate::id::PaneId;
use crate::json::Json;
use crate::pane::Pane;
use crate::process::Ended;
use crate::protocol::{End, Reply};
use crate::session::Session;
use std::time::{Duration, Instant};

/// A wait under way: each pane it watches, and the version it waits for
/// that pane's to differ from.
pub struct Waiting {
    watches: Vec<(PaneId, Version)>,
    deadline: Instant,
    /// How long it waits at most: its own timeout, else the server's.
    pub(crate) limit: Duration,
}

impl Waiting {
    /// A wait in `panes` from `now`, for `limit` at most, until a version
    /// differs from `since`, else from each pane's now.
    pub fn new(panes: &[&Pane], since: Option<Version>, now: Instant, limit: Duration) -> Waiting {
        Waiting {
            watches: (panes.iter())
                .map(|p| (p.id, since.unwrap_or_else(|| p.version())))
                .collect(),
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
    /// whole; at its limit, each pane as it stands.
    pub fn answer(&mut self, session: &Session, now: Instant) -> Option<Reply> {
        let told: Vec<Reply> = (self.watches.iter())
            .filter_map(|(id, since)| told(*id, session.panes.get(id), *since))
            .collect();
        let told = match told.is_empty() {
            false => told,
            true if now >= self.deadline => (self.watches.iter())
                .map(|(id, since)| timed_out(*id, *since, self.limit))
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
            ..Reply::default()
        })
    }
}

/// What pane `id` answers, if its version differs from `since`, its
/// program exited or it closed: `%2 0.57.1.3 screen done`, and as JSON.
fn told(id: PaneId, pane: Option<&Pane>, since: Version) -> Option<Reply> {
    let Some(pane) = pane else {
        let end = End::Failed {
            code: Code::Closed,
            message: format!("{id}: the pane closed"),
        };
        return Some(said(id, "closed".to_owned(), Vec::new(), end));
    };
    let (now, ended) = (pane.version(), pane.process.exit_status());
    if now == since && ended.is_none() {
        return None;
    }
    // A program started again is a new one: what its counts are now says
    // nothing of the last one's.
    let changed: Vec<&str> = match now.starts == since.starts {
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
    let ended_said = ended.map(crate::process::told);
    let line = std::iter::once(now.to_string())
        .chain(changed.iter().map(|c| (*c).to_owned()))
        .chain(ended_said)
        .collect::<Vec<_>>()
        .join(" ");
    let mut fields = vec![
        ("version", Json::str(now.to_string())),
        (
            "changed",
            Json::Array(changed.iter().map(|c| Json::str(*c)).collect()),
        ),
    ];
    fields.extend(ended.map(|ended| match ended {
        Ended::Exited(code) => ("exited", Json::int(code)),
        Ended::Signalled(signal) => ("signal", Json::int(signal)),
    }));
    Some(said(id, line, fields, End::Done))
}

/// What pane `id` answers when the time is up, its version still `since`.
fn timed_out(id: PaneId, since: Version, limit: Duration) -> Reply {
    let end = End::Failed {
        code: Code::Timeout,
        message: format!(
            "{id}: unchanged for {}s, still at version {since}",
            limit.as_secs_f64()
        ),
    };
    let fields = vec![("version", Json::str(since.to_string()))];
    said(id, format!("{since} timeout"), fields, end)
}

/// A pane's answer, said two ways: as text, a line naming the pane and
/// what it says; as JSON, the pane, `fields`, the status and the code of
/// one that is not 0.
fn said(id: PaneId, line: String, fields: Vec<(&'static str, Json<'static>)>, end: End) -> Reply {
    let pane = ("pane", Json::str(id.to_string()));
    let mut fields: Vec<_> = std::iter::once(pane).chain(fields).collect();
    fields.push(("status", Json::int(end.status())));
    fields.extend(end.code().map(|code| ("code", Json::str(code.name()))));
    Reply {
        end,
        text: format!("{id} {line}\n"),
        json: format!("{}\n", Json::Object(fields).render()),
        ids: vec![AnyRef::Pane(id)],
        ..Reply::default()
    }
}

#[cfg(test)]
mod tests {
    use crate::command::Version;

    /// A version is written and read back as `STARTS.SCREEN.BELLS.DONE`.
    #[test]
    fn a_version_reads_back_as_written() {
        let version = Version {
            starts: 1,
            screen: 57,
            bells: 0,
            done: 3,
        };
        assert_eq!(version.to_string(), "1.57.0.3");
        assert_eq!("1.57.0.3".parse(), Ok(version));
        assert!("1.57.0".parse::<Version>().is_err());
        assert!("1.57.0.3.4".parse::<Version>().is_err());
    }
}
