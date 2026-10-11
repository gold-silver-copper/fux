//! `fux wait`, and `send-keys --wait-idle`: a command line from the CLI that
//! answers once its pane shows what it waits for, or its time is up. The
//! server holds its connection meanwhile and asks it again whenever the
//! pane may have changed, or its time comes (`Waiting::due`).
use crate::command::{CaptureView, Until, Wait};
use crate::id::PaneId;
use crate::pane::Pane;
use crate::session::{Outcome, Session};
use std::time::Instant;

/// A wait under way.
pub struct Waiting {
    pane: PaneId,
    wait: Wait,
    /// When it began: a pane is quiet from then at the soonest.
    began: Instant,
    deadline: Instant,
    /// The screen's text as the wait began, which `--change` waits to see
    /// differ.
    first: String,
    /// The screen as the wait last read it: unchanged, it is not read again.
    read: Option<fux_vt::Mark>,
}

impl Waiting {
    pub fn new(pane: &Pane, wait: Wait, now: Instant) -> Waiting {
        Waiting {
            pane: pane.id,
            deadline: crate::after(now, wait.timeout),
            wait,
            began: now,
            first: text(pane),
            read: None,
        }
    }

    /// When to ask it again at the latest, whatever happens before: when its
    /// pane will have been quiet long enough, or its time is up. A pane
    /// quiet already, its input still going in, is asked again as that
    /// input goes, which wakes the server itself.
    pub fn due(&self, session: &Session) -> Instant {
        let quiet = match (&self.wait.until, session.panes.get(&self.pane)) {
            (Until::Idle(idle), Some(pane)) => {
                Some(crate::after(pane.changed_at.max(self.began), *idle))
            }
            _ => None,
        };
        let now = Instant::now();
        (quiet.filter(|at| *at > now)).map_or(self.deadline, |at| at.min(self.deadline))
    }

    /// Its answer, if it has one at `now`: what it waited for came, its pane
    /// went, or its time is up.
    pub fn answer(&mut self, session: &Session, now: Instant) -> Option<Outcome> {
        let Some(pane) = session.panes.get(&self.pane) else {
            return Some(match self.wait.until {
                Until::Exit => done(format!("{} closed\n", self.pane)),
                Until::Text(_) | Until::Gone(_) | Until::Idle(_) | Until::Change => {
                    failed(String::new(), format!("{} closed", self.pane))
                }
            });
        };
        let met = match &self.wait.until {
            Until::Exit => pane
                .process
                .exit_status()
                .map(|status| format!("exited {status}\n")),
            Until::Idle(idle) => {
                let quiet = crate::after(pane.changed_at.max(self.began), *idle);
                (pane.input_written() && now >= quiet).then(String::new)
            }
            Until::Text(texts) => reread(&mut self.read, pane).and_then(|text| {
                texts
                    .iter()
                    .any(|t| text.contains(t.as_str()))
                    .then(String::new)
            }),
            Until::Gone(texts) => reread(&mut self.read, pane).and_then(|text| {
                texts
                    .iter()
                    .all(|t| !text.contains(t.as_str()))
                    .then(String::new)
            }),
            Until::Change => {
                let first = std::mem::take(&mut self.first);
                let changed =
                    reread(&mut self.read, pane).and_then(|text| (text != first).then(String::new));
                self.first = first;
                changed
            }
        };
        let print = self.wait.print;
        let screen = || if print { text(pane) } else { String::new() };
        match met {
            Some(mut said) => {
                said.push_str(&screen());
                Some(done(said))
            }
            None if now >= self.deadline => {
                let seconds = self.wait.timeout.as_secs_f64();
                Some(failed(
                    screen(),
                    format!("{}: timed out after {seconds}s", self.pane),
                ))
            }
            None => None,
        }
    }
}

/// The pane's screen text, if it changed since `read`, when the wait last
/// read it.
fn reread(read: &mut Option<fux_vt::Mark>, pane: &Pane) -> Option<String> {
    let screen = pane.screen();
    if read.is_some_and(|read| !screen.changed_since(read)) {
        return None;
    }
    *read = Some(screen.mark());
    Some(text(pane))
}

/// A pane's screen as a wait reads and prints it: trimmed, its soft-wrapped
/// rows joined, so text a narrow pane wraps is found whole.
fn text(pane: &Pane) -> String {
    let view = CaptureView {
        join: true,
        ..CaptureView::default()
    };
    crate::capture::pane(pane, None, view, false)
}

fn done(stdout: String) -> Outcome {
    Outcome {
        status: 0,
        stdout,
        stderr: String::new(),
    }
}

fn failed(stdout: String, stderr: String) -> Outcome {
    Outcome {
        status: 1,
        stdout,
        stderr,
    }
}
