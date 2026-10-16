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
use crate::code::Code;
use crate::command::{AnyRef, CaptureView, Until, Wait};
use crate::id::PaneId;
use crate::json::Json;
use crate::log::Seq;
use crate::pane::{Pane, Start};
use crate::process::Ended;
use crate::protocol::{End, Reply};
use crate::session::Session;
use std::num::NonZeroU8;
use std::time::{Duration, Instant};

/// The exit statuses a wait ends with, besides 0 (what it waited for came)
/// and the 1 and 2 of any command.
pub const EXITED: NonZeroU8 = NonZeroU8::MIN.saturating_add(2);
pub const GONE: NonZeroU8 = NonZeroU8::MIN.saturating_add(3);
/// The program reads its terminal, and the text waited for has not come
/// (or gone): fux judges it cannot before the agent types.
pub const STOPPED: NonZeroU8 = NonZeroU8::MIN.saturating_add(6);
/// As `timeout(1)`.
pub const TIMED_OUT: NonZeroU8 = NonZeroU8::MIN.saturating_add(123);

/// Said first of what came when lines of it were let go past history's
/// budget.
const CUT: &str = "… earlier lines not kept (past history-bytes)\n";
/// Said first instead when more came than fux keeps for a wait.
const MORE_THAN_KEPT: &str =
    "… more came than the pane's journal keeps (journal-bytes): text may have passed unseen\n";

/// How long a bare `wait` on a pane that may go on alone waits for
/// something new before it answers the pane as it is: longer than the
/// pauses of a program polling for keys.
const MORE: Duration = Duration::from_secs(5);

/// How often a wait that asks what a pane's program is blocked in looks
/// again: that changes with nothing on the screen to wake the server.
const PROBE: Duration = Duration::from_millis(30);
/// How long a pane must look settled, the screen as it was, to be taken as
/// settled: output its program wrote just before is in fux's hands by then
/// (the kernel hands it on within a millisecond or two).
const HOLD: Duration = Duration::from_millis(10);

/// A wait under way.
pub struct Waiting {
    watches: Vec<Watch>,
    pub(crate) wait: Wait,
    /// The texts it looks for (`wait-text`), in order.
    texts: Vec<String>,
    /// When it began: a pane is quiet from then at the soonest.
    began: Instant,
    deadline: Instant,
    /// How long it waits at most: its own timeout, else the server's.
    pub(crate) limit: Duration,
    /// Printed before what the wait says: the pane `run` started.
    pub lead: String,
    /// The program has just started (`run`, `respawn`): its screen is
    /// blank, so a text is gone only once it has appeared.
    pub fresh: bool,
    /// A bare `wait`: on a pane that may go on alone (`waiting`, or quiet
    /// with nothing known) it waits for something new.
    pub more: bool,
    /// What the command it follows made, for its reply.
    pub made: Vec<AnyRef>,
    /// What each pane said, once it has.
    answers: Vec<Option<Reply>>,
    /// A capture to give once the pane settles (`capture` from a command
    /// line): its history and view; the client picks text or JSON.
    pub capture: Option<(Option<usize>, CaptureView)>,
}

/// What a wait's answer tells of a pane against how it was as the wait
/// began: where it counts from, the commands that had ended, the screen's
/// revision.
#[derive(Clone, Copy)]
struct Since {
    start: Start,
    revision: u64,
    /// What came since was more than the pane keeps, and which notice
    /// says so: some was let go before the wait read it.
    cut: Option<&'static str>,
}

/// One pane a wait watches, and what it found as it began.
struct Watch {
    pane: PaneId,
    /// What came after this is new to it.
    start: Start,
    /// Pieces of output it had not read yet were let go.
    missed: bool,
    /// The screen's rows as they read when it began, if it began after the
    /// last action (else the pane's, `Pane::before`).
    before: Option<std::collections::HashMap<fux_vt::RowId, crate::pane::Cells>>,
    /// The first text found: which, its line, and when it came.
    found: Option<(String, String, Seq)>,
    /// The text printed on each row, runs that go on from each other
    /// joined, though printed in several pieces and between others: where
    /// it starts and ends, and the text.
    stream: std::collections::HashMap<fux_vt::RowId, (u16, u16, String)>,
    /// The `wait-gone` texts seen since the wait began, for a fresh
    /// program, and the last piece that printed each, if one did.
    appeared: std::collections::HashMap<String, Option<Seq>>,
    /// How far it read the journal, for texts, texts gone and bells: each
    /// look reads only what is new.
    looked: Seq,
    bell: Option<(Seq, Option<String>)>,
    /// A text was found in the echo of what was typed alone.
    echo_skipped: bool,
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
    Gone,
    Exit(Ended),
    Bell(Option<String>),
    /// The pane settled, so.
    Settled(crate::ready::State),
    /// The program reads its terminal; the text waited for has not come
    /// (or gone).
    Stopped,
}

impl Waiting {
    /// A wait in `panes` from `now`, for `limit` at most unless it says.
    pub fn new(panes: &[&Pane], wait: Wait, now: Instant, limit: Duration) -> Waiting {
        let limit = wait.timeout.unwrap_or(limit);
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
            deadline: crate::after(now, limit),
            limit,
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
    pub fn answer(&mut self, session: &Session, now: Instant) -> Option<Reply> {
        let mut outcome = self.look(session, now)?;
        // A capture asked to settle first: its state line, then the capture.
        if let (Some((history, view)), Some(watch)) = (self.capture, self.watches.first())
            && let Some(pane) = session.panes.get(&watch.pane)
        {
            let copying = session.copying(pane.id);
            let (read, form) = crate::capture::pane(pane, history, view, true, copying);
            // Its JSON, read at the same moment, for `json` to give.
            outcome.json = form.unwrap_or_default();
            let line = outcome.text.lines().next().unwrap_or_default();
            outcome.text = format!("{line}\n{read}");
        }
        // An answer that starts with the pane's state names the pane itself.
        let named = self.lead.trim_end();
        let lead = if !named.is_empty() && outcome.text.starts_with(&format!("{named} ")) {
            ""
        } else {
            self.lead.as_str()
        };
        // What it made, then the pane it answers of: an answer of one pane
        // is that pane.
        let mut ids = self.made.clone();
        ids.extend(outcome.ids.iter().filter(|id| !self.made.contains(id)));
        Some(Reply {
            text: format!("{lead}{}", outcome.text),
            ids,
            ..outcome
        })
    }

    /// Each pane answers on its own, as a lone pane's wait would, and the
    /// wait once every one has: a line from each, in order, with the first
    /// failing status. At the limit, each pane yet to answer is said as it
    /// is.
    fn look(&mut self, session: &Session, now: Instant) -> Option<Reply> {
        let several = self.watches.len() > 1;
        let (wait, texts, limit) = (&self.wait, self.texts.as_slice(), self.limit);
        let (window, mode) = ((self.began, now), (self.fresh, self.more));
        let late = now >= self.deadline;
        for (watch, answer) in self.watches.iter_mut().zip(&mut self.answers) {
            if answer.is_some() {
                continue;
            }
            let Some(pane) = session.panes.get(&watch.pane) else {
                *answer = Some(wait.gone(watch.pane));
                continue;
            };
            if let Some(told) = watch.verdict(pane, wait, texts, window, mode) {
                *answer = Some(wait.told(pane, told, (several, limit), watch.since(pane)));
            } else if late {
                let told = Told::TimedOut(watch.hint(pane, texts));
                *answer = Some(wait.told(pane, told, (several, limit), watch.since(pane)));
            }
        }
        let answers: Vec<&Reply> = self
            .answers
            .iter()
            .map(Option::as_ref)
            .collect::<Option<_>>()?;
        if let [one] = answers.as_slice() {
            return Some((*one).clone());
        }
        // The first end with a status other than 0, in order, names the
        // whole: a failure, if one failed, with every pane's why.
        let first = (answers.iter()).find(|a| a.end.status() != 0);
        let why: String = (answers.iter())
            .filter_map(|a| match &a.end {
                End::Failed { message, .. } => Some(format!("{}\n", message.trim_end())),
                End::Done | End::Answered { .. } => None,
            })
            .collect();
        let first = first.and_then(|a| Some((NonZeroU8::new(a.end.status())?, a.end.code()?)));
        let end = match first {
            None => End::Done,
            Some((status, code)) if why.is_empty() => End::Answered { status, code },
            Some((status, code)) => End::Failed {
                status,
                code,
                message: why,
            },
        };
        Some(Reply {
            end,
            text: answers.iter().map(|a| a.text.as_str()).collect(),
            json: answers.iter().map(|a| a.json.as_str()).collect(),
            ids: answers.iter().flat_map(|a| a.ids.iter().copied()).collect(),
            ..Reply::default()
        })
    }
}

/// How a wait in a pane still there ended, to be told.
enum Told {
    Found(Found),
    /// With why the text it waited for may have gone unseen, if it may.
    Exited(Ended, Option<&'static str>),
    /// The program was started again (`respawn`) before.
    Replaced,
    /// With why the wait may never be met, if a reason shows.
    TimedOut(Option<&'static str>),
}

impl Told {
    /// The status a wait that ended so ends with, if not 0, and the code
    /// that names it.
    fn status(&self) -> Option<(NonZeroU8, Code)> {
        match self {
            // As timeout(1): the program's own status, once it exits.
            Told::Found(Found::Exit(ended)) => {
                let status = crate::process::shell_status(*ended);
                NonZeroU8::new(u8::try_from(status).unwrap_or(u8::MAX)).map(|s| (s, Code::Exited))
            }
            Told::Found(Found::Stopped) => Some((STOPPED, Code::Stopped)),
            Told::Found(Found::Text { .. } | Found::Gone | Found::Bell(_) | Found::Settled(_)) => {
                None
            }
            Told::Exited(..) | Told::Replaced => Some((EXITED, Code::Exited)),
            Told::TimedOut(_) => Some((TIMED_OUT, Code::Timeout)),
        }
    }
}

impl Wait {
    /// The outcome of a wait whose pane `id` closed: a settling action's
    /// (a shell that exited) said with status 4, not a failure; any other
    /// wait's a failure.
    fn gone(&self, id: PaneId) -> Reply {
        if !self.until.contains(&Until::Settled) {
            let message = format!("{id}: the pane closed");
            return failed(id, (GONE, Code::Closed), String::new(), message);
        }
        let stdout = format!("{id} closed\n");
        let state = vec![("state", Json::str("closed"))];
        answer(id, Some((GONE, Code::Closed)), stdout, state, None)
    }

    /// The outcome of a wait that ended so in `pane`.
    fn told(
        &self,
        pane: &Pane,
        told: Told,
        (several, limit): (bool, Duration),
        since: Since,
    ) -> Reply {
        use crate::ready::State;
        let (id, now) = (pane.id, Instant::now());
        let status = told.status();
        // A settling action, a wait the program stopped short of, an exit
        // or a text gone: its state, then what came since it began, if asked.
        let settled = |state, status, body| self.settled(pane, state, status, body, since);
        let came = || since_text(pane, since, self.marks);
        let asked = || match self.print {
            true => came(),
            false => String::new(),
        };
        let (said, met) = match told {
            // A text that cannot come: the screen, to act on what is there.
            Told::Found(Found::Stopped) => {
                let cut = since.cut.unwrap_or_default();
                let screen = format!("{cut}{}", text(pane, self.marks, false));
                return settled(State::Reading, status, screen);
            }
            Told::Found(Found::Exit(ended)) => {
                return settled(State::Exited(ended), status, came());
            }
            // A text gone leaves the pane as it is.
            Told::Found(Found::Gone) => {
                return settled(crate::ready::state(pane, now), status, came());
            }
            Told::Found(Found::Settled(state)) => return settled(state, status, asked()),
            // At its limit a settling wait fails (124) only while busy; `0
            // timeout` answers at once, as it is, and never fails.
            Told::TimedOut(_) if self.until.contains(&Until::Settled) => {
                let state = crate::ready::state(pane, now);
                let calm = limit.is_zero() || state != State::Busy;
                return settled(state, status.filter(|_| !calm), asked());
            }
            Told::Found(Found::Text { text, line }) => {
                let fields = vec![
                    ("met", Json::str("text")),
                    ("text", Json::str(text)),
                    ("line", Json::str(line.clone())),
                ];
                (line, Ok(fields))
            }
            Told::Found(Found::Bell(notification)) => {
                let said = notification
                    .as_ref()
                    .map_or_else(|| "bell".to_owned(), |n| format!("notification: {n}"));
                let notification = notification.map_or(Json::Null, Json::str);
                let fields = vec![("met", Json::str("bell")), ("notification", notification)];
                (said, Ok(fields))
            }
            Told::Exited(ended, hint) => {
                // Its last words, which an agent would otherwise ask for next:
                // a crash says why.
                let mut message = format!(
                    "{id}: its program {} while waiting for {}",
                    crate::process::told(ended),
                    self.describe()
                );
                for line in tail(pane, 3) {
                    message.push_str(&format!("\n  {line}"));
                }
                if let Some(hint) = hint {
                    message.push_str(&format!("\n({hint})"));
                }
                (String::new(), Err(message))
            }
            Told::Replaced => {
                let message = format!(
                    "{id}: its program was started again (respawn) while waiting for {}",
                    self.describe()
                );
                (String::new(), Err(message))
            }
            Told::TimedOut(hint) => {
                // What the pane is doing, so the next step is clear: still
                // busy, or settled on a prompt that will not change.
                let state = crate::ready::state(pane, now);
                let idle = now.saturating_duration_since(pane.changed_at).as_secs();
                let mut message = format!(
                    "{id}: timed out after {}s waiting for {}",
                    limit.as_secs_f64(),
                    self.describe()
                );
                // What it does: reading its terminal, waiting, busy…
                let word = state.word();
                message.push_str(&format!("; {id} {word}, the screen unchanged for {idle}s"));
                if state == State::Busy {
                    message.push_str("; still busy: the same wait waits on");
                }
                if let Some(line) = tail(pane, 1).pop() {
                    message.push_str(&format!("; last line: `{line}`"));
                }
                let queued = pane.input_queued();
                if queued > 0 {
                    message.push_str(&format!(
                        "; {queued} bytes of input not yet read by the program"
                    ));
                }
                if let Some(hint) = hint {
                    message.push_str(&format!("\n  {hint}"));
                }
                (String::new(), Err(message))
            }
        };
        let screen = self.print.then(|| text(pane, self.marks, false));
        let said = match (said.is_empty(), several) {
            (true, _) => said,
            (false, true) => format!("{id} {said}\n"),
            (false, false) => format!("{said}\n"),
        };
        let stdout = format!("{said}{}", screen.as_deref().unwrap_or_default());
        match met {
            Ok(fields) => {
                let screen = screen.map(|screen| ("screen", Json::str(screen)));
                answer(id, status, stdout, fields, screen)
            }
            // A failure always has a status: its told one.
            Err(message) => failed(
                id,
                status.unwrap_or((NonZeroU8::MIN, Code::Failed)),
                stdout,
                message,
            ),
        }
    }

    /// A settled pane's answer: its state line, then `body`.
    fn settled(
        &self,
        pane: &Pane,
        state: crate::ready::State,
        status: Option<(NonZeroU8, Code)>,
        body: String,
        since: Since,
    ) -> Reply {
        let (line, notes) = self.state_line(pane.id, pane, state, since);
        let mut fields = vec![("state", Json::str(state.word()))];
        fields.extend(notes);
        let stdout = format!("{line}\n{body}");
        let output = Some(("output", Json::str(body)));
        answer(pane.id, status, stdout, fields, output)
    }
}

/// A wait's failure in pane `id`, with its status and code: why, on
/// stderr, beside what it prints. It leaves no answer, so no JSON.
fn failed(id: PaneId, (status, code): (NonZeroU8, Code), text: String, message: String) -> Reply {
    Reply {
        end: End::Failed {
            status,
            code,
            message,
        },
        text,
        ids: vec![AnyRef::Pane(id)],
        ..Reply::default()
    }
}

/// A wait's answer of pane `id`: its JSON the pane, `fields`, the status,
/// then `last`, if any.
fn answer(
    id: PaneId,
    status: Option<(NonZeroU8, Code)>,
    stdout: String,
    fields: Vec<(&'static str, Json<'static>)>,
    last: Option<(&'static str, Json<'static>)>,
) -> Reply {
    let mut all = vec![("pane", Json::str(id.to_string()))];
    all.extend(fields);
    all.push(("status", Json::int(status.map_or(0, |(s, _)| s.get()))));
    all.extend(last);
    Reply {
        end: status.map_or(End::Done, |(status, code)| End::Answered { status, code }),
        text: stdout,
        json: format!("{}\n", Json::Object(all).render()),
        ids: vec![AnyRef::Pane(id)],
        ..Reply::default()
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
        if commands.finished > since.start.finished {
            let status = commands.last_status;
            match status {
                Some(status) => line.push_str(&format!(" · done {status}")),
                None => line.push_str(" · done"),
            }
            let status = status.map_or(Json::Null, Json::int);
            fields.push(("done", status));
        }
        if self.after && pane.revision == since.revision && state == crate::ready::State::Reading {
            line.push_str(" · no change");
            fields.push(("no_change", Json::Bool(true)));
        }
        if state == crate::ready::State::Busy {
            line.push_str(&format!(" · `{id} opts wait` waits on"));
        }
        let queued = pane.input_queued();
        if queued > 0 {
            line.push_str(&format!(" · {queued} bytes unread"));
            fields.push(("unread", Json::int(queued)));
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

/// A pane's last `n` lines with text, as a capture reads them: of its
/// screen, or once a full-screen program has left and the screen is blank,
/// of what it last drew.
fn tail(pane: &Pane, n: usize) -> Vec<String> {
    let text = text(pane, false, false);
    let lines: Vec<&str> = (text.lines().map(str::trim_end))
        .filter(|l| !l.is_empty())
        .collect();
    let last = lines
        .get(lines.len().saturating_sub(n)..)
        .unwrap_or_default();
    last.iter().map(|l| (*l).to_owned()).collect()
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

impl Watch {
    /// How `pane` stands as a wait begins: one armed before an action
    /// (`after`) counts from now, any other from the last action's input to
    /// the pane, or its program's start.
    fn new(pane: &Pane, after: bool) -> Watch {
        let screen = pane.screen();
        let start = match after {
            true => Start::at(Seq(screen.version()), screen),
            false => pane.since,
        };
        Watch {
            pane: pane.id,
            start,

            missed: false,
            before: after.then(|| crate::pane::rows_now(screen)),
            found: None,
            stream: std::collections::HashMap::new(),
            appeared: std::collections::HashMap::new(),
            looked: start.from,

            bell: None,
            echo_skipped: false,
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
        pane: &Pane,
        wait: &Wait,
        texts: &[String],
        window: (Instant, Instant),
        mode: (bool, bool),
    ) -> Option<Told> {
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
                None => Told::Exited(pane.process.exit_status()?, self.unseen()),
            }
        };
        Some(told)
    }

    /// Why a wait that timed out may never have been met: a text on the
    /// screen that nothing printed since the wait began, or one that came
    /// unseen.
    fn hint(&self, pane: &Pane, texts: &[String]) -> Option<&'static str> {
        if let Some(why) = self.unseen() {
            return Some(why);
        }
        let there = texts.iter().any(|t| on_screen(pane.screen(), t));
        there.then_some(
            "the text is on the screen, but on a line that reads as it did when the wait \
             began (nothing printed it since; drawn again the same, it brings nothing new); \
             read it with `PANE opts capture`",
        )
    }

    /// Why a text may have come unseen: more came than the pane keeps, or
    /// the text was in the echo of what was typed alone.
    fn unseen(&self) -> Option<&'static str> {
        if self.missed {
            return Some(
                "more was written since the wait began than the pane's journal keeps \
                 (journal-bytes): the text may have come unseen",
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
        // Any wait's answer reads the journal: past its budget, it says so.
        self.missed |= pane.record.lost_after(self.looked);
        if !texts.is_empty() && self.found.is_none() {
            self.read_prints(pane, texts);
        }
        // What came since the last look: the pieces that printed a text
        // waited to go, and the first bell.
        for (seq, p) in pane.record.printed_after(self.looked) {
            for until in &wait.until {
                if let Until::Gone(text) = until
                    && p.prints.iter().any(|r| found_in(r.text, text) > 0)
                {
                    self.appeared.insert(text.clone(), Some(seq));
                }
            }
        }
        if self.bell.is_none() {
            self.bell = pane.record.bell_after(self.looked);
        }
        self.looked = pane.record.last();
        let screen = pane.screen();
        let mut gone = None;
        let mut exit = None;
        let mut bell = None;
        let mut settled = None;
        for until in &wait.until {
            match until {
                Until::Text(_) => {}
                Until::Gone(text) => {
                    let present = on_screen(screen, text);
                    if present {
                        self.appeared.entry(text.clone()).or_default();
                    }
                    // Shown between two looks (drawn over, erased, scrolled
                    // away): printed since, though gone now.
                    // It went no earlier than the last piece that printed it.
                    let seen = self.appeared.get(text);
                    let last = seen.copied().flatten();
                    let had = !fresh || seen.is_some();
                    if !present && had && gone.is_none() {
                        gone = Some((Found::Gone, last.unwrap_or(self.start.from)));
                    }
                }
                Until::Exit => exit = pane.process.exit_status().map(Found::Exit),
                Until::Settled => settled = self.settled(pane, (began, now), more),
                Until::Bell => bell = self.bell.clone(),
            }
        }
        // Of a text, a bell and a text gone, the first to come; in one
        // piece, a text gone before a text came (the piece that erased it
        // printed the new one), and a text before the bell that ends it.
        let text = (self.found.clone()).map(|(text, line, at)| (Found::Text { text, line }, at));
        // The alert found, not the pane's last.
        let bell = bell.map(|(at, notification)| (Found::Bell(notification), at));
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
        // Rows scrolled off are printed on no more: a long wait lets them go.
        crate::pane::prune(&mut self.stream, pane.screen());
        let pieces = pane.record.printed_after(self.looked.max(self.start.from));
        let reach = texts.iter().map(|t| t.chars().count()).max().unwrap_or(0);
        // The characters of the texts waited for, case and blanks aside.
        let wanted: Vec<Vec<char>> = (texts.iter())
            .map(|t| folded(t.trim_start_matches('^').trim_end_matches('$')))
            .collect();
        let runs = pieces.flat_map(|(seq, p)| p.runs().map(move |(r, e, f)| (seq, r, e, f)));
        for (seq, whole, echo, fresh) in runs {
            // Only what it changed is news: what it printed as its row held
            // it, just before or as the wait began, is drawn again.
            let fresh = match self.before.as_ref().and_then(|b| b.get(&whole.row)) {
                Some(row) => {
                    let echo = *echo != crate::classify::Echo::Not;
                    let (a, b) = crate::pane::changed(row, &whole, echo);
                    (fresh.0.max(a), fresh.1.min(b))
                }
                None => fresh,
            };
            let Some(print) = part(&whole, fresh) else {
                continue;
            };
            // The runs printed one after another on the row, joined, whole:
            // a text may go on over what a run printed as it was.
            let output = *echo == crate::classify::Echo::Not;
            let stream = match self.stream.remove(&whole.row) {
                Some((start, end, mut text)) if output && end == whole.cols.0 => {
                    text.push_str(whole.text);
                    (start, whole.cols.1, text)
                }
                _ => (whole.cols.0, whole.cols.1, whole.text.to_owned()),
            };
            if output {
                self.stream.insert(whole.row, stream.clone());
            }
            // A text found here holds some of what the run changed: one of
            // its characters.
            let run = folded(print.text);
            if !wanted.iter().any(|text| overlaps(&run, text)) {
                continue;
            }
            // The piece's line, as long as the row still holds what it
            // printed there: else later output took its place.
            let screen = pane.screen();
            let intact = screen
                .row_by_id(whole.row)
                .is_some_and(|row| crate::pane::holds_at(&crate::pane::row_cells(&row), &whole));
            // A row a line wraps onto goes on from the row above's end.
            let above = (screen.row_by_id(whole.row))
                .and_then(|row| row.above())
                .filter(fux_vt::Row::wrapped)
                .and_then(|up| up.cells_text().chars().last());
            let line = match intact {
                true => Line::of(pane, &print, reach),
                false => Line::piece(&whole, above, fresh),
            };
            // In the joined runs, what this one changed is at their end.
            let before_it = stream
                .2
                .chars()
                .count()
                .saturating_sub(whole.text.chars().count());
            let joined = Line::piece(
                &fux_vt::Print {
                    cols: (stream.0, stream.1),
                    text: &stream.2,
                    ..whole
                },
                above,
                (
                    before_it.saturating_add(fresh.0),
                    before_it.saturating_add(fresh.1),
                ),
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
                // The answer is the whole line, not the rows read around
                // the text to find it.
                let line = match intact {
                    true => Line::of(pane, &print, usize::MAX >> 8)
                        .text
                        .replace(CUT_END, ""),
                    false => line,
                };
                self.found = Some((text, line, seq));
                return;
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

    fn since(&self, pane: &Pane) -> Since {
        Since {
            start: self.start,
            revision: self.revision,
            cut: match (self.missed, pane.cut()) {
                (true, _) => Some(MORE_THAN_KEPT),
                (false, true) => Some(CUT),
                (false, false) => None,
            },
        }
    }
}

/// Marks an end of a window on a line that is no end of the line: no
/// text, blank or word holds it.
const CUT_END: &str = "\u{1}";

/// A line of text, its soft-wrapped rows joined, from its first row; and
/// which of its bytes, if any, a piece of output just printed.
#[derive(Default)]
struct Line {
    text: String,
    printed: Vec<bool>,
}

impl Line {
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
            let mut line = Line::default();
            if first.above().is_some_and(|up| up.wrapped()) {
                line.push(CUT_END, false);
            }
            // Rows after the piece's own, once it is passed.
            let mut after: Option<usize> = None;
            let mut row = Some(first);
            while let Some(r) = row {
                let ours = r.id() == print.row;
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
        Line::piece(print, None, (0, print.text.chars().count()))
    }

    /// The piece's own text, its characters `from..to` taken as printed
    /// (what it changed): a line's start only if it starts its row and the
    /// row starts a line, and never its end, which the row may go on past.
    /// A row a line wraps onto goes on from `above`, the last character of
    /// the row above.
    fn piece(print: &fux_vt::Print<'_>, above: Option<char>, (from, to): (usize, usize)) -> Line {
        let mut line = Line::default();
        match above.filter(|_| print.cols.0 == 0) {
            Some(last) => {
                line.push(CUT_END, false);
                line.push(last.encode_utf8(&mut [0; 4]), false);
            }
            None if print.cols.0 > 0 => line.push(CUT_END, false),
            None => {}
        }
        for (n, c) in print.text.chars().enumerate() {
            line.push(c.encode_utf8(&mut [0; 4]), from <= n && n < to);
        }
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

    /// Where the line holds `text` as `found_in` finds it, on bytes that
    /// were just printed: its first such place, as `normal` counts it.
    fn holds(&self, text: &str) -> Option<usize> {
        let (line, printed) = normal_marked(&self.text, &self.printed);
        found_at_normal(&line, text).into_iter().find(|at| {
            let end = at.saturating_add(normal(bare(text)).len());
            printed.get(*at..end).is_some_and(|p| p.iter().any(|b| *b))
        })
    }
}

/// Whether `text` could be shown by an echo of what was `typed`: each of
/// its characters (blanks aside, case ignored) one that was typed, as
/// often, in whatever order the line's editing left them.
fn typed_from(typed: &str, text: &str) -> bool {
    let mut left: Vec<Option<char>> = (typed.chars().flat_map(fold))
        .filter(|c| !c.is_whitespace())
        .map(Some)
        .collect();
    (text.chars().flat_map(fold))
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
        for lower in fold(c) {
            out.push(lower);
            marks.resize(out.len(), mark);
        }
    }
    (out, marks)
}

/// Rows as lines: soft-wrapped rows joined into the lines they wrap, as
/// capture joins them.
fn lines<'a>(rows: impl Iterator<Item = fux_vt::Row<'a>>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let (mut wraps, mut padded) = (false, false);
    for row in rows {
        let text = row.cells_text();
        // A wrapped row's trailing blanks are part of its line.
        let text = match row.wrapped() {
            true => text,
            false => text.trim_end_matches(' ').to_owned(),
        };
        let wide = || row.cells().nth(1).is_some_and(|c| c.is_wide_continuation());
        match out.last_mut() {
            Some(last) if wraps => {
                if padded && wide() {
                    last.pop();
                }
                last.push_str(&text);
            }
            _ => out.push(text),
        }
        wraps = row.wrapped();
        padded = crate::capture::ends_padded(&row);
    }
    out
}

/// Whether the screen's lines show `text`, as `found_in` finds it.
fn on_screen(screen: &fux_vt::Screen, text: &str) -> bool {
    let shown = lines(screen.rows().skip(screen_top(screen)));
    shown.iter().any(|l| found_in(l, text) > 0)
}

/// Where the screen's lines start among the rows kept: at its top row, or
/// at the first row of a line wrapped onto it from history.
fn screen_top(screen: &fux_vt::Screen) -> usize {
    let Some(top) = screen.window().row(0) else {
        return screen.history_len();
    };
    screen
        .row_index(crate::pane::line_top(top).id())
        .unwrap_or(screen.history_len())
}

/// A text as a wait compares it: case ignored, runs of blanks as one, and
/// none at its ends.
fn normal(text: &str) -> String {
    normal_marked(text, &[]).0
}

/// A character as a wait compares it, case aside: lower case, and a
/// final ς as σ, since a word's last Σ lowers to ς only where the word is
/// seen whole.
pub(crate) fn fold(c: char) -> impl Iterator<Item = char> {
    c.to_lowercase().map(|c| if c == 'ς' { 'σ' } else { c })
}

/// The characters `from..to` of `print`, where they went; none if empty.
fn part<'a>(print: &fux_vt::Print<'a>, (from, to): (usize, usize)) -> Option<fux_vt::Print<'a>> {
    let at = |n: usize| {
        print
            .text
            .char_indices()
            .nth(n)
            .map_or(print.text.len(), |(i, _)| i)
    };
    let text = print.text.get(at(from)..at(to)).filter(|t| !t.is_empty())?;
    let col = |n: usize| {
        print
            .cols
            .0
            .saturating_add(u16::try_from(n).unwrap_or(u16::MAX))
    };
    Some(fux_vt::Print {
        cols: (col(from), col(to)),
        text,
        ..*print
    })
}

/// `text` as a wait compares it, case and runs of blanks aside, its
/// ends kept: a run of a line to set against a text.
fn folded(text: &str) -> Vec<char> {
    let mut out: Vec<char> = Vec::new();
    for c in text.chars() {
        match c.is_whitespace() {
            true if out.last() == Some(&' ') => {}
            true => out.push(' '),
            false => out.extend(fold(c)),
        }
    }
    out
}

/// Whether a text found on a line could take in some of `run`, printed on
/// it: one holds the other, or one's end is the other's start.
fn overlaps(run: &[char], text: &[char]) -> bool {
    let holds = |a: &[char], b: &[char]| {
        (0..=a.len().saturating_sub(b.len()))
            .any(|at| a.get(at..).is_some_and(|a| a.starts_with(b)))
    };
    let joins = |a: &[char], b: &[char]| {
        (1..=a.len().min(b.len())).any(|k| {
            a.get(a.len().saturating_sub(k)..)
                .is_some_and(|end| b.starts_with(end))
        })
    };
    holds(run, text) || holds(text, run) || joins(run, text) || joins(text, run)
}

/// How many times a line shows TEXT: case ignored, runs of blanks as one,
/// starting where a word does (`READY` is not in `NOT_READY`), `^` and `$`
/// holding it to the line's start and end.
fn found_in(line: &str, text: &str) -> usize {
    found_at_normal(&normal(line), text).len()
}

/// Where a line already made `normal` shows TEXT, as `found_in` finds it:
/// each place in it.
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
    let (first, _) = crate::capture::pane(pane, None, view(alt), false, false);
    if first.trim().is_empty() {
        return crate::capture::pane(pane, None, view(!alt), false, false).0;
    }
    first
}

/// What came since a wait began: the pane's lines from the first that
/// changed since (on the screen or in history) to its last line with text.
/// A full-screen program's whole screen; keys that went in on a full screen
/// the program has since left, what it printed after, else that last full
/// screen if it drew it since, else the screen as it is.
fn since_text(pane: &Pane, since: Since, marks: bool) -> String {
    let (screen, start) = (pane.screen(), since.start);
    if screen.mode(fux_vt::Mode::AlternateScreen) {
        return text(pane, marks, false);
    }
    // A row changed since, or moved (`fux_vt::Print`).
    let moved: std::collections::HashSet<fux_vt::RowId> = (pane.record.printed_after(start.from))
        .flat_map(|(_, p)| p.prints.iter().map(|r| r.row))
        .collect();
    // A resize gives every row it keeps a new number, and brings nothing.
    let resized: std::collections::HashSet<u64> = (pane.record.after(start.from))
        .filter(|e| matches!(e.event, crate::log::Event::Resize(_)))
        .map(|e| e.seq.0)
        .collect();
    let window = screen.window();
    // A row drawn again to show as it did at the input brings nothing.
    let new = |r: &fux_vt::Row<'_>| {
        (r.version() > start.from.0 && !resized.contains(&r.version()) || moved.contains(&r.id()))
            && (pane.before_shows.get(&r.id())).is_none_or(|was| {
                window.place(r).map(|y| (y, crate::pane::shows(r))).as_ref() != Some(was)
            })
    };
    let Some(first) = screen.rows().find(new) else {
        return match (screen.alternate_rows().any(|r| new(&r)), start.on_full) {
            (true, _) => text(pane, marks, true),
            (false, true) => text(pane, marks, false),
            (false, false) => String::new(),
        };
    };
    let from = (screen.row_index(crate::pane::line_top(first).id())).unwrap_or(0);
    let found = lines(screen.rows().skip(from));
    let texts: Vec<&str> = found.iter().map(|l| l.trim_end()).collect();
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
    use super::{found_in, normal, normal_marked};

    /// A text waited for: case ignored, blanks as one, from a word's start,
    /// and `^`/`$` for a line's start and end.
    /// A wide glyph that did not fit at a row's end wraps onto the next,
    /// and the blank it left is no part of the line, as capture shows it.
    #[test]
    fn a_wide_glyph_wrapped_on_joins_its_line_as_capture_shows_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut parser = fux_vt::Parser::new(fux_vt::Size::new(3, 4)?, 0)?;
        parser.process("aaa界".as_bytes())?;
        assert!(super::on_screen(parser.screen(), "aaa界"));
        Ok(())
    }

    #[test]
    fn a_text_is_found_as_a_reader_would_find_it() {
        assert_eq!(found_in("Server READY on :8080", "ready"), 1);
        assert_eq!(found_in("NOT_READY slot=3", "READY"), 0);
        assert_eq!(found_in("NOT READY slot=3", "READY"), 1);
        assert_eq!(found_in("Plan  [fast/safe]>", "plan [fast"), 1);
        // `^^x` waits for a line starting `^x`: met by its `x` just printed.
        let line = super::Line {
            text: "^x".into(),
            printed: vec![false, true],
        };
        assert_eq!(line.holds("^^x"), Some(0));
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
        // A word's last Σ, written either way, case ignored too, on the
        // screen and in the output read as it comes.
        assert_eq!(found_in("ΟΔΟΣ done", "οδος done"), 1);
        assert_eq!(found_in("οδος", "ΟΔΟΣ"), 1);
        assert_eq!(normal_marked("ΟΔΟΣ", &[]).0, normal("οδος"));
    }
}
