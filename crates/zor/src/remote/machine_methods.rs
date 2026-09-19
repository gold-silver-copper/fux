//! Authorized catalog edits and guarded machine actions; read replies never contain tokens.
use super::methods::{MethodSpec, NoParams, Request, described, handler, invalid, spec, to_value};
use super::{Capabilities, Descriptor};
use crate::machines::{
    self, MachineGuard, MachineSnapshot,
    catalog::{self, MachineEntry},
    intents::{ActionIntent, IntentLog},
    supervision::{ActionKind, ActionRecord, PaneIdentity},
    transport::Transport,
};
use bevy_ecs::prelude::*;
use bevy_remote::BrpResult;
use std::collections::BTreeMap;

described!(
    pub struct MachineParams {
        pub machine: String,
    }
);
described!(
    pub struct MachineEndpointParams {
        pub machine: String,
        #[serde(default)]
        pub workspace: Option<String>,
    }
);
described!(
    pub struct MachineEndpoint {
        pub descriptor: Descriptor,
    }
);
described!(
    pub struct MachineAddParams {
        pub name: String,
        #[serde(default)]
        pub control: Option<Transport>,
        #[serde(default)]
        pub attachments: BTreeMap<String, Transport>,
    }
);
described!(
    pub struct MachineRenameParams {
        pub machine: String,
        pub name: String,
    }
);
described!(
    pub struct MachineControlParams {
        pub machine: String,
        pub control: Option<Transport>,
    }
);
described!(
    pub struct MachineBindParams {
        pub machine: String,
        pub workspace: String,
        pub attachment: Option<Transport>,
    }
);
described!(
    pub struct MachineList {
        pub machines: Vec<MachineSnapshot>,
        pub problem: Option<String>,
    }
);
described!(
    pub struct MachineInspect {
        pub machine: MachineSnapshot,
    }
);
described!(
    pub struct MachineChanged {
        pub id: String,
    }
);
described!(
    pub struct MachineReloaded {
        pub reloaded: bool,
    }
);
described!(
    pub struct MachineActionParams {
        pub machine: String,
        pub task: String,
        pub operation: String,
        pub guard: MachineGuard,
    }
);
described!(
    pub struct MachineResumeParams {
        pub machine: String,
        pub task: String,
        pub operation: String,
        pub fux_instance: String,
        pub guard: MachineGuard,
    }
);
described!(
    pub struct MachineActionResult {
        pub action: ActionRecord,
    }
);
described!(
    pub struct MachineIntentParams {
        #[serde(default)]
        pub operation: Option<String>,
    }
);
described!(
    pub struct MachineIntents {
        pub intents: Vec<ActionIntent>,
    }
);
described!(
    pub struct MachineAttachParams {
        pub machine: String,
        pub task: String,
        pub guard: MachineGuard,
    }
);
described!(
    pub struct MachineAttachment {
        pub machine: String,
        pub task: String,
        pub attempt: u64,
        pub pane: PaneIdentity,
    }
);

fn list(mut req: Request, world: &mut World) -> BrpResult {
    let NoParams {} = req.parse()?;
    to_value(MachineList {
        machines: machines::snapshots(world),
        problem: machines::catalog_problem(world),
    })
}
fn inspect(mut req: Request, world: &mut World) -> BrpResult {
    let p: MachineParams = req.parse()?;
    to_value(MachineInspect {
        machine: machines::snapshot(world, &p.machine).map_err(invalid)?,
    })
}
fn endpoint(mut req: Request, world: &mut World) -> BrpResult {
    req.require(Capabilities::ADMIN)?;
    let p: MachineEndpointParams = req.parse()?;
    to_value(MachineEndpoint {
        descriptor: machines::endpoint_descriptor(world, &p.machine, p.workspace.as_deref())
            .map_err(invalid)?,
    })
}
fn add(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let p: MachineAddParams = req.parse()?;
    let id = catalog::fresh_id().map_err(|e| invalid(e.to_string()))?;
    machines::edit_catalog(world, |catalog| {
        catalog
            .add(MachineEntry {
                id: id.clone(),
                name: p.name,
                control: p.control,
                attachments: p.attachments,
            })
            .map_err(|e| e.to_string())
    })
    .map_err(invalid)?;
    to_value(MachineChanged { id })
}
fn rename(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let p: MachineRenameParams = req.parse()?;
    let mut id = String::new();
    machines::edit_catalog(world, |catalog| {
        let entry = catalog.find_mut(&p.machine).ok_or("unknown machine")?;
        id.clone_from(&entry.id);
        entry.name = p.name;
        Ok(())
    })
    .map_err(invalid)?;
    to_value(MachineChanged { id })
}
fn remove(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let p: MachineParams = req.parse()?;
    let mut id = String::new();
    machines::edit_catalog(world, |catalog| {
        id.clone_from(&catalog.find(&p.machine).ok_or("unknown machine")?.id);
        catalog.machines.retain(|m| m.id != id);
        Ok(())
    })
    .map_err(invalid)?;
    to_value(MachineChanged { id })
}
fn control(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let p: MachineControlParams = req.parse()?;
    let mut id = String::new();
    machines::edit_catalog(world, |catalog| {
        let entry = catalog.find_mut(&p.machine).ok_or("unknown machine")?;
        id.clone_from(&entry.id);
        entry.control = p.control;
        Ok(())
    })
    .map_err(invalid)?;
    to_value(MachineChanged { id })
}
fn bind(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let p: MachineBindParams = req.parse()?;
    let mut id = String::new();
    machines::edit_catalog(world, |catalog| {
        let entry = catalog.find_mut(&p.machine).ok_or("unknown machine")?;
        id.clone_from(&entry.id);
        if let Some(attachment) = p.attachment {
            entry.attachments.insert(p.workspace, attachment);
        } else {
            entry.attachments.remove(&p.workspace);
        }
        Ok(())
    })
    .map_err(invalid)?;
    to_value(MachineChanged { id })
}
fn reload(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let NoParams {} = req.parse()?;
    machines::reload(world).map_err(invalid)?;
    to_value(MachineReloaded { reloaded: true })
}
fn action(mut req: Request, world: &mut World, kind: ActionKind) -> BrpResult {
    req.mutation(world)?;
    let p: MachineActionParams = req.parse()?;
    let action = machines::queue_action(world, &p.machine, &p.task, kind, p.guard, &p.operation)
        .map_err(invalid)?;
    to_value(MachineActionResult { action })
}
fn cancel(req: Request, world: &mut World) -> BrpResult {
    action(req, world, ActionKind::Cancel)
}
fn stop(req: Request, world: &mut World) -> BrpResult {
    action(req, world, ActionKind::Stop)
}
fn reconcile(req: Request, world: &mut World) -> BrpResult {
    action(req, world, ActionKind::Reconcile)
}
fn resume(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let p: MachineResumeParams = req.parse()?;
    let kind = ActionKind::Resume {
        operation: p.operation.clone(),
        fux_instance: p.fux_instance,
    };
    let action = machines::queue_action(world, &p.machine, &p.task, kind, p.guard, &p.operation)
        .map_err(invalid)?;
    to_value(MachineActionResult { action })
}
fn intents(mut req: Request, world: &mut World) -> BrpResult {
    let p: MachineIntentParams = req.parse()?;
    let log = world.resource::<IntentLog>();
    to_value(MachineIntents {
        intents: log
            .records()
            .iter()
            .filter(|r| p.operation.as_ref().is_none_or(|op| &r.operation == op))
            .cloned()
            .collect(),
    })
}
fn attach(mut req: Request, world: &mut World) -> BrpResult {
    let p: MachineAttachParams = req.parse()?;
    let target =
        machines::exact_attachment(world, &p.machine, &p.task, &p.guard).map_err(invalid)?;
    to_value(MachineAttachment {
        machine: target.machine,
        task: target.task,
        attempt: target.attempt,
        pane: target.pane,
    })
}
handler!(brp_list, list);
handler!(brp_inspect, inspect);
handler!(brp_endpoint, endpoint);
handler!(brp_add, add);
handler!(brp_rename, rename);
handler!(brp_remove, remove);
handler!(brp_reload, reload);
handler!(brp_control, control);
handler!(brp_bind, bind);
handler!(brp_cancel, cancel);
handler!(brp_stop, stop);
handler!(brp_reconcile, reconcile);
handler!(brp_resume, resume);
handler!(brp_intents, intents);
handler!(brp_attach, attach);
pub const METHODS: &[MethodSpec] = &[
    spec!("zor/machine.list", brp_list, NoParams, MachineList),
    spec!(
        "zor/machine.inspect",
        brp_inspect,
        MachineParams,
        MachineInspect
    ),
    spec!(
        "zor/machine.endpoint",
        brp_endpoint,
        MachineEndpointParams,
        MachineEndpoint
    ),
    spec!("zor/machine.add", brp_add, MachineAddParams, MachineChanged),
    spec!(
        "zor/machine.rename",
        brp_rename,
        MachineRenameParams,
        MachineChanged
    ),
    spec!(
        "zor/machine.remove",
        brp_remove,
        MachineParams,
        MachineChanged
    ),
    spec!("zor/machine.reload", brp_reload, NoParams, MachineReloaded),
    spec!(
        "zor/machine.control",
        brp_control,
        MachineControlParams,
        MachineChanged
    ),
    spec!(
        "zor/machine.bind",
        brp_bind,
        MachineBindParams,
        MachineChanged
    ),
    spec!(
        "zor/machine.cancel",
        brp_cancel,
        MachineActionParams,
        MachineActionResult
    ),
    spec!(
        "zor/machine.stop",
        brp_stop,
        MachineActionParams,
        MachineActionResult
    ),
    spec!(
        "zor/machine.reconcile",
        brp_reconcile,
        MachineActionParams,
        MachineActionResult
    ),
    spec!(
        "zor/machine.resume",
        brp_resume,
        MachineResumeParams,
        MachineActionResult
    ),
    spec!(
        "zor/machine.intents",
        brp_intents,
        MachineIntentParams,
        MachineIntents
    ),
    spec!(
        "zor/machine.status",
        brp_intents,
        MachineIntentParams,
        MachineIntents
    ),
    spec!(
        "zor/machine.attach",
        brp_attach,
        MachineAttachParams,
        MachineAttachment
    ),
];
