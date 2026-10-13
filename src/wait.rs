//! `fux wait`, and send-keys' waits: a command line from the CLI that answers
//! once one of its panes does what one of its conditions asks, its program
//! exits first, its pane goes, or its time is up. The server holds its
//! connection meanwhile and asks it again whenever a pane may have changed,
//! or its time comes (`Waiting::due`).
//!
//! Text is looked for in every line written since the wait began, history
//! included, not only on the screen: a match that a flood scrolls away at
//! once is still found. History rows do not change, so each is read once.
//! For send-keys and the other waits armed before an action, a match counts
//! only on a line that changed since the wait began (it "appears anew"),
//! and not on a line holding the text typed (its echo).
use crate::command::{CaptureView, Until, Wait};
use crate::id::PaneId;
use crate::json::Json;
use crate::pane::Pane;
use crate::process::Ended;
use crate::session::{Outcome, Session};
use std::time::Instant;

/// The exit statuses a wait ends with, besides 0 (what it waited for came)
/// and the 1 and 2 of any command.
pub const EXITED: u8 = 3;
pub const GONE: u8 = 4;
/// The program reads its terminal, and the text waited for has not come:
/// it cannot before the agent types.
pub const STOPPED: u8 = 7;
/// As `timeout(1)`.
pub const TIMED_OUT: u8 = 124;

/// How often a wait that asks what a pane's program is blocked in looks
/// again: that changes with nothing on the screen to wake the server.
const PROBE: std::time::Duration = std::time::Duration::from_millis(30);

/// A wait under way.
pub struct Waiting {
    watches: Vec<Watch>,
    pub(crate) wait: Wait,
    /// When it began: a pane is quiet from then at the soonest.
    began: Instant,
    deadline: Instant,
    /// Printed before what the wait says: the pane `run` started.
    pub lead: String,
    /// The program has just started (`run`, `respawn`): its screen is
    /// blank, so a text is gone only once it has appeared.
    pub fresh: bool,
    /// What the command it follows made, for its reply.
    pub made: Vec<crate::command::AnyRef<crate::id::WsId>>,
    /// What each pane said, for a wait on each of several (`Wait::all`).
    answers: Vec<Option<Outcome>>,
    /// A capture to give once the pane settles (`capture` from a command
    /// line): its history, view and whether as JSON.
    pub capture: Option<(Option<usize>, CaptureView, bool)>,
}

/// What a wait's answer tells of a pane against how it was as the wait
/// began: where the cursor was (what came since is said), the commands that
/// had ended, the screen's revision.
#[derive(Clone, Copy)]
struct Since {
    cursor: Option<fux_vt::RowId>,
    finished: u64,
    revision: u64,
}

/// One pane a wait watches, and what it found as it began.
struct Watch {
    pane: PaneId,
    /// The screen's top row as the wait began: lines from it on count.
    anchor: Option<fux_vt::RowId>,
    /// The screen as the wait began: a row changed since may be new; and
    /// its lines then, by first row, for a wait armed before an action: a
    /// line that reads as it did (a redraw, a reflow on resize) is not.
    mark: fux_vt::Mark,
    was: std::collections::HashMap<fux_vt::RowId, String>,
    /// Lines whose text has read otherwise than as the wait began, at
    /// some look since: one erased and written again the same is new.
    touched: std::collections::HashSet<fux_vt::RowId>,
    /// The last history row read, and for each text, the first line found
    /// holding it in the history rows read: they do not change.
    read: Option<fux_vt::RowId>,
    in_history: Vec<Option<String>>,
    /// The wait began on the main screen: while a program holds the full
    /// screen, what the main screen got since (from the anchor) counts
    /// too, so a line printed just before it took the screen is seen.
    on_main: bool,
    /// The `--gone` texts seen since the wait began, for a fresh program.
    appeared: Vec<String>,
    /// A line held the text but was taken as the echo of what was typed.
    echo_skipped: bool,
    revision: u64,
    /// Commands ended as the wait began: one that ends since is said.
    finished: u64,
    /// The program's start (`Pane::starts`) the wait watches.
    starts: u64,
    alerts: u64,
    /// The row the cursor was on as the wait began: what is written from
    /// it on is what the action brought.
    cursor: Option<fux_vt::RowId>,
    /// No input has gone in since the program started: every line counts.
    all_new: bool,
}

/// What a wait found.
enum Found {
    Text {
        text: String,
        line: String,
    },
    Gone(String),
    Exit(Ended),
    Bell(Option<String>),
    /// The pane settled, so.
    Settled(crate::ready::State),
    /// The program reads its terminal; the text waited for has not come.
    Stopped(crate::ready::State),
}

impl Waiting {
    pub fn new(panes: &[&Pane], wait: Wait, now: Instant) -> Waiting {
        let texts = texts(&wait);
        let watches = panes
            .iter()
            .map(|pane| {
                let screen = pane.screen();
                let commands = screen.commands();
                let was = if wait.after {
                    (lines(screen.rows().skip(screen.history_len()), None).into_iter())
                        .map(|l| (l.first, l.text))
                        .collect()
                } else {
                    std::collections::HashMap::new()
                };
                // A wait of its own counts what came since the last input
                // to the pane, or since its program started.
                let cursor = screen
                    .window()
                    .row(screen.cursor_position().0)
                    .map(|row| row.id());
                let (anchor, mark, all_new, cursor, finished) = match (wait.after, pane.input_from)
                {
                    (true, _) => (
                        screen.window().row(0).map(|row| row.id()),
                        screen.mark(),
                        false,
                        cursor,
                        commands.finished,
                    ),
                    (false, Some(from)) => {
                        (from.cursor, from.mark, false, from.cursor, from.finished)
                    }
                    (false, None) => {
                        let first = screen.rows().next().map(|row| row.id());
                        (first, screen.mark(), true, first, 0)
                    }
                };
                Watch {
                    pane: pane.id,
                    anchor,
                    mark,
                    was,
                    touched: std::collections::HashSet::new(),
                    read: None,
                    in_history: vec![None; texts.len()],
                    on_main: !screen.mode(fux_vt::Mode::AlternateScreen),
                    appeared: Vec::new(),
                    echo_skipped: false,
                    revision: pane.revision,
                    finished,
                    starts: pane.starts,
                    alerts: pane.alerts.count(),
                    cursor,
                    all_new,
                }
            })
            .collect();
        Waiting {
            answers: vec![None; panes.len()],
            watches,
            deadline: crate::after(now, wait.timeout),
            wait,
            began: now,
            lead: String::new(),
            made: Vec::new(),
            fresh: false,
            capture: None,
        }
    }

    /// When to ask it again at the latest, whatever happens before: soon,
    /// while it asks what a pane's program is blocked in (that changes with
    /// nothing on the screen to wake the server), else when its time is up.
    pub fn due(&self, _session: &Session) -> Instant {
        let now = Instant::now();
        let probe = (self.wait.until.iter())
            .any(|u| matches!(u, Until::Settled | Until::Text(_)))
            .then(|| crate::after(now, PROBE));
        probe.map_or(self.deadline, |at| at.min(self.deadline))
    }

    /// Its answer, if it has one at `now`: what it waited for came in one of
    /// its panes, a pane's program exited first, a pane went, or its time is
    /// up.
    pub fn answer(&mut self, session: &Session, now: Instant) -> Option<Outcome> {
        let mut outcome = self.look(session, now)?;
        // A capture asked to settle first: its state line, then the capture.
        if let (Some((history, view, json)), Some(watch)) = (self.capture, self.watches.first())
            && let Some(pane) = session.panes.get(&watch.pane)
        {
            let read = crate::capture::pane(pane, history, view, json, session.copying(pane.id));
            outcome.stdout = if json {
                read
            } else {
                let line = outcome.stdout.lines().next().unwrap_or_default();
                format!("{line}\n{read}")
            };
        }
        // An answer that starts with the pane's state names the pane itself.
        let lead = if self.wait.until.contains(&Until::Settled) && self.capture.is_none() {
            ""
        } else {
            self.lead.as_str()
        };
        Some(Outcome {
            stdout: format!("{lead}{}", outcome.stdout),
            ids: self.made.clone(),
            ..outcome
        })
    }

    fn look(&mut self, session: &Session, now: Instant) -> Option<Outcome> {
        let texts = texts(&self.wait);
        let several = self.watches.len() > 1;
        if self.wait.all && several {
            return self.look_each(session, now, &texts);
        }
        // A pane that met it answers; one that can no longer (its program
        // exited or was started again, or the pane went) ends the wait only
        // once none is left that could.
        let mut ended: Option<(PaneId, Told)> = None;
        let mut live = false;
        for watch in &mut self.watches {
            let Some(pane) = session.panes.get(&watch.pane) else {
                ended.get_or_insert((watch.pane, Told::Gone));
                continue;
            };
            if pane.starts != watch.starts {
                ended.get_or_insert((watch.pane, Told::Replaced));
                continue;
            }
            if let Some(found) = watch.look(pane, &self.wait, &texts, (self.began, now), self.fresh)
            {
                let status = found.status();
                let id = watch.pane;
                let since = Some(watch.since());
                return Some(self.wait.told(
                    id,
                    Some(pane),
                    status,
                    Told::Found(found),
                    several,
                    since,
                ));
            }
            match pane.process.exit_status() {
                Some(status) => {
                    ended.get_or_insert((watch.pane, Told::Exited(status, watch.echo_hint())));
                }
                None => live = true,
            }
        }
        if !live && let Some((id, told)) = ended {
            let status = match told {
                Told::Gone => GONE,
                Told::Found(_) | Told::Exited(..) | Told::Replaced | Told::TimedOut(_) => EXITED,
            };
            return Some(
                self.wait
                    .told(id, session.panes.get(&id), status, told, several, None),
            );
        }
        if now >= self.deadline {
            // Said of a pane still running, if one is: what it is doing.
            let first = (self.watches.iter())
                .find(|w| {
                    (session.panes.get(&w.pane)).is_some_and(|p| p.process.exit_status().is_none())
                })
                .or(self.watches.first());
            let pane = first.and_then(|w| session.panes.get(&w.pane));
            let hint = first.zip(pane).and_then(|(w, p)| w.hint(p, &self.wait));
            let since = first.map(Watch::since);
            let first = first.map(|w| w.pane);
            return first.map(|id| {
                self.wait
                    .told(id, pane, TIMED_OUT, Told::TimedOut(hint), several, since)
            });
        }
        None
    }
}

impl Waiting {
    /// A wait on each of several panes: each answers on its own, as a
    /// lone pane's wait would, and the wait once every one has, or its time
    /// is up; a line from each, in order, and the first failing status.
    fn look_each(&mut self, session: &Session, now: Instant, texts: &[String]) -> Option<Outcome> {
        let window = (self.began, now);
        for (watch, answer) in self.watches.iter_mut().zip(self.answers.iter_mut()) {
            if answer.is_some() {
                continue;
            }
            let id = watch.pane;
            let Some(pane) = session.panes.get(&id) else {
                *answer = Some(self.wait.told(id, None, GONE, Told::Gone, true, None));
                continue;
            };
            let since = Some(watch.since());
            let told = if pane.starts != watch.starts {
                Some((EXITED, Told::Replaced))
            } else if let Some(found) = watch.look(pane, &self.wait, texts, window, self.fresh) {
                Some((found.status(), Told::Found(found)))
            } else {
                pane.process
                    .exit_status()
                    .map(|ended| (EXITED, Told::Exited(ended, watch.echo_hint())))
            };
            if let Some((status, told)) = told {
                *answer = Some(self.wait.told(id, Some(pane), status, told, true, since));
            }
        }
        if now >= self.deadline {
            for (watch, answer) in self.watches.iter().zip(self.answers.iter_mut()) {
                if answer.is_none() {
                    let pane = session.panes.get(&watch.pane);
                    let hint = pane.and_then(|p| watch.hint(p, &self.wait));
                    let told = Told::TimedOut(hint);
                    let since = Some(watch.since());
                    *answer = Some(
                        self.wait
                            .told(watch.pane, pane, TIMED_OUT, told, true, since),
                    );
                }
            }
        }
        if self.answers.iter().any(Option::is_none) {
            return None;
        }
        let answers: Vec<Outcome> = self.answers.iter().flatten().cloned().collect();
        let status = (answers.iter().map(|a| a.status))
            .find(|s| *s != 0)
            .unwrap_or(0);
        let mut stderr = String::new();
        for answer in &answers {
            if !answer.stderr.is_empty() {
                stderr.push_str(answer.stderr.trim_end());
                stderr.push('\n');
            }
        }
        Some(Outcome {
            status,
            stdout: answers.iter().map(|a| a.stdout.as_str()).collect(),
            stderr,
            json: answers.iter().map(|a| a.json.as_str()).collect(),
            ..Outcome::default()
        })
    }
}

impl Found {
    /// The status a wait that found it ends with.
    fn status(&self) -> u8 {
        match self {
            // As timeout(1): the program's own status, once it exits.
            Found::Exit(ended) => {
                u8::try_from(crate::process::shell_status(*ended)).unwrap_or(u8::MAX)
            }
            Found::Stopped(_) => STOPPED,
            Found::Text { .. } | Found::Gone(_) | Found::Bell(_) | Found::Settled(_) => 0,
        }
    }
}

/// How a wait ended, to be told.
enum Told {
    Found(Found),
    /// With why the text it waited for may have gone unseen, if it may.
    Exited(Ended, Option<&'static str>),
    /// The program was started again (`respawn`) before.
    Replaced,
    Gone,
    /// With why the wait may never be met, if a reason shows.
    TimedOut(Option<&'static str>),
}

impl Wait {
    /// The outcome of a wait that ended so in pane `id`.
    fn told(
        &self,
        id: PaneId,
        pane: Option<&Pane>,
        status: u8,
        told: Told,
        several: bool,
        since: Option<Since>,
    ) -> Outcome {
        let prefix = if several {
            format!("{id} ")
        } else {
            String::new()
        };
        // A settling action, or a wait the program stopped short of: its
        // state, then what came since it began.
        let settles = self.until.contains(&Until::Settled);
        // `0 timeout`: at once, as it is, and no failure.
        let at_once = settles && self.timeout.is_zero();
        let (state, status) = match (&told, pane) {
            (Told::Found(Found::Settled(state) | Found::Stopped(state)), _) => {
                (Some(*state), status)
            }
            (Told::TimedOut(_), Some(pane)) if at_once => {
                (Some(crate::ready::state(pane, Instant::now())), 0)
            }
            (Told::TimedOut(_), _) if settles => (Some(crate::ready::State::Busy), status),
            (
                Told::Found(_) | Told::Exited(..) | Told::Replaced | Told::Gone | Told::TimedOut(_),
                _,
            ) => (None, status),
        };
        // A settling action whose pane closed (a shell that exited): said,
        // with status 4, not a failure.
        if settles && matches!(told, Told::Gone) {
            return Outcome {
                status: GONE,
                stdout: format!("{id} closed\n"),
                json: format!(
                    "{}\n",
                    Json::Object(vec![
                        ("pane", Json::str(id.to_string())),
                        ("state", Json::str("closed")),
                        ("status", Json::Number(i64::from(GONE))),
                    ])
                    .render()
                ),
                ..Outcome::default()
            };
        }
        if let (Some(state), Some(pane)) = (state, pane) {
            let line = self.state_line(id, pane, state, since);
            // A text that cannot come: the screen, to act on what is there.
            let body = match told {
                Told::Found(Found::Stopped(_)) => text(pane, self.marks),
                Told::Found(_)
                | Told::Exited(..)
                | Told::Replaced
                | Told::Gone
                | Told::TimedOut(_)
                    if self.print =>
                {
                    since_text(pane, since.and_then(|s| s.cursor), self.marks)
                }
                Told::Found(_)
                | Told::Exited(..)
                | Told::Replaced
                | Told::Gone
                | Told::TimedOut(_) => String::new(),
            };
            // Said on stdout, with its status: still busy (124), stopped
            // short of the text (7). Not a failure: the answer is the screen.
            let stderr = String::new();
            let json = Json::Object(vec![
                ("pane", Json::str(id.to_string())),
                ("state", Json::str(state.word())),
                ("status", Json::Number(i64::from(status))),
                ("output", Json::str(body.clone())),
            ]);
            return Outcome {
                status,
                stdout: format!("{line}\n{body}"),
                stderr,
                json: format!("{}\n", json.render()),
                ..Outcome::default()
            };
        }
        let screen = pane.filter(|_| self.print).map(|p| text(p, self.marks));
        let (said, problem, json) = match told {
            Told::Found(found) => {
                let (said, json) = found.said();
                (said, None, json)
            }
            Told::Exited(ended, hint) => {
                let what = self.describe();
                // Its last words, which an agent would otherwise ask for next:
                // a crash says why.
                let last = pane.map(|p| tail(p, 3)).unwrap_or_default();
                let mut message = format!(
                    "{id}: its program {} while waiting for {what}",
                    crate::process::told(ended)
                );
                for line in &last {
                    message.push_str(&format!("\n  {line}"));
                }
                if let Some(hint) = hint {
                    message.push_str(&format!("\n({hint})"));
                }
                let mut json = ended_json("exited", ended);
                json.push((
                    "last_lines",
                    Json::Array(last.into_iter().map(Json::str).collect()),
                ));
                (String::new(), Some(message), json)
            }
            Told::Gone => (
                String::new(),
                Some(format!("{id}: the pane closed")),
                vec![("met", Json::str("closed"))],
            ),
            Told::Replaced => (
                String::new(),
                Some(format!(
                    "{id}: its program was started again (respawn) while waiting for {}",
                    self.describe()
                )),
                vec![("met", Json::str("respawned"))],
            ),
            Told::TimedOut(hint) => {
                // What the pane is doing, so the next step is clear: still
                // busy, or settled on a prompt that will not change.
                let seconds = self.timeout.as_secs_f64();
                let idle = pane.map(|p| Instant::now().saturating_duration_since(p.changed_at));
                let last = pane.and_then(|p| tail(p, 1).pop());
                let mut message = format!(
                    "{id}: timed out after {seconds}s waiting for {}",
                    self.describe()
                );
                if let Some(idle) = idle {
                    message.push_str(&format!(
                        "; its program runs, the screen unchanged for {}s",
                        idle.as_secs()
                    ));
                }
                if let Some(line) = &last {
                    message.push_str(&format!("; last line: `{line}`"));
                }
                let queued = pane.map_or(0, Pane::input_queued);
                if queued > 0 {
                    message.push_str(&format!(
                        "; {queued} bytes of input not yet read by the program"
                    ));
                }
                if let Some(hint) = hint {
                    message.push_str(&format!("\n  {hint}"));
                }
                let millis = |d: std::time::Duration| {
                    Json::Number(i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
                };
                (
                    String::new(),
                    Some(message),
                    vec![
                        ("met", Json::str("timeout")),
                        ("idle_ms", idle.map_or(Json::Null, millis)),
                        ("last_line", last.map_or(Json::Null, Json::str)),
                    ],
                )
            }
        };
        let mut fields = vec![("pane", Json::str(id.to_string()))];
        fields.extend(json);
        fields.push(("status", Json::Number(i64::from(status))));
        if let Some(screen) = &screen {
            fields.push(("screen", Json::str(screen.clone())));
        }
        let json = format!("{}\n", Json::Object(fields).render());
        let said = if said.is_empty() {
            said
        } else {
            format!("{prefix}{said}\n")
        };
        Outcome {
            status,
            stdout: format!("{said}{}", screen.unwrap_or_default()),
            stderr: problem.unwrap_or_default(),
            json,
            ..Outcome::default()
        }
    }
}

impl Wait {
    /// A settled pane's first line: its id and state, then what else is
    /// known: a shell command that ended, keys read that drew nothing, keys
    /// not read yet.
    fn state_line(
        &self,
        id: PaneId,
        pane: &Pane,
        state: crate::ready::State,
        since: Option<Since>,
    ) -> String {
        let mut line = format!("{id} {}", state.word());
        let commands = pane.screen().commands();
        if let Some(since) = since {
            if commands.finished > since.finished {
                match commands.last_status {
                    Some(status) => line.push_str(&format!(" · done {status}")),
                    None => line.push_str(" · done"),
                }
            }
            if self.after
                && pane.revision == since.revision
                && state == crate::ready::State::Reading
            {
                line.push_str(" · no change");
            }
        }
        let queued = pane.input_queued();
        if queued > 0 {
            line.push_str(&format!(" · {queued} bytes unread"));
        }
        line
    }

    /// Its conditions, as an error says what did not come.
    fn describe(&self) -> String {
        (self.until.iter())
            .map(Until::describe)
            .collect::<Vec<_>>()
            .join(" or ")
    }
}

/// A pane's last `n` lines with text: of its screen, or once a full-screen
/// program has left and the screen is blank, of what it last drew.
fn tail(pane: &Pane, n: usize) -> Vec<String> {
    let screen = pane.screen();
    let last = |rows: fux_vt::Rows<'_>| -> Vec<String> {
        let all = lines(rows, None);
        let kept: Vec<String> = (all.iter().rev())
            .map(|l| l.text.trim_end().to_owned())
            .filter(|l| !l.is_empty())
            .take(n)
            .collect();
        kept.into_iter().rev().collect()
    };
    let shown = last(screen.rows());
    if shown.is_empty() {
        last(screen.alternate_rows())
    } else {
        shown
    }
}

impl Until {
    /// The condition as an error says what did not come.
    fn describe(&self) -> String {
        match self {
            Until::Text(text) => format!("{text:?} to show"),
            Until::Gone(text) => format!("{text:?} to go"),
            Until::Exit => "the program to exit".into(),
            Until::Bell => "a bell".into(),
            Until::Settled => "the pane to settle".into(),
        }
    }
}

impl Found {
    /// What the wait prints, and its fields as JSON.
    fn said(&self) -> (String, Vec<(&'static str, Json<'static>)>) {
        match self {
            Found::Text { text, line } => (
                line.clone(),
                vec![
                    ("met", Json::str("text")),
                    ("text", Json::str(text.clone())),
                    ("line", Json::str(line.clone())),
                ],
            ),
            Found::Gone(text) => (
                String::new(),
                vec![
                    ("met", Json::str("gone")),
                    ("text", Json::str(text.clone())),
                ],
            ),
            Found::Exit(ended) => (crate::process::told(*ended), ended_json("exit", *ended)),
            Found::Settled(state) | Found::Stopped(state) => (
                state.word(),
                vec![
                    ("met", Json::str("settled")),
                    ("state", Json::str(state.word())),
                ],
            ),
            Found::Bell(notification) => (
                notification
                    .as_ref()
                    .map_or_else(|| "bell".to_owned(), |n| format!("notification: {n}")),
                vec![
                    ("met", Json::str("bell")),
                    (
                        "notification",
                        notification.clone().map_or(Json::Null, Json::str),
                    ),
                ],
            ),
        }
    }
}

/// An end as JSON: what it met, and its status or signal.
fn ended_json(met: &'static str, ended: Ended) -> Vec<(&'static str, Json<'static>)> {
    match ended {
        Ended::Exited(code) => vec![
            ("met", Json::str(met)),
            ("exit", Json::Number(i64::from(code))),
        ],
        Ended::Signalled(signal) => vec![
            ("met", Json::str(met)),
            ("signal", Json::Number(i64::from(signal))),
        ],
    }
}

impl Watch {
    /// Why a wait that timed out may never have been met: a text taken as
    /// the echo of what was typed, or one that was on the screen already
    /// and did not appear anew.
    fn hint(&self, pane: &Pane, wait: &Wait) -> Option<&'static str> {
        // Shown, on a line that reads as it did when the wait began.
        let texts = texts(wait);
        let shown = lines(pane.screen().rows().skip(pane.screen().history_len()), None);
        let unchanged = shown.iter().any(|l| {
            self.was.get(&l.first) == Some(&l.text)
                && texts.iter().any(|t| found_in(&l.text, t) > 0)
        });
        if wait.after && unchanged {
            return Some(
                "the text is on the screen, but on a line that reads as it did when the \
                 wait began (drawn again the same, it brings nothing new); `PANE opts str T \
                 wait-text wait` takes what is shown",
            );
        }
        self.echo_hint()
    }

    /// Why a text that showed went unseen: the line it was on held what was
    /// typed too.
    fn echo_hint(&self) -> Option<&'static str> {
        self.echo_skipped.then_some(
            "a line held the text along with what was typed, and was taken as its echo; \
             wait for text only the output has, or use `PANE opts str T wait-text wait`",
        )
    }

    /// What the pane shows of what the wait asks, if anything.
    fn look(
        &mut self,
        pane: &Pane,
        wait: &Wait,
        texts: &[String],
        (began, now): (Instant, Instant),
        fresh: bool,
    ) -> Option<Found> {
        let screen = pane.screen();
        // Only lines changed since it began (send-keys), or since the last
        // input to the pane (a wait of its own).
        let mark = (!self.all_new).then_some(self.mark);
        let shown = lines(screen.rows().skip(screen.history_len()), mark);
        for line in &shown {
            if self
                .was
                .get(&line.first)
                .is_some_and(|was| *was != line.text)
            {
                self.touched.insert(line.first);
            }
        }
        if !texts.is_empty() {
            self.read_history(screen, texts, wait);
        }
        // While a program holds the full screen, the main screen's lines
        // from where the wait began: what was printed just before it.
        let behind = if self.on_main && screen.mode(fux_vt::Mode::AlternateScreen) {
            let from = self
                .anchor
                .and_then(|a| screen.main_row_index(a))
                .unwrap_or(0);
            lines(screen.main_rows().skip(from), mark)
        } else {
            Vec::new()
        };
        for until in &wait.until {
            let found = match until {
                Until::Text(text) => {
                    let i = texts.iter().position(|t| t == text).unwrap_or(0);
                    let scrolled = self.in_history.get(i).cloned().flatten();
                    let mut skipped = false;
                    let (was, touched, cursor) = (&self.was, &self.touched, self.cursor);
                    // The newest line holding it, on the screen or behind it.
                    let line = (shown.iter().rev())
                        .chain(behind.iter().rev())
                        .filter(|l| is_new(l, was, touched) && found_in(&l.text, text) > 0)
                        .find(|l| {
                            let echo = echoed(l, wait, cursor, was);
                            skipped |= echo;
                            !echo
                        })
                        .map(|l| l.text.clone())
                        .or(scrolled);
                    self.echo_skipped |= skipped;
                    line.map(|line| Found::Text {
                        text: text.clone(),
                        line,
                    })
                }
                Until::Gone(text) => {
                    let present = shown.iter().any(|l| found_in(&l.text, text) > 0);
                    if present && !self.appeared.contains(text) {
                        self.appeared.push(text.clone());
                    }
                    let had = !fresh || self.appeared.contains(text);
                    (!present && had).then(|| Found::Gone(text.clone()))
                }
                Until::Exit => pane.process.exit_status().map(Found::Exit),
                Until::Settled => {
                    let state = crate::ready::state(pane, now);
                    state.settled().then_some(Found::Settled(state))
                }
                Until::Bell => (pane.alerts.count() > self.alerts).then(|| {
                    Found::Bell(
                        pane.alerts
                            .last
                            .clone()
                            .filter(|_| pane.alerts.notifications > 0),
                    )
                }),
            };
            if found.is_some() {
                return found;
            }
        }
        // A text that cannot come: the program reads its terminal, having
        // had a moment to print after what began the wait.
        let settles = wait.until.contains(&Until::Settled);
        let after_moment = now >= crate::after(began, crate::ready::SETTLE.saturating_mul(2));
        if !texts.is_empty() && !settles && after_moment && crate::ready::reading(pane, now) {
            return Some(Found::Stopped(crate::ready::State::Reading));
        }
        None
    }

    fn since(&self) -> Since {
        Since {
            cursor: self.cursor,
            finished: self.finished,
            revision: self.revision,
        }
    }

    /// Reads the history rows written since the wait began and not read
    /// yet, from the anchor, or the last row read, to the screen: for each
    /// text, the first line holding it, if none was found before.
    fn read_history(&mut self, screen: &fux_vt::Screen, texts: &[String], wait: &Wait) {
        let history = screen.history_len();
        let from = match (self.read, self.anchor) {
            (Some(read), _) => screen.row_index(read).map(|i| i.saturating_add(1)),
            (None, Some(anchor)) => screen.row_index(anchor),
            (None, None) => Some(history),
        };
        // An anchor gone past the history's limit: what is kept is read.
        let from = from.unwrap_or(0);
        if from >= history {
            return;
        }
        let rows: Vec<fux_vt::Row<'_>> = screen
            .rows()
            .skip(from)
            .take(history.saturating_sub(from))
            .collect();
        if let Some(last) = rows.last() {
            self.read = Some(last.id());
        }
        let mark = (!self.all_new).then_some(self.mark);
        let read = lines(rows.into_iter(), mark);
        let mut skipped = false;
        for (found, text) in self.in_history.iter_mut().zip(texts) {
            if found.is_none() {
                *found = (read.iter())
                    .filter(|l| is_new(l, &self.was, &self.touched) && found_in(&l.text, text) > 0)
                    .find(|l| {
                        let echo = echoed(l, wait, self.cursor, &self.was);
                        skipped |= echo;
                        !echo
                    })
                    .map(|l| l.text.clone());
            }
        }
        self.echo_skipped |= skipped;
    }
}

/// The texts a wait looks for, `--text` conditions', in order.
fn texts(wait: &Wait) -> Vec<String> {
    wait.until
        .iter()
        .filter_map(|u| match u {
            Until::Text(text) => Some(text.clone()),
            Until::Gone(_) | Until::Exit | Until::Bell | Until::Settled => None,
        })
        .collect()
}

/// A line of text, its soft-wrapped rows joined, and whether any of them
/// changed since the mark it was read against (always, read against none).
struct Line {
    text: String,
    new: bool,
    /// Its first row, which keeps its identity through edits and reflow.
    first: fux_vt::RowId,
}

/// Whether a line is new to a wait: a row of it changed since the wait
/// began, and it does not read as the line that began at its first row then
/// (a program redrawing its screen, or a reflow on a resize, brings nothing)
/// unless it read otherwise meanwhile (erased, then written the same again).
fn is_new(
    line: &Line,
    was: &std::collections::HashMap<fux_vt::RowId, String>,
    touched: &std::collections::HashSet<fux_vt::RowId>,
) -> bool {
    line.new && (was.get(&line.first) != Some(&line.text) || touched.contains(&line.first))
}

/// Rows as lines: soft-wrapped rows joined into the lines they wrap.
fn lines<'a>(rows: impl Iterator<Item = fux_vt::Row<'a>>, mark: Option<fux_vt::Mark>) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    let mut wraps = false;
    for row in rows {
        // A wrapped row's trailing blanks are part of its line.
        let text = if row.wrapped() {
            crate::capture::row_cells_text(row)
        } else {
            crate::capture::row_text(row)
        };
        let new = mark.is_none_or(|m| row.changed_since(m));
        match out.last_mut() {
            Some(last) if wraps => {
                last.text.push_str(&text);
                last.new |= new;
            }
            _ => out.push(Line {
                text,
                new,
                first: row.id(),
            }),
        }
        wraps = row.wrapped();
    }
    out
}

/// Whether a line is the echo of what was typed: only the line the cursor
/// was on as the keys went in, and only when they ended a line (Enter): a
/// shell or prompt shows the command there, and its output comes below. A
/// field edited in place (no Enter) shows its value, which counts.
fn echoed(
    line: &Line,
    wait: &Wait,
    cursor: Option<fux_vt::RowId>,
    was: &std::collections::HashMap<fux_vt::RowId, String>,
) -> bool {
    // While it still starts as it read before the keys (the prompt, the
    // keys' echo after it): output that cleared the row and wrote over it
    // (`clear; echo READY`) is not the echo.
    let before = was.get(&line.first).map_or("", |t| t.trim_end());
    wait.after && wait.lines > 0 && cursor == Some(line.first) && line.text.starts_with(before)
}

/// A text as a wait compares it: case ignored, runs of blanks as one. A
/// leading `^` holds it to a line's start, a trailing `$` to its end.
fn normal(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// How many times a line shows TEXT: case ignored, runs of blanks as one,
/// starting where a word does (`READY` is not in `NOT_READY`), `^` and `$`
/// holding it to the line's start and end.
fn found_in(line: &str, text: &str) -> usize {
    let (start, rest) = match text.strip_prefix('^') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (end, body) = match rest.strip_suffix('$') {
        Some(body) => (true, body),
        None => (false, rest),
    };
    let (line, body) = (normal(line), normal(body));
    if body.is_empty() {
        return 0;
    }
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let wordy = word(body.chars().next());
    line.match_indices(body.as_str())
        .filter(|(at, _)| {
            let before = line.get(..*at).and_then(|s| s.chars().next_back());
            let after_at = at.saturating_add(body.len());
            (!start || *at == 0) && (!end || after_at == line.len()) && !(wordy && word(before))
        })
        .count()
}

/// A pane's screen as a wait prints it: trimmed, soft-wrapped rows joined;
/// a full-screen program's last screen if it has left a blank one.
fn text(pane: &Pane, marks: bool) -> String {
    let view = CaptureView {
        join: true,
        marks,
        ..CaptureView::default()
    };
    let shown = crate::capture::pane(pane, None, view, false, false);
    // A full-screen program that has left its screen, or exited, leaves the
    // shell's, often blank: its last screen is what it said.
    if shown.trim().is_empty() {
        let alt = CaptureView { alt: true, ..view };
        return crate::capture::pane(pane, None, alt, false, false);
    }
    shown
}

/// What a pane shows from the row `cursor` was on to its last line with
/// text: what came since an action began. A full-screen program's whole
/// screen; all of it if that row is gone.
fn since_text(pane: &Pane, cursor: Option<fux_vt::RowId>, marks: bool) -> String {
    let screen = pane.screen();
    let from = cursor
        .filter(|_| !screen.mode(fux_vt::Mode::AlternateScreen))
        .and_then(|row| screen.row_index(row));
    let Some(from) = from else {
        return text(pane, marks);
    };
    let found = lines(screen.rows().skip(from), None);
    let texts: Vec<&str> = found.iter().map(|l| l.text.trim_end()).collect();
    let end = texts
        .iter()
        .rposition(|t| !t.is_empty())
        .map_or(0, |i| i.saturating_add(1));
    texts
        .get(..end)
        .unwrap_or_default()
        .iter()
        .map(|t| format!("{t}\n"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::found_in;

    /// A text waited for: case ignored, blanks as one, from a word's start,
    /// and `^`/`$` for a line's start and end.
    #[test]
    fn a_text_is_found_as_a_reader_would_find_it() {
        assert_eq!(found_in("Server READY on :8080", "ready"), 1);
        assert_eq!(found_in("NOT_READY slot=3", "READY"), 0);
        assert_eq!(found_in("NOT READY slot=3", "READY"), 1);
        assert_eq!(found_in("Plan  [fast/safe]>", "plan [fast"), 1);
        assert_eq!(found_in("[history] READY", "^READY"), 0);
        assert_eq!(found_in("READY now", "^ready"), 1);
        assert_eq!(found_in("Recovery key >", "key >$"), 1);
        assert_eq!(found_in("Recovery key > x", "key >$"), 0);
        assert_eq!(found_in("name> ", "name> "), 1);
        assert_eq!(found_in("total: 3", "$"), 0);
    }
}
