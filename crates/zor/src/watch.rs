//! Reconciled passive observation across fux workspaces. This module never owns pane processes.
pub(crate) mod events;

use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const MAX_OBSERVED_PANES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Handle {
    pub instance: String,
    pub workspace: String,
    pub stream: u64,
    pub pane: u32,
    pub pid: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Observation {
    pub handle: Handle,
    pub age_upper_bound_ms: u64,
    pub revision: u64,
    pub input_sequence: u64,
    pub agent: Option<String>,
    pub detected_pid: Option<i32>,
    pub state: String,
    pub rule: Option<String>,
    pub problem: Option<String>,
}

#[derive(Clone, Default, Serialize)]
pub struct Snapshot {
    pub rules_generation: u64,
    pub scan_duration_ms: u64,
    pub event_streams: usize,
    pub event_failures: u64,
    pub observations: Vec<Observation>,
    pub removed: Vec<Handle>,
    pub problems: BTreeMap<String, String>,
}

#[derive(Default)]
pub struct Registry {
    previous: BTreeMap<Handle, Observation>,
    next_workspace: usize,
    boundaries: BTreeMap<String, events::Boundary>,
}

impl Registry {
    pub fn invalidate_rules(&mut self) {
        for observation in self.previous.values_mut() {
            observation.problem = Some("rule collection changed".into());
        }
    }

    /// A full discovery boundary replaces the previous registry. Missing identities are explicit;
    /// a repeated pane number/PID in a new incarnation cannot inherit an old verdict.
    pub fn reconcile(
        &mut self,
        observations: Vec<Observation>,
        problems: BTreeMap<String, String>,
    ) -> Snapshot {
        let current: BTreeMap<_, _> = observations
            .into_iter()
            .map(|entry| (entry.handle.clone(), entry))
            .collect();
        let removed = self
            .previous
            .keys()
            .filter(|key| !current.contains_key(*key))
            .cloned()
            .collect();
        let observations = current.values().cloned().collect();
        self.previous = current;
        Snapshot {
            rules_generation: 0,
            scan_duration_ms: 0,
            event_streams: 0,
            event_failures: 0,
            observations,
            removed,
            problems,
        }
    }

    pub fn scan(
        &mut self,
        runtime: &Path,
        sets: &[crate::rules::RuleSet],
        forced: Option<&str>,
    ) -> Snapshot {
        let started = Instant::now();
        self.boundaries.clear();
        let deadline = started + Duration::from_secs(2);
        let mut problems = BTreeMap::new();
        let mut observations = Vec::new();
        let mut names = match crate::fux::request_until(
            &runtime.join("manager.sock"),
            json!({"request":"list"}),
            deadline,
        )
        .and_then(|reply| {
            reply
                .get("names")
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("manager did not return workspace names"))
        }) {
            Ok(names) if names.len() <= 64 => names,
            Ok(_) => {
                problems.insert("manager".into(), "workspace limit exceeded".into());
                return self.finish(started, observations, problems);
            }
            Err(error) => {
                problems.insert(
                    "manager".into(),
                    error.to_string().chars().take(256).collect(),
                );
                return self.finish(started, observations, problems);
            }
        };
        if !names.is_empty() {
            let offset = self.next_workspace % names.len();
            names.rotate_left(offset);
            self.next_workspace = offset + 1;
        }
        let mut visited = BTreeSet::new();
        for name in names {
            if Instant::now() >= deadline {
                problems.insert(
                    "scan".into(),
                    "discovery time budget exceeded; remaining workspaces unobserved".into(),
                );
                break;
            }
            let Some(name) = name.as_str().filter(|name| safe_name(name)) else {
                problems.insert("manager".into(), "unsafe workspace name".into());
                continue;
            };
            if !visited.insert(name.to_owned()) {
                continue;
            }
            let socket = runtime.join(format!("{name}.sock"));
            let result = self.workspace(&socket, name, sets, forced, &mut observations, deadline);
            match result {
                Ok(boundary) => {
                    self.boundaries.insert(name.into(), boundary);
                }
                Err(error) => {
                    problems.insert(name.into(), error.to_string().chars().take(256).collect());
                }
            }
        }
        self.finish(started, observations, problems)
    }

    fn finish(
        &mut self,
        started: Instant,
        mut observations: Vec<Observation>,
        problems: BTreeMap<String, String>,
    ) -> Snapshot {
        let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        for observation in &mut observations {
            observation.age_upper_bound_ms = elapsed;
        }
        let mut snapshot = self.reconcile(observations, problems);
        snapshot.scan_duration_ms = elapsed;
        snapshot
    }

    fn workspace(
        &self,
        socket: &Path,
        name: &str,
        sets: &[crate::rules::RuleSet],
        forced: Option<&str>,
        out: &mut Vec<Observation>,
        deadline: Instant,
    ) -> anyhow::Result<events::Boundary> {
        let expected = self
            .boundaries
            .values()
            .next()
            .map(|b| b.instance.clone())
            .or_else(|| out.first().map(|entry| entry.handle.instance.clone()));
        let reply = crate::fux::completed_until(
            socket,
            json!({"command":"list","id":1,"instance":expected}),
            deadline,
        )?;
        let value = reply
            .pointer("/result/value")
            .ok_or_else(|| anyhow::anyhow!("invalid listing"))?;
        let instance = value
            .get("instance")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 128)
            .ok_or_else(|| anyhow::anyhow!("missing server incarnation"))?;
        anyhow::ensure!(
            expected
                .as_deref()
                .is_none_or(|expected| expected == instance),
            "server changed during discovery; retry the scan"
        );
        let workspaces = value
            .get("workspaces")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("invalid workspaces"))?;
        anyhow::ensure!(workspaces.len() == 1, "expected one workspace");
        let workspace = workspaces
            .first()
            .ok_or_else(|| anyhow::anyhow!("workspace missing"))?;
        anyhow::ensure!(
            workspace.get("name").and_then(Value::as_str) == Some(name),
            "workspace identity mismatch"
        );
        let stream = workspace
            .pointer("/event_cursor/stream")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("fux lacks synchronized observation API"))?;
        anyhow::ensure!(stream != 0, "invalid workspace event stream");
        let sequence = workspace
            .pointer("/event_cursor/sequence")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("missing event sequence"))?;
        let tabs = workspace
            .get("tabs")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("missing tabs"))?;
        anyhow::ensure!(tabs.len() <= 32, "too many tabs");
        for tab in tabs {
            let panes = tab
                .get("panes")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("missing panes"))?;
            for pane in panes {
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "workspace scan time budget exceeded"
                );
                anyhow::ensure!(
                    out.len() < MAX_OBSERVED_PANES,
                    "observer capacity ({MAX_OBSERVED_PANES} panes) exceeded"
                );
                let id = pane
                    .get("id")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| anyhow::anyhow!("invalid pane id"))?;
                let pid = pane
                    .get("pid")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok());
                let handle = Handle {
                    instance: instance.into(),
                    workspace: name.into(),
                    stream,
                    pane: id,
                    pid,
                };
                let revision = pane
                    .get("revision")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anyhow::anyhow!("missing revision"))?;
                let input_sequence = pane
                    .get("input_sequence")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anyhow::anyhow!("missing input sequence"))?;
                let detected = forced
                    .map(|agent| {
                        (
                            agent.to_owned(),
                            pid.and_then(|pid| i32::try_from(pid).ok()),
                        )
                    })
                    .or_else(|| {
                        pid.and_then(|pid| i32::try_from(pid).ok())
                            .and_then(|pid| {
                                crate::platform::foreground_pgid(pid, None)
                                    .map(|pgid| crate::platform::job(pid, pgid))
                            })
                            .and_then(|job| crate::rules::ident::identify(&job, sets))
                            .map(|(agent, pid)| (agent.as_str().to_owned(), Some(pid)))
                    });
                let (candidate, detected_pid) = detected
                    .map(|(agent, pid)| (Some(agent), pid))
                    .unwrap_or_default();
                let mut observation = Observation {
                    handle: handle.clone(),
                    age_upper_bound_ms: 0,
                    revision,
                    input_sequence,
                    agent: candidate.clone(),
                    detected_pid,
                    state: "unknown".into(),
                    rule: None,
                    problem: None,
                };
                if let Some(set) = candidate
                    .as_ref()
                    .and_then(|candidate| sets.iter().find(|set| set.id == *candidate))
                {
                    if let Some(cached) = self.previous.get(&handle).filter(|entry| {
                        entry.revision == revision
                            && entry.agent == candidate
                            && entry.detected_pid == detected_pid
                            && entry.problem.is_none()
                    }) {
                        observation.state = cached.state.clone();
                        observation.rule = cached.rule.clone();
                    } else {
                        match capture(socket, &handle, deadline).and_then(|value| {
                            let actual = value
                                .get("revision")
                                .and_then(Value::as_u64)
                                .ok_or_else(|| anyhow::anyhow!("capture missing revision"))?;
                            let input = value
                                .get("input_sequence")
                                .and_then(Value::as_u64)
                                .ok_or_else(|| anyhow::anyhow!("capture missing input sequence"))?;
                            let screen = crate::rules::view::Captured::from_capture(&value)?;
                            Ok((crate::rules::evaluate(set, &screen), actual, input))
                        }) {
                            Ok((verdict, actual, input)) => {
                                observation.revision = actual;
                                observation.input_sequence = input;
                                // No matching evidence must never become apparent idle/completion.
                                if verdict.rule.is_some() {
                                    observation.state =
                                        format!("{:?}", verdict.state).to_lowercase();
                                }
                                observation.rule = verdict.rule;
                            }
                            Err(error) => {
                                observation.problem =
                                    Some(error.to_string().chars().take(256).collect())
                            }
                        }
                    }
                } else {
                    observation.problem = Some(
                        if candidate.is_some() {
                            "no rules for selected agent"
                        } else {
                            "no identified supported foreground agent"
                        }
                        .into(),
                    );
                }
                out.push(observation);
            }
        }
        Ok(events::Boundary {
            instance: instance.into(),
            cursor: events::Cursor { stream, sequence },
        })
    }

    pub(crate) fn invalidate_workspaces(&mut self, changes: &BTreeMap<String, String>) {
        for observation in self.previous.values_mut() {
            if let Some(problem) = changes.get(&observation.handle.workspace) {
                observation.problem = Some(problem.clone());
            }
        }
    }

    pub(crate) fn subscribed_scan(
        &mut self,
        runtime: &Path,
        sets: &[crate::rules::RuleSet],
        forced: Option<&str>,
        events: &mut events::Events,
    ) -> Snapshot {
        let started = Instant::now();
        let mut snapshot = self.scan(runtime, sets, forced);
        let mut problems = events.sync(runtime, &self.boundaries, started + Duration::from_secs(4));
        for observation in &snapshot.observations {
            if !self.boundaries.contains_key(&observation.handle.workspace) {
                problems
                    .entry(observation.handle.workspace.clone())
                    .or_insert_with(|| {
                        "event synchronization unavailable: workspace scan incomplete".into()
                    });
            }
        }
        self.invalidate_workspaces(&problems);
        invalidate_snapshot(&mut snapshot, &problems);
        let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        snapshot.scan_duration_ms = elapsed;
        for observation in &mut snapshot.observations {
            observation.age_upper_bound_ms = elapsed;
        }
        snapshot.event_streams = events.count();
        snapshot.event_failures = events.failures();
        snapshot
    }
}

pub(crate) fn invalidate_snapshot(snapshot: &mut Snapshot, changes: &BTreeMap<String, String>) {
    for observation in &mut snapshot.observations {
        if let Some(problem) = changes.get(&observation.handle.workspace) {
            observation.state = "unknown".into();
            observation.rule = None;
            observation.problem = Some(problem.clone());
        }
    }
    for (name, problem) in changes {
        snapshot
            .problems
            .insert(format!("events:{name}"), problem.clone());
    }
}

fn capture(socket: &Path, handle: &Handle, deadline: Instant) -> anyhow::Result<Value> {
    crate::fux::completed_until(
        socket,
        json!({"command":"capture","id":2,"instance":handle.instance,
        "pane":handle.pane,"format":"cells","max_bytes":131072}),
        deadline,
    )?
    .pointer("/result/value")
    .cloned()
    .ok_or_else(|| anyhow::anyhow!("invalid capture"))
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !matches!(name, "." | "..")
        && !name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
}

pub fn run(
    runtime: Option<PathBuf>,
    once: bool,
    extra: &[PathBuf],
    forced: Option<&str>,
) -> anyhow::Result<u8> {
    if let Some(forced) = forced {
        crate::osc::AgentId::new(forced)?;
    }
    let runtime = runtime.map(Ok).unwrap_or_else(crate::fux::runtime)?;
    anyhow::ensure!(runtime.is_absolute(), "fux runtime path must be absolute");
    let mut registry = Registry::default();
    let mut catalog = crate::rules::bundle::Catalog::load(extra)?;
    let reload = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _registration = ReloadSignal(signal_hook::flag::register(
        signal_hook::consts::SIGHUP,
        std::sync::Arc::clone(&reload),
    )?);
    let mut reload_problem = None;
    let mut events = events::Events::default();
    let mut output = std::io::stdout().lock();
    loop {
        if reload.swap(false, std::sync::atomic::Ordering::AcqRel) {
            match catalog.reload(extra) {
                Ok(()) => {
                    registry.invalidate_rules();
                    reload_problem = None;
                }
                Err(error) => {
                    reload_problem = Some(error.to_string().chars().take(256).collect::<String>())
                }
            }
        }
        let started = Instant::now();
        let mut snapshot = if once {
            registry.scan(&runtime, catalog.sets(), forced)
        } else {
            registry.subscribed_scan(&runtime, catalog.sets(), forced, &mut events)
        };
        if let Some(problem) = &reload_problem {
            snapshot.problems.insert("rules".into(), problem.clone());
        }
        snapshot.rules_generation = catalog.generation();
        serde_json::to_writer(&mut output, &snapshot)?;
        output.write_all(b"\n")?;
        output.flush()?;
        if once {
            return Ok(u8::from(!snapshot.problems.is_empty()));
        }
        let changes = events.wait(started + events::RESCAN, || {
            reload.load(std::sync::atomic::Ordering::Acquire)
        });
        registry.invalidate_workspaces(&changes);
    }
}

struct ReloadSignal(signal_hook::SigId);
impl Drop for ReloadSignal {
    fn drop(&mut self) {
        signal_hook::low_level::unregister(self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(instance: &str, workspace: &str, stream: u64) -> Observation {
        Observation {
            handle: Handle {
                instance: instance.into(),
                workspace: workspace.into(),
                stream,
                pane: 1,
                pid: Some(10),
            },
            age_upper_bound_ms: 0,
            revision: 1,
            input_sequence: 0,
            agent: Some("fixture".into()),
            detected_pid: Some(10),
            state: "blocked".into(),
            rule: Some("approval".into()),
            problem: None,
        }
    }

    #[test]
    fn registry_reconciles_identity_and_loss_without_transferring_evidence() {
        let mut registry = Registry::default();
        let first = observation("old", "default", 1);
        let second = observation("old", "other", 2);
        assert!(
            registry
                .reconcile(vec![first.clone(), second.clone()], BTreeMap::new())
                .removed
                .is_empty()
        );
        let mut problems = BTreeMap::new();
        problems.insert("default".into(), "unreachable".into());
        let frame = registry.reconcile(vec![second.clone()], problems);
        assert_eq!(frame.removed, vec![first.handle.clone()]);
        assert_eq!(frame.observations, vec![second]);
        let mut replacement = observation("new", "default", 1);
        replacement.state = "unknown".into();
        replacement.rule = None;
        let frame = registry.reconcile(vec![replacement.clone()], BTreeMap::new());
        assert_eq!(frame.observations, vec![replacement]);
        assert_eq!(frame.removed.len(), 1);
        assert!(!registry.previous.contains_key(&first.handle));
    }

    #[test]
    fn discovery_names_cannot_escape_the_runtime() {
        for name in ["", ".", "..", "../secret", "a/b", "a\\b", "\n"] {
            assert!(!safe_name(name));
        }
        assert!(safe_name("project-1"));
        assert!(!safe_name(&"a".repeat(129)));
    }
}
