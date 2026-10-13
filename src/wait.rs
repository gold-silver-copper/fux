//! `fux wait`, and send-keys' waits: a command line from the CLI that answers
//! once one of its panes does what one of its conditions asks, its program
//! exits first, its pane goes, or its time is up. The server holds its
//! connection meanwhile and asks it again whenever a pane may have changed,
//! or its time comes (`Waiting::due`).
//!
//! Text is looked for in every line written since the wait's start (the
//! action it was armed before; else the last input to the pane, or its
//! program's start), history included, not only on the screen: a match
//! that a flood scrolls away at once is still found. History rows do not
//! change, so each is read once. A match counts only on a line that changed
//! since (it "appears anew"), and never within the echo of what was typed.
use crate::command::{AnyRef, CaptureView, Until, Wait};
use crate::id::PaneId;
use crate::json::Json;
use crate::pane::{InputFrom, Pane};
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

/// Said first of what came when lines of it were let go past history's
/// limit.
const CUT: &str = "… earlier lines not kept (past history-lines)\n";

/// How long a bare `wait` on a pane that may go on alone waits for
/// something new before it answers the pane as it is: longer than the
/// pauses of a program polling for keys.
const MORE: std::time::Duration = std::time::Duration::from_secs(5);

/// How often a wait that asks what a pane's program is blocked in looks
/// again: that changes with nothing on the screen to wake the server.
const PROBE: std::time::Duration = std::time::Duration::from_millis(30);

/// A wait under way.
pub struct Waiting {
    watches: Vec<Watch>,
    pub(crate) wait: Wait,
    /// The texts it looks for (`wait-text`), in order.
    texts: Vec<String>,
    /// When it began: a pane is quiet from then at the soonest.
    began: Instant,
    deadline: Instant,
    /// Printed before what the wait says: the pane `run` started.
    pub lead: String,
    /// The program has just started (`run`, `respawn`): its screen is
    /// blank, so a text is gone only once it has appeared.
    pub fresh: bool,
    /// A bare `wait`: on a pane that may go on alone (`waiting`, or quiet
    /// with nothing known) it waits for something new.
    pub more: bool,
    /// What the command it follows made, for its reply.
    pub made: Vec<crate::command::AnyRef<crate::id::WsId>>,
    /// What each pane said, for a wait on each of several (`Wait::all`).
    answers: Vec<Option<Outcome>>,
    /// A capture to give once the pane settles (`capture` from a command
    /// line): its history, view and whether as JSON.
    pub capture: Option<(Option<usize>, CaptureView, bool)>,
}

/// What a wait's answer tells of a pane against how it was as the wait
/// began: where what came since starts, the commands that had ended, the
/// screen's revision.
#[derive(Clone, Copy)]
struct Since {
    cursor: Option<fux_vt::RowId>,
    finished: u64,
    revision: u64,
    /// The screen as it began: a row changed since is what came.
    mark: fux_vt::Mark,
    /// Lines written since were let go before the wait read them.
    cut: bool,
}

/// One pane a wait watches, and what it found as it began.
struct Watch {
    pane: PaneId,
    /// Where what it counts begins: as the wait began, for one armed
    /// before an action; else the last input to the pane, or its program's
    /// start. What came since is said from its cursor's row.
    from: InputFrom,
    /// The last input to the pane, to tell its echo from what the program
    /// wrote: the action's, for a wait armed before one (`Waiting::typed`).
    typed: InputFrom,
    /// The row lines count from: the screen's top as a wait armed before an
    /// action began, else `from`'s cursor row.
    anchor: Option<fux_vt::RowId>,
    /// The screen's lines where it counts from (`from`'s), by first row: a
    /// line that reads as it did (a redraw, a reflow on resize) is not new.
    was: std::collections::HashMap<fux_vt::RowId, String>,
    /// Armed before an action: what came is said from the top row it
    /// changed.
    after: bool,
    /// Lines whose text has read otherwise than as the wait began, at
    /// some look since: one erased and written again the same is new.
    touched: std::collections::HashSet<fux_vt::RowId>,
    /// The last history row read, and the last row let go read
    /// (`Departed::seq`); the first line found among them holding a text,
    /// and which: they do not change.
    read: Option<fux_vt::RowId>,
    departed: Option<u64>,
    scrolled: Option<(String, String)>,
    /// The wait began on the main screen: while a program holds the full
    /// screen, what the main screen got since (from the anchor) counts
    /// too, so a line printed just before it took the screen is seen.
    on_main: bool,
    /// The `--gone` texts seen since the wait began, for a fresh program.
    appeared: Vec<String>,
    /// A line held the text but was taken as the echo of what was typed.
    echo_skipped: bool,
    /// Rows written since it began were let go past history's limit
    /// before it read them: a text may have come unseen.
    dropped: bool,
    revision: u64,
    /// The program's start (`Pane::starts`) the wait watches.
    starts: u64,
    /// Since when its program has been seen reading its terminal, probe
    /// after probe: a text it stops short of is said only once that holds.
    reading_since: Option<Instant>,
    /// Since when, and at which revision, its pane has looked settled.
    settled_since: Option<(Instant, u64)>,
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
    Stopped,
}

impl Waiting {
    pub fn new(panes: &[&Pane], wait: Wait, now: Instant) -> Waiting {
        let texts = (wait.until.iter())
            .filter_map(|u| match u {
                Until::Text(text) => Some(text.clone()),
                Until::Gone(_) | Until::Exit | Until::Bell | Until::Settled => None,
            })
            .collect();
        Waiting {
            answers: vec![None; panes.len()],
            watches: panes.iter().map(|p| Watch::new(p, wait.after)).collect(),
            texts,
            deadline: crate::after(now, wait.timeout),
            wait,
            began: now,
            lead: String::new(),
            made: Vec::new(),
            fresh: false,
            more: false,
            capture: None,
        }
    }

    /// Learns what the action it was armed before typed in each pane (for
    /// one that typed nothing, the last input), to tell its echo.
    pub fn typed(&mut self, session: &Session) {
        for watch in &mut self.watches {
            if let Some(pane) = session.panes.get(&watch.pane) {
                watch.typed = pane.input_from.clone();
            }
        }
    }

    /// The panes whose rows let go it reads (`fux_vt::Screen::keep_departed`):
    /// all it watches, if it waits for a text.
    pub fn tapped(&self) -> impl Iterator<Item = PaneId> + '_ {
        (self.watches.iter())
            .filter(|_| !self.texts.is_empty())
            .map(|w| w.pane)
    }

    /// When to ask it again at the latest, whatever happens before: soon,
    /// while it asks what a pane's program is blocked in (that changes with
    /// nothing on the screen to wake the server), else when its time is up.
    pub fn due(&self) -> Instant {
        let probe = (self.wait.until.iter())
            .any(|u| matches!(u, Until::Settled | Until::Text(_)))
            .then(|| crate::after(Instant::now(), PROBE));
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
        let named = self.lead.trim_end();
        let lead = if !named.is_empty() && outcome.stdout.starts_with(&format!("{named} ")) {
            ""
        } else {
            self.lead.as_str()
        };
        // What it made, then the pane it answers of: an answer of one pane
        // is that pane.
        let mut ids = self.made.clone();
        ids.extend(outcome.ids.iter().filter(|id| !self.made.contains(id)));
        Some(Outcome {
            stdout: format!("{lead}{}", outcome.stdout),
            ids,
            ..outcome
        })
    }

    /// Over several panes, a wait for any (`fux wait`): a pane that met it
    /// answers; one that stopped short of the text, or can no longer meet
    /// it (its program exited or was started again, or the pane went), only
    /// once none is left that could, a stopped one first. A wait on each
    /// (`Wait::all`): each answers on its own, as a lone pane's wait would,
    /// and the wait once every one has, a line from each, in order, with
    /// the first failing status. At the limit, any is said of a pane at
    /// work, each of every pane yet to answer.
    fn look(&mut self, session: &Session, now: Instant) -> Option<Outcome> {
        let several = self.watches.len() > 1;
        let (wait, texts) = (&self.wait, self.texts.as_slice());
        let (window, mode) = ((self.began, now), (self.fresh, self.more));
        let verdicts: Vec<Option<(Told, Since)>> = (self.watches.iter_mut())
            .zip(&self.answers)
            .map(|(watch, answer)| {
                let pane = session.panes.get(&watch.pane);
                answer
                    .is_none()
                    .then(|| watch.verdict(pane, wait, texts, window, mode))?
            })
            .collect();
        let tell = |id: PaneId, (told, since): (Told, Since)| {
            wait.told(id, session.panes.get(&id), told, several, since)
        };
        let late = now >= self.deadline;
        if wait.all && several {
            for ((watch, answer), verdict) in
                (self.watches.iter().zip(&mut self.answers)).zip(verdicts)
            {
                if let Some(verdict) = verdict {
                    *answer = Some(tell(watch.pane, verdict));
                } else if late && answer.is_none() {
                    *answer = Some(watch.timed_out(session, wait, texts, several));
                }
            }
            let answers: Vec<&Outcome> = self
                .answers
                .iter()
                .map(Option::as_ref)
                .collect::<Option<_>>()?;
            let mut stderr = String::new();
            for answer in answers.iter().filter(|a| !a.stderr.is_empty()) {
                stderr.push_str(answer.stderr.trim_end());
                stderr.push('\n');
            }
            return Some(Outcome {
                status: (answers.iter().map(|a| a.status))
                    .find(|s| *s != 0)
                    .unwrap_or(0),
                stdout: answers.iter().map(|a| a.stdout.as_str()).collect(),
                stderr,
                json: answers.iter().map(|a| a.json.as_str()).collect(),
                ids: answers.iter().flat_map(|a| a.ids.iter().copied()).collect(),
                ..Outcome::default()
            });
        }
        let stopped =
            |v: &Option<(Told, Since)>| matches!(v, Some((Told::Found(Found::Stopped), _)));
        let met = (verdicts.iter())
            .position(|v| matches!(v, Some((Told::Found(f), _)) if !matches!(f, Found::Stopped)));
        let ended = || {
            (verdicts.iter().all(Option::is_some))
                .then(|| verdicts.iter().position(stopped).unwrap_or(0))
        };
        if let Some(at) = met.or_else(ended) {
            let id = self.watches.get(at)?.pane;
            return Some(tell(id, verdicts.into_iter().nth(at).flatten()?));
        }
        if !late {
            return None;
        }
        // One at work if one is, else one still running: one reading its
        // terminal waits for keys.
        let running = |w: &&Watch, busy: bool| {
            (session.panes.get(&w.pane)).is_some_and(|p| {
                let reading = crate::ready::state(p, now) == crate::ready::State::Reading;
                p.process.exit_status().is_none() && !(busy && reading)
            })
        };
        let first = (self.watches.iter())
            .find(|w| running(w, true))
            .or_else(|| self.watches.iter().find(|w| running(w, false)))
            .or(self.watches.first())?;
        Some(first.timed_out(session, wait, texts, several))
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

impl Told {
    /// The status a wait that ended so ends with.
    fn status(&self) -> u8 {
        match self {
            // As timeout(1): the program's own status, once it exits.
            Told::Found(Found::Exit(ended)) => {
                u8::try_from(crate::process::shell_status(*ended)).unwrap_or(u8::MAX)
            }
            Told::Found(Found::Stopped) => STOPPED,
            Told::Found(
                Found::Text { .. } | Found::Gone(_) | Found::Bell(_) | Found::Settled(_),
            ) => 0,
            Told::Exited(..) | Told::Replaced => EXITED,
            Told::Gone => GONE,
            Told::TimedOut(_) => TIMED_OUT,
        }
    }
}

impl Wait {
    /// The outcome of a wait that ended so in pane `id`.
    fn told(
        &self,
        id: PaneId,
        pane: Option<&Pane>,
        told: Told,
        several: bool,
        since: Since,
    ) -> Outcome {
        let prefix = if several {
            format!("{id} ")
        } else {
            String::new()
        };
        // A settling action, or a wait the program stopped short of: its
        // state, then what came since it began.
        let settles = self.until.contains(&Until::Settled);
        let now = Instant::now();
        let state = match (&told, pane) {
            (Told::Found(Found::Settled(state)), _) => Some(*state),
            (Told::Found(Found::Stopped), _) => Some(crate::ready::State::Reading),
            // An exit waited for is a state too: said as one, with what
            // came before it.
            (Told::Found(Found::Exit(ended)), _) => Some(crate::ready::State::Exited(*ended)),
            // A text gone, or a settling wait's limit, leaves the pane as
            // it is: said with its state.
            (Told::Found(Found::Gone(_)), Some(pane)) => Some(crate::ready::state(pane, now)),
            (Told::TimedOut(_), Some(pane)) if settles => Some(crate::ready::state(pane, now)),
            (
                Told::Found(_) | Told::Exited(..) | Told::Replaced | Told::Gone | Told::TimedOut(_),
                _,
            ) => None,
        };
        // At its limit a settling wait fails (124) only while busy; `0
        // timeout` answers at once, as it is, and never fails.
        let calm = self.timeout.is_zero() || state.is_some_and(|s| s != crate::ready::State::Busy);
        let status = match settles && calm && matches!(told, Told::TimedOut(_)) {
            true => 0,
            false => told.status(),
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
                ids: vec![AnyRef::Pane(id)],
                ..Outcome::default()
            };
        }
        if let (Some(state), Some(pane)) = (state, pane) {
            let (line, notes) = self.state_line(id, pane, state, since);
            // A text that cannot come: the screen, to act on what is there.
            // Else what came, if asked or of an exit or a text gone.
            let came = self.print || matches!(told, Told::Found(Found::Exit(_) | Found::Gone(_)));
            let body = if matches!(told, Told::Found(Found::Stopped)) {
                let cut = if since.cut { CUT } else { "" };
                format!("{cut}{}", text(pane, self.marks, false))
            } else if came {
                since_text(pane, since, self.marks)
            } else {
                String::new()
            };
            // Said on stdout, with its status: still busy (124), stopped
            // short of the text (7). Not a failure: the answer is the screen.
            let mut fields = vec![
                ("pane", Json::str(id.to_string())),
                ("state", Json::str(state.word())),
            ];
            fields.extend(notes);
            fields.push(("status", Json::Number(i64::from(status))));
            fields.push(("output", Json::str(body.clone())));
            let json = Json::Object(fields);
            return Outcome {
                status,
                stdout: format!("{line}\n{body}"),
                json: format!("{}\n", json.render()),
                ids: vec![AnyRef::Pane(id)],
                ..Outcome::default()
            };
        }
        let screen = pane
            .filter(|_| self.print)
            .map(|p| text(p, self.marks, false));
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
            ids: vec![AnyRef::Pane(id)],
            ..Outcome::default()
        }
    }
}

impl Wait {
    /// A settled pane's first line: its id and state, then what else is
    /// known: a shell command that ended, keys read that drew nothing, keys
    /// not read yet; and those notes as JSON fields.
    fn state_line(
        &self,
        id: PaneId,
        pane: &Pane,
        state: crate::ready::State,
        since: Since,
    ) -> (String, Vec<(&'static str, Json<'static>)>) {
        let mut line = format!("{id} {}", state.word());
        let mut fields = Vec::new();
        let commands = pane.screen().commands();
        if commands.finished > since.finished {
            let status = commands.last_status;
            match status {
                Some(status) => line.push_str(&format!(" · done {status}")),
                None => line.push_str(" · done"),
            }
            let status = status.map_or(Json::Null, |s| Json::Number(i64::from(s)));
            fields.push(("done", status));
        }
        if self.after && pane.revision == since.revision && state == crate::ready::State::Reading {
            line.push_str(" · no change");
            fields.push(("no_change", Json::Bool(true)));
        }
        let queued = pane.input_queued();
        if queued > 0 {
            line.push_str(&format!(" · {queued} bytes unread"));
            fields.push((
                "unread",
                Json::Number(i64::try_from(queued).unwrap_or(i64::MAX)),
            ));
        }
        (line, fields)
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
        let settled = |state: crate::ready::State| {
            let fields = vec![
                ("met", Json::str("settled")),
                ("state", Json::str(state.word())),
            ];
            (state.word(), fields)
        };
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
            Found::Settled(state) => settled(*state),
            Found::Stopped => settled(crate::ready::State::Reading),
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
    /// How `pane` stands as a wait begins: one armed before an action
    /// (`after`) counts from now, any other from the last input to the
    /// pane, or its program's start.
    fn new(pane: &Pane, after: bool) -> Watch {
        let screen = pane.screen();
        let (from, anchor) = if after {
            let from = InputFrom::new(screen, &[], pane.alerts.count());
            (from, screen.window().row(0).map(|row| row.id()))
        } else {
            (pane.input_from.clone(), pane.input_from.cursor)
        };
        Watch {
            pane: pane.id,
            was: from.shown.clone(),
            after,
            from,
            typed: pane.input_from.clone(),
            anchor,
            touched: std::collections::HashSet::new(),
            read: None,
            departed: screen.departed().last().map(|d| d.seq),
            scrolled: None,
            on_main: !screen.mode(fux_vt::Mode::AlternateScreen),
            appeared: Vec::new(),
            echo_skipped: false,
            dropped: false,
            revision: pane.revision,
            starts: pane.starts,
            reading_since: None,
            settled_since: None,
        }
    }

    /// What the wait has come to in this pane, if anything: what it waited
    /// for, or why that can no longer come; and what came since it began.
    fn verdict(
        &mut self,
        pane: Option<&Pane>,
        wait: &Wait,
        texts: &[String],
        window: (Instant, Instant),
        mode: (bool, bool),
    ) -> Option<(Told, Since)> {
        let Some(pane) = pane else {
            return Some((Told::Gone, self.since(None)));
        };
        let told = if pane.starts != self.starts {
            Told::Replaced
        } else {
            match self.look(pane, wait, texts, window, mode) {
                Some(found) => Told::Found(found),
                None => Told::Exited(pane.process.exit_status()?, self.unseen()),
            }
        };
        Some((told, self.since(Some(pane))))
    }

    /// The wait's answer at its limit: what the pane is doing, and why
    /// the wait may never be met, if a reason shows.
    fn timed_out(
        &self,
        session: &Session,
        wait: &Wait,
        texts: &[String],
        several: bool,
    ) -> Outcome {
        let pane = session.panes.get(&self.pane);
        let hint = pane.and_then(|p| self.hint(p, wait, texts));
        wait.told(
            self.pane,
            pane,
            Told::TimedOut(hint),
            several,
            self.since(pane),
        )
    }

    /// Why a wait that timed out may never have been met: a text on a line
    /// that reads as it did when the wait began, or one that came unseen.
    fn hint(&self, pane: &Pane, wait: &Wait, texts: &[String]) -> Option<&'static str> {
        let screen = pane.screen();
        let unchanged = wait.after
            && lines(screen.rows().skip(screen.history_len()), None)
                .iter()
                .any(|l| {
                    self.was.get(&l.first) == Some(&l.text)
                        && texts.iter().any(|t| found_in(&l.text, t) > 0)
                });
        if unchanged {
            return Some(
                "the text is on the screen, but on a line that reads as it did when the \
                 wait began (drawn again the same, it brings nothing new); read it with \
                 `PANE opts capture`",
            );
        }
        self.unseen()
    }

    /// Why a text may have come unseen: lines were let go past history's
    /// limit before the wait read them, or the line it was on held what was
    /// typed too.
    fn unseen(&self) -> Option<&'static str> {
        if self.dropped {
            return Some(
                "lines written since the input were let go past history-lines before the \
                 wait read them: the text may have come unseen",
            );
        }
        self.echo_skipped.then_some(
            "the text was on the line typed, within what was typed, and was taken as its \
             echo, as a later wait takes it too; wait for text only the output has, or \
             read the screen with `PANE opts capture`",
        )
    }

    /// What the pane shows of what the wait asks, if anything.
    fn look(
        &mut self,
        pane: &Pane,
        wait: &Wait,
        texts: &[String],
        (began, now): (Instant, Instant),
        (fresh, more): (bool, bool),
    ) -> Option<Found> {
        let screen = pane.screen();
        // Only lines changed since it began.
        let mark = Some(self.from.mark);
        let shown = lines(screen.rows().skip(screen.history_len()), mark);
        for line in &shown {
            if (self.was.get(&line.first)).is_some_and(|was| *was != line.text) {
                self.touched.insert(line.first);
            }
        }
        if !texts.is_empty() {
            self.read_history(screen, texts);
        }
        // While a program holds the full screen, the main screen's lines
        // from where the wait began: what was printed just before it.
        let behind = if self.on_main && screen.mode(fux_vt::Mode::AlternateScreen) {
            let from = (self.anchor)
                .and_then(|a| screen.main_row_index(a))
                .unwrap_or(0);
            lines(screen.main_rows().skip(from), mark)
        } else {
            Vec::new()
        };
        // The texts are looked for together, at the first of them.
        let mut texts_looked = false;
        for until in &wait.until {
            let found = match until {
                Until::Text(_) if texts_looked => None,
                Until::Text(_) => {
                    texts_looked = true;
                    // Of the lines scrolled away, the first; else the first
                    // behind the screen or on it, where it came.
                    let scrolled = self.scrolled.clone();
                    scrolled
                        .or_else(|| {
                            (behind.iter().chain(&shown))
                                .filter(|l| is_new(l, &self.was, &self.touched))
                                .find_map(|l| {
                                    holds(screen, &self.typed, l, texts, &mut self.echo_skipped)
                                })
                        })
                        .map(|(text, line)| Found::Text { text, line })
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
                    // Settled on two looks a probe apart, the screen as it
                    // was: output still in the terminal's buffers, which
                    // the kernel has yet to hand fux, is in by then.
                    // A bare `wait` on a pane that may go on alone
                    // (`waiting`, or quiet with nothing known) waits for
                    // something new, what it waits on or a change, for
                    // `MORE` at most.
                    let maybe = matches!(
                        state,
                        crate::ready::State::Waiting | crate::ready::State::Settled
                    );
                    let more = more && now < crate::after(began, MORE);
                    if !state.settled() || (more && maybe && pane.revision == self.revision) {
                        self.settled_since = None;
                        None
                    } else if let crate::ready::State::Exited(_) = state {
                        Some(Found::Settled(state))
                    } else {
                        let since = (self.settled_since)
                            .filter(|since| since.1 == pane.revision)
                            .unwrap_or((now, pane.revision));
                        self.settled_since = Some(since);
                        (now >= crate::after(since.0, PROBE)).then_some(Found::Settled(state))
                    }
                }
                Until::Bell => (pane.alerts.count() > self.from.alerts).then(|| {
                    let notification = pane.alerts.last.clone();
                    Found::Bell(notification.filter(|_| pane.alerts.notifications > 0))
                }),
            };
            if found.is_some() {
                return found;
            }
        }
        // A text that cannot come: the program reads its terminal, having
        // had a moment to print after what began the wait, seen so on more
        // than one probe, never on one look alone.
        if texts.is_empty() || wait.until.contains(&Until::Settled) {
            return None;
        }
        if crate::ready::state(pane, now) != crate::ready::State::Reading {
            self.reading_since = None;
            return None;
        }
        let since = *self.reading_since.get_or_insert(now);
        let moment = crate::ready::SETTLE.saturating_mul(2);
        let held = now >= crate::after(began, moment) && now >= crate::after(since, moment);
        held.then_some(Found::Stopped)
    }

    fn since(&self, pane: Option<&Pane>) -> Since {
        Since {
            cursor: pane.and_then(|p| self.top_changed(p)).or(self.from.cursor),
            finished: self.from.finished,
            revision: self.revision,
            mark: self.from.mark,
            cut: self.dropped,
        }
    }

    /// The topmost row the keys changed, from the screen's top (or the
    /// cursor's row, scrolled into history) on: a menu or form redrawn in
    /// place on the main screen, above the cursor's row or below it. What
    /// came is said from there.
    fn top_changed(&self, pane: &Pane) -> Option<fux_vt::RowId> {
        if !self.after {
            return None;
        }
        let screen = pane.screen();
        let at = self.from.cursor.and_then(|row| screen.row_index(row))?;
        lines(screen.rows().skip(at.min(screen.history_len())), None)
            .into_iter()
            .find(|l| self.was.get(&l.first).map(String::as_str) != Some(l.text.as_str()))
            .map(|l| l.first)
    }

    /// Reads the rows let go since the last look (scrolled off the
    /// alternate screen or a region, or out of history past its limit),
    /// then the history rows written since the wait began and not read
    /// yet, from the anchor, or the last row read, to the screen: the
    /// first line holding a text, if none was found before.
    fn read_history(&mut self, screen: &fux_vt::Screen, texts: &[String]) {
        let mark = Some(self.from.mark);
        let seen = self.departed;
        let departed = (screen.departed().iter()).filter(|d| seen.is_none_or(|s| d.seq > s));
        let mut read = departed_lines(departed, mark);
        if let Some(last) = screen.departed().last() {
            self.departed = Some(last.seq);
        }
        let history = screen.history_len();
        let from = match (self.read, self.anchor) {
            (Some(read), _) => screen.row_index(read).map(|i| i.saturating_add(1)),
            (None, Some(anchor)) => screen.row_index(anchor),
            (None, None) => Some(history),
        };
        // An anchor gone past the history's limit before the first read:
        // what is kept is read, and the rest may have held a text.
        self.dropped |= self.read.is_none() && self.anchor.is_some() && from.is_none();
        let from = from.unwrap_or(0);
        if from < history {
            let rows: Vec<fux_vt::Row<'_>> = screen
                .rows()
                .skip(from)
                .take(history.saturating_sub(from))
                .collect();
            if let Some(last) = rows.last() {
                self.read = Some(last.id());
            }
            read.extend(lines(rows.into_iter(), mark));
        }
        if self.scrolled.is_none() {
            self.scrolled = (read.iter())
                .filter(|l| is_new(l, &self.was, &self.touched))
                .find_map(|l| holds(screen, &self.typed, l, texts, &mut self.echo_skipped));
        }
    }
}

/// The first of `texts` that `line` holds outside the echo of what was
/// `typed`, and the line; `skipped` if one was in the echo.
fn holds(
    screen: &fux_vt::Screen,
    typed: &InputFrom,
    line: &Line,
    texts: &[String],
    skipped: &mut bool,
) -> Option<(String, String)> {
    for text in texts.iter().filter(|t| found_in(&line.text, t) > 0) {
        if !echoed(screen, typed, line, text) {
            return Some((text.clone(), line.text.clone()));
        }
        *skipped = true;
    }
    None
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

/// The screen's lines as they read now, by first row.
pub(crate) fn shown(screen: &fux_vt::Screen) -> std::collections::HashMap<fux_vt::RowId, String> {
    let shown = lines(screen.rows().skip(screen.history_len()), None);
    shown.into_iter().map(|l| (l.first, l.text)).collect()
}

/// Rows as lines: soft-wrapped rows joined into the lines they wrap.
fn lines<'a>(rows: impl Iterator<Item = fux_vt::Row<'a>>, mark: Option<fux_vt::Mark>) -> Vec<Line> {
    joined(rows.map(|row| {
        let new = mark.is_none_or(|m| row.changed_since(m));
        (row.id(), row.wrapped(), row.cells_text(), new)
    }))
}

/// Rows let go as lines, as `lines` reads rows still kept.
fn departed_lines<'a>(
    rows: impl Iterator<Item = &'a fux_vt::Departed>,
    mark: Option<fux_vt::Mark>,
) -> Vec<Line> {
    joined(rows.map(|row| {
        let new = mark.is_none_or(|m| row.changed_since(m));
        (row.id, row.wrapped, row.text.clone(), new)
    }))
}

/// Rows, each its first row's id, whether it wraps, its text and whether
/// it is new, joined into the lines they wrap.
fn joined(rows: impl Iterator<Item = (fux_vt::RowId, bool, String, bool)>) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    let mut wraps = false;
    for (first, wrapped, text, new) in rows {
        // A wrapped row's trailing blanks are part of its line.
        let text = if wrapped {
            text
        } else {
            text.trim_end_matches(' ').to_owned()
        };
        match out.last_mut() {
            Some(last) if wraps => {
                last.text.push_str(&text);
                last.new |= new;
            }
            _ => out.push(Line { text, new, first }),
        }
        wraps = wrapped;
    }
    out
}

/// Whether `text` on `line` is in the echo of what was typed: on the row
/// the cursor was on as the keys went in, the prompt and the first line
/// typed; below it, each line typed after, on the first line from there on
/// that shows it, in order. What the program writes past the echo, or in
/// its place, is output.
fn echoed(
    screen: &fux_vt::Screen,
    typed: &crate::pane::InputFrom,
    line: &Line,
    text: &str,
) -> bool {
    let Some(cursor) = typed.cursor else {
        return false;
    };
    let before = typed.before.trim_end();
    let mut keys = typed.echo.iter().map(|t| t.trim());
    let within = |end: Option<usize>| {
        end.is_some_and(|end| found_in(line.text.get(..end).unwrap_or(&line.text), text) > 0)
    };
    let first = keys.next();
    if line.first == cursor {
        // Keys alone (Up, Enter: a command recalled): only a row that held
        // a prompt, and starts with it still, shows one; an empty row, or
        // one written over, takes the program's output.
        return match first {
            Some(first) => within(echo_end(&line.text, before, first, true)),
            None => typed.lines > 0 && !before.is_empty() && line.text.starts_with(before),
        };
    }
    let Some(from) = screen.row_index(cursor) else {
        return false;
    };
    let mut next = keys.next();
    for shown in lines(screen.rows().skip(from), None).iter().skip(1) {
        let Some(keys_now) = next else {
            return false;
        };
        let end = echo_end(&shown.text, before, keys_now, false);
        if shown.first == line.first {
            return within(end);
        }
        if end.is_some() {
            next = keys.next();
        }
    }
    false
}

/// Where the echo of a line typed, `keys`, ends on a line of `text`: after
/// the prompt (`before`: the row the keys went in on must still start with
/// it, as output that cleared the row and wrote over it does not; a line
/// below may), the blanks after it, and as many characters as were typed.
/// Only while the line shows them, their first word or its start (`2` of
/// `2**10`, still being drawn), or on the keys' own row nothing yet: a
/// program that echoes nothing, or writes over the echo, puts its own
/// output there.
fn echo_end(text: &str, before: &str, keys: &str, own_row: bool) -> Option<usize> {
    let rest = match text.strip_prefix(before) {
        Some(rest) => rest,
        None if own_row => return None,
        None => text,
    };
    let shown = rest.trim_start();
    let word = keys.split_whitespace().next().unwrap_or_default();
    let shows = match shown.split_whitespace().next() {
        Some(first) => word.starts_with(first) || first.starts_with(word),
        None => own_row,
    };
    let end = (shown.char_indices().nth(keys.chars().count())).map_or(shown.len(), |(at, _)| at);
    shows.then(|| text.len().saturating_sub(shown.len()).saturating_add(end))
}

/// A text as a wait compares it: case ignored, runs of blanks as one, and
/// none at its ends. A leading `^` holds it to a line's first word, a
/// trailing `$` to its last.
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
            // A control key's echo just before (`^C`, a program's output
            // after Ctrl-C) is no text of the line's: the text starts there.
            let mut before = line.get(..*at).unwrap_or_default();
            while let Some(rest) = control_echo(before) {
                before = rest;
            }
            let after_at = at.saturating_add(body.len());
            (!start || before.is_empty())
                && (!end || after_at == line.len())
                && !(wordy && word(before.chars().next_back()))
        })
        .count()
}

/// `text` without the control key's echo it ends with (`^C`, lowercased
/// as a wait compares: `^c`), if it ends with one.
fn control_echo(text: &str) -> Option<&str> {
    let mut chars = text.char_indices().rev();
    let (_, key) = chars.next()?;
    let (caret, _) = chars.next().filter(|(_, c)| *c == '^')?;
    let control = key.is_ascii_lowercase() || "@[\\]^_?".contains(key);
    control.then(|| text.get(..caret)).flatten()
}

/// A pane's screen as a wait prints it: trimmed, soft-wrapped rows joined;
/// once blank (a full-screen program that left its screen, or exited, often
/// leaves the shell's so), the screen the program last drew. `alt`: that
/// last full screen first.
fn text(pane: &Pane, marks: bool, alt: bool) -> String {
    let view = |alt| CaptureView {
        join: true,
        marks,
        alt,
        ..CaptureView::default()
    };
    let first = crate::capture::pane(pane, None, view(alt), false, false);
    if first.trim().is_empty() {
        return crate::capture::pane(pane, None, view(!alt), false, false);
    }
    first
}

/// What a pane shows from the row `cursor` was on to its last line with
/// text: what came since an action began. A full-screen program's whole
/// screen.
fn since_text(pane: &Pane, since: Since, marks: bool) -> String {
    let screen = pane.screen();
    let on_full = screen.mode(fux_vt::Mode::AlternateScreen);
    let Some(cursor) = since.cursor.filter(|_| !on_full) else {
        return text(pane, marks, false);
    };
    let changed = since.revision != pane.revision;
    let drew_full = || screen.alternate_rows().any(|r| r.changed_since(since.mark));
    let full = || text(pane, marks, true);
    let mut cut = false;
    let from = match screen.row_index(cursor) {
        Some(from) => from,
        // The keys went in on a full screen the program has since left:
        // what it printed after, else that last full screen (an editor
        // that saved and quit).
        None if screen.alternate_rows().any(|r| r.id() == cursor) => {
            match screen.rows().position(|r| r.changed_since(since.mark)) {
                Some(from) => from,
                None if changed => return full(),
                None => return text(pane, marks, false),
            }
        }
        // Its row let go past history's limit: what is kept, said so.
        None => {
            cut = true;
            0
        }
    };
    let found = lines(screen.rows().skip(from), None);
    let texts: Vec<&str> = found.iter().map(|l| l.text.trim_end()).collect();
    let Some(end) = texts
        .iter()
        .rposition(|t| !t.is_empty())
        .map(|i| i.saturating_add(1))
    else {
        // Nothing from that row on: the program drew on the full screen
        // and left it, or cleared the screen and drew above; or it only
        // moved the cursor, and nothing came.
        let history = screen.history_len();
        let above = (screen.rows().skip(history))
            .take(from.saturating_sub(history))
            .any(|r| r.changed_since(since.mark));
        return match (changed && drew_full(), changed && above) {
            (true, _) => full(),
            (false, true) => text(pane, marks, false),
            (false, false) => String::new(),
        };
    };
    let lead = if cut { CUT } else { "" };
    let body: String = (texts.get(..end).unwrap_or_default().iter())
        .map(|t| format!("{t}\n"))
        .collect();
    format!("{lead}{body}")
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
        assert_eq!(found_in("^CINTERRUPT_IGNORED", "interrupt_ignored"), 1);
        assert_eq!(found_in("^C^CREADY", "^ready"), 1);
        assert_eq!(found_in("x^CREADY", "^ready"), 0);
        // Blanks as one, leading ones too: `^` holds a text to a line's
        // first word, indented or not.
        assert_eq!(found_in("    READY", "^READY"), 1);
        assert_eq!(found_in("READY", " READY"), 1);
    }
}
