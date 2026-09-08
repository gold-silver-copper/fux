//! Bounded producer freshness, separate from prompt evidence and task journal generations.
use super::{model::*, store::Store, submit};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const TTL_MS: u64 = 6000;
const MAX_BYTES: u64 = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentState {
    Unknown,
    Working,
    Blocked,
    Idle,
}
impl AgentState {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Idle => "idle",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub state: AgentState,
    pub operation: String,
    pub input_operation: u64,
    pub message: AgentMessage,
}

pub fn parse(text: &str) -> Result<Observation> {
    anyhow::ensure!(text.len() <= 1024, "adapter observation size limit");
    Ok(serde_json::from_str(text)?)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pulse {
    v: u32,
    launch: String,
    marker: String,
    producer: String,
    sequence: u64,
    received_ms: u64,
    monotonic_ms: u64,
    #[serde(default)]
    observation: Option<Observation>,
    #[serde(default)]
    input_sequence: Option<u64>,
}

fn monotonic_ms() -> Result<u64> {
    #[cfg(target_os = "linux")]
    let clock = nix::time::ClockId::CLOCK_BOOTTIME;
    #[cfg(not(target_os = "linux"))]
    let clock = nix::time::ClockId::CLOCK_MONOTONIC;
    let time = nix::time::clock_gettime(clock)?;
    let seconds = u64::try_from(time.tv_sec()).context("invalid monotonic clock")?;
    seconds
        .checked_mul(1000)
        .and_then(|ms| ms.checked_add(u64::try_from(time.tv_nsec()).ok()? / 1_000_000))
        .context("monotonic clock overflow")
}

fn directory(root: &Path, create: bool) -> Result<Option<PathBuf>> {
    let path = root.join("heartbeats");
    if create {
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        meta.is_dir() && meta.uid() == nix::unistd::geteuid().as_raw() && meta.mode() & 0o077 == 0,
        "heartbeat directory must be owned and private"
    );
    Ok(Some(path))
}

fn observation_receipt<'a>(
    journal: &'a Journal,
    launch: &Launch,
    observation: &Observation,
) -> Result<&'a Receipt> {
    let prompt = journal
        .prompts
        .get(&observation.operation)
        .context("observation prompt missing")?;
    let owner = super::integration::for_prompt(journal, prompt)
        .context("observation requires managed integration")?;
    let binding = prompt
        .report_binding
        .as_ref()
        .context("observation requires native binding")?;
    let producer = launch
        .integration
        .as_ref()
        .and_then(|integration| integration.producer.as_ref());
    anyhow::ensure!(
        owner.id == launch.id
            && owner.marker == launch.marker
            && Some(&binding.producer) == producer
            && binding.message == observation.message
            && binding.input_operation == observation.input_operation,
        "adapter observation identity mismatch"
    );
    let receipt = prompt
        .receipt
        .as_ref()
        .context("observation receipt missing")?;
    anyhow::ensure!(
        receipt.operation == observation.input_operation,
        "observation receipt mismatch"
    );
    Ok(receipt)
}

fn read(root: &Path, launch: &Launch, journal: &Journal) -> Result<Option<Pulse>> {
    let Some(directory) = directory(root, false)? else {
        return Ok(None);
    };
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join(format!("{}.json", launch.marker)))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let meta = file.metadata()?;
    anyhow::ensure!(
        meta.is_file()
            && meta.nlink() == 1
            && meta.len() <= MAX_BYTES
            && meta.uid() == nix::unistd::geteuid().as_raw()
            && meta.mode() & 0o077 == 0,
        "invalid heartbeat file"
    );
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() as u64 <= MAX_BYTES, "heartbeat size limit");
    let pulse: Pulse = serde_json::from_slice(&bytes)?;
    let integration = launch.integration.as_ref().context("integration missing")?;
    if pulse.v == 1
        && pulse.launch == launch.id
        && pulse.marker == launch.marker
        && integration.retired.iter().any(|old| {
            old.producer == pulse.producer
                && pulse.received_ms >= old.registered_ms
                && pulse.received_ms <= old.retired_ms
        })
    {
        // Registration fences the old lifetime atomically. Its last heartbeat cannot
        // lend freshness to the replacement; the replacement starts without a pulse.
        return Ok(None);
    }
    anyhow::ensure!(
        pulse.v == 1
            && pulse.launch == launch.id
            && pulse.marker == launch.marker
            && Some(&pulse.producer) == integration.producer.as_ref()
            && pulse.sequence > 0
            && integration
                .registered_ms
                .is_some_and(|at| pulse.received_ms >= at),
        "heartbeat identity or timestamp mismatch"
    );
    if let Some(observation) = &pulse.observation {
        let receipt = observation_receipt(journal, launch, observation)?;
        anyhow::ensure!(
            pulse.input_sequence == Some(receipt.input_sequence),
            "observation input sequence mismatch"
        );
    } else {
        anyhow::ensure!(
            pulse.input_sequence.is_none(),
            "input sequence without observation"
        );
    }
    Ok(Some(pulse))
}

fn active<'a>(
    journal: &'a Journal,
    id: &str,
    marker: &str,
    producer: &str,
) -> Result<(&'a Launch, &'a Target)> {
    let launch = journal.launches.get(id).context("launch not found")?;
    anyhow::ensure!(launch.marker == marker, "launch marker mismatch");
    let integration = launch.integration.as_ref().context("integration missing")?;
    anyhow::ensure!(
        integration.producer.as_deref() == Some(producer),
        "heartbeat producer mismatch"
    );
    anyhow::ensure!(
        launch.phase == LaunchPhase::Attached && !launch.stop_requested,
        "managed launch is not active"
    );
    let target = &launch
        .session
        .as_ref()
        .and_then(|id| journal.sessions.get(id))
        .context("managed session missing")?
        .target;
    Ok((launch, target))
}

/// Exact retries retain their original timestamp. Only a newer live pulse extends freshness.
pub fn record(
    root: &Path,
    id: &str,
    marker: &str,
    producer: &str,
    sequence: u64,
    observation: Option<Observation>,
) -> Result<Value> {
    anyhow::ensure!(
        super::model::id(producer) && sequence > 0,
        "invalid heartbeat producer/sequence"
    );
    let store = Store::open(root)?;
    let (launch, target) = active(store.journal(), id, marker, producer)?;
    let input_sequence = observation
        .as_ref()
        .map(|observation| {
            observation_receipt(store.journal(), launch, observation)
                .map(|receipt| receipt.input_sequence)
        })
        .transpose()?;
    if let Some(previous) = read(root, launch, store.journal())? {
        anyhow::ensure!(sequence >= previous.sequence, "heartbeat sequence is stale");
        if sequence == previous.sequence {
            anyhow::ensure!(
                previous.observation == observation,
                "heartbeat sequence has different observation"
            );
            return Ok(serde_json::to_value(previous)?);
        }
    }
    let target = target.clone();
    let now = super::now_ms()?;
    let monotonic = monotonic_ms()?;
    drop(store);
    let deadline = Instant::now() + Duration::from_secs(2);
    submit::verify_target(&target, deadline)?;
    if let Some(input_sequence) = input_sequence {
        let capture = submit::request(
            &target,
            "capture",
            json!({"pane":target.pane,"max_bytes":1}),
            deadline,
        )?;
        anyhow::ensure!(
            capture
                .pointer("/result/value/input_sequence")
                .and_then(Value::as_u64)
                == Some(input_sequence),
            "intervening input invalidated adapter observation"
        );
    }
    // Recheck the specific launch and sequence after I/O. An unrelated journal commit
    // does not invalidate the pulse, but stop/replacement/concurrent newer pulses do.
    let store = Store::open(root)?;
    let (launch, current_target) = active(store.journal(), id, marker, producer)?;
    anyhow::ensure!(current_target == &target, "heartbeat target changed");
    if let Some(observation) = &observation {
        observation_receipt(store.journal(), launch, observation)?;
    }
    if let Some(previous) = read(root, launch, store.journal())? {
        anyhow::ensure!(sequence >= previous.sequence, "heartbeat sequence is stale");
        if sequence == previous.sequence {
            anyhow::ensure!(
                previous.observation == observation,
                "heartbeat sequence has different observation"
            );
            return Ok(serde_json::to_value(previous)?);
        }
        anyhow::ensure!(
            now >= previous.received_ms && monotonic >= previous.monotonic_ms,
            "heartbeat clock moved backwards"
        );
    }
    anyhow::ensure!(
        launch
            .integration
            .as_ref()
            .and_then(|value| value.registered_ms)
            .is_some_and(|at| now >= at),
        "heartbeat precedes registration"
    );
    let pulse = Pulse {
        v: 1,
        launch: id.into(),
        marker: marker.into(),
        producer: producer.into(),
        sequence,
        received_ms: now,
        monotonic_ms: monotonic,
        observation,
        input_sequence,
    };
    let bytes = serde_json::to_vec(&pulse)?;
    anyhow::ensure!(bytes.len() as u64 <= MAX_BYTES, "heartbeat size limit");
    let directory = directory(root, true)?.context("heartbeat directory missing")?;
    // Same private temporary namespace as Store; the held writer lock prevents cleanup
    // from racing this write. One retained small file per bounded managed launch.
    let temporary = root.join(format!(".journal-{}.tmp", super::store::nonce()?));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, directory.join(format!("{marker}.json")))?;
        File::open(&directory)?.sync_all()?;
        File::open(root)?.sync_all()?;
        Ok(())
    })();
    if temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(serde_json::to_value(pulse)?)
}

pub(crate) fn view(root: &Path, launch: &Launch, journal: &Journal) -> Value {
    let observation = (|| -> Result<Value> {
        let now = super::now_ms()?;
        let monotonic = monotonic_ms()?;
        let pulse = read(root, launch, journal)?;
        let wall_age = pulse
            .as_ref()
            .and_then(|pulse| now.checked_sub(pulse.received_ms));
        let monotonic_age = pulse
            .as_ref()
            .and_then(|pulse| monotonic.checked_sub(pulse.monotonic_ms));
        let age = wall_age
            .zip(monotonic_age)
            .map(|(wall, monotonic)| wall.max(monotonic));
        let clock_changed = wall_age
            .zip(monotonic_age)
            .is_some_and(|(wall, monotonic)| wall.abs_diff(monotonic) > 1000);
        let status = if launch.phase != LaunchPhase::Attached || launch.stop_requested {
            "inactive"
        } else if pulse.is_none() {
            "never-seen"
        } else if age.is_none() {
            "clock-rollback"
        } else if clock_changed {
            "clock-changed"
        } else if age.is_some_and(|age| age < TTL_MS) {
            "current"
        } else {
            "expired"
        };
        Ok(
            json!({"status":status,"sequence":pulse.as_ref().map(|pulse| pulse.sequence),
                "received_ms":pulse.as_ref().map(|pulse| pulse.received_ms),"age_ms":age,
            "ttl_ms":TTL_MS,"scope":"producer-freshness-only",
            "observation":pulse.as_ref().and_then(|pulse| pulse.observation.as_ref()),
            "input_sequence":pulse.as_ref().and_then(|pulse| pulse.input_sequence)}),
        )
    })();
    observation.unwrap_or_else(|error| json!({"status":"unavailable","ttl_ms":TTL_MS,
        "problem":format!("{error:#}").chars().take(256).collect::<String>(),"scope":"producer-freshness-only"}))
}
