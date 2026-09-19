//! Authorized plugin management and invocation. Management requires administrator authority;
//! a plugin's ordinary token cannot install code, enable peers, or advance another cursor.
use bevy_ecs::prelude::*;
use bevy_remote::{BrpError, BrpResult};
use super::methods::{MethodSpec, NoParams, Request, codes, described, error, handler, spec, to_value, unauthorized};
use super::token::Capabilities;
use crate::plugins::{self, PluginError, PluginRecord, Binding, HookCursors, RunContext};

described!(pub struct PluginNameParams { pub name: String });
described!(pub struct PluginPathParams { pub path: String, #[serde(default)] pub enabled: bool });
described!(pub struct PluginRunParams { pub name: String, pub action: String, pub workspace: Option<String>, pub task: Option<String>, pub expected_fux_instance: Option<String> });
described!(pub struct PluginPaneParams { pub name: String, pub pane: String, pub workspace: String, pub target: Option<u64>, pub task: Option<String> });
described!(pub struct PluginLinkParams { pub url: String });
described!(pub struct PluginLogsParams { pub name: String, #[serde(default = "default_limit")] pub limit: usize });
described!(pub struct PluginCursorParams { pub name: String, pub zor: Option<u64>, pub fux: Option<u64>, pub zor_instance: Option<String>, pub fux_instance: Option<String> });
described!(pub struct PluginList { pub plugins: Vec<PluginRecord> });
described!(pub struct PluginInspected { pub plugin: PluginRecord });
described!(pub struct PluginRunStarted { pub run: u64 });
described!(pub struct PluginPaneOpened { pub opening: u64 });
described!(pub struct PluginLinkOpened { pub name: String, pub run: u64 });
described!(pub struct PluginLogs { pub lines: Vec<String> });
described!(pub struct PluginBindings { pub bindings: Vec<Binding> });
described!(pub struct PluginCursor { pub cursors: HookCursors });
described!(pub struct PluginSubmitted { pub operation: u64 });
described!(pub struct PluginOperationParams { pub operation: u64 });
described!(pub struct PluginOperation { pub operation: u64, pub state: String, pub name: Option<String>, pub problem: Option<String> });
described!(pub struct PluginUninstalled { pub name: String });
fn default_limit() -> usize { 100 }
fn failure(e: PluginError) -> BrpError {
    error(if matches!(e, PluginError::NotFound(_)) { codes::NOT_FOUND } else { codes::INVALID }, e.to_string())
}
fn admin(req: &Request, world: &World) -> Result<(), BrpError> { req.require(Capabilities::ADMIN)?; req.mutation(world) }
fn list(mut req: Request, world: &mut World) -> BrpResult {
    let NoParams {} = req.parse()?;
    to_value(PluginList { plugins: plugins::records(world) })
}
fn inspect(mut req: Request, world: &mut World) -> BrpResult {
    let p: PluginNameParams = req.parse()?;
    let plugin = plugins::find(world, &p.name).map_err(failure)?;
    to_value(PluginInspected { plugin: plugins::record(world, plugin) })
}
fn install(mut req: Request, world: &mut World) -> BrpResult {
    admin(&req, world)?;
    let p: PluginPathParams = req.parse()?;
    to_value(PluginSubmitted { operation: plugins::install(world, std::path::Path::new(&p.path), p.enabled).map_err(failure)? })
}
fn link(mut req: Request, world: &mut World) -> BrpResult {
    admin(&req, world)?;
    let p: PluginPathParams = req.parse()?;
    to_value(PluginSubmitted { operation: plugins::link(world, std::path::Path::new(&p.path), p.enabled).map_err(failure)? })
}
fn enable(mut req: Request, world: &mut World) -> BrpResult {
    admin(&req, world)?;
    let p: PluginNameParams = req.parse()?;
    let plugin = plugins::find(world, &p.name).map_err(failure)?;
    plugins::enable(world, plugin).map_err(failure)?;
    to_value(PluginInspected { plugin: plugins::record(world, plugin) })
}
fn disable(mut req: Request, world: &mut World) -> BrpResult {
    admin(&req, world)?;
    let p: PluginNameParams = req.parse()?;
    let plugin = plugins::find(world, &p.name).map_err(failure)?;
    plugins::disable(world, plugin).map_err(failure)?;
    to_value(PluginInspected { plugin: plugins::record(world, plugin) })
}
fn uninstall(mut req: Request, world: &mut World) -> BrpResult {
    admin(&req, world)?;
    let p: PluginNameParams = req.parse()?;
    let plugin = plugins::find(world, &p.name).map_err(failure)?;
    plugins::uninstall(world, plugin).map_err(failure)?;
    to_value(PluginUninstalled { name: p.name })
}
fn run(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let p: PluginRunParams = req.parse()?;
    if let Some(expected) = &p.expected_fux_instance
        && world.resource::<crate::lifecycle::Link>().instance.as_ref() != Some(expected)
    {
        return Err(error(codes::INVALID, "plugin action belongs to a different fux incarnation"));
    }
    let plugin = plugins::find(world, &p.name).map_err(failure)?;
    to_value(PluginRunStarted { run: plugins::run_action(world, plugin, &p.action, RunContext { workspace: p.workspace, task: p.task }).map_err(failure)? })
}
fn pane(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let p: PluginPaneParams = req.parse()?;
    let plugin = plugins::find(world, &p.name).map_err(failure)?;
    to_value(PluginPaneOpened { opening: plugins::open_pane(world, plugin, &p.pane, &p.workspace, p.target, p.task).map_err(failure)? })
}
fn open_link(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let p: PluginLinkParams = req.parse()?;
    let (plugin, run) = plugins::open_link(world, &p.url).map_err(failure)?;
    to_value(PluginLinkOpened { name: world.get::<crate::model::PluginId>(plugin).map(|id| id.0.clone()).unwrap_or_default(), run })
}
fn logs(mut req: Request, world: &mut World) -> BrpResult {
    let p: PluginLogsParams = req.parse()?;
    let plugin = plugins::find(world, &p.name).map_err(failure)?;
    to_value(PluginLogs { lines: plugins::logs(world, plugin, p.limit).map_err(failure)? })
}
fn bindings(mut req: Request, world: &mut World) -> BrpResult {
    let NoParams {} = req.parse()?;
    to_value(PluginBindings { bindings: plugins::bindings(world) })
}
fn cursor(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let p: PluginCursorParams = req.parse()?;
    let plugin = plugins::find(world, &p.name).map_err(failure)?;
    if !plugins::owns_token(world, plugin, req.token()) && req.require(Capabilities::ADMIN).is_err() {
        return Err(unauthorized("token does not own this plugin cursor"));
    }
    to_value(PluginCursor { cursors: plugins::set_cursor_for(world, plugin, p.zor, p.fux, p.zor_instance, p.fux_instance).map_err(failure)? })
}
fn operation(mut req: Request, world: &mut World) -> BrpResult {
    req.require(Capabilities::ADMIN)?;
    let p: PluginOperationParams = req.parse()?;
    let op = plugins::loading::operation(world, p.operation).map_err(failure)?;
    to_value(PluginOperation { operation: op.operation, state: op.state, name: op.name, problem: op.problem })
}
handler!(brp_operation, operation);
handler!(brp_uninstall, uninstall);
handler!(brp_list, list); handler!(brp_inspect, inspect); handler!(brp_install, install);
handler!(brp_link, link); handler!(brp_enable, enable); handler!(brp_disable, disable);
handler!(brp_run, run); handler!(brp_pane, pane); handler!(brp_open_link, open_link);
handler!(brp_logs, logs); handler!(brp_bindings, bindings); handler!(brp_cursor, cursor);
pub const METHODS: &[MethodSpec] = &[
    spec!("zor/plugin.list", brp_list, NoParams, PluginList),
    spec!("zor/plugin.inspect", brp_inspect, PluginNameParams, PluginInspected),
    spec!("zor/plugin.install", brp_install, PluginPathParams, PluginSubmitted),
    spec!("zor/plugin.link", brp_link, PluginPathParams, PluginSubmitted),
    spec!("zor/plugin.operation", brp_operation, PluginOperationParams, PluginOperation),
    spec!("zor/plugin.enable", brp_enable, PluginNameParams, PluginInspected),
    spec!("zor/plugin.disable", brp_disable, PluginNameParams, PluginInspected),
    spec!("zor/plugin.uninstall", brp_uninstall, PluginNameParams, PluginUninstalled),
    spec!("zor/plugin.run", brp_run, PluginRunParams, PluginRunStarted),
    spec!("zor/plugin.pane", brp_pane, PluginPaneParams, PluginPaneOpened),
    spec!("zor/plugin.open_link", brp_open_link, PluginLinkParams, PluginLinkOpened),
    spec!("zor/plugin.logs", brp_logs, PluginLogsParams, PluginLogs),
    spec!("zor/plugin.bindings", brp_bindings, NoParams, PluginBindings),
    spec!("zor/plugin.cursor", brp_cursor, PluginCursorParams, PluginCursor),
];
