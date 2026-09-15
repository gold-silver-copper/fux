//! `Update`/`Lifecycle`: wait outcomes from retained evidence (TASKS.md:311-345) and task
//! states from the current attempt. Delivery, agent reaction, wait outcome and task outcome are
//! separate facts (invariant 20); a terminal wait outcome is never erased (TASKS.md:341-342).

use bevy_ecs::prelude::*;

use super::*;

/// `NeedsInput` on an attempt follows the merged observation (`Blocked`), informational only:
/// it drives `Running ↔ Blocked` and the prompt's non-terminal `NeedsInput`, never a terminal
/// outcome (invariant 26).
pub fn needs_input(world: &mut World) {
    let live: Vec<Entity> = world
        .query_filtered::<(Entity, &AttemptState), With<Attempt>>()
        .iter(world)
        .filter(|(_, s)| **s == AttemptState::Live)
        .map(|(e, _)| e)
        .collect();
    for attempt in live {
        let blocked = crate::providers::observation_of(world, attempt)
            .is_some_and(|o| o.state == AgentState::Blocked);
        let mut entity = world.entity_mut(attempt);
        if blocked {
            if !entity.contains::<NeedsInput>() {
                entity.insert(NeedsInput);
            }
        } else if entity.contains::<NeedsInput>() {
            entity.remove::<NeedsInput>();
        }
    }
}

/// Precedence for one prompt whose wait is not terminal (TASKS.md:322-339): a scoped response
/// after delivery, then the attempt's final evidence, then the deadline, then a needs-input
/// claim or blocked observation; a failed delivery is `Uncertain`.
pub fn resolve_waits(world: &mut World) {
    let now_ms = now(world);
    let prompts: Vec<(Entity, Entity, WaitState)> = world
        .query_filtered::<(Entity, &PromptOf, &WaitState), With<Prompt>>()
        .iter(world)
        .filter(|(_, _, w)| !w.is_terminal())
        .map(|(e, of, w)| (e, of.0, *w))
        .collect();
    for (prompt, attempt, current) in prompts {
        let delivery = world.get::<Delivery>(prompt).copied().unwrap_or_default();
        let deadline = world.get::<Deadline>(prompt).map_or(u64::MAX, |d| d.0);
        let receipt_seq = world.get::<Receipt>(prompt).and_then(|r| r.seq);
        let response = world.get::<ResponseEvent>(prompt).map(|r| r.kind.clone());
        let evidence = world.get::<FinalEvidence>(attempt).cloned();
        let capture = world.get::<LastCapture>(attempt).map(|c| c.seq);
        let blocked = world.get::<NeedsInput>(attempt).is_some();

        let delivered_and_captured = delivery == Delivery::Delivered
            && match (capture, receipt_seq) {
                (Some(seen), Some(written)) => seen >= written,
                _ => true,
            };
        let next = if response.as_deref() == Some("response") && delivered_and_captured {
            Some(WaitState::ResponseObserved)
        } else if let Some(evidence) = evidence {
            if evidence.exit_code.is_some() {
                Some(WaitState::ProcessExited)
            } else {
                world
                    .entity_mut(prompt)
                    .insert(Problem("closure without an observed exit status".into()));
                Some(WaitState::Uncertain)
            }
        } else if delivery == Delivery::Failed {
            world
                .entity_mut(prompt)
                .insert(Problem("input was not delivered".into()));
            Some(WaitState::Uncertain)
        } else if now_ms >= deadline
            && matches!(
                delivery,
                Delivery::Submitting | Delivery::Delivered | Delivery::Uncertain
            )
        {
            Some(WaitState::TimedOut)
        } else if response.as_deref() == Some("needs-input") || blocked {
            Some(WaitState::NeedsInput)
        } else if current == WaitState::NeedsInput {
            Some(WaitState::Pending)
        } else {
            None
        };
        if let Some(next) = next
            && next != current
        {
            world.entity_mut(prompt).insert(next);
        }
    }
}

/// `Open → Running` when the attempt is live, `Running ↔ Blocked` with `NeedsInput`,
/// back to `Open` when the attempt finished (TASKS.md:136-137). Closed tasks are untouched.
pub fn derive_task_states(world: &mut World) {
    let tasks: Vec<(Entity, TaskState)> = world
        .query_filtered::<(Entity, &TaskState), With<Task>>()
        .iter(world)
        .filter(|(_, s)| !s.is_closed())
        .map(|(e, s)| (e, *s))
        .collect();
    for (task, state) in tasks {
        let current = attempt_of(world, task);
        let live =
            current.is_some_and(|a| world.get::<AttemptState>(a) == Some(&AttemptState::Live));
        let blocked = current.is_some_and(|a| world.get::<NeedsInput>(a).is_some());
        let next = match (live, blocked) {
            (true, true) => TaskState::Blocked,
            (true, false) => TaskState::Running,
            (false, _) => TaskState::Open,
        };
        if next == state {
            continue;
        }
        if state == TaskState::Open && next == TaskState::Blocked {
            world.entity_mut(task).insert(TaskState::Running);
        }
        let state = world.get::<TaskState>(task).copied().unwrap_or(state);
        if state.may_become(next) {
            world.entity_mut(task).insert(next);
        }
    }
}
