//! Optional observation of a multiplexer-owned pane through its local control interface.
//! This process never spawns, signals, or owns the observed command.
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::fux::completed as request;

pub fn run(
    socket: &Path,
    pane_id: u32,
    expected_pid: u32,
    forced: Option<&str>,
    sets: &[crate::rules::RuleSet],
) -> anyhow::Result<u8> {
    use crate::state::{Event, Machine, Observation, ObservationState};
    let mut machine = Machine::new(crate::state::Config::default());
    let forced = forced.map(crate::osc::AgentId::new).transpose()?;
    let pid = i32::try_from(expected_pid)?;
    let mut output = std::io::stdout().lock();
    let mut loss = crate::platform::probe::LossTracker::new();
    let mut active_agent = forced.clone();
    // The observer may start immediately before its parent's control accept loop.
    let startup = Instant::now() + Duration::from_secs(3);
    // The pane's screen and the output sequence it reflects are kept between samples, so an idle
    // pane costs one `list` and no capture or re-emulation until fux reports a new sequence.
    let mut cache = CaptureCache::default();
    let mut instance: Option<String> = None;
    loop {
        let listing = match request(socket, json!({"command":"list","id":1,"instance":instance})) {
            Ok(value) => value,
            Err(_) if Instant::now() < startup => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(error) => return Err(error),
        };
        let observed_instance = listing
            .pointer("/result/value/instance")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("listing has no server instance"))?;
        anyhow::ensure!(
            instance
                .as_deref()
                .is_none_or(|expected| expected == observed_instance),
            "server instance changed during observation"
        );
        instance = Some(observed_instance.to_owned());
        let panes = listing
            .pointer("/result/value/workspaces")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|workspace| {
                workspace
                    .get("tabs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .flat_map(|tab| {
                tab.get("panes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            });
        let Some(pane) = panes
            .into_iter()
            .find(|pane| pane.get("id").and_then(Value::as_u64) == Some(u64::from(pane_id)))
        else {
            return Ok(0);
        };
        if pane.get("pid").and_then(Value::as_u64) != Some(u64::from(expected_pid)) {
            return Ok(0);
        }
        cache.refresh(pane.get("revision").and_then(Value::as_u64), |revision| {
            let response = request(socket,
                json!({"command":"capture","id":2,"pane":pane_id,"attrs":true,"scrollback":0,"max_bytes":131072,"if_revision":revision,"instance":instance}))?;
            response.pointer("/result/value").cloned()
                .ok_or_else(|| anyhow::anyhow!("invalid capture response"))
        })?;
        let job =
            crate::platform::foreground_pgid(pid, None).map(|pgid| crate::platform::job(pid, pgid));
        let detected = job
            .as_ref()
            .and_then(|job| crate::rules::ident::identify(job, sets));
        let mut exited = false;
        if forced.is_none() {
            let shell = job
                .as_ref()
                .is_some_and(|job| job.processes.iter().any(|process| process.pid == pid));
            if let Some(change) = loss.update(detected.clone(), shell) {
                match change {
                    crate::platform::probe::Detection::AgentFound { id, .. } => {
                        active_agent = Some(id)
                    }
                    crate::platform::probe::Detection::Exited { agent } => {
                        active_agent = Some(agent);
                        exited = true;
                    }
                    crate::platform::probe::Detection::AgentLost => active_agent = None,
                }
            }
        }
        let agent = cache.screen.as_ref().and(active_agent.clone());
        exited &= cache.screen.is_some();
        let verdict = agent
            .as_ref()
            .and_then(|agent| sets.iter().find(|set| set.id == agent.as_str()))
            .zip(cache.screen.as_ref())
            .map(|(set, screen)| crate::rules::evaluate(set, screen))
            .map(|verdict| Observation {
                state: match verdict.state {
                    crate::rules::RuleState::Unknown => ObservationState::Unknown,
                    crate::rules::RuleState::Working => ObservationState::Working,
                    crate::rules::RuleState::Blocked => ObservationState::Blocked,
                    crate::rules::RuleState::Idle => ObservationState::Idle,
                    crate::rules::RuleState::Skip => ObservationState::Skip,
                },
                visible: verdict.visible,
            });
        let now = Instant::now();
        let mut events = machine.observe(verdict, agent, detected.map(|(_, pid)| pid), exited, now);
        events.extend(machine.tick(now));
        for event in events {
            let report = match event {
                Event::Changed {
                    state,
                    agent,
                    seq,
                    visible,
                    exited,
                    ..
                } => Some(crate::osc::Report::new(
                    state, agent, seq, visible, exited, None,
                )?),
                Event::Heartbeat {
                    state,
                    agent,
                    seq,
                    visible,
                } => Some(crate::osc::Report::new(
                    state, agent, seq, visible, false, None,
                )?),
                _ => None,
            };
            if let Some(report) = report {
                output.write_all(&crate::osc::format(&report))?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[derive(Default)]
struct CaptureCache {
    revision: Option<u64>,
    screen: Option<crate::screen::Screen>,
}

impl CaptureCache {
    fn refresh(
        &mut self,
        listed: Option<u64>,
        mut fetch: impl FnMut(Option<u64>) -> anyhow::Result<Value>,
    ) -> anyhow::Result<()> {
        if listed.is_some() && listed == self.revision {
            return Ok(());
        }
        let value = fetch(self.revision)?;
        let revision = value
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("capture is missing revision"))?;
        if value.get("unchanged").and_then(Value::as_bool) == Some(true) {
            anyhow::ensure!(
                self.revision == Some(revision),
                "unexpected unchanged capture"
            );
            return Ok(());
        }
        if value.get("truncated").and_then(Value::as_bool) == Some(true) {
            // Valid but incomplete evidence is unavailable, not an observer failure. Remember
            // the revision to avoid repeatedly recapturing it, and recover on new output/size.
            self.screen = None;
        } else {
            self.screen = Some(captured_screen(&value)?);
        }
        self.revision = Some(revision);
        Ok(())
    }
}

/// Interpret one coherent capture, never metadata from an earlier listing.
pub(crate) fn captured_screen(value: &Value) -> anyhow::Result<crate::screen::Screen> {
    let dimension = |name| -> anyhow::Result<u16> {
        let n = value
            .get(name)
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("capture is missing {name}"))?;
        anyhow::ensure!((2..=512).contains(&n), "invalid capture dimension");
        Ok(u16::try_from(n)?)
    };
    anyhow::ensure!(
        value.get("truncated").and_then(Value::as_bool) == Some(false),
        "cannot classify a truncated capture"
    );
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("capture is missing text"))?;
    anyhow::ensure!(text.len() <= 131072, "capture text exceeds limit");
    let mut screen = crate::screen::Screen::new(dimension("rows")?, dimension("columns")?);
    screen.process(text.as_bytes());
    screen.set_observed_title(
        value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let progress = value
        .get("progress")
        .and_then(Value::as_array)
        .and_then(|values| {
            let state = u8::try_from(values.first()?.as_u64()?).ok()?;
            let percent = u8::try_from(values.get(1)?.as_u64()?).ok()?;
            (state <= 4 && percent <= 100)
                .then_some(crate::rules::view::Progress { state, percent })
        });
    screen.set_observed_progress(progress);
    Ok(screen)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::rules::view::ScreenView;

    #[test]
    fn cache_uses_capture_revision_and_recovers_after_incomplete_evidence() {
        let mut cache = CaptureCache::default();
        let complete = |revision, rows, columns| {
            json!({"revision":revision,
            "rows":rows,"columns":columns,"text":"ready","title":"",
            "progress":null,"truncated":false,"unchanged":false})
        };
        // Output and resize raced a listing at revision 1. The capture at 2 wins.
        cache
            .refresh(Some(1), |seen| {
                assert_eq!(seen, None);
                Ok(complete(2, 8, 20))
            })
            .expect("capture after race");
        assert_eq!(cache.revision, Some(2));
        assert_eq!(cache.screen.as_ref().expect("screen").size(), (8, 20));
        let mut calls = 0;
        cache
            .refresh(Some(2), |_| {
                calls += 1;
                Ok(complete(2, 8, 20))
            })
            .expect("cached");
        assert_eq!(calls, 0, "idle panes must not request capture");
        cache
            .refresh(Some(3), |_| {
                Ok(json!({"revision":3,"truncated":true,"unchanged":false}))
            })
            .expect("truncation is recoverable");
        assert!(cache.screen.is_none(), "old evidence must be invalidated");
        cache
            .refresh(Some(3), |_| {
                calls += 1;
                Ok(complete(3, 8, 20))
            })
            .expect("cached truncation");
        assert_eq!(calls, 0);
        cache
            .refresh(Some(4), |seen| {
                assert_eq!(seen, Some(3));
                Ok(complete(4, 24, 80))
            })
            .expect("recovered");
        assert_eq!(
            cache.screen.as_ref().expect("restored screen").size(),
            (24, 80)
        );
        assert!(
            CaptureCache::default()
                .refresh(None, |_| Ok(json!({"revision":1,"unchanged":true})))
                .is_err()
        );
    }

    #[test]
    fn captures_preserve_bottom_right_evidence_across_resize() {
        for (rows, columns) in [(24, 80), (8, 20), (2, 2)] {
            let value = json!({"rows":rows,"columns":columns,
                "text":format!("\u{1b}[{rows};{columns}HX"),
                "title":"captured title","progress":[1,50],"truncated":false});
            let screen = captured_screen(&value).expect("valid capture");
            assert_eq!(screen.size(), (rows, columns));
            assert_eq!(
                screen.lines().last().expect("last line").chars().last(),
                Some('X')
            );
            assert_eq!(screen.title(), "captured title");
            assert_eq!(screen.progress().expect("progress").percent, 50);
        }
    }

    #[test]
    fn incomplete_or_oversized_captures_are_not_classified() {
        assert!(captured_screen(&json!({"text":"blocked"})).is_err());
        assert!(
            captured_screen(&json!({"text":"blocked","rows":24,"columns":80,"truncated":true}))
                .is_err()
        );
        assert!(
            captured_screen(&json!({"text":"blocked","rows":24,"columns":513,"truncated":false}))
                .is_err()
        );
    }
}
