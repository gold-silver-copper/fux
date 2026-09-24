//! The BRP method registry fux serves: every stock method behind one guard
//! (`guard`), and the fux extensions.
use crate::frame;
use bevy_ecs::prelude::*;
use bevy_remote::RemotePlugin;

mod guard;
#[cfg(test)]
mod property;
#[cfg(test)]
mod tests;

pub fn remote() -> RemotePlugin {
    // Registered after the stock methods, so the guarded forms replace them by
    // name (docs/prompt-brp-policy.md): every write is checked against the
    // type policy (`crate::policy`) and the hierarchy before it happens.
    guard::guard(RemotePlugin::default())
        .with_method_main("fux.policy", policy)
        .with_method_main("fux.invariants", invariants)
        .with_method_main("fux.attach", frame::attach)
        .with_method_main("fux.frame", frame::frame)
        .with_watching_method_main("fux.frame+watch", frame::frame_watch)
}

/// `fux.invariants`: every structural rule the world currently breaks, one
/// line each; an empty list means consistent. Read-only, so black-box
/// harnesses can ask fux instead of recomputing its rules from outside.
fn invariants(In(_): In<Option<serde_json::Value>>, world: &mut World) -> bevy_remote::BrpResult {
    Ok(serde_json::Value::from(crate::invariants::violations(
        world,
    )))
}

/// `fux.policy`: what clients may do with each type fux opened, the same table
/// as the README's. Every other registered type is read-only.
fn policy(In(_): In<Option<serde_json::Value>>, world: &mut World) -> bevy_remote::BrpResult {
    let rows: Vec<serde_json::Value> = crate::policy::table(world)
        .into_iter()
        .map(|(path, policy)| {
            serde_json::json!({
                "type": path,
                "write": policy.access.write,
                "spawn": policy.access.spawn,
                "remove": policy.access.remove && !policy.required,
                "trigger": policy.access.trigger,
                "required": policy.required,
                "note": policy.note,
            })
        })
        .collect();
    Ok(serde_json::json!({
        "default": "read-only: query and watch, nothing else",
        "types": rows,
    }))
}
