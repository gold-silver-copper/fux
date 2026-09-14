//! Independent, bounded observation workers. A machine's failure cannot serialize other reads.
use super::{
    Binding,
    connection::{Gateway, GatewayLease, SharedGateway},
};
use crate::{dashboard::View, service::client::Client};
use anyhow::{Context, Result, ensure};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};

const REQUEST_BUDGET: Duration = Duration::from_secs(6);
const POLL_INTERVAL: Duration = Duration::from_secs(1);
pub const FRESH_FOR: Duration = Duration::from_secs(5);

#[derive(Clone, PartialEq, Eq)]
pub enum Source {
    Unavailable {
        reason: String,
    },
    Local {
        socket: PathBuf,
    },
    Remote {
        binding: Binding,
        koh_binary: PathBuf,
    },
}
#[derive(Clone)]
pub struct MachineSource {
    pub control: SharedGateway,
    pub attachments: std::collections::BTreeMap<String, super::Binding>,
    pub id: String,
    pub name: String,
    pub source: Source,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub attempt: Option<(String, String)>,
    pub process: Option<(String, u32, Option<u32>)>,
    pub machine_id: String,
    pub service_instance: String,
    pub row_key: String,
}

#[derive(Clone)]
pub struct Observation {
    pub machine_id: String,
    pub machine_name: String,
    pub view: Option<View>,
    /// Time the last successful read finished; errors never renew cached evidence.
    pub observed_at: Option<Instant>,
    pub problem: Option<String>,
}
impl Observation {
    pub fn fresh(&self, now: Instant) -> bool {
        self.problem.is_none()
            && self.view.as_ref().is_some_and(|view| !view.stale)
            && self
                .observed_at
                .is_some_and(|at| now.saturating_duration_since(at) <= FRESH_FOR)
    }
    pub fn row_fresh(&self, row: &crate::dashboard::Row, now: Instant) -> bool {
        self.fresh(now)
            && row.age_upper_bound_ms.is_none_or(|age| {
                self.observed_at.is_some_and(|at| {
                    Duration::from_millis(age).saturating_add(now.saturating_duration_since(at))
                        <= FRESH_FOR
                })
            })
    }
    pub fn selection(&self, row_key: &str) -> Option<Selection> {
        let view = self.view.as_ref()?;
        let row = view.rows.iter().find(|row| row.key == row_key)?;
        Some(Selection {
            attempt: row
                .expected
                .as_ref()
                .map(|expected| (expected.attempt.clone(), expected.session.clone())),
            process: row
                .expected
                .as_ref()
                .map(|expected| &expected.target)
                .or(row.target.as_ref())
                .map(|target| (target.instance.clone(), target.pane, target.pid)),
            machine_id: self.machine_id.clone(),
            service_instance: view.service_instance.clone(),
            row_key: row_key.into(),
        })
    }
    pub fn contains(&self, selection: &Selection) -> bool {
        self.selection(&selection.row_key).as_ref() == Some(selection)
    }
}

struct Worker {
    control: SharedGateway,
    latest: Arc<Mutex<Observation>>,
    stop: mpsc::SyncSender<()>,
    thread: Option<JoinHandle<()>>,
}
impl Worker {
    fn start(
        id: String,
        name: String,
        control: SharedGateway,
        mut fetch: impl FnMut(Instant) -> Result<View> + Send + 'static,
    ) -> Result<Self> {
        let latest = Arc::new(Mutex::new(Observation {
            machine_id: id,
            machine_name: name,
            view: None,
            observed_at: None,
            problem: Some("Connecting".into()),
        }));
        let published = Arc::clone(&latest);
        let (stop, stopped) = mpsc::sync_channel(1);
        let retiring = control.clone();
        let thread = std::thread::Builder::new()
            .name("zor-machine-observer".into())
            .spawn(move || {
                loop {
                    if stopped.try_recv().is_ok() {
                        break;
                    }
                    let result = fetch(Instant::now() + REQUEST_BUDGET);
                    let Ok(mut observation) = published.lock() else {
                        break;
                    };
                    match result {
                        Ok(view) => {
                            observation.view = Some(view);
                            observation.observed_at = Some(Instant::now());
                            observation.problem = None;
                        }
                        Err(error) => {
                            observation.problem =
                                Some(format!("{error:#}").chars().take(2048).collect());
                        }
                    }
                    drop(observation);
                    match stopped.recv_timeout(POLL_INTERVAL) {
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        _ => break,
                    }
                }
                // Retire the published lease before dropping this thread's fetch
                // closure. Last-owner helper cleanup then runs independently per host.
                let _ = retiring.replace(None);
            })?;
        Ok(Self {
            control,
            latest,
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.stop.try_send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = self.control.replace(None);
    }
}

pub struct Supervision {
    workers: Vec<Worker>,
    retired: Vec<Worker>,
}
impl Supervision {
    pub fn start(sources: Vec<MachineSource>) -> Result<Self> {
        ensure!(sources.len() <= 33, "machine observation limit exceeded");
        let mut ids = std::collections::BTreeSet::new();
        for source in &sources {
            ensure!(
                ids.insert(&source.id),
                "duplicate machine observation identity"
            );
        }
        let mut supervision = Self {
            workers: Vec::new(),
            retired: Vec::new(),
        };
        for source in sources {
            let control = source.control.clone();
            let mut gateway: Option<GatewayLease> = None;
            let worker = Worker::start(source.id, source.name, control, move |deadline| {
                let client = match &source.source {
                    Source::Unavailable { reason } => anyhow::bail!("{reason}"),
                    Source::Local { socket } => Client::socket(socket.clone())?,
                    Source::Remote {
                        binding,
                        koh_binary,
                    } => {
                        if gateway
                            .as_mut()
                            .is_some_and(|helper| helper.poll().is_err())
                        {
                            gateway = None;
                            source.control.replace(None)?;
                        }
                        if gateway.is_none() {
                            let lease =
                                GatewayLease::new(Gateway::start(binding, koh_binary, deadline)?);
                            source.control.replace(Some(lease.clone()))?;
                            gateway = Some(lease);
                        }
                        let lease = gateway.as_ref().context("gateway unavailable")?;
                        // A remote read failure names the transport evidence koh provided;
                        // without status reporting that is explicitly unknown, never inferred.
                        return lease.client()?.supervision(deadline).with_context(|| {
                            format!("transport {}", lease.transport_description())
                        });
                    }
                };
                client.supervision(deadline)
            })?;
            supervision.workers.push(worker);
        }
        Ok(supervision)
    }
    /// Replace only changed control endpoints. Retiring reads and helper cleanup must
    /// not block healthy hosts or navigation. A bounded retirement set prevents rapid
    /// profile edits from accumulating unbounded threads and connections.
    pub fn reload(
        &mut self,
        current: &mut Vec<MachineSource>,
        mut next: Vec<MachineSource>,
    ) -> Result<()> {
        self.retired.retain(|worker| {
            worker
                .thread
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
        });
        let unchanged = |source: &MachineSource| {
            current
                .iter()
                .position(|old| old.id == source.id && old.source == source.source)
        };
        let removed = current
            .iter()
            .filter(|old| {
                !next
                    .iter()
                    .any(|new| old.id == new.id && old.source == new.source)
            })
            .count();
        ensure!(
            self.retired.len() + removed <= 33,
            "previous machine connections are still closing; retry reload shortly"
        );
        // Validate and start replacements before modifying the active set. On failure,
        // existing observations, routing and helpers remain authoritative.
        ensure!(next.len() <= 33, "machine observation limit exceeded");
        let mut ids = std::collections::BTreeSet::new();
        ensure!(
            next.iter().all(|source| ids.insert(source.id.clone())),
            "duplicate machine observation identity"
        );
        let additions = next
            .iter()
            .filter(|source| unchanged(source).is_none())
            .cloned()
            .collect();
        let mut added = Self::start(additions)?;
        let mut old: Vec<_> = self.workers.drain(..).map(Some).collect();
        for source in &mut next {
            let worker = if let Some(index) = unchanged(source) {
                source.control = current
                    .get(index)
                    .context("machine source missing")?
                    .control
                    .clone();
                let worker = old
                    .get_mut(index)
                    .and_then(Option::take)
                    .context("machine worker missing")?;
                if let Ok(mut observation) = worker.latest.lock() {
                    observation.machine_name.clone_from(&source.name);
                }
                worker
            } else {
                added.workers.remove(0)
            };
            self.workers.push(worker);
        }
        for worker in old.into_iter().flatten() {
            let _ = worker.stop.try_send(());
            self.retired.push(worker);
        }
        *current = next;
        Ok(())
    }
    /// Copies bounded latest observations; no I/O and no lock is held during a remote read.
    pub fn observations(&self) -> Result<Vec<Observation>> {
        self.workers
            .iter()
            .map(|worker| {
                worker
                    .latest
                    .lock()
                    .map(|value| value.clone())
                    .map_err(|_| anyhow::anyhow!("machine observation worker failed"))
            })
            .collect()
    }
}
impl Drop for Supervision {
    fn drop(&mut self) {
        // Stop every worker first so shutdown waits concurrently, not one deadline per host.
        for worker in self.workers.iter().chain(&self.retired) {
            let _ = worker.stop.try_send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reload_preserves_rename_and_replaces_control_without_waiting_for_old_read() -> Result<()> {
        let source = MachineSource {
            control: Default::default(),
            attachments: Default::default(),
            id: "one".into(),
            name: "Before".into(),
            source: Source::Unavailable {
                reason: "old".into(),
            },
        };
        let (release, blocked) = mpsc::channel();
        let (entered, ready) = mpsc::channel();
        let worker = Worker::start(
            source.id.clone(),
            source.name.clone(),
            source.control.clone(),
            move |_| {
                let _ = entered.send(());
                blocked.recv_timeout(Duration::from_secs(2))?;
                Ok(view("old"))
            },
        )?;
        ready.recv_timeout(Duration::from_secs(1))?;
        let original = Arc::clone(&worker.latest);
        let mut supervision = Supervision {
            workers: vec![worker],
            retired: vec![],
        };
        let mut current = vec![source.clone()];
        let mut renamed = source.clone();
        renamed.name = "After".into();
        supervision.reload(&mut current, vec![renamed.clone()])?;
        assert!(Arc::ptr_eq(
            &original,
            &supervision.workers.first().context("worker")?.latest
        ));
        assert_eq!(
            supervision
                .observations()?
                .first()
                .context("observation")?
                .machine_name,
            "After"
        );
        let before = Instant::now();
        renamed.source = Source::Unavailable {
            reason: "replacement".into(),
        };
        supervision.reload(&mut current, vec![renamed])?;
        assert!(
            before.elapsed() < Duration::from_millis(500),
            "reload waited for old read"
        );
        assert!(!Arc::ptr_eq(
            &original,
            &supervision.workers.first().context("worker")?.latest
        ));
        assert!(
            supervision
                .observations()?
                .first()
                .context("observation")?
                .view
                .is_none()
        );
        assert_eq!(supervision.retired.len(), 1);
        assert!(
            supervision
                .reload(&mut current, vec![source.clone(), source])
                .is_err()
        );
        assert_eq!(current.first().context("source")?.name, "After");
        release.send(())?;
        supervision.reload(&mut current, vec![])?;
        assert!(supervision.observations()?.is_empty());
        Ok(())
    }
    fn view(instance: &str) -> View {
        View {
            service_instance: instance.into(),
            observation_sequence: 1,
            task_generation: None,
            state_directory: PathBuf::from("/remote/state"),
            stale: false,
            problems: Vec::new(),
            rows: vec![crate::dashboard::Row {
                expected: None,
                key: "task:same-name".into(),
                kind: "task".into(),
                label: "same-name".into(),
                status: "open".into(),
                task_outcome: None,
                attention: false,
                detail: String::new(),
                age_upper_bound_ms: None,
                target: None,
                evidence: None,
            }],
        }
    }
    #[test]
    fn cached_selection_is_scoped_to_machine_and_service_and_failure_is_stale() {
        let now = Instant::now();
        let mut observation = Observation {
            machine_id: "one".into(),
            machine_name: "One".into(),
            view: Some(view("first")),
            observed_at: Some(now),
            problem: None,
        };
        let selected = observation.selection("task:same-name");
        assert!(
            selected
                .as_ref()
                .is_some_and(|selected| observation.contains(selected))
        );
        assert!(observation.fresh(now));
        if let Some(row) = observation.view.as_ref().and_then(|view| view.rows.first()) {
            let mut aging = row.clone();
            aging.age_upper_bound_ms = Some(4_999);
            assert!(observation.row_fresh(&aging, now));
            assert!(!observation.row_fresh(&aging, now + Duration::from_millis(2)));
        }

        assert!(!observation.fresh(now + FRESH_FOR + Duration::from_millis(1)));
        observation.problem = Some("offline".into());
        assert!(!observation.fresh(now));
        assert!(observation.view.is_some());
        observation.machine_id = "two".into();
        assert!(
            !selected
                .as_ref()
                .is_some_and(|selected| observation.contains(selected))
        );
        observation.machine_id = "one".into();
        observation.view = Some(view("replacement"));
        assert!(
            !selected
                .as_ref()
                .is_some_and(|selected| observation.contains(selected))
        );
    }
    #[test]
    fn selection_survives_route_changes_but_not_attempt_or_process_replacement() -> Result<()> {
        let mut observation = Observation {
            machine_id: "one".into(),
            machine_name: "One".into(),
            view: Some(view("service")),
            observed_at: Some(Instant::now()),
            problem: None,
        };
        let expected = crate::tasks::supervise::Expected {
            task: "same-name".into(),
            attempt: "attempt-one".into(),
            session: "session-one".into(),
            target: crate::tasks::model::Target {
                runtime: PathBuf::from("/remote/fux"),
                instance: "fux-one".into(),
                workspace: "default".into(),
                stream: 1,
                pane: 7,
                pid: Some(123),
                origin: None,
            },
        };
        observation
            .view
            .as_mut()
            .context("view")?
            .rows
            .first_mut()
            .context("row")?
            .expected = Some(expected.clone());
        let selected = observation
            .selection("task:same-name")
            .context("selection")?;
        let mut moved = expected.clone();
        moved.target.workspace = "other".into();
        moved.target.stream = 2;
        observation
            .view
            .as_mut()
            .context("view")?
            .rows
            .first_mut()
            .context("row")?
            .expected = Some(moved);
        assert!(observation.contains(&selected));
        for replacement in [
            crate::tasks::supervise::Expected {
                attempt: "attempt-two".into(),
                ..expected.clone()
            },
            crate::tasks::supervise::Expected {
                session: "session-two".into(),
                ..expected.clone()
            },
            crate::tasks::supervise::Expected {
                target: crate::tasks::model::Target {
                    pid: Some(124),
                    ..expected.target.clone()
                },
                ..expected.clone()
            },
            crate::tasks::supervise::Expected {
                target: crate::tasks::model::Target {
                    instance: "fux-two".into(),
                    ..expected.target.clone()
                },
                ..expected.clone()
            },
        ] {
            observation
                .view
                .as_mut()
                .context("view")?
                .rows
                .first_mut()
                .context("row")?
                .expected = Some(replacement);
            assert!(!observation.contains(&selected));
        }
        Ok(())
    }
    #[test]
    fn slow_machine_never_blocks_healthy_publication_or_snapshot_reads() -> Result<()> {
        let (release, blocked) = mpsc::sync_channel(1);
        let (entered, waiting) = mpsc::sync_channel(1);
        let slow = Worker::start(
            "slow".into(),
            "Slow".into(),
            SharedGateway::default(),
            move |deadline| {
                let _ = entered.try_send(());
                let _ = blocked.recv_timeout(deadline.saturating_duration_since(Instant::now()));
                anyhow::bail!("offline")
            },
        )?;
        waiting.recv_timeout(Duration::from_secs(1))?;
        let fast = Worker::start(
            "fast".into(),
            "Fast".into(),
            SharedGateway::default(),
            |_| Ok(view("healthy")),
        )?;
        let supervision = Supervision {
            workers: vec![slow, fast],
            retired: vec![],
        };
        let deadline = Instant::now() + Duration::from_secs(1);
        let result = (|| -> Result<()> {
            loop {
                let observations = supervision.observations()?;
                ensure!(
                    observations.first().is_some_and(|item| item.view.is_none()),
                    "slow worker unexpectedly completed"
                );
                if observations
                    .get(1)
                    .is_some_and(|item| item.fresh(Instant::now()))
                {
                    break;
                }
                ensure!(
                    Instant::now() < deadline,
                    "healthy worker blocked by slow machine"
                );
                std::thread::yield_now();
            }
            Ok(())
        })();
        let _ = release.try_send(());
        drop(supervision);
        result
    }
}
