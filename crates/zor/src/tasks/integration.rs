//! Managed adapter registration and prompt arming. All application policy belongs to zor.
use super::{model::*, store::Store, submit};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::Path,
    time::{Duration, Instant},
};

const PLUGIN: &str = include_str!("../../integrations/opencode.mjs");

fn private_directory(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let meta = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        meta.is_dir() && meta.uid() == nix::unistd::geteuid().as_raw() && meta.mode() & 0o077 == 0,
        "adapter directory must be owned and private"
    );
    Ok(())
}

pub(super) fn prepare(root: &Path, marker: &str, kind: IntegrationKind) -> Result<Integration> {
    let root = fs::canonicalize(root)?;
    let directory = root.join("adapters");
    private_directory(&directory)?;
    let socket = directory.join(format!("{marker}.sock"));
    anyhow::ensure!(
        socket.as_os_str().len() < 104,
        "adapter socket path is too long; use a shorter state directory"
    );
    let digest = format!("{:x}", Sha256::digest(PLUGIN.as_bytes()));
    let plugin = directory.join(format!("opencode-{digest}.mjs"));
    match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&plugin)
    {
        Ok(file) => {
            let meta = file.metadata()?;
            anyhow::ensure!(
                meta.is_file()
                    && meta.uid() == nix::unistd::geteuid().as_raw()
                    && meta.mode() & 0o077 == 0
                    && meta.nlink() == 1
                    && meta.len() == PLUGIN.len() as u64,
                "invalid retained adapter asset"
            );
            let mut bytes = Vec::new();
            file.take((PLUGIN.len() + 1) as u64)
                .read_to_end(&mut bytes)?;
            anyhow::ensure!(bytes == PLUGIN.as_bytes(), "adapter asset content changed");
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Same private uncommitted-file namespace that Store cleans after a crash.
            let temporary = root.join(format!(".journal-{}.tmp", super::store::nonce()?));
            let result = (|| -> Result<()> {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&temporary)?;
                file.write_all(PLUGIN.as_bytes())?;
                file.sync_all()?;
                fs::rename(&temporary, &plugin)?;
                File::open(&directory)?.sync_all()?;
                Ok(())
            })();
            if temporary.exists() {
                let _ = fs::remove_file(&temporary);
            }
            result?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(Integration {
        kind,
        root,
        binary: fs::canonicalize(std::env::current_exe()?)?,
        plugin,
        socket,
        producer: None,
        registered_ms: None,
        storage_environment: None,
        retired: Vec::new(),
    })
}

/// Executes the real application in the same process, preserving inherited configuration.
/// No PTY, child supervisor, personal config edit, or background service is introduced here.
pub fn exec_opencode(plugin: &Path, argv: Vec<String>) -> Result<u8> {
    let mut config: Value = match std::env::var("OPENCODE_CONFIG_CONTENT") {
        Ok(text) => {
            anyhow::ensure!(
                text.len() <= 131072,
                "inline OpenCode config exceeds integration limit"
            );
            serde_json::from_str(&text)
                .context("managed integration requires strict JSON in OPENCODE_CONFIG_CONTENT")?
        }
        Err(std::env::VarError::NotPresent) => json!({}),
        Err(error) => return Err(error.into()),
    };
    let object = config
        .as_object_mut()
        .context("inline OpenCode config must be an object")?;
    let plugins = object
        .entry("plugin")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context("inline OpenCode plugins must be an array")?;
    let plugin = plugin.to_str().context("non-UTF8 integration asset path")?;
    if !plugins.iter().any(|p| p.as_str() == Some(plugin)) {
        plugins.push(json!(plugin));
    }
    let program = argv.first().context("missing OpenCode command")?;
    let error = std::process::Command::new(program)
        .args(argv.iter().skip(1))
        .env("OPENCODE_CONFIG_CONTENT", serde_json::to_string(&config)?)
        .exec();
    Err(error).context("exec managed OpenCode")
}

fn rpc(
    integration: &Integration,
    marker: &str,
    mut request: Value,
    deadline: Instant,
) -> Result<Value> {
    let object = request
        .as_object_mut()
        .context("adapter request must be an object")?;
    object.insert("v".into(), json!(1));
    object.insert("marker".into(), json!(marker));
    let mut bytes = serde_json::to_vec(&request)?;
    anyhow::ensure!(bytes.len() <= 131072, "adapter request limit exceeded");
    bytes.push(b'\n');
    let mut stream = crate::fux::connect(&integration.socket, deadline)?;
    crate::fux::same_user(&stream)?;
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .context("adapter deadline expired")
    };
    stream.set_write_timeout(Some(remaining()?))?;
    stream.write_all(&bytes)?;
    let mut output = Vec::new();
    loop {
        stream.set_read_timeout(Some(remaining()?))?;
        let mut chunk = [0; 1024];
        let n = stream.read(&mut chunk)?;
        anyhow::ensure!(
            n != 0 && output.len() + n <= 4096,
            "missing or oversized adapter response"
        );
        output.extend_from_slice(chunk.get(..n).context("adapter read size")?);
        if output.contains(&b'\n') {
            break;
        }
    }
    let value: Value = serde_json::from_slice(&output)?;
    anyhow::ensure!(
        value.get("v").and_then(Value::as_u64) == Some(1),
        "incompatible adapter protocol"
    );
    Ok(value)
}

pub fn register(root: &Path, id: &str, marker: &str, producer: &str) -> Result<Value> {
    anyhow::ensure!(super::model::id(producer), "invalid adapter producer");
    let store = Store::open(root)?;
    let launch = store
        .journal()
        .launches
        .get(id)
        .context("launch not found")?
        .clone();
    if launch.marker != marker
        && store.journal().launches.values().any(|candidate| {
            candidate.task.as_deref() == Some(id)
                && candidate.marker == marker
                && candidate.resume.is_some()
                && !matches!(candidate.phase, LaunchPhase::Attached | LaunchPhase::Closed)
        })
    {
        anyhow::bail!("managed launch is not ready");
    }
    anyhow::ensure!(launch.marker == marker, "managed launch marker mismatch");
    let integration = launch
        .integration
        .as_ref()
        .context("launch has no configured integration")?;
    if integration.producer.as_deref() == Some(producer) {
        return Ok(serde_json::to_value(integration)?);
    }
    anyhow::ensure!(
        !integration
            .retired
            .iter()
            .any(|old| old.producer == producer),
        "adapter producer is retired; use a new lifetime identity"
    );
    anyhow::ensure!(
        integration.retired.len() < 16,
        "adapter lifetime retention limit reached"
    );
    anyhow::ensure!(
        launch.phase == LaunchPhase::Attached && !launch.stop_requested,
        "managed launch is not ready"
    );
    let target = store
        .journal()
        .sessions
        .get(launch.session.as_ref().context("managed session missing")?)
        .context("managed session missing")?
        .target
        .clone();
    drop(store);
    let deadline = Instant::now() + Duration::from_secs(2);
    submit::verify_target(&target, deadline)?;
    let hello = rpc(integration, marker, json!({"op":"hello"}), deadline)?;
    anyhow::ensure!(
        hello.get("producer").and_then(Value::as_str) == Some(producer)
            && hello.get("status").and_then(Value::as_str) == Some("ready"),
        "adapter producer handshake mismatch"
    );
    let storage_environment = hello
        .get("storage_environment")
        .filter(|value| !value.is_null())
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .context("invalid adapter storage environment")?;
    if let Some(environment) = &storage_environment {
        validate_storage_environment(environment)?;
    }
    // Registration replacement cannot establish that the native application moved
    // an already-open database. Preserve eligibility only for an unchanged namespace;
    // once unavailable in this launch, a later plugin reload cannot invent it.
    let storage_environment = if integration.producer.is_none()
        || integration.storage_environment == storage_environment
    {
        storage_environment
    } else {
        None
    };
    submit::verify_target(&target, deadline)?;
    let now = super::now_ms()?;
    let mut store = Store::open(root)?;
    let current = store.journal().launches.get(id).context("launch missing")?;
    let profile = current
        .integration
        .as_ref()
        .context("integration missing")?;
    anyhow::ensure!(
        current.marker == marker
            && current.phase == LaunchPhase::Attached
            && !current.stop_requested
            && profile.producer == integration.producer
            && profile.registered_ms == integration.registered_ms
            && current
                .session
                .as_ref()
                .and_then(|id| store.journal().sessions.get(id))
                .is_some_and(|session| session.target == target),
        "adapter registration changed during endpoint probe; retry"
    );
    store.transaction(|journal| {
        let affected: Vec<_> = journal.prompts.values().filter(|prompt|
            for_prompt(journal, prompt).is_some_and(|owner| owner.id == id)
                && prompt.arm.is_some() && !prompt.released && prompt.response.is_none()
                && !super::wait::terminal(&prompt.wait))
            .map(|prompt| prompt.id.clone()).collect();
        let profile = journal.launches.get_mut(id).and_then(|l| l.integration.as_mut())
            .context("integration missing")?;
        if let Some(old) = &profile.producer {
            let registered_ms = profile.registered_ms.context("registration time missing")?;
            anyhow::ensure!(now >= registered_ms, "adapter registration clock moved backwards");
            profile.retired.push(RetiredProducer { producer: old.clone(), registered_ms, retired_ms: now });
        }
        profile.producer = Some(producer.into());
        profile.registered_ms = Some(now);
        profile.storage_environment = storage_environment;
        for id in affected {
            let prompt = journal.prompts.get_mut(&id).context("prompt missing")?;
            prompt.wait = WaitOutcome::Uncertain;
            prompt.wait_problem = Some("adapter producer retired; input was not replayed; inspect delivery and explicitly abandon unresolved coordination before a new prompt".into());
        }
        Ok(())
    })?;
    Ok(serde_json::to_value(
        store
            .journal()
            .launches
            .get(id)
            .and_then(|l| l.integration.as_ref()),
    )?)
}

/// Retained ancestry survives producer replacement, but cannot authorize new input/events.
pub(super) fn retired(journal: &Journal, prompt: &Prompt) -> bool {
    prompt.arm.as_ref().is_some_and(|arm| {
        for_prompt(journal, prompt)
            .and_then(|launch| launch.integration.as_ref())
            .is_some_and(|integration| {
                integration
                    .retired
                    .iter()
                    .any(|old| old.producer == arm.producer)
            })
    })
}

/// A point-in-time endpoint diagnostic, never a retained heartbeat or prompt report.
pub fn status(root: &Path, id: &str) -> Result<Value> {
    let started = Instant::now();
    let started_ms = super::now_ms()?;
    let store = Store::open(root)?;
    let journal = store.journal();
    anyhow::ensure!(
        journal.tasks.contains_key(id) || journal.launches.contains_key(id),
        "task not found"
    );
    let generation = journal.generation;
    let launch = journal.launches.get(id).cloned();
    let target = launch
        .as_ref()
        .and_then(|launch| launch.session.as_ref())
        .and_then(|session| journal.sessions.get(session))
        .map(|session| session.target.clone());
    // A slow or unavailable adapter must not hold the journal writer lock.
    drop(store);
    let integration = launch
        .as_ref()
        .and_then(|launch| launch.integration.as_ref());
    let mut problem = None;
    let mut stage = "retained";
    let mut availability = match (launch.as_ref(), integration) {
        (_, None) => "not-configured",
        (Some(launch), _) if launch.phase != LaunchPhase::Attached || launch.stop_requested => {
            "inactive"
        }
        (_, Some(integration)) if integration.producer.is_none() => "not-registered",
        (Some(launch), Some(integration)) => {
            let deadline = started + Duration::from_secs(2);
            let probe = (|| -> Result<()> {
                stage = "target";
                let target = target.as_ref().context("managed session missing")?;
                submit::verify_target(target, deadline)?;
                stage = "adapter";
                let hello = rpc(integration, &launch.marker, json!({"op":"hello"}), deadline)?;
                anyhow::ensure!(
                    hello.get("producer").and_then(Value::as_str)
                        == integration.producer.as_deref()
                        && hello.get("status").and_then(Value::as_str) == Some("ready"),
                    "adapter producer handshake mismatch"
                );
                stage = "target";
                submit::verify_target(target, deadline)?;
                Ok(())
            })();
            match probe {
                Ok(()) => {
                    stage = "complete";
                    "reachable"
                }
                Err(error) => {
                    problem = Some(format!("{error:#}").chars().take(512).collect::<String>());
                    "unavailable"
                }
            }
        }
        _ => anyhow::bail!("integration requires a launch"),
    };
    // Do not present a successful sample as current after concurrent journal changes.
    let store = Store::open(root)?;
    let current_generation = store.journal().generation;
    if current_generation != generation {
        availability = "changed";
        stage = "journal";
        problem = Some("journal changed during adapter observation; retry".into());
    }
    Ok(
        json!({"v":1,"task_id":id,"generation":generation,"current_generation":current_generation,
        "kind":integration.map(|value| &value.kind),
        "producer":integration.and_then(|value| value.producer.as_ref()),
        "registered_ms":integration.and_then(|value| value.registered_ms),
        "heartbeat":launch.as_ref().filter(|launch| launch.integration.is_some()).map(|launch| super::heartbeat::view(root, launch, store.journal())),
        "availability":availability,"stage":stage,"problem":problem,
        "started_ms":started_ms,"finished_ms":super::now_ms()?,
        "duration_ms":started.elapsed().as_millis(),"scope":"point-in-time-endpoint-probe"}),
    )
}

pub(super) fn for_prompt<'a>(journal: &'a Journal, prompt: &Prompt) -> Option<&'a Launch> {
    journal
        .attempts
        .get(&prompt.attempt)
        .and_then(|a| journal.sessions.get(&a.session))
        .and_then(|s| s.launch.as_ref())
        .and_then(|id| journal.launches.get(id))
        .filter(|l| l.integration.is_some())
}

pub(super) fn arm(store: &mut Store, id: &str, deadline: Instant) -> Result<()> {
    let prompt = store
        .journal()
        .prompts
        .get(id)
        .context("prompt missing")?
        .clone();
    let Some(launch) = for_prompt(store.journal(), &prompt).cloned() else {
        return Ok(());
    };
    let integration = launch.integration.as_ref().context("integration missing")?;
    let producer = integration
        .producer
        .as_ref()
        .context("adapter has not registered; retry after startup")?;
    anyhow::ensure!(
        !retired(store.journal(), &prompt),
        "prompt adapter producer is retired"
    );
    let receipt = prompt.receipt.as_ref().context("input receipt missing")?;
    if prompt.arm.is_none() {
        store.transaction(|journal| {
            journal.prompts.get_mut(id).context("prompt missing")?.arm = Some(PromptArm {
                producer: producer.clone(),
                input_operation: receipt.operation,
                acknowledged: false,
                input_started: false,
                disarm_requested: false,
                disarmed: false,
            });
            Ok(())
        })?;
    }
    let reply = rpc(
        integration,
        &launch.marker,
        json!({"op":"arm","producer":producer,
        "prompt":{"operation":prompt.id,"token":prompt.report_token,"input_operation":receipt.operation,
            "text":prompt.text,"deadline_ms":prompt.deadline_ms}}),
        deadline.min(Instant::now() + Duration::from_secs(2)),
    )?;
    anyhow::ensure!(
        reply.get("status").and_then(Value::as_str) == Some("armed")
            && reply.get("producer").and_then(Value::as_str) == Some(producer)
            && reply.get("operation").and_then(Value::as_str) == Some(id)
            && reply.get("token").and_then(Value::as_str) == prompt.report_token.as_deref()
            && reply.get("input_operation").and_then(Value::as_u64) == Some(receipt.operation),
        "adapter did not acknowledge this prompt; reconcile and retry the same operation"
    );
    store.transaction(|journal| {
        journal
            .prompts
            .get_mut(id)
            .and_then(|p| p.arm.as_mut())
            .context("prompt arm missing")?
            .acknowledged = true;
        Ok(())
    })
}

/// Retire only a released arm whose input was never attempted by zor. Once the
/// reserved receipt is proven, persist intent before RPC so lost acknowledgements
/// can retry after receipt expiry without weakening the original proof.
pub(super) fn disarm(store: &mut Store, id: &str) -> Result<()> {
    let prompt = store
        .journal()
        .prompts
        .get(id)
        .context("prompt missing")?
        .clone();
    let Some(arm) = &prompt.arm else {
        return Ok(());
    };
    if retired(store.journal(), &prompt) {
        // Replacement fences this lifetime at registration and at the new endpoint.
        // Do not invent an acknowledgement from an adapter that is gone.
        return Ok(());
    }
    if arm.disarmed || arm.input_started {
        return Ok(());
    }
    anyhow::ensure!(prompt.released, "only released prompt arms can be retired");
    let launch = for_prompt(store.journal(), &prompt)
        .context("integration missing")?
        .clone();
    let integration = launch.integration.as_ref().context("integration missing")?;
    let session = launch
        .session
        .as_ref()
        .and_then(|s| store.journal().sessions.get(s))
        .context("managed session missing")?;
    let target = session.target.clone();
    let receipt = prompt.receipt.as_ref().context("input receipt missing")?;
    let deadline = Instant::now() + Duration::from_secs(4);
    submit::verify_target(&target, deadline)?;
    if !arm.disarm_requested {
        let response = submit::request(
            &target,
            "input-status",
            json!({"operation":receipt.operation}),
            deadline,
        )?;
        let (phase, current) =
            submit::receipt(response, &target, Some(receipt), prompt.text.len() + 1)?;
        anyhow::ensure!(
            phase == Delivery::Reserved,
            "cannot retire arm: input is not proven unsent"
        );
        store.transaction(|journal| {
            let prompt = journal.prompts.get_mut(id).context("prompt missing")?;
            prompt.delivery = phase;
            prompt.receipt = Some(current);
            prompt.arm.as_mut().context("arm missing")?.disarm_requested = true;
            Ok(())
        })?;
    }
    let reply = rpc(
        integration,
        &launch.marker,
        json!({"op":"disarm","producer":arm.producer,
        "prompt":{"operation":prompt.id,"token":prompt.report_token,"input_operation":receipt.operation,
            "text":prompt.text,"deadline_ms":prompt.deadline_ms}}),
        deadline.min(Instant::now() + Duration::from_secs(2)),
    )?;
    anyhow::ensure!(
        reply.get("status").and_then(Value::as_str) == Some("disarmed")
            && reply.get("producer").and_then(Value::as_str) == Some(&arm.producer)
            && reply.get("operation").and_then(Value::as_str) == Some(id)
            && reply.get("token").and_then(Value::as_str) == prompt.report_token.as_deref()
            && reply.get("input_operation").and_then(Value::as_u64) == Some(receipt.operation),
        "adapter did not acknowledge retirement; retry abandon for the same operation"
    );
    store.transaction(|journal| {
        journal
            .prompts
            .get_mut(id)
            .and_then(|p| p.arm.as_mut())
            .context("arm missing")?
            .disarmed = true;
        Ok(())
    })
}

const STORAGE_VARIABLES: [&str; 5] = [
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
];
pub(super) fn validate_storage_environment(
    environment: &std::collections::BTreeMap<String, Option<String>>,
) -> Result<()> {
    anyhow::ensure!(
        environment.len() == STORAGE_VARIABLES.len()
            && STORAGE_VARIABLES
                .iter()
                .all(|key| environment.contains_key(*key))
            && environment.get("HOME").is_some_and(Option::is_some)
            && environment.values().flatten().all(|value| !value.is_empty()
                && Path::new(value).is_absolute()
                && !value.chars().any(char::is_control))
            && environment
                .values()
                .flatten()
                .map(String::len)
                .sum::<usize>()
                <= 2048,
        "invalid adapter storage environment"
    );
    Ok(())
}

pub(super) fn validate(journal: &Journal) -> Result<()> {
    for launch in journal.launches.values() {
        if let Some(integration) = &launch.integration {
            if let Some(environment) = &integration.storage_environment {
                anyhow::ensure!(
                    integration.producer.is_some(),
                    "storage environment needs a registered adapter"
                );
                validate_storage_environment(environment)?;
            }
            anyhow::ensure!(
                integration.retired.len() <= 16,
                "adapter lifetime retention limit"
            );
            let mut producers = std::collections::BTreeSet::new();
            let mut since = launch.created_ms;
            for old in &integration.retired {
                anyhow::ensure!(
                    super::model::id(&old.producer)
                        && producers.insert(&old.producer)
                        && integration.producer.as_ref() != Some(&old.producer)
                        && old.registered_ms >= since
                        && old.retired_ms >= old.registered_ms,
                    "invalid retired adapter lifetime"
                );
                since = old.retired_ms;
            }
            anyhow::ensure!(
                integration.retired.is_empty()
                    || integration.registered_ms.is_some_and(|at| at >= since),
                "invalid current adapter registration time"
            );
            anyhow::ensure!(
                [
                    &integration.root,
                    &integration.binary,
                    &integration.plugin,
                    &integration.socket
                ]
                .iter()
                .all(|p| p.is_absolute()
                    && p.to_str()
                        .is_some_and(|s| s.len() <= 4096 && !s.chars().any(char::is_control)))
                    && integration.socket
                        == integration
                            .root
                            .join("adapters")
                            .join(format!("{}.sock", launch.marker))
                    && integration.socket.as_os_str().len() < 104
                    && integration.plugin.parent()
                        == Some(integration.root.join("adapters").as_path())
                    && integration
                        .plugin
                        .file_name()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.strip_prefix("opencode-"))
                        .and_then(|s| s.strip_suffix(".mjs"))
                        .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                    && integration.producer.is_some() == integration.registered_ms.is_some()
                    && integration
                        .producer
                        .as_ref()
                        .is_none_or(|p| super::model::id(p))
                    && integration
                        .registered_ms
                        .is_none_or(|t| t >= launch.created_ms),
                "invalid integration profile"
            );
            let argv = super::launch::command(launch);
            anyhow::ensure!(
                argv.len() <= 128
                    && argv.iter().all(|a| a.len() <= 4096)
                    && argv.iter().map(String::len).sum::<usize>() <= 16384,
                "integrated launch exceeds fux argv limits"
            );
        }
    }
    for prompt in journal.prompts.values() {
        let launch = for_prompt(journal, prompt);
        if let Some(arm) = &prompt.arm {
            let integration = launch
                .and_then(|l| l.integration.as_ref())
                .context("arm without integration")?;
            anyhow::ensure!(
                (integration.producer.as_ref() == Some(&arm.producer)
                    || integration
                        .retired
                        .iter()
                        .any(|old| old.producer == arm.producer))
                    && prompt
                        .receipt
                        .as_ref()
                        .is_some_and(|r| r.operation == arm.input_operation)
                    && (!arm.input_started || arm.acknowledged)
                    && (!arm.disarm_requested
                        || (prompt.released
                            && !arm.input_started
                            && prompt.delivery == Delivery::Reserved
                            && prompt
                                .receipt
                                .as_ref()
                                .is_some_and(|r| r.bytes_written == 0)
                            && prompt.report_binding.is_none()
                            && prompt.response.is_none()))
                    && (!arm.disarmed || arm.disarm_requested),
                "invalid prompt arm"
            );
            anyhow::ensure!(
                !retired(journal, prompt)
                    || prompt.response.is_some()
                    || super::wait::terminal(&prompt.wait)
                    || prompt.wait == WaitOutcome::Uncertain,
                "retired adapter prompt lacks uncertain coordination"
            );
        }
        if launch.is_some()
            && (matches!(
                prompt.delivery,
                Delivery::Submitting | Delivery::Queued | Delivery::Delivered
            ) || prompt.report_binding.is_some()
                || prompt.response.is_some())
        {
            let arm = prompt
                .arm
                .as_ref()
                .context("integrated submission has no prompt arm")?;
            anyhow::ensure!(
                arm.acknowledged && arm.input_started,
                "integrated submission lacks durable adapter/input acknowledgement"
            );
            if let Some(binding) = &prompt.report_binding {
                anyhow::ensure!(
                    binding.producer == arm.producer
                        && binding.input_operation == arm.input_operation,
                    "binding differs from armed producer/operation"
                );
            }
            anyhow::ensure!(
                prompt.response.is_none() || prompt.report_binding.is_some(),
                "integrated response has no native binding"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod storage_tests {
    use super::validate_storage_environment;
    use std::collections::BTreeMap;

    fn environment() -> BTreeMap<String, Option<String>> {
        super::STORAGE_VARIABLES
            .into_iter()
            .map(|key| {
                (
                    key.into(),
                    (key == "HOME").then(|| "/tmp/agent-home".into()),
                )
            })
            .collect()
    }
    #[test]
    fn retains_absent_xdg_without_defaulting_to_controller_paths() {
        let value = environment();
        assert!(validate_storage_environment(&value).is_ok());
        assert_eq!(value.get("XDG_DATA_HOME"), Some(&None));
    }
    #[test]
    fn rejects_unknown_secret_fields_and_unusable_or_unbounded_paths() {
        let mut value = environment();
        value.insert("OPENCODE_CONFIG_CONTENT".into(), Some("secret".into()));
        assert!(validate_storage_environment(&value).is_err());
        for path in [
            None,
            Some("".into()),
            Some("relative".into()),
            Some("/tmp/line\npath".into()),
            Some(format!("/{}", "x".repeat(2048))),
        ] {
            let mut value = environment();
            value.insert("HOME".into(), path);
            assert!(validate_storage_environment(&value).is_err());
        }
        let mut value = environment();
        value.remove("XDG_DATA_HOME");
        assert!(validate_storage_environment(&value).is_err());
    }
}
