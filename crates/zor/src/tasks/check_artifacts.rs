//! Output capture belongs to the same durable check execution as its supplied source.
use super::{artifact, model::*};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
};

pub(super) fn pending_count(journal: &Journal) -> usize {
    journal
        .checks
        .values()
        .filter(|check| check.phase == CheckPhase::Submitted)
        .map(|check| check.artifact_requests.len())
        .sum()
}
pub(super) fn reserved(journal: &Journal, id: &str) -> bool {
    journal
        .checks
        .values()
        .any(|check| check.artifact_requests.values().any(|value| value == id))
}
pub(super) fn prepare(
    journal: &Journal,
    task: &Task,
    source: &Option<String>,
    requests: &BTreeMap<String, String>,
) -> Result<()> {
    anyhow::ensure!(
        requests.len() <= 8 && (requests.is_empty() || source.is_some()),
        "artifact capture requires a source and at most eight requests"
    );
    let mut ids = BTreeSet::new();
    for (name, artifact) in requests {
        anyhow::ensure!(
            id(name)
                && id(artifact)
                && task.required_artifacts.contains_key(name)
                && ids.insert(artifact)
                && !journal.artifacts.contains_key(artifact)
                && !reserved(journal, artifact),
            "invalid artifact request, unknown requirement or reserved artifact ID"
        );
    }
    Ok(())
}
pub(super) fn validate(journal: &Journal) -> Result<()> {
    let mut ids = BTreeSet::new();
    for check in journal.checks.values() {
        anyhow::ensure!(
            check.artifact_requests.len() <= 8
                && (check.artifact_requests.is_empty() || check.source.is_some()),
            "invalid check artifact requests"
        );
        for (name, problem) in &check.artifact_problems {
            anyhow::ensure!(
                check.phase != CheckPhase::Submitted
                    && check.artifact_requests.contains_key(name)
                    && !problem.is_empty()
                    && problem.len() <= 512,
                "invalid artifact capture problem"
            );
        }
        for (name, artifact_id) in &check.artifact_requests {
            anyhow::ensure!(
                id(name)
                    && id(artifact_id)
                    && ids.insert(artifact_id)
                    && journal
                        .tasks
                        .get(&check.task)
                        .is_some_and(|task| task.required_artifacts.contains_key(name)),
                "invalid or conflicting check artifact reservation"
            );
            let retained = journal.artifacts.get(artifact_id);
            let failed = check.artifact_problems.contains_key(name);
            anyhow::ensure!(
                match check.phase {
                    CheckPhase::Submitted => retained.is_none() && !failed,
                    CheckPhase::Uncertain => retained.is_none() && failed,
                    CheckPhase::Finished => retained.is_some() != failed,
                },
                "artifact capture outcome does not match check phase"
            );
            if let Some(artifact) = retained {
                anyhow::ensure!(
                    artifact.check.as_ref() == Some(&check.id)
                        && artifact.requirement.as_ref() == Some(name),
                    "artifact capture does not match reservation"
                );
            }
        }
    }
    for artifact in journal.artifacts.values() {
        match (&artifact.check, &artifact.source) {
            (None, None) => {}
            (Some(check_id), Some(source)) => {
                let check = journal
                    .checks
                    .get(check_id)
                    .context("artifact check missing")?;
                anyhow::ensure!(
                    check.phase == CheckPhase::Finished
                        && check.source.as_ref() == Some(source)
                        && check.task == artifact.task
                        && check.attempt == artifact.attempt
                        && artifact.created_generation > check.created_generation
                        && artifact
                            .requirement
                            .as_ref()
                            .and_then(|name| check.artifact_requests.get(name))
                            == Some(&artifact.id),
                    "invalid artifact check/source association"
                );
            }
            _ => anyhow::bail!("artifact check and source must be present together"),
        }
    }
    Ok(())
}
pub(super) fn capture(check: &mut Check, directory: Option<&File>, task: &Task) -> Vec<Artifact> {
    let mut collected = Vec::new();
    for (name, id) in &check.artifact_requests {
        let result = (|| {
            anyhow::ensure!(
                check.phase == CheckPhase::Finished,
                "check outcome uncertain; artifacts were not captured"
            );
            let directory = directory.context("check source directory unavailable")?;
            let path = task
                .required_artifacts
                .get(name)
                .context("artifact requirement missing")?;
            let bytes = artifact::read(directory.try_clone()?, path)?;
            Ok::<_, anyhow::Error>(Artifact {
                id: id.clone(),
                check: Some(check.id.clone()),
                source: check.source.clone(),
                requirement: Some(name.clone()),
                created_generation: 0,
                task: check.task.clone(),
                attempt: check.attempt.clone(),
                path: path.clone(),
                created_ms: super::now_ms()?,
                bytes,
            })
        })();
        match result {
            Ok(artifact) => collected.push(artifact),
            Err(error) => {
                check.artifact_problems.insert(
                    name.clone(),
                    format!("{error:#}").chars().take(128).collect(),
                );
            }
        }
    }
    collected
}

pub(super) fn summaries(journal: &Journal, task: &str) -> Vec<Value> {
    journal.tasks.get(task).into_iter().flat_map(|task| task.required_artifacts.keys())
        .filter_map(|name| {
            let check = journal.checks.values().filter(|check| check.task == task
                && journal.tasks.get(task).is_some_and(|task| check.attempt == task.attempt)
                && check.artifact_requests.contains_key(name))
                .max_by_key(|check| check.created_generation)?;
            let problem = check.artifact_problems.get(name);
            Some(json!({"name":name,"check":check.id,"source":check.source,"artifact":check.artifact_requests.get(name),
                "status":if check.phase == CheckPhase::Submitted {"pending"} else if problem.is_some() {"failed"} else {"collected"},
                "problem":problem}))
        }).collect()
}
