//! `fux wait`, and send-keys' waits: a command line from the CLI that answers
//! once each of its panes has: done what one of its conditions asks, its
//! program exited first, its pane gone, or its time is up. The server holds its
//! connection meanwhile and asks it again whenever a pane may have changed,
//! or its time comes (`Waiting::due`).
//!
//! A wait counts from a number in its pane's log (`crate::log`): what came
//! after it is new to it. That is the input of the action it follows
//! (made once the action has acted, in the same turn), else the last
//! action's input to the pane, or its program's start. Text is looked
//! for in what was printed since, piece by piece as the pane recorded it
//! (`Pane::prints`), on the line each piece went on wherever that line is
//! now (on the screen, in history, or let go), and in the piece's own text,
//! though drawn over since. A match counts only if it holds text the
//! program printed since, never only the echo of what was typed
//! (`crate::classify`).
use crate::after;
use crate::classify::Echo;
use crate::code::Code;
use crate::command::{AnyRef, CaptureView, Source, Until, Wait};
use crate::id::PaneId;
use crate::json::Json;
use crate::log::{Event, Seq};
use crate::pane::{Before, Dropped, Pane, Start};
use crate::process::Ended;
use crate::protocol::{End, Reply};
use crate::ready::{self, State};
use crate::session::Session;
use fux_vt::{Print, Row, RowId};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Said first of what came when lines of it were let go past history's
/// budget.
const CUT: &str = "… earlier lines not kept (past history-bytes)\n";
/// Said first instead when more came than fux keeps for a wait.
const MORE_THAN_KEPT: &str =
    "… more came than the pane's journal keeps (journal-bytes): text may have passed unseen\n";
/// Said first instead when a piece of output printed more than fux
/// records of one (`fux_vt::Prints::lost`): its caps are fux-vt's, said
/// in its README (`Prints`), not again here.
const PRINTS_LOST: &str = "… a piece of output printed more than fux records of one (fux-vt keeps \
     a few bytes of printed text a screen cell between reads, and records nothing past that, or \
     after many runs, or after a resize): text may have passed unseen\n";

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
    /// Each pane it watches, in order, and how far it came there.
    watches: Vec<(PaneId, Look)>,
    pub(crate) wait: Wait,
    /// The texts it looks for (`wait-text`), in order.
    texts: Vec<String>,
    /// When it began: a pane is quiet from then at the soonest.
    began: Instant,
    deadline: Instant,
    /// How long it waits at most: its own timeout, else the server's.
    pub(crate) limit: Duration,
    follows: Follows,
}

/// A pane of a wait: still looked at, or its answer, which no look
/// changes.
enum Look {
    Looking(Watch),
    Told(Reply),
}

/// What a wait follows, which decides how it reads its panes and what its
/// answer holds besides theirs.
pub enum Follows {
    /// Nothing: a bare `wait`, which on a pane that may go on alone
    /// (`waiting`, or quiet with nothing known) waits for something new.
    Wait,
    /// An action (send-keys, send-mouse, signal), counted from its input.
    Action,
    /// The program a command has just started (run, split, respawn): its
    /// screen is blank, so a text is gone only once it has appeared.
    Started,
    /// A capture to give once the pane settles (`capture` from a command
    /// line): its history and view; the client picks text or JSON.
    Capture(Option<usize>, CaptureView),
}

impl Follows {
    /// Whether its program has just started, whether it waits for
    /// something new (a bare `wait`), and whether it follows an action.
    fn mode(&self) -> (bool, bool, bool) {
        match self {
            Follows::Started => (true, false, false),
            Follows::Wait => (false, true, false),
            Follows::Action => (false, false, true),
            Follows::Capture(..) => (false, false, false),
        }
    }
}

/// One pane a wait watches, and what it found as it began.
struct Watch {
    /// What came after this is new to it.
    start: Start,
    /// The rows as they read at `start` (`Pane::before`).
    before: Before,
    /// Pieces of output it had not read yet were let go.
    missed: bool,
    /// A piece of output printed more than it records (`Prints::lost`).
    lost: bool,
    /// The first text found: which, its line, and when it came.
    found: Option<(String, String, Seq)>,
    /// The text printed on each row, runs that go on from each other
    /// joined, though printed in several pieces and between others: where
    /// it starts and ends, and the text.
    stream: HashMap<RowId, (u16, u16, String)>,
    /// The `wait-gone` texts seen since the wait began, for a fresh
    /// program, and the last piece that printed each, if one did.
    appeared: HashMap<String, Option<Seq>>,
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
    /// It waits for a command the input began (a `line`'s) to end, not
    /// only for quiet: any wait but a capture's that counts from an
    /// action's input. A command marked after the program's start (a
    /// full-screen program `run` began, marking its own) is no input's.
    ends: bool,
    /// The screen's highlighted runs as an action's wait began: a run its
    /// input highlighted anew (a selection moved) is said.
    lit: Vec<crate::capture::Lit>,
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
    Settled(State),
    /// The program reads its terminal; the text waited for has not come
    /// (or gone).
    Stopped,
    /// The shell command marked running ended (`wait-done`).
    Done,
}

impl Waiting {
    /// A wait in `panes` from `now`, for `limit` at most unless it says,
    /// after what it `follows`.
    pub fn new(
        panes: &[&Pane],
        wait: Wait,
        follows: Follows,
        now: Instant,
        limit: Duration,
    ) -> Waiting {
        let limit = wait.timeout.unwrap_or(limit);
        let texts = (wait.until.iter())
            .filter_map(|u| match u {
                Until::Text(text) => Some(text.clone()),
                Until::Gone(_) | Until::Exit | Until::Bell | Until::Settled | Until::Done => None,
            })
            .collect();
        Waiting {
            watches: (panes.iter())
                .map(|p| (p.id, Look::Looking(Watch::new(p, &follows))))
                .collect(),
            texts,
            deadline: after(now, limit),
            limit,
            wait,
            began: now,
            follows,
        }
    }

    /// When to ask it again at the latest, whatever happens before: soon,
    /// while it asks what a pane's program is blocked in (that changes with
    /// nothing on the screen to wake the server), else when its time is up.
    pub fn due(&self) -> Instant {
        let now = Instant::now();
        let probe = (self.wait.until.iter())
            .any(Until::probes)
            .then(|| after(now, PROBE));
        // A pane seen settled is looked at again once its hold is up.
        let held = (self.watches.iter())
            .filter_map(|(_, look)| match look {
                Look::Looking(watch) => watch.settled_since,
                Look::Told(..) => None,
            })
            .map(|(since, _)| after(since, HOLD).max(now))
            .min();
        // Past its limit, a pane whose command it lasts for is looked at
        // as one probed is: its end is output, which wakes the server too.
        let deadline = match self.deadline > now {
            true => self.deadline,
            false => after(now, PROBE),
        };
        [probe, held]
            .into_iter()
            .flatten()
            .fold(deadline, Instant::min)
    }

    /// Its answer, if it has one at `now`: in each of its panes, what it
    /// waited for came, its program exited first, it went, or its time is
    /// up.
    pub fn answer(&mut self, session: &Session, now: Instant) -> Option<Reply> {
        let mut outcome = self.look(session, now)?;
        // A capture asked to settle first: its state line, then the capture.
        let capture = match self.follows {
            Follows::Capture(history, view) => Some((history, view)),
            Follows::Wait | Follows::Action | Follows::Started => None,
        };
        let watched = self
            .watches
            .first()
            .and_then(|(id, _)| session.panes.get(id));
        if let (Some((history, view)), Some(pane)) = (capture, watched) {
            let copying = session.copying(pane.id);
            let (read, form) = crate::capture::pane(pane, history, view, true, copying);
            // Its JSON, read at the same moment, for `json` to give.
            outcome.json = form.unwrap_or_default();
            let line = outcome.text.lines().next().unwrap_or_default();
            // Where the cursor is, as the JSON's `cursor` says it of the
            // lines read, said of the screen, as a mouse event counts.
            let live = match view.source {
                Source::Shown | Source::LastOutput => true,
                Source::Main => !pane.screen().mode(fux_vt::Mode::AlternateScreen),
                Source::Alternate => pane.screen().mode(fux_vt::Mode::AlternateScreen),
            };
            let on_text = after_text(pane.screen()) || {
                let (row, col) = pane.screen().cursor_position();
                within_line(pane.screen(), row, col)
            };
            let cursor =
                (cursor(pane).filter(|_| live && on_text)).map(|(note, _)| format!(" · {note}"));
            outcome.text = format!("{line}{}\n{read}", cursor.unwrap_or_default());
        }
        Some(outcome)
    }

    /// Each pane answers on its own, as a lone pane's wait would, and the
    /// wait once every one has: a line from each, in order, with the first
    /// failing status. At the limit, each pane yet to answer is said as it
    /// is.
    fn look(&mut self, session: &Session, now: Instant) -> Option<Reply> {
        let (wait, texts, limit) = (&self.wait, self.texts.as_slice(), self.limit);
        let (window, mode) = ((self.began, now), self.follows.mode());
        let late = now >= self.deadline;
        for (id, look) in &mut self.watches {
            let watch = match look {
                Look::Looking(watch) => watch,
                Look::Told(..) => continue,
            };
            let Some(pane) = session.panes.get(id) else {
                *look = Look::Told(wait.gone(*id));
                continue;
            };
            if let Some(told) = watch.verdict(pane, wait, texts, window, mode) {
                *look = Look::Told(wait.told(pane, told, limit, watch, mode.2));
            } else if late && !watch.lasts(pane, wait) {
                let told = Told::TimedOut(watch.hint(pane, texts));
                *look = Look::Told(wait.told(pane, told, limit, watch, mode.2));
            }
        }
        let answers: Vec<&Reply> = (self.watches.iter())
            .map(|(_, look)| match look {
                Look::Looking(_) => None,
                Look::Told(reply) => Some(reply),
            })
            .collect::<Option<_>>()?;
        if let [one] = answers.as_slice() {
            return Some((*one).clone());
        }
        // The first end with a status other than 0, in order, names the
        // whole: a failure, if one failed, with every pane's why.
        let first = answers.iter().find_map(|a| a.end.code());
        let why: String = (answers.iter())
            .filter_map(|a| match &a.end {
                End::Failed { message, .. } => Some(format!("{}\n", message.trim_end())),
                End::Done | End::Answered(_) => None,
            })
            .collect();
        let end = match first {
            None => End::Done,
            Some(code) if why.is_empty() => End::Answered(code),
            Some(code) => End::Failed { code, message: why },
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

impl Wait {
    /// The outcome of a wait whose pane `id` closed: a settling action's
    /// (a shell that exited) said with status 4, not a failure; any other
    /// wait's a failure.
    fn gone(&self, id: PaneId) -> Reply {
        let end = match self.until.contains(&Until::Settled) {
            true => End::Answered(Code::Closed),
            false => End::Failed {
                code: Code::Closed,
                message: format!("{id}: the pane closed"),
            },
        };
        said(id, "closed".into(), Vec::new(), Default::default(), end)
    }

    /// The outcome of a wait that ended so in `pane`, after an action if
    /// `action`.
    fn told(&self, pane: &Pane, told: Told, limit: Duration, watch: &Watch, action: bool) -> Reply {
        let (id, now) = (pane.id, Instant::now());
        let state = || watch.state(pane, now);
        let came = || since_text(pane, watch, self.marks);
        let output = |body: String| {
            let field = ("output", Json::str(body.clone()));
            (body, vec![field])
        };
        // What met it, or its last lines, then the screen if asked.
        let shown = |first: String, field: Option<(&'static str, Json<'static>)>| {
            let screen = self.print.then(|| text(pane, self.marks, false));
            let body = format!("{first}{}", screen.as_deref().unwrap_or_default());
            let screen = screen.map(|screen| ("screen", Json::str(screen)));
            (body, field.into_iter().chain(screen).collect())
        };
        // A pane's last `n` lines with text, as a capture reads them: of its
        // screen, or once a full-screen program has left and the screen is
        // blank, of what it last drew.
        let last = |n: usize| {
            let text = text(pane, false, false);
            let lines: Vec<&str> = (text.lines().map(str::trim_end))
                .filter(|l| !l.is_empty())
                .collect();
            let tail = lines.get(lines.len().saturating_sub(n)..);
            let lines: String = (tail.unwrap_or_default().iter())
                .map(|l| format!("{l}\n"))
                .collect();
            let field = Some(("output", Json::str(lines.clone())));
            shown(lines, field)
        };
        let failed = |code, message: String| End::Failed { code, message };
        // Why it failed, and why it may never be met, if a reason shows.
        let why = |reason: String, hint: Option<&str>| match hint {
            Some(hint) => format!("{reason}; {hint}"),
            None => reason,
        };
        let (state, met, body, end) = match told {
            // A text that cannot come: the screen, to act on what is there.
            Told::Found(Found::Stopped) => {
                let cut = watch.cut(pane).unwrap_or_default();
                let screen = format!("{cut}{}", text(pane, self.marks, false));
                let end = End::Answered(Code::Stopped);
                (State::Reading, None, output(screen), end)
            }
            // Met: the program's own status is its state's (`exited 3`).
            Told::Found(Found::Exit(ended)) => {
                (State::Exited(ended), None, output(came()), End::Done)
            }
            // A text gone leaves the pane as it is.
            Told::Found(Found::Gone) => (state(), None, output(came()), End::Done),
            // Alone, it was met; with what else it waited for, that did not
            // come while the command ran.
            Told::Found(Found::Done) => {
                let rest: Vec<String> = (self.until.iter())
                    .filter(|u| **u != Until::Done)
                    .map(Until::describe)
                    .collect();
                let end = match rest.is_empty() {
                    true => End::Done,
                    false => {
                        let rest = rest.join(" or ");
                        let reason = format!("{id}: its command ended while waiting for {rest}");
                        failed(Code::Ended, reason)
                    }
                };
                (state(), None, output(came()), end)
            }
            Told::Found(Found::Settled(state)) => {
                let asked = self.print.then(came).unwrap_or_default();
                (state, None, output(asked), End::Done)
            }
            // At its limit a settling wait fails (124) only while busy; `0
            // timeout` answers at once, as it is, and never fails.
            Told::TimedOut(_) if self.until.contains(&Until::Settled) => {
                let state = state();
                let end = match limit.is_zero() || state != State::Busy {
                    true => End::Done,
                    false => End::Answered(Code::Timeout),
                };
                let asked = self.print.then(came).unwrap_or_default();
                (state, None, output(asked), end)
            }
            Told::Found(Found::Text { text, line }) => {
                let note = format!("met {text:?}");
                let body = shown(format!("{line}\n"), None);
                let fields = vec![
                    ("met", Json::str("text")),
                    ("text", Json::str(text)),
                    ("line", Json::str(line)),
                ];
                (state(), Some((note, fields)), body, End::Done)
            }
            Told::Found(Found::Bell(notification)) => {
                let note = (notification.as_ref()).map_or_else(
                    || "met bell".to_owned(),
                    |n| format!("met notification {n:?}"),
                );
                let notification = notification.map_or(Json::Null, Json::str);
                let fields = vec![("met", Json::str("bell")), ("notification", notification)];
                let body = shown(String::new(), None);
                (state(), Some((note, fields)), body, End::Done)
            }
            // Its last words, which an agent would otherwise ask for next: a
            // crash says why.
            Told::Exited(ended, hint) => {
                let told = crate::process::told(ended);
                let reason = format!(
                    "{id}: its program {told} while waiting for {}",
                    self.describe()
                );
                let end = failed(Code::Exited, why(reason, hint));
                (State::Exited(ended), None, last(3), end)
            }
            Told::Replaced => {
                let reason = format!(
                    "{id}: its program was started again (respawn) while waiting for {}",
                    self.describe()
                );
                (
                    state(),
                    None,
                    shown(String::new(), None),
                    failed(Code::Exited, reason),
                )
            }
            // How long the screen was still, so the next step is clear: the
            // state line says what the pane does.
            Told::TimedOut(hint) => {
                let idle = now.saturating_duration_since(pane.changed_at).as_secs();
                let reason = format!(
                    "{id}: timed out after {}s waiting for {}; the screen unchanged for {idle}s",
                    limit.as_secs_f64(),
                    self.describe()
                );
                // How to wait longer, which the limit's 30s often is not.
                let mut longer = format!(
                    "to wait longer, the same with SECS timeout among its options \
                     (`{id} 300 timeout wait`)"
                );
                if pane.command_running() {
                    longer.push_str(&format!(
                        ", or with wait-done for as long as its shell command runs \
                         (`{id} wait-done wait`)"
                    ));
                }
                let end = failed(Code::Timeout, format!("{}; {longer}", why(reason, hint)));
                (state(), None, last(1), end)
            }
        };
        let mut notes = self.notes(pane, state, watch, action);
        // What met it, or with nothing met, what waits on while it is busy.
        match met {
            Some(met) => notes.push(met),
            // A marked shell command: as long as it runs (a `line` at its
            // limit, a build quiet for minutes).
            // At the limit it is still at work, which is no failure: the
            // status (124) says only that the time ran out.
            None if state == State::Busy => {
                let (word, what) = match pane.command_running() {
                    true => ("wait-done wait", "waits for its command to end"),
                    false => ("wait", "waits for it to settle"),
                };
                let at = match limit.is_zero() {
                    true => String::new(),
                    false => format!(
                        "still at work at the {}s limit, not failed: ",
                        limit.as_secs()
                    ),
                };
                notes.push((format!("{at}`{id} {word}` {what}"), Vec::new()));
            }
            None => {}
        }
        said(id, state.word(), notes, body, end)
    }
}

/// How many pieces of input an exited program never read are named.
const LOST: usize = 5;

/// A note of a state line, or a body, and its JSON fields.
type Note = (String, Vec<(&'static str, Json<'static>)>);

/// A pane's answer to a wait, one record said two ways: as text, its state
/// line (`%3 reading · done 0`, the pane, its state and the notes) and then
/// `body`'s text; as JSON, the pane, the state, each note's fields, the
/// status, the code of one that is not 0, and `body`'s fields.
fn said(id: PaneId, state: String, notes: Vec<Note>, body: Note, end: End) -> Reply {
    let mut line = format!("{id} {state}");
    let pane = ("pane", Json::str(id.to_string()));
    let mut fields = vec![pane, ("state", Json::str(state))];
    for (note, said) in notes {
        line.push_str(&format!(" · {note}"));
        fields.extend(said);
    }
    fields.push(("status", Json::int(end.status())));
    fields.extend(end.code().map(|code| ("code", Json::str(code.name()))));
    let (body, said) = body;
    fields.extend(said);
    Reply {
        end,
        text: format!("{line}\n{body}"),
        json: format!("{}\n", Json::Object(fields).render()),
        ids: vec![AnyRef::Pane(id)],
        ..Reply::default()
    }
}

impl Wait {
    /// What a state line says after the state: a shell command that ended,
    /// an action's keys read that drew nothing, keys not read yet.
    fn notes(&self, pane: &Pane, state: State, watch: &Watch, action: bool) -> Vec<Note> {
        let mut notes = Vec::new();
        let commands = pane.screen().commands();
        if commands.finished > watch.start.finished {
            let status = commands.last_status;
            let said = status.map_or_else(|| "done".to_owned(), |s| format!("done {s}"));
            notes.push((said, vec![("done", status.map_or(Json::Null, Json::int))]));
        }
        // A marked command runs on, though the pane settled: one the keys
        // went into, or one that took the full screen. (Busy, it waits on.)
        let running = pane.command_running();
        if running && state != State::Busy {
            let field = ("command_running", Json::Bool(true));
            notes.push(("command running".into(), vec![field]));
        }
        // What it draws next may differ, keys or not.
        if state == State::Redrawing {
            let field = ("redraws", Json::Bool(true));
            notes.push(("redraws on its own".into(), vec![field]));
        }
        // Not a full-screen program, nor a line editor reading (bracketed
        // paste on, as Python's REPL has it): it may well be at work
        // still, to end on its own, which it is waited for so. One blocked
        // off its terminal (on the network: `koh exec`, a download) may as
        // well wait for input there, which fux cannot see. One that redraws
        // on its own read every key and drew after them: not at work on them.
        let waiting = state == State::Waiting;
        let screen = pane.screen();
        let keys = [fux_vt::Mode::AlternateScreen, fux_vt::Mode::BracketedPaste];
        // A remote client that reports nothing of what it runs: fux sees
        // the client, not that, and says so, with the wait that is right.
        let remote = (!matches!(state, State::Exited(_) | State::Absent))
            .then(|| pane.remote())
            .flatten();
        if let Some(remote) = &remote {
            let (id, name) = (pane.id, &remote.name);
            // Said on every answer, so kept short: what waits right there.
            let (wait, word) = match remote.terminal {
                true => (
                    format!("`{id} str TEXT wait-text wait` waits for a text"),
                    "wait-text",
                ),
                // Ended with its command: the client's end, or the shell
                // command's that runs it.
                false => {
                    let word = if running { "wait-done" } else { "wait-exit" };
                    (format!("`{id} {word} wait` waits for its end"), word)
                }
            };
            let fields = vec![
                ("remote", Json::str(name.clone())),
                ("wait_with", Json::str(word)),
            ];
            notes.push((
                format!("{name} is remote, its state unseen: {wait}"),
                fields,
            ));
        } else if waiting && !keys.into_iter().any(|m| screen.mode(m)) {
            let word = if running { "wait-done" } else { "wait-exit" };
            let id = pane.id;
            let why = if crate::ready::owed(pane) {
                "its relay said nothing of its program since your input, as when that ended"
            } else if crate::ready::elsewhere(pane, Instant::now()) {
                "not reading its terminal, so fux cannot tell when it waits for input"
            } else {
                "may be at work"
            };
            notes.push((
                format!("{why}: `{id} {word} wait` waits for its end"),
                vec![("wait_with", Json::str(word))],
            ));
        }
        // What an action's keys moved, which plain text does not show: the
        // cursor, where text goes on under or right of it (a line editor's
        // after Left or Home, an editor's), not at the end of what was
        // typed, unless the keys moved it alone (End); and a selection
        // drawn in reverse video or a background of its own, newly
        // highlighted. A few runs at most: more is a screen drawn anew,
        // shown whole.
        if action && pane.process.child().is_some() {
            let screen = pane.screen();
            let (row, col) = screen.cursor_position();
            // A line edited where a program reads it (a line editor, a
            // REPL), changed: where the cursor is, at the end of it too.
            let edit = pane.edited
                && matches!(state, State::Reading | State::Settled)
                && !screen.mode(fux_vt::Mode::AlternateScreen)
                && pane.revision != watch.revision
                && after_text(screen);
            let said = edit || within_line(screen, row, col) || moved_alone(screen, watch);
            notes.extend(cursor(pane).filter(|_| said));
            let lit: Vec<_> = (crate::capture::highlights(screen).into_iter())
                .filter(|l| {
                    !watch
                        .lit
                        .iter()
                        .any(|b| (b.row, b.col, b.len) == (l.row, l.col, l.len))
                })
                .collect();
            if !lit.is_empty() && lit.len() <= 3 {
                let said: Vec<String> = (lit.iter())
                    .map(|l| {
                        let end = l.col.saturating_add(l.len).saturating_sub(1);
                        format!("{},{}-{end} {:?}", l.row, l.col, l.text.trim())
                    })
                    .collect();
                let field = Json::Array(
                    (lit.into_iter())
                        .map(|l| {
                            Json::Object(vec![
                                ("row", Json::int(l.row)),
                                ("col", Json::int(l.col)),
                                ("len", Json::int(l.len)),
                                ("text", Json::str(l.text)),
                            ])
                        })
                        .collect(),
                );
                notes.push((
                    format!("highlight {}", said.join(", ")),
                    vec![("highlight", field)],
                ));
            }
        }
        // A line typed that a shell in front reads again after, where
        // nothing has marked a command (a shell fux did not start, `koh
        // connect SERVER -- sh`): no `done`, and no status, can come. A
        // REPL's, a calculator's line is no command a shell could mark.
        let marked = commands.started > 0 || commands.finished > 0 || commands.prompt;
        let main = !pane.screen().mode(fux_vt::Mode::AlternateScreen);
        let unmarked = action && pane.line_ended && state == State::Reading && !marked && main;
        if unmarked && pane.shell_in_front() {
            let field = ("marked", Json::Bool(false));
            notes.push((
                "no status: nothing here marks its commands".into(),
                vec![field],
            ));
        }
        if action && pane.revision == watch.revision && state == State::Reading {
            notes.push(("no change".into(), vec![("no_change", Json::Bool(true))]));
        }
        // What of the action's input its program, exited, never read.
        let (lost, field, said) = match &pane.lost {
            Dropped::Unread(lost) => (
                lost,
                "lost",
                "it exited before reading {}, which went nowhere",
            ),
            Dropped::Unanswered(lost) => (
                lost,
                "unanswered",
                "it ended before its relay said its program read {}, which likely went nowhere",
            ),
            Dropped::None => (&Vec::new(), "", ""),
        };
        if action && !lost.is_empty() {
            let mut names = lost.iter().take(LOST).cloned().collect::<Vec<_>>();
            let more = lost.len().saturating_sub(LOST);
            if more > 0 {
                names.push(format!("{more} more"));
            }
            let list = Json::Array(lost.iter().cloned().map(Json::str).collect());
            notes.push((said.replace("{}", &names.join(", ")), vec![(field, list)]));
        }
        let queued = pane.input_queued();
        if queued > 0 && pane.process.exit_status().is_none() {
            notes.push((
                format!("{queued} bytes unread"),
                vec![("unread", Json::int(queued))],
            ));
        }
        notes
    }

    /// Its conditions, as an error says what did not come.
    fn describe(&self) -> String {
        (self.until.iter())
            .map(Until::describe)
            .collect::<Vec<_>>()
            .join(" or ")
    }
}

/// Where the cursor of `pane`'s program is, as a note: its row and column
/// on the screen, from 0, while its program runs and shows it.
fn cursor(pane: &Pane) -> Option<Note> {
    let screen = pane.screen();
    let shows = pane.process.child().is_some() && screen.mode(fux_vt::Mode::ShowCursor);
    let (row, col) = screen.cursor_position();
    let at = Json::Array(vec![Json::int(row), Json::int(col)]);
    let mut note = (format!("cursor {row},{col}"), vec![("cursor", at)]);
    // On a line wrapped at the pane's width, which answers print joined:
    // the screen rows it takes, as the cursor counts them.
    if let Some((from, rows)) = wrapped_rows(screen, row) {
        let to = from.saturating_add(u16::try_from(rows.len().saturating_sub(1)).unwrap_or(0));
        let said: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
        note.0
            .push_str(&format!(" on rows {from}-{to} {}", said.join(" ")));
        let texts = Json::Array(rows.into_iter().map(Json::str).collect());
        let fields = vec![("from", Json::int(from)), ("text", texts)];
        note.1.push(("cursor_rows", Json::Object(fields)));
    }
    shows.then_some(note)
}

/// The rows of the screen that the line holding `row` takes, if it is
/// wrapped at the pane's width (four at most, around `row`): the first,
/// and each row's text.
fn wrapped_rows(screen: &fux_vt::Screen, row: u16) -> Option<(u16, Vec<String>)> {
    let window = screen.window();
    let wraps = |y: u16| window.row(y).is_some_and(|r| r.continues());
    let mut from = row;
    while from > 0 && row.saturating_sub(from) < 3 && wraps(from.saturating_sub(1)) {
        from = from.saturating_sub(1);
    }
    let mut to = row;
    while wraps(to) && to.saturating_sub(from) < 3 && to.saturating_add(1) < window.rows() {
        to = to.saturating_add(1);
    }
    let text = |y: u16| {
        let row = window.row(y)?;
        let cells: String = (row.cells().filter(|c| !c.is_wide_continuation()))
            .map(|c| c.shown())
            .collect();
        Some(cells.trim_end().to_owned())
    };
    (from < to).then(|| (from, (from..=to).filter_map(text).collect()))
}

/// Whether the screen shows text left of the cursor on its row: at the
/// end of a line typed or printed, not on a blank row.
fn after_text(screen: &fux_vt::Screen) -> bool {
    let (row, col) = screen.cursor_position();
    let upto = usize::from(col).saturating_add(usize::from(screen.pending_wrap()));
    (screen.window().row(row))
        .is_some_and(|r| r.cells().take(upto).any(|c| !c.shown().trim().is_empty()))
}

/// Whether the screen shows text under the cursor at `row`, `col`, or
/// right of it on its row: the cursor is within a line, not at its end.
/// A glyph in the last column with a wrap pending is behind the cursor.
fn within_line(screen: &fux_vt::Screen, row: u16, col: u16) -> bool {
    let from = usize::from(col).saturating_add(usize::from(screen.pending_wrap()));
    (screen.window().row(row))
        .is_some_and(|r| r.cells().skip(from).any(|c| !c.shown().trim().is_empty()))
}

/// Whether the cursor moved since `watch` began, every row showing as it
/// did then: the input moved the cursor alone.
fn moved_alone(screen: &fux_vt::Screen, watch: &Watch) -> bool {
    let window = screen.window();
    screen.cursor_position() != watch.start.cursor
        && (0..window.rows()).all(|y| {
            window.row(y).is_some_and(|r| {
                let was = watch.before.get(&r.id()).map(|(_, was)| *was);
                was == Some((y, crate::pane::shows(&r)))
            })
        })
}

impl Until {
    /// Whether it waits on what the screen shows: a text, or a text gone.
    fn shows(&self) -> bool {
        match self {
            Until::Text(_) | Until::Gone(_) => true,
            Until::Exit | Until::Bell | Until::Settled | Until::Done => false,
        }
    }

    /// Whether it asks what the pane's program is blocked in, which changes
    /// with nothing on the screen to wake the server: to settle, or to know
    /// a text shown or gone cannot come (status 7).
    fn probes(&self) -> bool {
        match self {
            Until::Text(_) | Until::Gone(_) | Until::Settled => true,
            Until::Exit | Until::Bell | Until::Done => false,
        }
    }

    /// The condition as an error says what did not come.
    fn describe(&self) -> String {
        match self {
            Until::Text(text) => format!("{text:?} to show"),
            Until::Gone(text) => format!("{text:?} to go"),
            Until::Exit => "the program to exit".into(),
            Until::Bell => "a bell".into(),
            Until::Settled => "the pane to settle".into(),
            Until::Done => "its shell command to end".into(),
        }
    }
}

impl Watch {
    /// How `pane` stands as a wait begins: it counts from the last action's
    /// input to the pane (the action's own, for a wait that follows one), or
    /// its program's start; what it `follows` says whether it waits for a
    /// command that input began to end.
    fn new(pane: &Pane, follows: &Follows) -> Watch {
        let start = pane.since;
        Watch {
            start,
            before: pane.before.clone(),

            missed: false,
            lost: false,
            found: None,
            stream: HashMap::new(),
            appeared: HashMap::new(),
            looked: start.from,

            bell: None,
            echo_skipped: false,
            revision: pane.revision,
            starts: pane.starts,
            reading_since: None,
            settled_since: None,
            ends: start.input && !matches!(follows, Follows::Capture(..)),
            lit: match follows {
                Follows::Action => crate::capture::highlights(pane.screen()),
                Follows::Wait | Follows::Started | Follows::Capture(..) => Vec::new(),
            },
        }
    }

    /// Whether `wait` lasts in `pane` past its limit: it waits for the
    /// shell command there to end (`wait-done`), given no time limit, and
    /// one runs, or ended and the pane has yet to settle after.
    fn lasts(&self, pane: &Pane, wait: &Wait) -> bool {
        let commands = pane.screen().commands();
        wait.timeout.is_none()
            && wait.until.contains(&Until::Done)
            && (commands.running() || commands.finished > self.start.finished)
    }

    /// What `pane`'s program is doing at `now`, as this wait sees it: a
    /// command its input began is waited for, if it waits for that.
    fn state(&self, pane: &Pane, now: Instant) -> State {
        ready::state(pane, now, self.ends.then_some(self.start.started))
    }

    /// What the wait has come to in this pane, if anything: what it waited
    /// for, or why that can no longer come; and what came since it began.
    fn verdict(
        &mut self,
        pane: &Pane,
        wait: &Wait,
        texts: &[String],
        window: (Instant, Instant),
        mode: (bool, bool, bool),
    ) -> Option<Told> {
        // An action's wait answers once all its keys went in: what the
        // first brought is no answer to the rest.
        if mode.2 && pane.starts == self.starts && pane.holds_keys() {
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
        let there = texts.iter().any(|t| on_screen(pane, t));
        there.then_some(
            "the text is on the screen, but on a line that reads as it did when the wait \
             began (nothing printed it since; drawn again the same, it brings nothing new); \
             read it with `PANE capture`",
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
        if self.lost {
            return Some(
                "a piece of output printed more than fux records of one (fux-vt keeps a few \
                 bytes of printed text a screen cell between reads, and records nothing past \
                 that, or after many runs, or after a resize): the text may have come unseen",
            );
        }
        self.echo_skipped.then_some(
            "the text was only in the echo of what was typed, which a wait never takes; \
             wait for text only the output has, or read the screen with `PANE capture`",
        )
    }

    /// What the pane shows of what the wait asks, if anything.
    fn look(
        &mut self,
        pane: &Pane,
        wait: &Wait,
        texts: &[String],
        (began, now): (Instant, Instant),
        (fresh, more, action): (bool, bool, bool),
    ) -> Option<Found> {
        // Any wait's answer reads the journal: past its budget, it says so.
        self.missed |= pane.record.lost_after(self.looked);
        self.lost |= pane.record.prints_lost_after(self.looked);
        if !texts.is_empty() && self.found.is_none() {
            self.read_prints(pane, texts, action);
        }
        // What a piece printed past what it records may be on a row of the
        // screen that changed since the wait began.
        if self.lost && self.found.is_none() {
            let window = pane.screen().window();
            let changed: String = (0..pane.screen().size().rows())
                .filter_map(|y| window.row(y))
                .filter(|row| row.version() > self.start.from.0)
                .map(|row| format!("{}\n", row.cells_text().trim_end()))
                .collect();
            self.found = first_line(&changed, texts).map(|(t, l)| (t, l, pane.record.last()));
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
        let mut gone = None;
        let mut exit = None;
        let mut bell = None;
        let mut settled = None;
        let mut done = None;
        for until in &wait.until {
            match until {
                Until::Text(_) => {}
                Until::Gone(text) => {
                    let present = on_screen(pane, text);
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
                Until::Settled => settled = self.settled(pane, (began, now), (fresh, more)),
                Until::Bell => bell = self.bell.clone(),
                // Ended, and the pane settled after: the shell at its prompt.
                Until::Done if pane.screen().commands().finished > self.start.finished => {
                    let settled = self.settled(pane, (began, now), (fresh, false));
                    done = settled.map(|_| Found::Done);
                }
                Until::Done => {}
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
        let found = (first.map(|(found, _)| found))
            .or(exit)
            .or(done)
            .or(settled);
        if found.is_some() {
            return found;
        }
        // A text that cannot come, or go: the program reads its terminal,
        // having had a moment to print after what began the wait, seen so
        // on more than one probe, never on one look alone.
        let shown = (wait.until.iter()).any(Until::shows);
        if !shown || wait.until.contains(&Until::Settled) {
            return None;
        }
        if self.state(pane, now) != State::Reading {
            self.reading_since = None;
            return None;
        }
        let since = *self.reading_since.get_or_insert(now);
        let moment = ready::SETTLE.saturating_mul(2);
        let held = now >= after(began, moment) && now >= after(since, moment);
        held.then_some(Found::Stopped)
    }

    /// Reads what was printed since it last read, in the order it came:
    /// the first of `texts` that a piece of output printed, on its line or
    /// in the piece itself, is found.
    fn read_prints(&mut self, pane: &Pane, texts: &[String], action: bool) {
        // Rows scrolled off are printed on no more: a long wait lets them go.
        crate::pane::prune(&mut self.stream, pane.screen());
        let pieces = pane.record.printed_after(self.looked.max(self.start.from));
        let reach = texts.iter().map(|t| t.chars().count()).max().unwrap_or(0);
        // The characters of the texts waited for, case and blanks aside.
        let wanted: Vec<String> = (texts.iter()).map(|t| normal(bare(t))).collect();
        let runs = pieces.flat_map(|(seq, p)| p.runs().map(move |(r, e, f)| (seq, r, e, f)));
        for (seq, whole, echo, fresh) in runs {
            // Only what it changed is news: what it printed as its row held
            // it as the action began (`Pane::before`) is drawn again.
            let fresh = match self.before.get(&whole.row).filter(|_| action) {
                Some((row, _)) => {
                    let echo = *echo != Echo::Not;
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
            let output = *echo == Echo::Not;
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
            let run = normal(print.text);
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
                .filter(Row::wrapped)
                .and_then(|up| up.copied(0, usize::MAX).last())
                .and_then(|(_, text)| text.chars().last());
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
                &Print {
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
                Echo::Not => false,
                Echo::Is => true,
                // The line as typed since, if no key submitted it: later
                // keys edit what this piece showed.
                Echo::Of(typed) => {
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
        (fresh, more): (bool, bool),
    ) -> Option<Found> {
        let state = self.state(pane, now);
        // A bare `wait` on a pane that may go on alone (`waiting`, or quiet
        // with nothing known) waits for something new, what it waits on or
        // a change, for `MORE` at most.
        let more = more && now < after(began, MORE) && pane.revision == self.revision;
        // A program just started that reads its terminal before it printed
        // anything (ssh, `koh connect`: they take keys while they connect)
        // has yet to show what it is: it settles once it prints, or once
        // quiet as long as a program that waits on something else.
        let blank = fresh
            && pane.record.printed_after(self.start.from).next().is_none()
            && now < after(pane.changed_at, ready::QUIET);
        let unsettled = match state {
            State::Busy => true,
            State::Waiting | State::Redrawing | State::Settled => more,
            // An exit settles at once.
            State::Exited(_) => return Some(Found::Settled(state)),
            State::Reading => blank,
            State::Absent => false,
        };
        if unsettled {
            self.settled_since = None;
            return None;
        }
        // Settled on two looks a hold apart, the screen as it was: output
        // still in the terminal's buffers, which the kernel has yet to hand
        // fux, is in by then.
        let since = (self.settled_since)
            .filter(|since| since.1 == pane.revision)
            .unwrap_or((now, pane.revision));
        self.settled_since = Some(since);
        (now >= after(since.0, HOLD)).then_some(Found::Settled(state))
    }

    /// Whether what came since it began was more than the pane keeps, and
    /// the notice that says so: some was let go before the wait read it.
    fn cut(&self, pane: &Pane) -> Option<&'static str> {
        match (self.missed, self.lost, pane.cut()) {
            (true, _, _) => Some(MORE_THAN_KEPT),
            (false, true, _) => Some(PRINTS_LOST),
            (false, false, true) => Some(CUT),
            (false, false, false) => None,
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
    fn of(pane: &Pane, print: &Print<'_>, reach: usize) -> Line {
        let screen = pane.screen();
        if let Some(at) = screen.row_by_id(print.row) {
            let width = usize::from(screen.size().cols()).max(1);
            let rows = reach.div_ceil(width).saturating_add(1);
            let mut first = at;
            for _ in 0..rows {
                match first.above().filter(Row::continues) {
                    Some(up) => first = up,
                    None => break,
                }
            }
            let mut line = Line::default();
            if first.above().is_some_and(|up| up.continues()) {
                line.push(CUT_END, false);
            }
            // Rows after the piece's own, once it is passed.
            let mut after: Option<usize> = None;
            let mut row = Some(first);
            while let Some(r) = row {
                let ours = r.id() == print.row;
                for (col, text) in r.copied(0, usize::MAX) {
                    let col = u16::try_from(col).unwrap_or(u16::MAX);
                    line.push(text, ours && print.cols.0 <= col && col < print.cols.1);
                }
                after = match (ours, after) {
                    (true, _) => Some(0),
                    (false, Some(n)) => Some(n.saturating_add(1)),
                    (false, None) => None,
                };
                row = r.below().filter(|_| r.continues());
                if row.is_some() && after.is_some_and(|n| n >= rows) {
                    line.push(CUT_END, false);
                    return line;
                }
            }
            // Without the blanks at its end.
            line.text = line.text.trim_end_matches(' ').to_owned();
            line.printed.truncate(line.text.len());
            return line;
        }
        Line::piece(print, None, (0, print.text.chars().count()))
    }

    /// The piece's own text, its characters `from..to` taken as printed
    /// (what it changed): a line's start only if it starts its row and the
    /// row starts a line, and never its end, which the row may go on past.
    /// A row a line wraps onto goes on from `above`, the last character of
    /// the row above.
    fn piece(print: &Print<'_>, above: Option<char>, (from, to): (usize, usize)) -> Line {
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

/// The pane's lines from row `from` on, of the rows it keeps (history
/// first), joined and trimmed as `capture` prints them: empty if none has
/// text.
fn lines_from(pane: &Pane, from: usize) -> String {
    let kept = pane.screen().history_len();
    let view = CaptureView {
        rows: (u16::try_from(from.saturating_sub(kept)).ok()).map(|top| (top, u16::MAX)),
        ..CaptureView::default()
    };
    let (text, _) = crate::capture::pane(pane, Some(kept.saturating_sub(from)), view, false, false);
    if text == "\n" { String::new() } else { text }
}

/// The first of `texts` that a line of `lines` holds, and that line.
fn first_line(lines: &str, texts: &[String]) -> Option<(String, String)> {
    lines.lines().find_map(|line| {
        (texts.iter())
            .find(|t| found_in(line, t) > 0)
            .map(|t| (t.clone(), line.to_owned()))
    })
}

/// Whether the screen's lines show `text`, as `found_in` finds it.
fn on_screen(pane: &Pane, text: &str) -> bool {
    let shown = lines_from(pane, screen_top(pane.screen()));
    shown.lines().any(|l| found_in(l, text) > 0)
}

/// Where the screen's lines start among the rows kept: at its top row, or
/// at the first row of a line wrapped onto it from history.
fn screen_top(screen: &fux_vt::Screen) -> usize {
    (screen.window().row(0))
        .and_then(|top| screen.row_index(crate::pane::line_top(top).id()))
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
fn part<'a>(print: &Print<'a>, (from, to): (usize, usize)) -> Option<Print<'a>> {
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
    Some(Print {
        cols: (col(from), col(to)),
        text,
        ..*print
    })
}

/// Whether a text found on a line could take in some of `run`, printed on
/// it, both `normal`: one holds the other, or one's end is the other's
/// start. A run's normal form is a part of its line's, so a text the line
/// holds over a character the run printed passes; a run of blanks alone
/// is `""`, which every text holds.
fn overlaps(run: &str, text: &str) -> bool {
    let joins = |a: &str, b: &str| {
        (1..=a.len().min(b.len())).any(|k| {
            a.get(a.len().saturating_sub(k)..)
                .is_some_and(|end| b.starts_with(end))
        })
    };
    run.contains(text) || text.contains(run) || joins(run, text) || joins(text, run)
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
        marks,
        source: if alt {
            Source::Alternate
        } else {
            Source::Shown
        },
        ..CaptureView::default()
    };
    let (first, _) = crate::capture::pane(pane, None, view(alt), false, false);
    if first.trim().is_empty() {
        return crate::capture::pane(pane, None, view(!alt), false, false).0;
    }
    first
}

/// Where what came since a wait began starts on a full screen drawn as a
/// shell's scrolls (a relayed shell's, `koh connect`'s or mosh's): if the
/// line typed on was the screen's last with text, it reads now as it did
/// then and more (a prompt, and the line typed at it), and every row above
/// it shows as it did, moved up as far as the screen scrolled: that line,
/// where it is now. Else a full-screen program's whole screen is what came.
fn scrolled_on(pane: &Pane, watch: &Watch) -> Option<usize> {
    let screen = pane.screen();
    let window = screen.window();
    let typed = usize::from(watch.start.cursor.0);
    let mut was = vec![None; usize::from(window.rows())];
    for (cells, (y, shows)) in watch.before.values() {
        if let Some(row) = was.get_mut(usize::from(*y)) {
            *row = Some((cells, *shows));
        }
    }
    let below = was.get(typed.saturating_add(1)..).unwrap_or_default();
    if below
        .iter()
        .any(|row| row.is_some_and(|(cells, _)| !cells.blank()))
    {
        return None;
    }
    let (line, _) = (*was.get(typed)?)?;
    if line.blank() {
        return None;
    }
    let now: Vec<Option<u64>> = (0..window.rows())
        .map(|y| window.row(y).map(|row| crate::pane::shows(&row)))
        .collect();
    let up = (0..=typed).find(|up| {
        (0..typed.saturating_sub(*up)).all(|y| {
            let then = was.get(y.saturating_add(*up)).copied().flatten();
            then.map(|(_, shows)| shows) == now.get(y).copied().flatten()
        })
    })?;
    let row = window.row(u16::try_from(typed.saturating_sub(up)).ok()?)?;
    crate::pane::row_cells(&row)
        .starts_with(line)
        .then(|| screen.row_index(crate::pane::line_top(row).id()))?
}

/// What came since a wait began: the pane's lines from the first that
/// changed since (on the screen or in history) to its last line with text.
/// A full-screen program's whole screen; keys that went in on a full screen
/// the program has since left, what it printed after, else that last full
/// screen if it drew it since, else the screen as it is. One that ended
/// there (an editor quit, its program gone over ssh): its last full screen,
/// which leaving it hid, then what came on the main screen after.
fn since_text(pane: &Pane, watch: &Watch, marks: bool) -> String {
    let (screen, start) = (pane.screen(), watch.start);
    if screen.mode(fux_vt::Mode::AlternateScreen) {
        return match start.on_full.then(|| scrolled_on(pane, watch)).flatten() {
            Some(from) => lines_from(pane, from),
            None => text(pane, marks, false),
        };
    }
    // Its last full screen, which leaving it hid, if it ended there.
    let last = (start.on_full && pane.process.exit_status().is_some()).then(|| {
        let view = CaptureView {
            marks,
            source: Source::Alternate,
            ..CaptureView::default()
        };
        crate::capture::pane(pane, None, view, false, false).0
    });
    // A row changed since, or moved (`Print`).
    let moved: HashSet<RowId> = (pane.record.printed_after(start.from))
        .flat_map(|(_, p)| p.prints.iter().map(|r| r.row))
        .collect();
    // A resize gives every row it keeps a new number, and brings nothing.
    let resized: HashSet<u64> = (pane.record.after(start.from))
        .filter(|e| match e.event {
            Event::Resize(_) => true,
            Event::Output(_)
            | Event::Bell
            | Event::Notification(_)
            | Event::Title(_)
            | Event::Done(_) => false,
        })
        .map(|e| e.seq.0)
        .collect();
    let window = screen.window();
    // A row drawn again to show as it did at the input brings nothing, nor
    // does one in history now that nothing printed in since and that reads
    // as it did then, if on the screen (its line ended, the top row erased
    // under it, maybe after a late redraw scrolled it there).
    let kept = |r: &Row<'_>| {
        window.place(r).is_none()
            && !moved.contains(&r.id())
            && (watch.before.get(&r.id())).is_none_or(|(_, (_, was))| *was == crate::pane::shows(r))
    };
    let new = |r: &Row<'_>| {
        !kept(r)
            && (r.version() > start.from.0 && !resized.contains(&r.version())
                || moved.contains(&r.id()))
            && (watch.before.get(&r.id())).is_none_or(|(_, was)| {
                window.place(r).map(|y| (y, crate::pane::shows(r))).as_ref() != Some(was)
            })
    };
    let Some(first) = screen.rows().find(new) else {
        return match (
            last,
            screen.alternate_rows().any(|r| new(&r)),
            start.on_full,
        ) {
            (Some(last), _, _) => last,
            (None, true, _) => text(pane, marks, true),
            (None, false, true) => text(pane, marks, false),
            (None, false, false) => String::new(),
        };
    };
    let from = (screen.row_index(crate::pane::line_top(first).id())).unwrap_or(0);
    let lead = watch.cut(pane).unwrap_or_default();
    format!(
        "{}{lead}{}",
        last.unwrap_or_default(),
        lines_from(pane, from)
    )
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
        let (id, size) = (crate::id::PaneId::of(1), fux_vt::Size::new(3, 4)?);
        let mut pane = super::Pane::new(id, "sh".into(), crate::pane::Runs::Shell, size, 0)?;
        pane.output("aaa界".as_bytes());
        assert!(super::on_screen(&pane, "aaa界"));
        Ok(())
    }

    /// A pane that has answered is looked at no more: only the one still
    /// to answer sets when the wait is asked again. (Its settled pane once
    /// asked for "now" on every turn, and the server spun a core.)
    #[test]
    fn an_answered_pane_sets_no_time_to_look_again() -> Result<(), String> {
        use crate::command::{Until, Wait};
        use std::time::{Duration, Instant};
        let mut session = crate::session::testing::started(crate::config::Config::default())?;
        crate::session::testing::run(&mut session, "%1 horizontal split")?;
        let ids = [crate::id::PaneId::of(1), crate::id::PaneId::of(2)];
        let panes: Vec<_> = ids.iter().filter_map(|id| session.panes.get(id)).collect();
        let wait = Wait {
            until: vec![Until::Settled],
            timeout: None,
            print: false,
            marks: false,
        };
        let began = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .ok_or("no instant a second back")?;
        let limit = Duration::from_secs(60);
        let mut waiting = super::Waiting::new(&panes, wait, super::Follows::Wait, began, limit);
        assert!(waiting.answer(&session, began).is_none());
        // %2 changes on screen before its hold is up, so it settles later;
        // %1, still, answers at its hold.
        let two = session.panes.get_mut(&ids[1]).ok_or("no %2")?;
        two.revision += 1;
        let later = crate::after(began, super::HOLD);
        assert!(waiting.answer(&session, later).is_none());
        let two = session.panes.get_mut(&ids[1]).ok_or("no %2")?;
        two.revision += 1;
        let far = crate::after(began, Duration::from_secs(2));
        assert!(waiting.answer(&session, far).is_none());
        assert!(waiting.due() > Instant::now());
        Ok(())
    }

    /// A text printed and drawn over in one piece is still met, from the
    /// piece's record; past the record's cap (`fux_vt::Prints::lost`), a
    /// text printed after what it keeps is met on the line it changed, and
    /// the answer says the record was cut.
    #[test]
    fn a_text_printed_past_the_record_s_cap_is_met_on_its_line() -> Result<(), String> {
        use crate::command::{Until, Wait};
        use std::time::{Duration, Instant};
        let (now, limit) = (Instant::now(), Duration::from_secs(60));
        let id = crate::id::PaneId::of(1);
        let waiting = |session: &crate::session::Session, text: &str| {
            let wait = Wait {
                until: vec![Until::Text(text.into())],
                timeout: None,
                print: false,
                marks: false,
            };
            let pane = session.panes.get(&id).ok_or("no %1")?;
            Ok::<_, String>(super::Waiting::new(
                &[pane],
                wait,
                super::Follows::Wait,
                now,
                limit,
            ))
        };
        // Drawn over, below the cap: kept.
        let mut session = crate::session::testing::started(crate::config::Config::default())?;
        let mut wait = waiting(&session, "SECRET")?;
        let pane = session.panes.get_mut(&id).ok_or("no %1")?;
        pane.output(b"SECRET\rxxxxxx\r\n");
        let answer = wait.answer(&session, now).ok_or("not met 1")?;
        assert!(answer.text.contains("met \"SECRET\""), "{}", answer.text);
        // Kept before the cap, then 65,535 copies of `x`, past it: met.
        let mut session = crate::session::testing::started(crate::config::Config::default())?;
        let mut wait = waiting(&session, "SECRET")?;
        let pane = session.panes.get_mut(&id).ok_or("no %1")?;
        pane.output(b"SECRET\rxxxxxx\x1b[65535b\r\n");
        let answer = wait.answer(&session, now).ok_or("not met 2")?;
        assert!(answer.text.contains("met \"SECRET\""), "{}", answer.text);
        // Printed after the cap (16 KiB on this screen): not in the record,
        // but on a line it changed.
        let mut session = crate::session::testing::started(crate::config::Config::default())?;
        let mut wait = waiting(&session, "LATE")?;
        let pane = session.panes.get_mut(&id).ok_or("no %1")?;
        pane.output(b"x\x1b[20000b\r\n\x1b[2J\x1b[HLATE\r\n");
        assert!(pane.record.prints_lost_after(crate::log::Seq(0)));
        let answer = wait.answer(&session, now).ok_or("not met 3")?;
        assert!(answer.text.contains("met \"LATE\""), "{}", answer.text);
        Ok(())
    }

    /// `cost$$` waits for a line ending `cost$`: the `$` printed after
    /// `cost`, apart from it, meets it. Only the last `$` marks the end.
    #[test]
    fn a_text_s_own_dollar_printed_alone_meets_it() -> Result<(), String> {
        use crate::command::{Until, Wait};
        use std::time::{Duration, Instant};
        let mut session = crate::session::testing::started(crate::config::Config::default())?;
        let id = crate::id::PaneId::of(1);
        let pane = session.panes.get(&id).ok_or("no %1")?;
        let wait = Wait {
            until: vec![Until::Text("cost$$".into())],
            timeout: None,
            print: false,
            marks: false,
        };
        let (now, limit) = (Instant::now(), Duration::from_secs(60));
        let mut waiting = super::Waiting::new(&[pane], wait, super::Follows::Wait, now, limit);
        let pane = session.panes.get_mut(&id).ok_or("no %1")?;
        pane.output(b"cost");
        assert!(waiting.answer(&session, now).is_none());
        let pane = session.panes.get_mut(&id).ok_or("no %1")?;
        pane.output(b"$\r\n");
        let answer = waiting.answer(&session, now).ok_or("not met")?;
        assert!(answer.text.contains("met \"cost$$\""), "{}", answer.text);
        Ok(())
    }

    /// A wait that follows an action reads what came against the rows as
    /// that action began, though another action reaches the pane before it
    /// looks (its input snapshots rows that already hold the text).
    #[test]
    fn a_wait_after_an_action_keeps_the_rows_that_action_began_on() -> Result<(), String> {
        use crate::command::{Until, Wait};
        use std::time::{Duration, Instant};
        let mut session = crate::session::testing::started(crate::config::Config::default())?;
        let id = crate::id::PaneId::of(1);
        let pane = session.panes.get_mut(&id).ok_or("no %1")?;
        pane.note_input();
        let wait = Wait {
            until: vec![Until::Text("READY".into())],
            timeout: None,
            print: false,
            marks: false,
        };
        let (now, limit) = (Instant::now(), Duration::from_secs(60));
        let pane = session.panes.get(&id).ok_or("no %1")?;
        let mut waiting = super::Waiting::new(&[pane], wait, super::Follows::Action, now, limit);
        let pane = session.panes.get_mut(&id).ok_or("no %1")?;
        pane.output(b"READY\r\n");
        pane.note_input();
        let answer = waiting.answer(&session, now).ok_or("not met")?;
        assert!(answer.text.contains("met \"READY\""), "{}", answer.text);
        Ok(())
    }

    /// A wide glyph that did not fit at a row's end wraps onto the next:
    /// the blank it left there is no part of the line a wait reads, as it
    /// is no part of a copy.
    #[test]
    fn a_wait_reads_a_wide_glyph_wrapped_on_without_its_blank() -> Result<(), String> {
        use crate::command::{Until, Wait};
        use std::time::{Duration, Instant};
        let mut session = crate::session::testing::started(crate::config::Config::default())?;
        let id = crate::id::PaneId::of(1);
        let pane = session.panes.get(&id).ok_or("no %1")?;
        let cols = usize::from(pane.screen().size().cols());
        let wait = Wait {
            until: vec![Until::Text("x世".into())],
            timeout: None,
            print: false,
            marks: false,
        };
        let (now, limit) = (Instant::now(), Duration::from_secs(60));
        let mut waiting = super::Waiting::new(&[pane], wait, super::Follows::Wait, now, limit);
        let pane = session.panes.get_mut(&id).ok_or("no %1")?;
        let row: String = std::iter::repeat_n('x', cols.saturating_sub(1)).collect();
        pane.output(format!("{row}世\r\n").as_bytes());
        let answer = waiting.answer(&session, now).ok_or("not met")?;
        assert!(answer.text.contains("met \"x世\""), "{}", answer.text);
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
