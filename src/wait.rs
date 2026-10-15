//! `fux wait`, and send-keys' waits: a command line from the CLI that answers
//! once each of its panes has: done what one of its conditions asks, its
//! program exited first, its pane gone, or its time is up. The server holds its
//! connection meanwhile and asks it again whenever a pane may have changed,
//! or its time comes (`Waiting::due`).
//!
//! A wait counts from a number in its pane's log (`crate::log`): what came
//! after it is new to it. That is the action it was armed before, else the
//! last action's input to the pane, or its program's start. Text is looked
//! for in what was printed since, piece by piece as the pane recorded it
//! (`Pane::prints`), on the line each piece went on wherever that line is
//! now (on the screen, in history, or let go), and in the piece's own text,
//! though drawn over since. A match counts only if it holds text the
//! program printed since, never only the echo of what was typed
//! (`crate::classify`).
use crate::command::{AnyRef, CaptureView, Until, Wait};
use crate::id::PaneId;
use crate::json::Json;
use crate::log::Seq;
use crate::pane::Pane;
use crate::process::Ended;
use crate::session::{Outcome, Session};
use std::time::Instant;

/// The exit statuses a wait ends with, besides 0 (what it waited for came)
/// and the 1 and 2 of any command.
pub const EXITED: u8 = 3;
pub const GONE: u8 = 4;
/// The program reads its terminal, and the text waited for has not come
/// (or gone): fux judges it cannot before the agent types.
pub const STOPPED: u8 = 7;
/// As `timeout(1)`.
pub const TIMED_OUT: u8 = 124;

/// Said first of what came when lines of it were let go past history's
/// limit.
const CUT: &str = "… earlier lines not kept (past history-lines)\n";
/// Said first instead when more came than fux keeps for a wait.
const MORE_THAN_KEPT: &str =
    "… more came than fux keeps for a wait (1 MiB): text may have passed unseen\n";

/// How long a bare `wait` on a pane that may go on alone waits for
/// something new before it answers the pane as it is: longer than the
/// pauses of a program polling for keys.
const MORE: std::time::Duration = std::time::Duration::from_secs(5);

/// How often a wait that asks what a pane's program is blocked in looks
/// again: that changes with nothing on the screen to wake the server.
const PROBE: std::time::Duration = std::time::Duration::from_millis(30);
/// How long a pane must look settled, the screen as it was, to be taken as
/// settled: output its program wrote just before is in fux's hands by then
/// (the kernel hands it on within a millisecond or two).
const HOLD: std::time::Duration = std::time::Duration::from_millis(10);

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
    /// What each pane said, once it has.
    answers: Vec<Option<Outcome>>,
    /// A capture to give once the pane settles (`capture` from a command
    /// line): its history, view and whether as JSON.
    pub capture: Option<(Option<usize>, CaptureView, bool)>,
}

/// What a wait's answer tells of a pane against how it was as the wait
/// began: where it counts from, the commands that had ended, the screen's
/// revision.
#[derive(Clone, Copy)]
struct Since {
    from: Seq,
    /// The full screen was in use then.
    on_full: bool,
    finished: u64,
    revision: u64,
    /// What came since was more than the pane keeps, and which notice
    /// says so: some was let go before the wait read it.
    cut: Option<&'static str>,
}

/// One pane a wait watches, and what it found as it began.
struct Watch {
    pane: PaneId,
    /// What came after this is new to it.
    from: Seq,
    on_full: bool,
    /// The action the pane's prints are kept since, and how many of them
    /// it has read: a new action starts them again.
    since: Seq,
    read: usize,
    /// The screen's rows as they read when it began, if it began after the
    /// last action (else the pane's, `Pane::before`).
    before: Option<std::collections::HashMap<fux_vt::RowId, String>>,
    /// The first text found: which, its line, and when it came.
    found: Option<(String, String, Seq)>,
    /// The text printed on each row, runs that go on from each other
    /// joined, though printed in several pieces and between others: where
    /// it starts and ends, and the text.
    stream: std::collections::HashMap<fux_vt::RowId, (u16, u16, String)>,
    /// The `wait-gone` texts seen since the wait began, for a fresh program.
    appeared: Vec<String>,
    /// A text was found in the echo of what was typed alone.
    echo_skipped: bool,
    finished: u64,
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
    /// The program reads its terminal; the text waited for has not come
    /// (or gone).
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

    /// When to ask it again at the latest, whatever happens before: soon,
    /// while it asks what a pane's program is blocked in (that changes with
    /// nothing on the screen to wake the server), else when its time is up.
    pub fn due(&self) -> Instant {
        let now = Instant::now();
        let probe = (self.wait.until.iter())
            .any(|u| matches!(u, Until::Settled | Until::Text(_) | Until::Gone(_)))
            .then(|| crate::after(now, PROBE));
        // A pane seen settled is looked at again once its hold is up.
        let held = (self.watches.iter())
            .filter_map(|w| w.settled_since)
            .map(|(since, _)| crate::after(since, HOLD).max(now))
            .min();
        [probe, held]
            .into_iter()
            .flatten()
            .fold(self.deadline, Instant::min)
    }

    /// Its answer, if it has one at `now`: in each of its panes, what it
    /// waited for came, its program exited first, it went, or its time is
    /// up.
    pub fn answer(&mut self, session: &Session, now: Instant) -> Option<Outcome> {
        let mut outcome = self.look(session, now)?;
        // A capture asked to settle first: its state line, then the capture.
        if let (Some((history, view, json)), Some(watch)) = (self.capture, self.watches.first())
            && let Some(pane) = session.panes.get(&watch.pane)
        {
            let copying = session.copying(pane.id);
            let read = crate::capture::pane(pane, history, view, json, copying);
            outcome.stdout = if json {
                read
            } else {
                // Its JSON, read at the same moment, for `json` to give.
                outcome.json = crate::capture::pane(pane, history, view, true, copying);
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

    /// Each pane answers on its own, as a lone pane's wait would, and the
    /// wait once every one has: a line from each, in order, with the first
    /// failing status. At the limit, each pane yet to answer is said as it
    /// is.
    fn look(&mut self, session: &Session, now: Instant) -> Option<Outcome> {
        let several = self.watches.len() > 1;
        let (wait, texts) = (&self.wait, self.texts.as_slice());
        let (window, mode) = ((self.began, now), (self.fresh, self.more));
        let late = now >= self.deadline;
        for (watch, answer) in self.watches.iter_mut().zip(&mut self.answers) {
            if answer.is_some() {
                continue;
            }
            let pane = session.panes.get(&watch.pane);
            if let Some((told, since)) = watch.verdict(pane, wait, texts, window, mode) {
                *answer = Some(wait.told(watch.pane, pane, told, several, since));
            } else if late {
                *answer = Some(watch.timed_out(session, wait, texts, several));
            }
        }
        let answers: Vec<&Outcome> = self
            .answers
            .iter()
            .map(Option::as_ref)
            .collect::<Option<_>>()?;
        if let [one] = answers.as_slice() {
            return Some((*one).clone());
        }
        let mut stderr = String::new();
        for answer in answers.iter().filter(|a| !a.stderr.is_empty()) {
            stderr.push_str(answer.stderr.trim_end());
            stderr.push('\n');
        }
        Some(Outcome {
            status: (answers.iter().map(|a| a.status))
                .find(|s| *s != 0)
                .unwrap_or(0),
            stdout: answers.iter().map(|a| a.stdout.as_str()).collect(),
            stderr,
            json: answers.iter().map(|a| a.json.as_str()).collect(),
            ids: answers.iter().flat_map(|a| a.ids.iter().copied()).collect(),
            ..Outcome::default()
        })
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
                let cut = since.cut.unwrap_or_default();
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
                // What it does: reading its terminal, waiting, busy…
                if let (Some(idle), Some(pane)) = (idle, pane) {
                    message.push_str(&format!(
                        "; {id} {}, the screen unchanged for {}s",
                        crate::ready::state(pane, Instant::now()).word(),
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
        let all = lines(rows);
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
    let (key, n) = match ended {
        Ended::Exited(code) => ("exit", code),
        Ended::Signalled(signal) => ("signal", signal),
    };
    vec![("met", Json::str(met)), (key, Json::Number(i64::from(n)))]
}

impl Watch {
    /// How `pane` stands as a wait begins: one armed before an action
    /// (`after`) counts from now, any other from the last action's input to
    /// the pane, or its program's start.
    fn new(pane: &Pane, after: bool) -> Watch {
        let screen = pane.screen();
        let (from, (finished, on_full)) = match after {
            true => (
                Seq(screen.version()),
                (
                    screen.commands().finished,
                    screen.mode(fux_vt::Mode::AlternateScreen),
                ),
            ),
            false => (pane.since, pane.since_was),
        };
        Watch {
            pane: pane.id,
            from,
            on_full,
            since: pane.since,
            read: if after { pane.prints.len() } else { 0 },
            before: after.then(|| crate::pane::rows_now(screen)),
            found: None,
            stream: std::collections::HashMap::new(),
            appeared: Vec::new(),
            echo_skipped: false,
            finished,
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
        // An action's wait answers once all its keys went in: what the
        // first brought is no answer to the rest.
        if wait.after && pane.starts == self.starts && pane.holds_keys() {
            return None;
        }
        let told = if pane.starts != self.starts {
            Told::Replaced
        } else {
            match self.look(pane, wait, texts, window, mode) {
                Some(found) => Told::Found(found),
                None => Told::Exited(pane.process.exit_status()?, self.unseen(Some(pane))),
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
        let hint = pane.and_then(|p| self.hint(p, texts));
        wait.told(
            self.pane,
            pane,
            Told::TimedOut(hint),
            several,
            self.since(pane),
        )
    }

    /// Why a wait that timed out may never have been met: a text on the
    /// screen that nothing printed since the wait began, or one that came
    /// unseen.
    fn hint(&self, pane: &Pane, texts: &[String]) -> Option<&'static str> {
        if let Some(why) = self.unseen(Some(pane)) {
            return Some(why);
        }
        let screen = pane.screen();
        let shown = lines(screen.rows().skip(screen_top(screen)));
        let there = shown
            .iter()
            .any(|l| texts.iter().any(|t| found_in(&l.text, t) > 0));
        there.then_some(
            "the text is on the screen, but on a line that reads as it did when the wait \
             began (nothing printed it since; drawn again the same, it brings nothing new); \
             read it with `PANE opts capture`",
        )
    }

    /// Why a text may have come unseen: more came than the pane keeps, or
    /// the text was in the echo of what was typed alone.
    fn unseen(&self, pane: Option<&Pane>) -> Option<&'static str> {
        if pane.is_some_and(|p| p.prints_lost) {
            return Some(
                "more was written since the input than fux keeps for a wait: the text \
                 may have come unseen",
            );
        }
        self.echo_skipped.then_some(
            "the text was only in the echo of what was typed, which a wait never takes; \
             wait for text only the output has, or read the screen with `PANE opts capture`",
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
        if !texts.is_empty() && self.found.is_none() {
            self.read_prints(pane, texts);
        }
        let screen = pane.screen();
        let mut gone = None;
        let mut exit = None;
        let mut bell = None;
        let mut settled = None;
        for until in &wait.until {
            match until {
                Until::Text(_) => {}
                Until::Gone(text) => {
                    let shown = lines(screen.rows().skip(screen_top(screen)));
                    let present = shown.iter().any(|l| found_in(&l.text, text) > 0);
                    // Shown between two looks (drawn over, erased, scrolled
                    // away): printed since, though gone now.
                    // It went no earlier than the last piece that printed it.
                    let last = (pane.prints.iter().filter(|p| p.seq.0 > self.from))
                        .filter(|p| p.prints.iter().any(|r| found_in(r.text, text) > 0))
                        .map(|p| p.seq.0)
                        .next_back();
                    if (present || last.is_some()) && !self.appeared.contains(text) {
                        self.appeared.push(text.clone());
                    }
                    let had = !fresh || self.appeared.contains(text);
                    if !present && had && gone.is_none() {
                        gone = Some((Found::Gone(text.clone()), last.unwrap_or(self.from)));
                    }
                }
                Until::Exit => exit = pane.process.exit_status().map(Found::Exit),
                Until::Settled => settled = self.settled(pane, (began, now), more),
                Until::Bell => bell = pane.record.bell_after(self.from),
            }
        }
        // Of a text, a bell and a text gone, the first to come; in one
        // piece, a text gone before a text came (the piece that erased it
        // printed the new one), and a text before the bell that ends it.
        let text = (self.found.clone()).map(|(text, line, at)| (Found::Text { text, line }, at));
        let bell = bell.map(|at| {
            let notification = pane.alerts.last.clone();
            (
                Found::Bell(notification.filter(|_| pane.alerts.last_notified)),
                at,
            )
        });
        let first = [gone, text, bell]
            .into_iter()
            .flatten()
            .min_by_key(|(_, at)| *at);
        let found = first.map(|(found, _)| found).or(exit).or(settled);
        if found.is_some() {
            return found;
        }
        // A text that cannot come, or go: the program reads its terminal,
        // having had a moment to print after what began the wait, seen so
        // on more than one probe, never on one look alone.
        let shown = (wait.until.iter()).any(|u| matches!(u, Until::Text(_) | Until::Gone(_)));
        if !shown || wait.until.contains(&Until::Settled) {
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

    /// Reads what was printed since it last read, in the order it came:
    /// the first of `texts` that a piece of output printed, on its line or
    /// in the piece itself, is found.
    fn read_prints(&mut self, pane: &Pane, texts: &[String]) {
        if pane.since != self.since {
            self.since = pane.since;
            self.read = 0;
        }
        let prints = pane.prints.get(self.read..).unwrap_or_default();
        self.read = pane.prints.len();
        let before = self.before.as_ref().unwrap_or(&pane.before);
        let reach = texts.iter().map(|t| t.chars().count()).max().unwrap_or(0);
        // The characters of the texts waited for, case and blanks aside.
        let wanted: std::collections::HashSet<char> = (texts.iter())
            .flat_map(|t| t.trim_start_matches('^').trim_end_matches('$').chars())
            .flat_map(|c| match c.is_whitespace() {
                true => vec![' '],
                false => c.to_lowercase().collect(),
            })
            .collect();
        let pieces = prints.iter().filter(|p| p.seq.0 > self.from);
        let runs = pieces.flat_map(|p| {
            (p.prints.iter().zip(&p.echo).zip(&p.again)).map(move |((r, e), a)| (p.seq, r, e, *a))
        });
        for (seq, print, echo, again) in runs.filter(|(_, r, _, _)| !r.text.is_empty()) {
            // Printed again where it was, just before or as the wait began:
            // nothing new.
            let was = |rows: &std::collections::HashMap<_, String>| {
                rows.get(&print.row)
                    .is_some_and(|row| crate::pane::holds_at(row, &print))
            };
            if again || was(before) {
                continue;
            }
            // The runs printed one after another on the row, joined.
            let output = *echo == crate::classify::Echo::Not;
            let stream = match self.stream.remove(&print.row) {
                Some((start, end, mut text)) if output && end == print.cols.0 => {
                    text.push_str(print.text);
                    (start, print.cols.1, text)
                }
                _ => (print.cols.0, print.cols.1, print.text.to_owned()),
            };
            if output {
                self.stream.insert(print.row, stream.clone());
            }
            // A text found here holds some of the run: one of its
            // characters.
            let some = |c: char| {
                (c.is_whitespace() && wanted.contains(&' '))
                    || c.to_lowercase().any(|c| wanted.contains(&c))
            };
            if !print.text.chars().any(some) {
                continue;
            }
            // The piece's line, as long as the row still holds what it
            // printed there: else later output took its place.
            let screen = pane.screen();
            let intact = screen
                .row_by_id(print.row)
                .is_some_and(|row| crate::pane::holds_at(&row.cells_text(), &print));
            // A row a line wraps onto goes on from the row above's end.
            let above = (screen.row_by_id(print.row))
                .and_then(|row| row.above())
                .filter(fux_vt::Row::wrapped)
                .and_then(|up| up.cells_text().chars().last());
            let line = match intact {
                true => Line::of(pane, &print, reach),
                false => Line::piece(&print, above),
            };
            let joined = Line::piece(
                &fux_vt::Print {
                    cols: (stream.0, stream.1),
                    text: &stream.2,
                    ..print
                },
                above,
            );
            let lines = [line, joined];
            // A text an echo may show is no news here: one made of what was
            // typed, as its keys may have edited it into any order.
            let echoed = |t: &String| match echo {
                crate::classify::Echo::Not => false,
                crate::classify::Echo::Is => true,
                // The line as typed since, if no key submitted it: later
                // keys edit what this piece showed.
                crate::classify::Echo::Of(typed) => {
                    let now = pane.typed();
                    let typed = if now.starts_with(&**typed) {
                        now
                    } else {
                        typed
                    };
                    typed_from(typed, bare(t))
                }
            };
            let held = |t: &String| lines.iter().any(|l| l.holds(t).is_some());
            self.echo_skipped |= texts.iter().any(|t| echoed(t) && held(t));
            // On its line where the piece went, else in the piece itself:
            // of several texts, the one furthest left.
            let found = lines.iter().find_map(|l| {
                (texts.iter().filter(|t| !echoed(t)))
                    .filter_map(|t| l.holds(t).map(|at| (at, t)))
                    .min_by_key(|(at, _)| *at)
                    .map(|(_, t)| (t.clone(), l.text.replace(CUT_END, "")))
            });
            if let Some((text, line)) = found {
                self.found = Some((text, line, seq.0));
                return;
            }
        }
        // More was printed than the pane keeps: the lines changed since,
        // as they read now, on the screen and in history.
        if pane.prints_lost {
            let screen = pane.screen();
            let changed = screen.rows().filter(|r| r.version() > self.from.0);
            let found = changed.map(|row| Line::of_row(row)).find_map(|l| {
                let at = texts.iter().filter_map(|t| l.holds(t).map(|at| (at, t)));
                at.min_by_key(|(at, _)| *at)
                    .map(|(_, t)| (t.clone(), l.text.clone()))
            });
            if let Some((text, line)) = found {
                self.found = Some((text, line, Seq(screen.version())));
            }
        }
    }

    /// Whether the pane has settled: on two looks a hold apart, the screen
    /// as it was.
    fn settled(
        &mut self,
        pane: &Pane,
        (began, now): (Instant, Instant),
        more: bool,
    ) -> Option<Found> {
        let state = crate::ready::state(pane, now);
        // Settled on two looks a hold apart, the screen as it
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
            (now >= crate::after(since.0, HOLD)).then_some(Found::Settled(state))
        }
    }

    fn since(&self, pane: Option<&Pane>) -> Since {
        Since {
            from: self.from,
            on_full: self.on_full,
            finished: self.finished,
            revision: self.revision,
            cut: pane.and_then(|p| match (p.prints_lost, p.cut()) {
                (true, _) => Some(MORE_THAN_KEPT),
                (false, true) => Some(CUT),
                (false, false) => None,
            }),
        }
    }
}

/// Marks an end of a window on a line that is no end of the line: no
/// text, blank or word holds it.
const CUT_END: &str = "\u{1}";

/// A line of text, its soft-wrapped rows joined, from its first row; and
/// which of its bytes, if any, a piece of output just printed.
struct Line {
    text: String,
    printed: Vec<bool>,
    /// The rows it was read from, if on the screen or in history.
    rows: Vec<fux_vt::RowId>,
}

impl Line {
    /// Row `row`'s text alone, all of it taken as printed.
    fn of_row(row: fux_vt::Row<'_>) -> Line {
        let text = row.cells_text().trim_end_matches(' ').to_owned();
        Line {
            printed: vec![true; text.len()],
            text,
            rows: vec![row.id()],
        }
    }

    /// The line the piece `print` went on, wherever it is now: on the
    /// screen or in history; else (let go) the piece's own text. Of a line wrapped over many rows, the rows within `reach`
    /// characters of the piece: a text that holds any of it lies there.
    /// An end of the window that is no end of the line is marked so that
    /// `^` and `$` find no line's start or end there.
    fn of(pane: &Pane, print: &fux_vt::Print<'_>, reach: usize) -> Line {
        let screen = pane.screen();
        if let Some(at) = screen.row_by_id(print.row) {
            let width = usize::from(screen.size().cols()).max(1);
            let rows = reach.div_ceil(width).saturating_add(1);
            let mut first = at;
            for _ in 0..rows {
                match first.above().filter(fux_vt::Row::wrapped) {
                    Some(up) => first = up,
                    None => break,
                }
            }
            let mut line = Line {
                text: String::new(),
                printed: Vec::new(),
                rows: Vec::new(),
            };
            if first.above().is_some_and(|up| up.wrapped()) {
                line.push(CUT_END, false);
            }
            // Rows after the piece's own, once it is passed.
            let mut after: Option<usize> = None;
            let mut row = Some(first);
            while let Some(r) = row {
                let ours = r.id() == print.row;
                line.rows.push(r.id());
                let cells = r.cells().enumerate();
                for (col, cell) in cells.filter(|(_, c)| !c.is_wide_continuation()) {
                    let text = match cell.contents() {
                        text if cell.has_contents() && text != fux_vt::PLACEHOLDER => text,
                        _ => " ",
                    };
                    let col = u16::try_from(col).unwrap_or(u16::MAX);
                    line.push(text, ours && print.cols.0 <= col && col < print.cols.1);
                }
                after = match (ours, after) {
                    (true, _) => Some(0),
                    (false, Some(n)) => Some(n.saturating_add(1)),
                    (false, None) => None,
                };
                row = r.below().filter(|_| r.wrapped());
                if row.is_some() && after.is_some_and(|n| n >= rows) {
                    line.push(CUT_END, false);
                    return line;
                }
            }
            return line.trimmed();
        }
        Line::piece(print, None)
    }

    /// The piece's own text, all of it printed: a line's start only if
    /// it starts its row and the row starts a line, and never its end,
    /// which the row may go on past. A row a line wraps onto goes on from
    /// `above`, the last character of the row above.
    fn piece(print: &fux_vt::Print<'_>, above: Option<char>) -> Line {
        let mut line = Line {
            text: String::new(),
            printed: Vec::new(),
            rows: Vec::new(),
        };
        match above.filter(|_| print.cols.0 == 0) {
            Some(last) => {
                line.push(CUT_END, false);
                line.push(last.encode_utf8(&mut [0; 4]), false);
            }
            None if print.cols.0 > 0 => line.push(CUT_END, false),
            None => {}
        }
        line.push(print.text, true);
        line.push(CUT_END, false);
        line
    }

    fn push(&mut self, text: &str, printed: bool) {
        self.text.push_str(text);
        self.printed.resize(self.text.len(), printed);
    }

    /// Without the blanks at its end.
    fn trimmed(mut self) -> Line {
        self.text = self.text.trim_end_matches(' ').to_owned();
        self.printed.truncate(self.text.len());
        self
    }

    /// Where the line holds `text` as `found_at` finds it, on bytes that
    /// were just printed: its first such place, as `normal` counts it.
    fn holds(&self, text: &str) -> Option<usize> {
        let (line, printed) = normal_marked(&self.text, &self.printed);
        found_at_normal(&line, text).into_iter().find(|at| {
            let end =
                at.saturating_add(normal(text.trim_start_matches('^').trim_end_matches('$')).len());
            printed.get(*at..end).is_some_and(|p| p.iter().any(|b| *b))
        })
    }
}

/// Whether `text` could be shown by an echo of what was `typed`: each of
/// its characters (blanks aside, case ignored) one that was typed, as
/// often, in whatever order the line's editing left them.
fn typed_from(typed: &str, text: &str) -> bool {
    let mut left: Vec<Option<char>> = (typed.to_lowercase().chars())
        .filter(|c| !c.is_whitespace())
        .map(Some)
        .collect();
    text.to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .all(|c| {
            let slot = left.iter_mut().find(|l| **l == Some(c));
            slot.map(Option::take).is_some()
        })
}

/// A text waited for without its `^` and `$`.
fn bare(text: &str) -> &str {
    let text = text.strip_prefix('^').unwrap_or(text);
    text.strip_suffix('$').unwrap_or(text)
}

/// `text` as `normal` makes it, with which of its bytes come from bytes
/// of `text` that are `marked`.
fn normal_marked(text: &str, marked: &[bool]) -> (String, Vec<bool>) {
    let (mut out, mut marks) = (String::new(), Vec::new());
    let mut blank: Option<bool> = None;
    for (at, c) in text.char_indices() {
        let mark = marked.get(at).copied().unwrap_or(false);
        if c.is_whitespace() {
            blank = Some(blank.unwrap_or(false) || mark);
            continue;
        }
        if let Some(mark) = blank.take()
            && !out.is_empty()
        {
            out.push(' ');
            marks.push(mark);
        }
        for lower in c.to_lowercase() {
            out.push(lower);
            marks.resize(out.len(), mark);
        }
    }
    (out, marks)
}

/// A screen's lines, its soft-wrapped rows joined.
struct Shown {
    text: String,
}

/// Rows as lines: soft-wrapped rows joined into the lines they wrap.
fn lines<'a>(rows: impl Iterator<Item = fux_vt::Row<'a>>) -> Vec<Shown> {
    let mut out: Vec<Shown> = Vec::new();
    let mut wraps = false;
    for row in rows {
        let text = row.cells_text();
        // A wrapped row's trailing blanks are part of its line.
        let text = match row.wrapped() {
            true => text,
            false => text.trim_end_matches(' ').to_owned(),
        };
        match out.last_mut() {
            Some(last) if wraps => last.text.push_str(&text),
            _ => out.push(Shown { text }),
        }
        wraps = row.wrapped();
    }
    out
}

/// Where the screen's lines start among the rows kept: at its top row, or
/// at the first row of a line wrapped onto it from history.
fn screen_top(screen: &fux_vt::Screen) -> usize {
    let Some(mut top) = screen.window().row(0) else {
        return screen.history_len();
    };
    while let Some(up) = top.above().filter(fux_vt::Row::wrapped) {
        top = up;
    }
    screen.row_index(top.id()).unwrap_or(screen.history_len())
}

/// A text as a wait compares it: case ignored, runs of blanks as one, and
/// none at its ends.
fn normal(text: &str) -> String {
    blanks_as_one(text).to_lowercase()
}

/// `text` with runs of blanks as one, and none at its ends.
fn blanks_as_one(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How many times a line shows TEXT: case ignored, runs of blanks as one,
/// starting where a word does (`READY` is not in `NOT_READY`), `^` and `$`
/// holding it to the line's start and end.
fn found_in(line: &str, text: &str) -> usize {
    found_at(line, text).len()
}

/// Where a line shows TEXT, as `found_in` finds it: each place in the line
/// as `normal` makes it.
fn found_at(line: &str, text: &str) -> Vec<usize> {
    found_at_normal(&normal(line), text)
}

/// Where a line already made `normal` shows TEXT, as `found_at` finds it.
fn found_at_normal(line: &str, text: &str) -> Vec<usize> {
    let (start, rest) = match text.strip_prefix('^') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (end, body) = match rest.strip_suffix('$') {
        Some(body) => (true, body),
        None => (false, rest),
    };
    let body = normal(body);
    if body.is_empty() {
        return Vec::new();
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
        .map(|(at, _)| at)
        .collect()
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

/// What came since a wait began: the pane's lines from the first that
/// changed since (on the screen or in history) to its last line with text.
/// A full-screen program's whole screen; keys that went in on a full screen
/// the program has since left, what it printed after, else that last full
/// screen if it drew it since, else the screen as it is.
fn since_text(pane: &Pane, since: Since, marks: bool) -> String {
    let screen = pane.screen();
    if screen.mode(fux_vt::Mode::AlternateScreen) {
        return text(pane, marks, false);
    }
    // A row changed since, or moved (`fux_vt::Print`).
    let moved: std::collections::HashSet<fux_vt::RowId> = (pane.prints.iter())
        .filter(|p| p.seq.0 > since.from)
        .flat_map(|p| p.prints.iter().map(|r| r.row))
        .collect();
    // A resize gives every row it keeps a new number, and brings nothing.
    let resized: std::collections::HashSet<u64> = (pane.record.after(since.from))
        .filter(|e| matches!(e.event, crate::log::Event::Resize(_)))
        .map(|e| e.seq.0)
        .collect();
    let window = screen.window();
    // A row drawn again to show as it did at the input brings nothing.
    let new = |r: &fux_vt::Row<'_>| {
        (r.version() > since.from.0 && !resized.contains(&r.version()) || moved.contains(&r.id()))
            && (pane.before_shows.get(&r.id()))
                != (window.place(r).map(|y| (y, crate::pane::shows(r)))).as_ref()
    };
    let Some(mut first) = screen.rows().find(new) else {
        return match (screen.alternate_rows().any(|r| new(&r)), since.on_full) {
            (true, _) => text(pane, marks, true),
            (false, true) => text(pane, marks, false),
            (false, false) => String::new(),
        };
    };
    while let Some(up) = first.above().filter(fux_vt::Row::wrapped) {
        first = up;
    }
    let from = screen.row_index(first.id()).unwrap_or(0);
    let found = lines(screen.rows().skip(from));
    let texts: Vec<&str> = found.iter().map(|l| l.text.trim_end()).collect();
    let end = (texts.iter())
        .rposition(|t| !t.is_empty())
        .map_or(0, |i| i.saturating_add(1));
    let lead = since.cut.unwrap_or_default();
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
