//! Retained artifacts (ARTIFACTS.md): manual collection reads one bounded regular file beneath
//! the task's launch directory and retains its exact bytes; check captures reserve their ids at
//! submission and are read from the check's directory after its command exited, publishing
//! with the command evidence in the same update.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use bevy_ecs::prelude::*;
use bevy_ecs::relationship::RelationshipTarget;
use bevy_log::error;
use nix::fcntl::OFlag;

use super::{
    ArtifactBytes, ArtifactPolicy, CaptureRequest, CapturedBy, CheckError, MAX_CAPTURES_PER_CHECK,
    bounded_problem, managed_attempt, next_generation, open_task, refused, seal_policies, task_cwd,
};
use crate::model::*;

/// Per-artifact byte bound (ARTIFACTS.md:51).
pub const MAX_ARTIFACT_BYTES: usize = 64 * 1024;
/// Aggregate retained artifact bytes per journal (ARTIFACTS.md:51).
pub const MAX_TOTAL_ARTIFACT_BYTES: usize = 512 * 1024;
pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_PATH_COMPONENTS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSpec {
    pub id: String,
    pub path: String,
    pub requirement: Option<String>,
}

/// Relative UTF-8, bounded, no control characters, no empty/`.`/`..` components (ARTIFACTS.md:42-43).
pub fn validate_path(path: &str) -> Result<(), CheckError> {
    let components: Vec<&str> = path.split('/').collect();
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.starts_with('/')
        || path.chars().any(char::is_control)
        || components.len() > MAX_PATH_COMPONENTS
        || components
            .iter()
            .any(|c| c.is_empty() || *c == "." || *c == "..")
    {
        return refused(
            "artifact path must be relative, bounded, without empty, '.' or '..' components",
        );
    }
    Ok(())
}

/// Opens `root/path` following no symlink at any component; the file must be regular with one
/// hard link and at most `MAX_ARTIFACT_BYTES` (ARTIFACTS.md:42-49).
pub fn read_file(root: &Path, path: &str) -> Result<Vec<u8>, String> {
    let mut current = PathBuf::from(root);
    let components: Vec<&str> = path.split('/').collect();
    let Some((last, parents)) = components.split_last() else {
        return Err("empty path".into());
    };
    for component in parents {
        current.push(component);
        let meta = std::fs::symlink_metadata(&current).map_err(|e| format!("{component}: {e}"))?;
        if !meta.is_dir() {
            return Err(format!("{component} is not a directory"));
        }
    }
    current.push(last);
    let file: File = OpenOptions::new()
        .read(true)
        .custom_flags(OFlag::O_NOFOLLOW.bits() | OFlag::O_NONBLOCK.bits())
        .open(&current)
        .map_err(|e| format!("{path}: {e}"))?;
    let before = file.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() {
        return Err(format!("{path} is not a regular file"));
    }
    if before.nlink() != 1 {
        return Err(format!("{path} has more than one hard link"));
    }
    let size = usize::try_from(before.len()).unwrap_or(usize::MAX);
    if size > MAX_ARTIFACT_BYTES {
        return Err(format!("{path} exceeds {MAX_ARTIFACT_BYTES} bytes"));
    }
    let mut bytes = Vec::with_capacity(size);
    (&file)
        .take(MAX_ARTIFACT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{path}: {e}"))?;
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(format!("{path} exceeds {MAX_ARTIFACT_BYTES} bytes"));
    }
    let after = file.metadata().map_err(|e| e.to_string())?;
    if after.len() != before.len() || after.ino() != before.ino() || after.ctime() != before.ctime()
    {
        return Err(format!("{path} changed while it was read"));
    }
    Ok(bytes)
}

fn retained_total(world: &mut World) -> usize {
    world
        .query::<&ArtifactBytes>()
        .iter(world)
        .map(|b| b.0.len())
        .sum()
}

/// Every artifact of `task` (through its attempts).
pub fn artifacts_of(world: &World, task: Entity) -> Vec<Entity> {
    let mut out = Vec::new();
    if let Some(attempts) = world.get::<Attempts>(task) {
        for attempt in attempts.iter() {
            if let Some(artifacts) = world.get::<Artifacts>(attempt) {
                out.extend(artifacts.iter());
            }
        }
    }
    out
}

/// Artifacts reserved/captured by `check`.
pub fn captured_by(world: &mut World, check: Entity) -> Vec<Entity> {
    world
        .query_filtered::<(Entity, &CapturedBy), With<Artifact>>()
        .iter(world)
        .filter(|(_, by)| by.0 == check)
        .map(|(e, _)| e)
        .collect()
}

/// Manual collection (`owner` is an attempt) or a capture reservation on a `Queued` check
/// (`owner` is a check). Identical retries read the record; a different task, path or
/// requirement conflicts (ARTIFACTS.md:15-17); a check-reserved id is never claimed manually.
pub fn retain_artifact(
    world: &mut World,
    owner: Entity,
    spec: ArtifactSpec,
) -> Result<Entity, CheckError> {
    if world.get::<Check>(owner).is_some() {
        if world.get::<CheckState>(owner) != Some(&CheckState::Queued) {
            return refused("captures are reserved only on a queued check");
        }
        let task = world
            .get::<CheckOf>(owner)
            .map(|c| c.0)
            .ok_or_else(|| CheckError::NotFound("task".into()))?;
        let attempt = managed_attempt(world, task)?;
        let Some(name) = spec.requirement.clone() else {
            return refused("a check capture names a declared artifact requirement");
        };
        let request = CaptureRequest {
            name,
            artifact: spec.id.clone(),
        };
        if let Some(existing) = world.resource::<Ids>().artifact(&spec.id) {
            return if world.get::<CapturedBy>(existing).map(|c| c.0) == Some(owner) {
                Ok(existing)
            } else {
                Err(CheckError::Conflict(format!(
                    "artifact {} is already reserved",
                    spec.id
                )))
            };
        }
        validate_requests(
            world,
            task,
            world.get::<CheckOn>(owner).is_some(),
            core::slice::from_ref(&request),
        )?;
        return reserve(world, attempt, owner, &request);
    }
    if world.get::<Attempt>(owner).is_none() {
        return Err(CheckError::NotFound("attempt or check".into()));
    }
    let task = world
        .get::<AttemptOf>(owner)
        .map(|a| a.0)
        .ok_or_else(|| CheckError::NotFound("task".into()))?;
    if !valid_id(&spec.id) {
        return Err(CheckError::Model(
            crate::model::graph::ModelError::InvalidId(spec.id),
        ));
    }
    validate_path(&spec.path)?;
    if let Some(existing) = world.resource::<Ids>().artifact(&spec.id) {
        if world.get::<CapturedBy>(existing).is_some() {
            return Err(CheckError::Conflict(format!(
                "artifact {} is reserved by a check and cannot be collected manually",
                spec.id
            )));
        }
        let same = world
            .get::<ArtifactOf>(existing)
            .and_then(|a| world.get::<AttemptOf>(a.0))
            .map(|t| t.0)
            == Some(task)
            && world
                .get::<ArtifactPath>(existing)
                .is_some_and(|p| p.0 == spec.path)
            && world.get::<Requirement>(existing).map(|r| &r.0) == spec.requirement.as_ref();
        return if same {
            Ok(existing)
        } else {
            Err(CheckError::Conflict(format!(
                "artifact {} already exists with different intent",
                spec.id
            )))
        };
    }
    open_task(world, task)?;
    let attempt = managed_attempt(world, task)?;
    if world.get::<Ownership>(owner) != Some(&Ownership::Managed) {
        return refused("adopted attempts grant no artifact authority");
    }
    if let Some(name) = &spec.requirement {
        let declared = world
            .get::<ArtifactPolicy>(task)
            .and_then(|p| p.required.iter().find(|r| &r.name == name))
            .map(|r| r.path.clone());
        if declared.as_deref() != Some(spec.path.as_str()) {
            return refused("artifact does not match its required name and path");
        }
    }
    let cwd = task_cwd(world, task)?;
    let bytes = read_file(Path::new(&cwd), &spec.path).map_err(CheckError::Refused)?;
    if retained_total(world) + bytes.len() > MAX_TOTAL_ARTIFACT_BYTES {
        return refused("retained artifact bytes would exceed 512 KiB");
    }
    let generation = next_generation(world);
    let artifact = spawn_artifact(
        world,
        crate::model::ArtifactSpec {
            id: &spec.id,
            attempt,
            path: &spec.path,
            requirement: spec.requirement.as_deref(),
        },
    )?;
    world.entity_mut(artifact).insert((
        ArtifactState::Collected,
        ArtifactBytes(bytes),
        CreatedGeneration(generation),
    ));
    seal_policies(world, task, false);
    Ok(artifact)
}

/// Capture requests at submission (ARTIFACTS.md:73-82): a source is required, at most eight,
/// distinct declared names, unused valid ids, within the record bound.
pub(super) fn validate_requests(
    world: &mut World,
    task: Entity,
    has_source: bool,
    requests: &[CaptureRequest],
) -> Result<(), CheckError> {
    if requests.is_empty() {
        return Ok(());
    }
    if !has_source {
        return refused("artifact capture requires a source-bound check");
    }
    if requests.len() > MAX_CAPTURES_PER_CHECK {
        return refused("at most eight artifact captures per check");
    }
    let policy = world
        .get::<ArtifactPolicy>(task)
        .cloned()
        .unwrap_or_default();
    for (i, request) in requests.iter().enumerate() {
        if !valid_id(&request.artifact) {
            return Err(CheckError::Model(
                crate::model::graph::ModelError::InvalidId(request.artifact.clone()),
            ));
        }
        if !policy.required.iter().any(|r| r.name == request.name) {
            return refused(format!(
                "artifact requirement {} is not declared",
                request.name
            ));
        }
        if requests.get(..i).is_some_and(|earlier| {
            earlier
                .iter()
                .any(|r| r.name == request.name || r.artifact == request.artifact)
        }) {
            return refused("duplicate artifact capture name or id");
        }
        if world
            .resource::<Ids>()
            .artifact(&request.artifact)
            .is_some()
        {
            return Err(CheckError::Conflict(format!(
                "artifact {} is already reserved",
                request.artifact
            )));
        }
    }
    let count = world
        .query_filtered::<(), With<Artifact>>()
        .iter(world)
        .count();
    if count + requests.len() > world.resource::<Limits>().artifacts {
        return Err(CheckError::Model(crate::model::graph::ModelError::Limit(
            "artifact",
        )));
    }
    Ok(())
}

/// Reserves one capture id on `check`: a `Pending` artifact bound to the check.
pub(super) fn reserve(
    world: &mut World,
    attempt: Entity,
    check: Entity,
    request: &CaptureRequest,
) -> Result<Entity, CheckError> {
    let task = world
        .get::<AttemptOf>(attempt)
        .map(|a| a.0)
        .ok_or_else(|| CheckError::NotFound("task".into()))?;
    let path = world
        .get::<ArtifactPolicy>(task)
        .and_then(|p| p.required.iter().find(|r| r.name == request.name))
        .map(|r| r.path.clone())
        .ok_or_else(|| {
            CheckError::Refused(format!(
                "artifact requirement {} is not declared",
                request.name
            ))
        })?;
    let artifact = spawn_artifact(
        world,
        crate::model::ArtifactSpec {
            id: &request.artifact,
            attempt,
            path: &path,
            requirement: Some(&request.name),
        },
    )?;
    world.entity_mut(artifact).insert(CapturedBy(check));
    Ok(artifact)
}

/// After the check's terminal publication: reads every reserved capture from the check's
/// directory (an uncertain outcome skips reading and records a problem on each), all in this
/// update (ARTIFACTS.md:84-95).
pub(super) fn capture(world: &mut World, check: Entity, outcome: CheckState, generation: u64) {
    let reserved = captured_by(world, check);
    if reserved.is_empty() {
        return;
    }
    let cwd = world
        .get::<super::CheckCwd>(check)
        .map(|c| PathBuf::from(&c.0));
    for artifact in reserved {
        if world.get::<ArtifactState>(artifact) != Some(&ArtifactState::Pending) {
            continue;
        }
        let Some(path) = world.get::<ArtifactPath>(artifact).map(|p| p.0.clone()) else {
            continue;
        };
        let read = match (outcome, &cwd) {
            (CheckState::Uncertain, _) => {
                Err("command outcome uncertain: not collected".to_owned())
            }
            (_, None) => Err("check directory unknown".to_owned()),
            (_, Some(cwd)) => read_file(cwd, &path),
        };
        let read = read.and_then(|bytes| {
            if retained_total(world) + bytes.len() > MAX_TOTAL_ARTIFACT_BYTES {
                Err("retained artifact bytes would exceed 512 KiB".to_owned())
            } else {
                Ok(bytes)
            }
        });
        match read {
            Ok(bytes) => {
                world.entity_mut(artifact).insert((
                    ArtifactState::Collected,
                    ArtifactBytes(bytes),
                    CreatedGeneration(generation),
                ));
            }
            Err(problem) => {
                error!("check {check}: capture {path}: {problem}");
                world
                    .entity_mut(artifact)
                    .insert((ArtifactState::Failed, bounded_problem(problem)));
            }
        }
    }
}
