//! Startup recovery (RECOVERY.md:45-55, 64-71; invariant 27), `PostStartup`, after the journal
//! restored: nothing is in flight in a fresh process, so every `Submitting` operation and
//! prompt becomes `Uncertain` in this one update, every attempt that was awaiting live or final
//! evidence is marked `Uncertain`, and one listing sweep is scheduled for the first fux link.
//! No launch is created and no prompt is replayed; `Prepared` launches stay `Prepared`.

use bevy_ecs::prelude::*;

use super::*;

pub fn recover(world: &mut World) {
    let operations: Vec<Entity> = world
        .query_filtered::<(Entity, &OperationPhase), With<Operation>>()
        .iter(world)
        .filter(|(_, p)| **p == OperationPhase::Submitting)
        .map(|(e, _)| e)
        .collect();
    for op in operations {
        set_phase(world, op, OperationPhase::Uncertain);
        world
            .entity_mut(op)
            .insert(Problem("restart while submitting: outcome unknown".into()));
    }

    let prompts: Vec<Entity> = world
        .query_filtered::<(Entity, &Delivery), With<Prompt>>()
        .iter(world)
        .filter(|(_, d)| **d == Delivery::Submitting)
        .map(|(e, _)| e)
        .collect();
    for prompt in prompts {
        set_delivery(world, prompt, Delivery::Uncertain);
        world
            .entity_mut(prompt)
            .insert(Problem("restart while submitting: delivery unknown".into()));
    }

    let attempts: Vec<(Entity, AttemptState)> = world
        .query_filtered::<(Entity, &AttemptState), (With<Attempt>, Without<Lost>)>()
        .iter(world)
        .filter(|(_, s)| {
            matches!(
                s,
                AttemptState::Launching | AttemptState::Live | AttemptState::Finishing
            )
        })
        .map(|(e, s)| (e, *s))
        .collect();
    let mut sweep = false;
    for (attempt, state) in attempts {
        sweep = true;
        observe(
            world,
            attempt,
            Observed::Missing(format!(
                "restart while {state:?}: live evidence not yet reconciled"
            )),
        );
        world.entity_mut(attempt).insert(Heartbeat::default());
        if state == AttemptState::Finishing {
            world.entity_mut(attempt).insert(AwaitFinal);
        }
    }
    world.resource_mut::<Link>().sweep_due = sweep;
}
