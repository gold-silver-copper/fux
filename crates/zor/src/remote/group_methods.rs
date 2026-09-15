//! `zor/group.*`, `zor/worktree.*` (milestone 6, owner GroupsWorktrees): thin envelopes over
//! `crate::groups` and `crate::worktrees`. Mutations carry the instance nonce; every id is
//! resolved through `Ids`.

use bevy_ecs::prelude::*;
use bevy_remote::{BrpError, BrpResult};

use super::methods::{MethodSpec, Request, codes, described, error, handler, spec, to_value};
use crate::groups::{self, AfterSpec, GroupError, GroupRecord, GroupSpec, MemberSpec};
use crate::model::{Ids, OperationId};
use crate::worktrees::{self, WorktreeError, WorktreeRecord, WorktreeRequest};

// ---------------------------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------------------------

described!(
    pub struct GroupMember {
        pub operation: String,
        pub prompt: String,
    }
);
described!(
    pub struct GroupAfter {
        pub operation: String,
        pub prerequisite: String,
    }
);
described!(
    pub struct GroupCreateParams {
        pub group: String,
        pub concurrency: u32,
        pub members: Vec<GroupMember>,
        #[serde(default)]
        pub after: Vec<GroupAfter>,
    }
);
described!(
    pub struct GroupParams {
        pub group: String,
    }
);
described!(
    pub struct GroupMemberInfo {
        pub operation: String,
        pub task: String,
        pub prompt: String,
        pub phase: String,
        pub delivery: String,
        pub status: String,
        pub after: Vec<String>,
        pub problem: Option<String>,
    }
);
described!(
    /// Retained coordination state, not a live worker probe (GROUPS.md:109-116).
    pub struct GroupInfo {
        pub group: String,
        pub intent: String,
        pub status: String,
        pub concurrency: u32,
        pub cursor: u32,
        pub active_count: u32,
        pub members: Vec<GroupMemberInfo>,
        pub problem: Option<String>,
    }
);
described!(
    pub struct GroupAdmitted {
        pub group: String,
        pub admitted: Vec<String>,
        pub info: GroupInfo,
    }
);
described!(
    pub struct WorktreeAllocateParams {
        pub worktree: String,
        pub task: String,
        pub repo: String,
        pub branch: String,
        pub base: String,
    }
);
described!(
    pub struct WorktreeParams {
        pub worktree: String,
    }
);
described!(
    pub struct WorktreeRemoveParams {
        pub worktree: String,
        #[serde(default)]
        pub force: bool,
    }
);
described!(
    pub struct WorktreeInfo {
        pub worktree: String,
        pub task: String,
        pub state: String,
        pub repo: String,
        pub branch: String,
        pub base: String,
        pub path: String,
        pub force: bool,
        pub problem: Option<String>,
        pub in_flight: bool,
    }
);
described!(
    pub struct WorktreeList {
        pub worktrees: Vec<WorktreeInfo>,
    }
);

fn snake<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn group_info(record: GroupRecord) -> GroupInfo {
    GroupInfo {
        group: record.id,
        intent: snake(&record.intent),
        status: snake(&record.status),
        concurrency: record.concurrency,
        cursor: record.cursor,
        active_count: record.active_count,
        members: record
            .members
            .into_iter()
            .map(|m| GroupMemberInfo {
                operation: m.operation,
                task: m.task,
                prompt: m.prompt,
                phase: snake(&m.phase),
                delivery: snake(&m.delivery),
                status: snake(&m.status),
                after: m.after,
                problem: m.problem,
            })
            .collect(),
        problem: record.problem,
    }
}

fn worktree_info(record: WorktreeRecord) -> WorktreeInfo {
    WorktreeInfo {
        worktree: record.id,
        task: record.task,
        state: snake(&record.state),
        repo: record.repo,
        branch: record.branch,
        base: record.base,
        path: record.path,
        force: record.force,
        problem: record.problem,
        in_flight: record.in_flight,
    }
}

fn group_error(e: GroupError) -> BrpError {
    match e {
        GroupError::NotFound(what) => error(codes::NOT_FOUND, format!("{what} not found")),
        GroupError::Lifecycle(e) => error(codes::INVALID, e.to_string()),
        other => error(codes::INVALID, other.to_string()),
    }
}

fn worktree_error(e: WorktreeError) -> BrpError {
    match e {
        WorktreeError::NotFound(what) => error(codes::NOT_FOUND, format!("{what} not found")),
        other => error(codes::INVALID, other.to_string()),
    }
}

fn find_group(world: &World, id: &str) -> Result<Entity, BrpError> {
    world
        .resource::<Ids>()
        .group(id)
        .ok_or_else(|| error(codes::NOT_FOUND, format!("group {id} not found")))
}

fn find_worktree(world: &World, id: &str) -> Result<Entity, BrpError> {
    world
        .resource::<Ids>()
        .worktree(id)
        .ok_or_else(|| error(codes::NOT_FOUND, format!("worktree {id} not found")))
}

fn group_view(world: &World, group: Entity) -> BrpResult {
    let record = groups::inspect(world, group)
        .ok_or_else(|| error(codes::NOT_FOUND, "group record incomplete"))?;
    to_value(group_info(record))
}

fn worktree_view(world: &World, worktree: Entity) -> BrpResult {
    let record = worktrees::inspect(world, worktree)
        .ok_or_else(|| error(codes::NOT_FOUND, "worktree record incomplete"))?;
    to_value(worktree_info(record))
}

// ---------------------------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------------------------

fn group_create(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: GroupCreateParams = req.parse()?;
    let group = groups::create(
        world,
        GroupSpec {
            id: params.group,
            concurrency: params.concurrency,
            members: params
                .members
                .into_iter()
                .map(|m| MemberSpec {
                    operation: m.operation,
                    prompt: m.prompt,
                })
                .collect(),
            after: params
                .after
                .into_iter()
                .map(|a| AfterSpec {
                    operation: a.operation,
                    prerequisite: a.prerequisite,
                })
                .collect(),
        },
    )
    .map_err(group_error)?;
    group_view(world, group)
}

fn group_inspect(mut req: Request, world: &mut World) -> BrpResult {
    let params: GroupParams = req.parse()?;
    let group = find_group(world, &params.group)?;
    group_view(world, group)
}

fn group_admit(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: GroupParams = req.parse()?;
    let group = find_group(world, &params.group)?;
    let admitted = groups::admit(world, group).map_err(group_error)?;
    let admitted = admitted
        .into_iter()
        .filter_map(|m| world.get::<OperationId>(m).map(|o| o.0.clone()))
        .collect();
    let record = groups::inspect(world, group)
        .ok_or_else(|| error(codes::NOT_FOUND, "group record incomplete"))?;
    to_value(GroupAdmitted {
        group: params.group,
        admitted,
        info: group_info(record),
    })
}

fn group_intent(
    mut req: Request,
    world: &mut World,
    apply: fn(&mut World, Entity) -> Result<(), GroupError>,
) -> BrpResult {
    req.mutation(world)?;
    let params: GroupParams = req.parse()?;
    let group = find_group(world, &params.group)?;
    apply(world, group).map_err(group_error)?;
    group_view(world, group)
}

fn group_pause(req: Request, world: &mut World) -> BrpResult {
    group_intent(req, world, groups::pause)
}

fn group_resume(req: Request, world: &mut World) -> BrpResult {
    group_intent(req, world, groups::resume)
}

fn group_cancel(req: Request, world: &mut World) -> BrpResult {
    group_intent(req, world, groups::cancel)
}

fn worktree_allocate(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: WorktreeAllocateParams = req.parse()?;
    let task = world
        .resource::<Ids>()
        .task(&params.task)
        .ok_or_else(|| error(codes::NOT_FOUND, format!("task {} not found", params.task)))?;
    let worktree = worktrees::allocate(
        world,
        task,
        WorktreeRequest {
            id: params.worktree,
            repo: params.repo,
            branch: params.branch,
            base: params.base,
        },
    )
    .map_err(worktree_error)?;
    worktree_view(world, worktree)
}

fn worktree_list(_req: Request, world: &mut World) -> BrpResult {
    let worktrees = worktrees::list(world)
        .into_iter()
        .map(worktree_info)
        .collect();
    to_value(WorktreeList { worktrees })
}

fn worktree_inspect(mut req: Request, world: &mut World) -> BrpResult {
    let params: WorktreeParams = req.parse()?;
    let worktree = find_worktree(world, &params.worktree)?;
    worktree_view(world, worktree)
}

fn worktree_remove(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: WorktreeRemoveParams = req.parse()?;
    let worktree = find_worktree(world, &params.worktree)?;
    worktrees::remove(world, worktree, params.force).map_err(worktree_error)?;
    worktree_view(world, worktree)
}

fn worktree_reconcile(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: WorktreeParams = req.parse()?;
    let worktree = find_worktree(world, &params.worktree)?;
    worktrees::reconcile(world, worktree).map_err(worktree_error)?;
    worktree_view(world, worktree)
}

handler!(brp_group_create, group_create);
handler!(brp_group_inspect, group_inspect);
handler!(brp_group_admit, group_admit);
handler!(brp_group_pause, group_pause);
handler!(brp_group_resume, group_resume);
handler!(brp_group_cancel, group_cancel);
handler!(brp_worktree_allocate, worktree_allocate);
handler!(brp_worktree_list, worktree_list);
handler!(brp_worktree_inspect, worktree_inspect);
handler!(brp_worktree_remove, worktree_remove);
handler!(brp_worktree_reconcile, worktree_reconcile);

pub static METHODS: &[MethodSpec] = &[
    spec!(
        "zor/group.create",
        brp_group_create,
        GroupCreateParams,
        GroupInfo
    ),
    spec!(
        "zor/group.inspect",
        brp_group_inspect,
        GroupParams,
        GroupInfo
    ),
    spec!(
        "zor/group.admit",
        brp_group_admit,
        GroupParams,
        GroupAdmitted
    ),
    spec!("zor/group.pause", brp_group_pause, GroupParams, GroupInfo),
    spec!("zor/group.resume", brp_group_resume, GroupParams, GroupInfo),
    spec!("zor/group.cancel", brp_group_cancel, GroupParams, GroupInfo),
    spec!(
        "zor/worktree.allocate",
        brp_worktree_allocate,
        WorktreeAllocateParams,
        WorktreeInfo
    ),
    spec!(
        "zor/worktree.list",
        brp_worktree_list,
        super::methods::NoParams,
        WorktreeList
    ),
    spec!(
        "zor/worktree.inspect",
        brp_worktree_inspect,
        WorktreeParams,
        WorktreeInfo
    ),
    spec!(
        "zor/worktree.remove",
        brp_worktree_remove,
        WorktreeRemoveParams,
        WorktreeInfo
    ),
    spec!(
        "zor/worktree.reconcile",
        brp_worktree_reconcile,
        WorktreeParams,
        WorktreeInfo
    ),
];
