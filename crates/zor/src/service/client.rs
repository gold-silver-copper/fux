//! Typed service reads over an explicit private socket. Never starts a service or opens task data.
pub mod tasks;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::PathBuf, time::Instant};

#[derive(Clone, Debug)]
pub struct Client {
    socket: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub service_instance: String,
    pub features: BTreeSet<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub service_instance: String,
    pub sequence: u64,
    pub stale: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    pub published_age_ms: Option<u64>,
    pub snapshot: crate::watch::Snapshot,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Overview {
    pub runtime: PathBuf,
    pub state_directory: PathBuf,
    pub generation: Option<u64>,
    pub rows: Vec<crate::dashboard::Row>,
    pub integrations: Vec<crate::dashboard::Integration>,
    #[serde(default)]
    pub native_integrations: Vec<crate::dashboard::NativeIntegration>,
}

#[derive(Debug)]
pub struct RemoteFailure {
    pub code: String,
    pub message: Option<String>,
}
impl std::fmt::Display for RemoteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "zor service refused request: {}", self.code)?;
        if let Some(message) = &self.message {
            write!(f, ": {message}")?;
        }
        Ok(())
    }
}
impl std::error::Error for RemoteFailure {}

#[derive(Debug)]
pub struct StaleIdentity;
impl std::fmt::Display for StaleIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("zor service incarnation changed; refresh before acting")
    }
}
impl std::error::Error for StaleIdentity {}

#[derive(Debug)]
pub struct MalformedReply;
impl std::fmt::Display for MalformedReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid zor service reply")
    }
}
impl std::error::Error for MalformedReply {}

impl Client {
    pub fn local(directory: Option<PathBuf>) -> Result<Self> {
        Self::socket(
            directory
                .map_or_else(super::directory, Ok)?
                .join("control.sock"),
        )
    }
    /// A caller-owned koh proxy is a connection location, never a local runtime directory.
    pub fn socket(socket: PathBuf) -> Result<Self> {
        ensure!(socket.is_absolute(), "service socket must be absolute");
        Ok(Self { socket })
    }
    pub fn capabilities(&self, deadline: Instant) -> Result<Capabilities> {
        let body = exchange(
            &self.socket,
            json!({"v":1,"id":1,"op":"capabilities"}),
            deadline,
        )?;
        let capabilities: Capabilities = decode(body)?;
        identity(&capabilities.service_instance)?;
        ensure!(
            capabilities.features.len() <= 32
                && capabilities
                    .features
                    .iter()
                    .all(|v| !v.is_empty() && v.len() <= 64),
            "invalid capability list"
        );
        Ok(capabilities)
    }
    pub fn snapshot(&self, deadline: Instant) -> Result<Snapshot> {
        let value = exchange(
            &self.socket,
            json!({"v":1,"id":1,"op":"snapshot"}),
            deadline,
        )?;
        let snapshot: Snapshot = decode(value)?;
        validate_snapshot(&snapshot)?;
        Ok(snapshot)
    }
    pub(crate) fn overview(&self, instance: &str, deadline: Instant) -> Result<Overview> {
        identity(instance)?;
        let overview: Overview =
            self.task_read(instance, json!({"action":"overview"}), deadline)?;
        validate_overview(&overview)?;
        Ok(overview)
    }
    /// One retained transport session per poll, with capabilities and incarnation
    /// revalidated on every response rather than cached across a service restart.
    pub fn supervision(&self, deadline: Instant) -> Result<crate::dashboard::View> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Reply {
            service_instance: String,
            features: BTreeSet<String>,
            observation: Snapshot,
            overview: Overview,
        }
        let began = Instant::now();
        let reply: Reply = decode(exchange(
            &self.socket,
            json!({"v":1,"id":1,"op":"supervision"}),
            deadline,
        )?)?;
        identity(&reply.service_instance)?;
        ensure!(
            reply.features.len() <= 32
                && reply
                    .features
                    .iter()
                    .all(|feature| !feature.is_empty() && feature.len() <= 64)
                && reply.features.contains("supervision-v1"),
            "service lacks valid supervision-v1 capability"
        );
        ensure!(
            reply.service_instance == reply.observation.service_instance,
            "inconsistent supervision service incarnation"
        );
        validate_snapshot(&reply.observation)?;
        validate_overview(&reply.overview)?;
        crate::dashboard::compose_typed(
            &reply.observation,
            &reply.overview,
            u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX),
        )
    }
    pub fn view(&self, deadline: Instant) -> Result<crate::dashboard::View> {
        let began = Instant::now();
        let snapshot = self.snapshot(deadline)?;
        let overview = self.overview(&snapshot.service_instance, deadline)?;
        crate::dashboard::compose_typed(
            &snapshot,
            &overview,
            u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX),
        )
    }
}

fn validate_snapshot(snapshot: &Snapshot) -> Result<()> {
    identity(&snapshot.service_instance)?;
    ensure!(
        snapshot.snapshot.observations.len() <= crate::watch::MAX_OBSERVED_PANES
            && snapshot.snapshot.removed.len() <= crate::watch::MAX_OBSERVED_PANES,
        "snapshot pane limit exceeded"
    );
    let mut handles = BTreeSet::new();
    for observation in &snapshot.snapshot.observations {
        let handle = &observation.handle;
        identity(&handle.instance)?;
        ensure!(
            crate::fux::endpoint::valid_name(&handle.workspace)
                && handle.stream > 0
                && handle.pane > 0
                && handle.pid.is_none_or(|pid| pid > 0)
                && handles.insert(handle),
            "invalid or duplicate observed identity"
        );
    }
    Ok(())
}
fn validate_overview(overview: &Overview) -> Result<()> {
    ensure!(
        overview.runtime.is_absolute() && overview.state_directory.is_absolute(),
        "invalid service overview locations"
    );
    ensure!(
        overview.rows.len() <= crate::dashboard::MAX_OVERVIEW_ROWS
            && overview.integrations.len() <= crate::tasks::model::MAX_TASKS
            && overview.native_integrations.len() <= crate::tasks::model::MAX_TASKS,
        "service overview row limit exceeded"
    );
    let mut keys = BTreeSet::new();
    for row in &overview.rows {
        ensure!(
            !row.key.is_empty() && keys.insert(&row.key),
            "duplicate or missing overview row identity"
        );
    }
    Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|error| anyhow::Error::new(MalformedReply).context(error))
}
fn identity(instance: &str) -> Result<()> {
    ensure!(
        !instance.is_empty() && instance.len() <= 128 && !instance.chars().any(char::is_control),
        "invalid service identity"
    );
    Ok(())
}
fn same_instance(value: &Value, expected: &str) -> Result<()> {
    let actual = value
        .get("service_instance")
        .and_then(Value::as_str)
        .ok_or(MalformedReply)?;
    if actual != expected {
        return Err(StaleIdentity.into());
    }
    Ok(())
}

/// Shared bounded transport; wire JSON remains private to the service boundary.
pub(super) fn exchange(socket: &std::path::Path, value: Value, deadline: Instant) -> Result<Value> {
    let deadline = deadline.min(Instant::now() + std::time::Duration::from_secs(3));
    let id = value
        .get("id")
        .and_then(Value::as_u64)
        .context("request correlation ID")?;
    let mut stream =
        local_ipc::connect_until(socket, deadline).context("zor service unavailable")?;
    ensure!(
        local_ipc::peer_is_current_user(&stream)?,
        "service socket belongs to another user"
    );
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    ensure!(
        bytes.len() <= super::MAX_REQUEST,
        "zor service request exceeds limit"
    );
    local_ipc::write_all_until(
        &mut stream,
        &bytes,
        deadline.min(Instant::now() + std::time::Duration::from_secs(1)),
    )?;
    let output =
        local_ipc::FrameReader::new(super::MAX_RESPONSE).next_frame(&mut stream, deadline)?;
    let mut response: Value = serde_json::from_slice(&output)
        .map_err(|error| anyhow::Error::new(MalformedReply).context(error))?;
    let body = response.as_object_mut().ok_or(MalformedReply)?;
    if body.remove("v") != Some(json!(1)) || body.remove("id") != Some(json!(id)) {
        return Err(MalformedReply.into());
    }
    match body
        .remove("status")
        .and_then(|value| value.as_str().map(str::to_owned))
        .as_deref()
    {
        Some("completed") => Ok(response),
        Some("failed") => {
            let code = body
                .get("error")
                .and_then(Value::as_str)
                .ok_or(MalformedReply)?;
            ensure!(
                !code.is_empty()
                    && code.len() <= 256
                    && code
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)),
                "invalid service error code"
            );
            if code == "service-instance-conflict" {
                return Err(StaleIdentity.into());
            }
            if code == "task-busy" {
                return Err(crate::tasks::store::Busy.into());
            }
            let message = body
                .get("message")
                .map(|value| -> Result<String> {
                    let message = value.as_str().ok_or(MalformedReply)?;
                    ensure!(
                        message.chars().count() <= 512,
                        "service error message exceeds limit"
                    );
                    Ok(message
                        .chars()
                        .map(|ch| if ch.is_control() { '?' } else { ch })
                        .collect())
                })
                .transpose()?;
            Err(RemoteFailure {
                code: code.into(),
                message,
            }
            .into())
        }
        _ => Err(MalformedReply.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
        time::Duration,
    };

    fn snapshot() -> Value {
        json!({"v":1,"id":1,"status":"completed","service_instance":"service-a","sequence":1,"stale":false,"published_age_ms":0,
            "snapshot":{"rules_generation":1,"scan_duration_ms":1,"event_streams":0,"event_failures":0,"observations":[],"removed":[],"problems":{}}})
    }
    fn with_peer<T>(replies: Vec<Vec<u8>>, call: impl FnOnce(Client) -> Result<T>) -> Result<T> {
        let root = tempfile::tempdir()?;
        let socket = root.path().join("service.sock");
        let listener = UnixListener::bind(&socket)?;
        let peer = std::thread::spawn(move || -> std::io::Result<()> {
            for reply in replies {
                let (mut stream, _) = listener.accept()?;
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                let mut request = String::new();
                BufReader::new(&mut stream).read_line(&mut request)?;
                let _ = stream.write_all(&reply);
            }
            Ok(())
        });
        let result = call(Client::socket(socket)?);
        peer.join()
            .map_err(|_| anyhow::anyhow!("peer panicked"))??;
        result
    }
    fn line(value: Value) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    #[test]
    fn refusal_explanations_are_bounded_and_cannot_emit_terminal_controls() -> Result<()> {
        with_peer(
            vec![line(
                json!({"v":1,"id":1,"status":"failed","error":"task-failed",
            "message":"pane process changed\u{1b}[2J"}),
            )?],
            |client| {
                let error = client
                    .snapshot(Instant::now() + Duration::from_secs(1))
                    .err()
                    .context("refusal")?;
                assert!(error.to_string().contains("pane process changed?[2J"));
                assert!(!error.to_string().contains('\u{1b}'));
                Ok(())
            },
        )?;
        for message in [json!(false), json!("x".repeat(513))] {
            with_peer(
                vec![line(
                    json!({"v":1,"id":1,"status":"failed","error":"task-failed","message":message}),
                )?],
                |client| {
                    let error = client
                        .snapshot(Instant::now() + Duration::from_secs(1))
                        .err()
                        .context("malformed refusal")?;
                    assert!(!error.is::<RemoteFailure>());
                    Ok(())
                },
            )?;
        }
        Ok(())
    }

    #[test]
    fn observed_attachment_reply_can_move_routes_but_cannot_replace_processes() -> Result<()> {
        use crate::tasks::model::{Origin, Target};
        let handle = crate::watch::Handle {
            instance: "fux-one".into(),
            workspace: "before".into(),
            stream: 1,
            pane: 2,
            pid: Some(3),
        };
        let target = Target {
            runtime: "/remote/fux".into(),
            instance: handle.instance.clone(),
            workspace: "after".into(),
            stream: 2,
            pane: 2,
            pid: Some(3),
            origin: Some(Origin {
                workspace: "before".into(),
                stream: 1,
            }),
        };
        for (valid, response) in [
            (true, target.clone()),
            (
                false,
                Target {
                    pid: Some(4),
                    ..target.clone()
                },
            ),
            (
                false,
                Target {
                    runtime: "relative".into(),
                    ..target.clone()
                },
            ),
            (
                false,
                Target {
                    origin: None,
                    ..target.clone()
                },
            ),
        ] {
            with_peer(
                vec![line(
                    json!({"v":1,"id":1,"status":"completed","service_instance":"service-a","value":response}),
                )?],
                |client| {
                    let result = client.observed_attachment(
                        "service-a",
                        &handle,
                        Instant::now() + Duration::from_secs(1),
                    );
                    assert_eq!(result.is_ok(), valid);
                    if valid {
                        assert_eq!(result?.workspace, "after");
                    }
                    Ok(())
                },
            )?;
        }
        Ok(())
    }

    #[test]
    fn combined_supervision_uses_one_reply_and_rejects_inconsistent_authority() -> Result<()> {
        let mut observation = snapshot();
        let body = observation.as_object_mut().context("snapshot object")?;
        body.remove("v");
        body.remove("id");
        body.remove("status");
        let valid = json!({"v":1,"id":1,"status":"completed",
            "service_instance":"service-a","features":["supervision-v1"],
            "observation":observation,
            "overview":{"runtime":"/remote/fux","state_directory":"/remote/state",
                "generation":null,"rows":[],"integrations":[],"native_integrations":[]}});
        with_peer(vec![line(valid.clone())?], |client| {
            let view = client.supervision(Instant::now() + Duration::from_secs(1))?;
            assert_eq!(view.service_instance, "service-a");
            assert!(!view.stale);
            Ok(())
        })?;
        for change in ["incarnation", "capability", "location", "shape"] {
            let mut invalid = valid.clone();
            match change {
                "incarnation" => {
                    *invalid
                        .pointer_mut("/observation/service_instance")
                        .context("instance")? = json!("replacement");
                }
                "capability" => {
                    *invalid.get_mut("features").context("features")? = json!(["snapshot-v1"]);
                }
                "location" => {
                    *invalid
                        .pointer_mut("/overview/runtime")
                        .context("runtime")? = json!("relative");
                }
                _ => {
                    invalid
                        .as_object_mut()
                        .context("reply")?
                        .insert("unexpected".into(), json!(true));
                }
            }
            with_peer(vec![line(invalid)?], |client| {
                assert!(
                    client
                        .supervision(Instant::now() + Duration::from_secs(1))
                        .is_err(),
                    "{change}"
                );
                Ok(())
            })?;
        }
        Ok(())
    }

    #[test]
    fn attachment_resolution_accepts_route_changes_but_not_process_substitution() -> Result<()> {
        use crate::tasks::{
            model::{Origin, Target},
            supervise::Expected,
        };
        let target = Target {
            runtime: "/remote".into(),
            instance: "fux-one".into(),
            workspace: "old".into(),
            stream: 1,
            pane: 2,
            pid: Some(3),
            origin: Some(Origin {
                workspace: "old".into(),
                stream: 1,
            }),
        };
        let expected = Expected {
            task: "same".into(),
            attempt: "attempt".into(),
            session: "session".into(),
            target: target.clone(),
        };
        let mut moved = target;
        moved.workspace = "new".into();
        moved.stream = 2;
        for valid in [true, false] {
            let mut reply = moved.clone();
            if !valid {
                reply.pid = Some(4);
            }
            with_peer(
                vec![line(
                    json!({"v":1,"id":1,"status":"completed","service_instance":"service-a","value":reply}),
                )?],
                |client| {
                    let result = client.task_attachment(
                        "service-a",
                        &expected,
                        Instant::now() + Duration::from_secs(1),
                    );
                    if valid {
                        assert_eq!(result?.workspace, "new");
                    } else {
                        assert!(result.is_err());
                    }
                    Ok(())
                },
            )?;
        }
        Ok(())
    }

    #[test]
    fn busy_reads_retry_within_deadline_but_supervision_is_sent_once() -> Result<()> {
        let busy = json!({"v":1,"id":1,"status":"failed","error":"task-busy"});
        let listed = json!({"v":1,"id":1,"status":"completed","service_instance":"service-a",
            "value":{"generation":0,"tasks":[],"launches":[]}});
        with_peer(vec![line(busy.clone())?, line(listed)?], |client| {
            assert!(
                client
                    .task_list("service-a", Instant::now() + Duration::from_secs(1))?
                    .tasks
                    .is_empty()
            );
            Ok(())
        })?;
        let expected = crate::tasks::supervise::Expected {
            task: "same".into(),
            attempt: "attempt".into(),
            session: "session".into(),
            target: crate::tasks::model::Target {
                runtime: "/remote".into(),
                instance: "fux".into(),
                workspace: "default".into(),
                stream: 1,
                pane: 1,
                pid: Some(1),
                origin: None,
            },
        };
        with_peer(vec![line(busy)?], |client| {
            let error = client
                .task_supervise(
                    "service-a",
                    &expected,
                    crate::tasks::supervise::Action::Cancel,
                    Instant::now() + Duration::from_millis(100),
                )
                .err()
                .context("expected busy mutation")?;
            assert!(
                error.is::<crate::tasks::store::Busy>(),
                "mutation retried after definite busy response: {error:#}"
            );
            Ok(())
        })?;
        Ok(())
    }

    #[test]
    fn task_reads_reject_cross_task_links_and_wrong_service_identity() -> Result<()> {
        let value = json!({"generation":1,"launch":null,
            "task":{"id":"same","requested_runtime":"/remote","title":"remote task","created_ms":1,"outcome":"open","attempt":"attempt-one"},
            "attempt":{"id":"attempt-one","task":"same","session":"session-one","state":"active"},
            "session":{"id":"session-one","target":{"runtime":"/remote","instance":"fux-one","workspace":"other","stream":1,"pane":2,"pid":3},"agent":null,"ownership":"adopted","created_ms":1},
            "prompts":[],"checks":[]});
        let envelope = |instance: &str, value: Value| json!({"v":1,"id":1,"status":"completed","service_instance":instance,"value":value});
        with_peer(
            vec![line(envelope("service-a", value.clone()))?],
            |client| {
                let inspection = client.task_inspect(
                    "service-a",
                    "same",
                    Instant::now() + Duration::from_secs(1),
                )?;
                assert_eq!(
                    inspection.task.as_ref().map(|task| task.title.as_str()),
                    Some("remote task")
                );
                assert_eq!(
                    inspection
                        .session
                        .as_ref()
                        .map(|session| session.target.workspace.as_str()),
                    Some("other")
                );
                Ok(())
            },
        )?;
        let mut crossed = value.clone();
        *crossed
            .pointer_mut("/attempt/task")
            .context("attempt task")? = json!("other-task");
        let mut missing = value.clone();
        missing
            .as_object_mut()
            .context("inspection object")?
            .remove("session");
        for reply in [
            envelope("service-b", value),
            envelope("service-a", crossed),
            envelope("service-a", missing),
        ] {
            with_peer(vec![line(reply)?], |client| {
                assert!(
                    client
                        .task_inspect("service-a", "same", Instant::now() + Duration::from_secs(1))
                        .is_err()
                );
                Ok(())
            })?;
        }
        with_peer(
            vec![line(envelope(
                "service-a",
                json!({"v":1,"generation":1,"task_id":"other","task_outcome":null,"attempt":null,"session":null}),
            ))?],
            |client| {
                assert!(
                    client
                        .task_result("service-a", "same", Instant::now() + Duration::from_secs(1))
                        .is_err()
                );
                Ok(())
            },
        )?;
        Ok(())
    }

    #[test]
    fn reads_validate_correlation_schema_and_remote_failure_provenance() -> Result<()> {
        let valid = snapshot();
        let mut wrong_id = valid.clone();
        *wrong_id.get_mut("id").context("reply ID")? = json!(2);
        let mut missing_stale = valid.clone();
        missing_stale
            .as_object_mut()
            .context("object")?
            .remove("stale");
        let mut missing_age = valid.clone();
        missing_age
            .as_object_mut()
            .context("object")?
            .remove("published_age_ms");
        for value in [
            wrong_id,
            missing_stale,
            missing_age,
            json!({"v":1,"id":1,"status":"completed"}),
            json!({"v":1,"id":2,"status":"failed","error":"task-busy"}),
        ] {
            with_peer(vec![line(value)?], |client| {
                let error = client
                    .snapshot(Instant::now() + Duration::from_secs(1))
                    .err()
                    .context("expected invalid reply")?;
                assert!(error.is::<MalformedReply>(), "{error:#}");
                assert!(!error.is::<crate::tasks::store::Busy>());
                Ok(())
            })?;
        }
        with_peer(vec![line(valid)?], |client| {
            assert_eq!(
                client
                    .snapshot(Instant::now() + Duration::from_secs(1))?
                    .service_instance,
                "service-a"
            );
            Ok(())
        })?;
        with_peer(
            vec![line(
                json!({"v":1,"id":1,"status":"failed","error":"not-authorized"}),
            )?],
            |client| {
                let error = client
                    .snapshot(Instant::now() + Duration::from_secs(1))
                    .err()
                    .context("expected refusal")?;
                assert_eq!(
                    error
                        .downcast_ref::<RemoteFailure>()
                        .context("remote failure")?
                        .code,
                    "not-authorized"
                );
                Ok(())
            },
        )
    }

    #[test]
    fn capabilities_are_explicit_and_bounded() -> Result<()> {
        with_peer(
            vec![line(
                json!({"v":1,"id":1,"status":"completed","service_instance":"service-a","features":["snapshot-v1","overview-v1"]}),
            )?],
            |client| {
                let capabilities = client.capabilities(Instant::now() + Duration::from_secs(1))?;
                assert!(capabilities.features.contains("overview-v1"));
                assert!(!capabilities.features.contains("automatic-restart"));
                Ok(())
            },
        )?;
        with_peer(
            vec![line(
                json!({"v":1,"id":1,"status":"completed","service_instance":"service-a"}),
            )?],
            |client| {
                assert!(
                    client
                        .capabilities(Instant::now() + Duration::from_secs(1))
                        .is_err()
                );
                Ok(())
            },
        )
    }

    #[test]
    fn overview_cannot_join_different_service_incarnations() -> Result<()> {
        with_peer(
            vec![
                line(snapshot())?,
                line(
                    json!({"v":1,"id":1,"status":"completed","service_instance":"service-b","value":{}}),
                )?,
            ],
            |client| {
                let error = client
                    .view(Instant::now() + Duration::from_secs(1))
                    .err()
                    .context("expected stale identity")?;
                assert!(error.is::<StaleIdentity>());
                Ok(())
            },
        )
    }

    #[test]
    fn malformed_frames_and_oversize_never_produce_a_view() -> Result<()> {
        for bytes in [
            b"{broken}\n".to_vec(),
            vec![b'x'; super::super::MAX_RESPONSE + 2],
            b"{\"v\":1}".to_vec(),
        ] {
            with_peer(vec![bytes], |client| {
                assert!(
                    client
                        .snapshot(Instant::now() + Duration::from_secs(1))
                        .is_err()
                );
                Ok(())
            })?;
        }
        Ok(())
    }

    #[test]
    fn partial_progress_cannot_renew_the_absolute_request_deadline() -> Result<()> {
        let root = tempfile::tempdir()?;
        let socket = root.path().join("slow.sock");
        let listener = UnixListener::bind(&socket)?;
        let bytes = line(snapshot())?;
        let peer = std::thread::spawn(move || -> std::io::Result<()> {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            let mut request = String::new();
            BufReader::new(&mut stream).read_line(&mut request)?;
            for byte in bytes {
                if stream.write_all(&[byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(())
        });
        let start = Instant::now();
        assert!(
            Client::socket(socket)?
                .snapshot(start + Duration::from_millis(150))
                .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        peer.join()
            .map_err(|_| anyhow::anyhow!("slow peer panicked"))??;
        Ok(())
    }

    #[test]
    fn unavailable_proxy_does_not_create_a_local_service_directory() -> Result<()> {
        let root = tempfile::tempdir()?;
        let proxy = root.path().join("missing/control.sock");
        assert!(
            Client::socket(proxy.clone())?
                .view(Instant::now() + Duration::from_millis(150))
                .is_err()
        );
        assert!(!proxy.parent().context("proxy parent")?.exists());
        Ok(())
    }
}
