//! `zor/agent.*`, `zor/rules.*` (milestone 6, owner Providers): observed panes with their
//! merged evidence (DASHBOARD.md:38-40), and the rule collection (OBSERVATION-CONTRACT.md:150-164).

use bevy_asset::{AssetServer, Assets};
use bevy_ecs::prelude::*;
use bevy_remote::BrpResult;

use super::methods::{MethodSpec, Request, codes, described, error, handler, spec, to_value};
use crate::providers::{self, AgentRecord, Rules, RulesBundle};

described!(
    pub struct AgentListParams {}
);
described!(
    /// One observed pane (`AgentView` row): identity, merged state and its evidence.
    pub struct AgentRow {
        pub id: u64,
        pub instance: String,
        pub workspace: String,
        pub pane: u64,
        pub pid: Option<u32>,
        /// `Unknown|Working|Blocked|Idle|None`.
        pub state: String,
        /// `native`, `passive` or `none`.
        pub source: String,
        pub rule: Option<String>,
        /// The secondary passive state when native evidence wins.
        pub passive: String,
        pub since_ms: u64,
        pub age_upper_bound_ms: u64,
        pub attempt: Option<u64>,
        pub task: Option<String>,
        pub provider: Option<String>,
        pub producer: Option<String>,
        pub last_event: Option<String>,
        pub last_event_ms: u64,
        pub problem: Option<String>,
    }
);
described!(
    pub struct AgentList {
        pub agents: Vec<AgentRow>,
    }
);
described!(
    pub struct AgentInspectParams {
        pub agent: u64,
    }
);
described!(
    pub struct RulesListParams {}
);
described!(
    pub struct RuleBundleRow {
        pub id: String,
        /// `builtin:<file>` or the external asset path.
        pub source: String,
        pub rules: usize,
        pub effective: bool,
    }
);
described!(
    pub struct RulesList {
        pub generation: u64,
        pub problem: Option<String>,
        pub bundles: Vec<RuleBundleRow>,
    }
);
described!(
    pub struct RulesReloadParams {}
);
described!(
    pub struct RulesReloaded {
        /// External bundle files requested.
        pub requested: usize,
        pub generation: u64,
    }
);

fn row(record: AgentRecord) -> AgentRow {
    AgentRow {
        id: record.id,
        instance: record.instance,
        workspace: record.workspace,
        pane: record.pane,
        pid: record.pid,
        state: record.state,
        source: record.source,
        rule: record.rule,
        passive: record.passive,
        since_ms: record.since_ms,
        age_upper_bound_ms: record.age_upper_bound_ms,
        attempt: record.attempt,
        task: record.task,
        provider: record.provider,
        producer: record.producer,
        last_event: record.last_event,
        last_event_ms: record.last_event_ms,
        problem: record.problem,
    }
}

fn agent_list(mut req: Request, world: &mut World) -> BrpResult {
    let _: AgentListParams = req.parse()?;
    let agents = providers::agent_records(world)
        .into_iter()
        .map(|(_, r)| row(r))
        .collect();
    to_value(AgentList { agents })
}

fn agent_inspect(mut req: Request, world: &mut World) -> BrpResult {
    let params: AgentInspectParams = req.parse()?;
    providers::agent_records(world)
        .into_iter()
        .map(|(_, r)| r)
        .find(|r| r.id == params.agent)
        .map_or_else(
            || {
                Err(error(
                    codes::NOT_FOUND,
                    format!("agent {} not found", params.agent),
                ))
            },
            |record| to_value(row(record)),
        )
}

fn rules_list(mut req: Request, world: &mut World) -> BrpResult {
    let _: RulesListParams = req.parse()?;
    let rules = world.resource::<Rules>();
    let assets = world.resource::<Assets<RulesBundle>>();
    to_value(RulesList {
        generation: rules.generation,
        problem: rules.problem.clone(),
        bundles: rules
            .list(assets)
            .into_iter()
            .map(|b| RuleBundleRow {
                id: b.id,
                source: b.source,
                rules: b.rules,
                effective: b.effective,
            })
            .collect(),
    })
}

/// Re-reads the rules directory and reloads every bundle; an invalid file keeps its previous
/// published value and surfaces in `zor/rules.list`.
fn rules_reload(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let _: RulesReloadParams = req.parse()?;
    let server = world.resource::<AssetServer>().clone();
    let mut rules = world.resource_mut::<Rules>();
    let requested = rules.reload(&server);
    to_value(RulesReloaded {
        requested,
        generation: rules.generation,
    })
}

handler!(brp_agent_list, agent_list);
handler!(brp_agent_inspect, agent_inspect);
handler!(brp_rules_list, rules_list);
handler!(brp_rules_reload, rules_reload);

pub const METHODS: &[MethodSpec] = &[
    spec!("zor/agent.list", brp_agent_list, AgentListParams, AgentList),
    spec!(
        "zor/agent.inspect",
        brp_agent_inspect,
        AgentInspectParams,
        AgentRow
    ),
    spec!("zor/rules.list", brp_rules_list, RulesListParams, RulesList),
    spec!(
        "zor/rules.reload",
        brp_rules_reload,
        RulesReloadParams,
        RulesReloaded
    ),
];
