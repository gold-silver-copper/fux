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
/// Keys sent changed nothing on the screen, and the wait was for it to
/// settle (send-keys --wait-idle): they may not have reached the program.
pub const UNCHANGED: u8 = 5;
/// Input given `--if-revision` found the screen changed, and sent nothing.
pub const CHANGED: u8 = 6;
/// As `timeout(1)`.
pub const TIMED_OUT: u8 = 124;

/// How long a shell back at its prompt, fewer commands ended than lines
/// typed, shows no change before `--done` takes it as done.
/// Longer than a slow prompt (git status, starship) takes to draw.
const DONE_SETTLE: std::time::Duration = std::time::Duration::from_millis(500);

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
    /// Commands ended as the wait began, and the count `--done` waits for.
    finished: u64,
    done_at: u64,
    /// The program's start (`Pane::starts`) the wait watches.
    starts: u64,
    alerts: u64,
}

/// What a wait found.
enum Found {
    Text { text: String, line: String },
    Gone(String),
    Idle { changed: bool },
    Change(u64),
    Exit(Ended),
    Done(Option<i32>),
    Bell(Option<String>),
}

impl Waiting {
    pub fn new(panes: &[&Pane], wait: Wait, now: Instant) -> Waiting {
        let texts = texts(&wait);
        let watches = panes
            .iter()
            .map(|pane| {
                let screen = pane.screen();
                let commands = screen.commands();
                // Keys typed while a command runs wait for it, then run: a
                // wait for their end waits for that one's first.
                let ahead = u64::from(wait.after && commands.running);
                // The command lines typed, each a command's end to wait for.
                let typed = if wait.after {
                    u64::from(wait.lines.max(1))
                } else {
                    1
                };
                let was = if wait.after {
                    (lines(screen.rows().skip(screen.history_len()), None).into_iter())
                        .map(|l| (l.first, l.text))
                        .collect()
                } else {
                    std::collections::HashMap::new()
                };
                Watch {
                    pane: pane.id,
                    anchor: screen.window().row(0).map(|row| row.id()),
                    mark: screen.mark(),
                    was,
                    touched: std::collections::HashSet::new(),
                    read: None,
                    in_history: vec![None; texts.len()],
                    on_main: !screen.mode(fux_vt::Mode::AlternateScreen),
                    appeared: Vec::new(),
                    echo_skipped: false,
                    revision: pane.revision,
                    finished: commands.finished,
                    done_at: commands
                        .finished
                        .saturating_add(ahead)
                        .saturating_add(typed),
                    starts: pane.starts,
                    alerts: pane.alerts.count(),
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
        }
    }

    /// When to ask it again at the latest, whatever happens before: when a
    /// pane will have been quiet long enough, or its time is up. A pane
    /// quiet already, its input still going in, is asked again as that
    /// input goes, which wakes the server itself.
    pub fn due(&self, session: &Session) -> Instant {
        let now = Instant::now();
        let idle = self.wait.until.iter().find_map(|u| match u {
            Until::Idle(idle) => Some(*idle),
            Until::Text(_)
            | Until::Gone(_)
            | Until::Change(_)
            | Until::Exit
            | Until::Done
            | Until::Bell => None,
        });
        let quiet = idle.and_then(|idle| {
            self.watches
                .iter()
                .filter_map(|w| session.panes.get(&w.pane))
                .map(|pane| crate::after(pane.changed_at.max(self.began), idle))
                .filter(|at| *at > now)
                .min()
        });
        // A wait for a command's end looks again once the screen settles.
        let settle = self.wait.until.contains(&Until::Done).then(|| {
            (self.watches.iter())
                .filter_map(|w| session.panes.get(&w.pane))
                .map(|pane| crate::after(pane.changed_at, DONE_SETTLE))
                .filter(|at| *at > now)
                .min()
        });
        [quiet, settle.flatten()]
            .into_iter()
            .flatten()
            .fold(self.deadline, Instant::min)
    }

    /// Its answer, if it has one at `now`: what it waited for came in one of
    /// its panes, a pane's program exited first, a pane went, or its time is
    /// up.
    pub fn answer(&mut self, session: &Session, now: Instant) -> Option<Outcome> {
        let outcome = self.look(session, now)?;
        Some(Outcome {
            stdout: format!("{}{}", self.lead, outcome.stdout),
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
                let status = found.status(self.wait.after);
                let id = watch.pane;
                return Some(
                    self.wait
                        .told(id, Some(pane), status, Told::Found(found), several),
                );
            }
            match pane.process.exit_status() {
                Some(status) => {
                    ended.get_or_insert((watch.pane, Told::Exited(status)));
                }
                None => live = true,
            }
        }
        if !live && let Some((id, told)) = ended {
            let status = match told {
                Told::Gone => GONE,
                Told::Found(_) | Told::Exited(_) | Told::Replaced | Told::TimedOut(_) => EXITED,
            };
            return Some(
                self.wait
                    .told(id, session.panes.get(&id), status, told, several),
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
            let first = first.map(|w| w.pane);
            return first.map(|id| {
                self.wait
                    .told(id, pane, TIMED_OUT, Told::TimedOut(hint), several)
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
                *answer = Some(self.wait.told(id, None, GONE, Told::Gone, true));
                continue;
            };
            let told = if pane.starts != watch.starts {
                Some((EXITED, Told::Replaced))
            } else if let Some(found) = watch.look(pane, &self.wait, texts, window, self.fresh) {
                Some((found.status(self.wait.after), Told::Found(found)))
            } else {
                pane.process
                    .exit_status()
                    .map(|ended| (EXITED, Told::Exited(ended)))
            };
            if let Some((status, told)) = told {
                *answer = Some(self.wait.told(id, Some(pane), status, told, true));
            }
        }
        if now >= self.deadline {
            for (watch, answer) in self.watches.iter().zip(self.answers.iter_mut()) {
                if answer.is_none() {
                    let pane = session.panes.get(&watch.pane);
                    let hint = pane.and_then(|p| watch.hint(p, &self.wait));
                    let told = Told::TimedOut(hint);
                    *answer = Some(self.wait.told(watch.pane, pane, TIMED_OUT, told, true));
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
    fn status(&self, after: bool) -> u8 {
        match self {
            Found::Idle { changed: false } if after => UNCHANGED,
            // As timeout(1): the program's own status, once it exits.
            Found::Exit(ended) => {
                u8::try_from(crate::process::shell_status(*ended)).unwrap_or(u8::MAX)
            }
            Found::Text { .. }
            | Found::Gone(_)
            | Found::Idle { .. }
            | Found::Change(_)
            | Found::Done(_)
            | Found::Bell(_) => 0,
        }
    }
}

/// How a wait ended, to be told.
enum Told {
    Found(Found),
    Exited(Ended),
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
    ) -> Outcome {
        let prefix = if several {
            format!("{id} ")
        } else {
            String::new()
        };
        let screen = pane.filter(|_| self.print).map(|p| text(p, self.marks));
        let (said, problem, json) = match told {
            Told::Found(found) => {
                let (said, json) = found.said();
                let problem = (self.after && matches!(found, Found::Idle { changed: false }))
                    .then(|| format!("{id}: the screen did not change after the keys"));
                (said, problem, json)
            }
            Told::Exited(ended) => {
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
            Until::Idle(idle) => format!("the screen to settle for {}ms", idle.as_millis()),
            Until::Change(_) => "a change".into(),
            Until::Exit => "the program to exit".into(),
            Until::Done => "a command to end".into(),
            Until::Bell => "a bell".into(),
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
            Found::Idle { changed } => (
                String::new(),
                vec![
                    ("met", Json::str("idle")),
                    ("changed", Json::Bool(*changed)),
                ],
            ),
            Found::Change(revision) => (
                String::new(),
                vec![
                    ("met", Json::str("change")),
                    (
                        "revision",
                        Json::Number(i64::try_from(*revision).unwrap_or(i64::MAX)),
                    ),
                ],
            ),
            Found::Exit(ended) => (crate::process::told(*ended), ended_json("exit", *ended)),
            Found::Done(status) => (
                status.map_or_else(|| "done".to_owned(), |s| format!("done {s}")),
                vec![
                    ("met", Json::str("done")),
                    (
                        "exit",
                        status.map_or(Json::Null, |s| Json::Number(i64::from(s))),
                    ),
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
    /// Why a wait that timed out may never have been met: a command's end
    /// awaited in a pane whose shell marks none, a text taken as the echo of
    /// what was typed, or one that was on the screen already and did not
    /// appear anew.
    fn hint(&self, pane: &Pane, wait: &Wait) -> Option<&'static str> {
        let commands = pane.screen().commands();
        if wait.until.contains(&Until::Done) {
            if commands.finished == 0 && !commands.running {
                return Some(
                    "no command has been marked in this pane: --done needs a shell that \
                     marks its commands (bash, zsh and fish started by fux, unless \
                     `set shell-marks off`); for a program, wait for its text, or --idle",
                );
            }
            if commands.running {
                return Some(
                    "a command is still running, as the shell's marks say; if it ended, \
                     the shell has stopped marking its commands (exec, or PROMPT_COMMAND \
                     replaced)",
                );
            }
            return Some(
                "the shell is at its prompt with no command running: --done waits for a \
                 command to end after the wait begins",
            );
        }
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
                 wait began (drawn again the same, it brings nothing new); `fux wait --text` \
                 takes what is shown",
            );
        }
        self.echo_skipped.then_some(
            "a line held the text along with what was typed, and was taken as its echo; \
             wait for text only the output has, or use `fux wait --text`",
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
        // A send-keys wait takes only lines changed since it began.
        let mark = wait.after.then_some(self.mark);
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
                    let (was, touched) = (&self.was, &self.touched);
                    // The newest line holding it, on the screen or behind it.
                    let line = (shown.iter().rev())
                        .chain(behind.iter().rev())
                        .filter(|l| is_new(l, was, touched) && found_in(&l.text, text) > 0)
                        .find(|l| {
                            let echo = echoes(&l.text, text, &wait.echo);
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
                Until::Idle(idle) => {
                    let quiet = crate::after(pane.changed_at.max(began), *idle);
                    (pane.input_written() && now >= quiet).then_some(Found::Idle {
                        changed: pane.revision != self.revision,
                    })
                }
                Until::Change(since) => {
                    let past = since.unwrap_or(self.revision);
                    (pane.revision > past).then_some(Found::Change(pane.revision))
                }
                Until::Exit => pane.process.exit_status().map(Found::Exit),
                // A command ended since, the shell is back at its prompt and
                // everything typed has gone in; and either as many commands
                // ended as lines were typed, or nothing has changed for a
                // moment (a line read by the running command, a loop over
                // several lines, Ctrl-C: fewer ends than lines).
                Until::Done => {
                    let commands = screen.commands();
                    let settled = now >= crate::after(pane.changed_at, DONE_SETTLE);
                    (commands.finished > self.finished
                        && !commands.running
                        && pane.input_written()
                        && (commands.finished >= self.done_at || settled))
                        .then_some(Found::Done(commands.last_status))
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
        None
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
        let mark = wait.after.then_some(self.mark);
        let read = lines(rows.into_iter(), mark);
        let mut skipped = false;
        for (found, text) in self.in_history.iter_mut().zip(texts) {
            if found.is_none() {
                *found = (read.iter())
                    .filter(|l| is_new(l, &self.was, &self.touched) && found_in(&l.text, text) > 0)
                    .find(|l| {
                        let echo = echoes(&l.text, text, &wait.echo);
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
            Until::Gone(_)
            | Until::Idle(_)
            | Until::Change(_)
            | Until::Exit
            | Until::Done
            | Until::Bell => None,
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

/// Whether a line is the echo of typed text holding `text`: a shell, or an
/// input box, shows what is typed, and that is not what the keys brought.
fn echoes(line: &str, _text: &str, echo: &[String]) -> bool {
    // A line holding a typed line, or ending in the start of one (an input
    // box redrawn as the keys come in), is its echo, whatever is awaited:
    // a prompt with the command typed after it is not the next prompt.
    // Only where it stands alone: typed `a` is not the echo in `answer: A`.
    let line = line.trim_end();
    echo.iter().any(|typed| {
        let typed = typed.trim_end();
        let alone = line.match_indices(typed).any(|(at, _)| {
            let after = at.saturating_add(typed.len());
            !word_char(line.get(..at).and_then(|s| s.chars().next_back()))
                && !word_char(line.get(after..).and_then(|s| s.chars().next()))
        });
        alone
            || (1..typed.chars().count()).any(|n| {
                let start: String = typed.chars().take(n).collect();
                line.strip_suffix(start.as_str())
                    .is_some_and(|before| !word_char(before.chars().next_back()))
            })
    })
}

/// Whether a character joins the typed text it touches into a longer word.
fn word_char(c: Option<char>) -> bool {
    c.is_some_and(char::is_alphanumeric)
}

/// How many times a line shows TEXT. Lines are trimmed, so blanks that end
/// TEXT (a prompt's `> `) may also be the line's end.
fn found_in(line: &str, text: &str) -> usize {
    let trimmed = text.trim_end();
    let at_end = trimmed.len() < text.len() && !trimmed.is_empty() && line.ends_with(trimmed);
    line.matches(text)
        .count()
        .saturating_add(usize::from(at_end))
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
