//! Authorized dashboard scene and interaction commands; no raw World mutation.
use bevy_ecs::prelude::*;
use bevy_remote::BrpResult;
use crate::dashboard::{self, Input, Open, Row, State};
use super::methods::{MethodSpec, Request, described, handler, invalid, spec, to_value};
use super::schema::Described;

described!(pub struct DashboardRowsParams { #[serde(default)] pub machine: Option<String> });
described!(pub struct DashboardRows { pub rows: Vec<Row> });
described!(pub struct DashboardParams { pub id: u64 });
described!(pub struct DashboardInputParams {
    pub id: u64,
    pub viewer: u64,
    pub revision: u64,
    #[serde(default)] pub provider_node: Option<u64>,
    pub input: Input,
});
described!(pub struct DashboardReturnParams { pub id: u64, pub handoff: u64 });
impl Described for Open {
    const NAME: &'static str = "DashboardOpenParams";
    const FIELDS: &'static [(&'static str, &'static str)] = &[("workspace","String"),("node","u64"),("generation","u64"),("machine","Option<String>"),("viewer","Option<u64>")];
}
impl Described for State {
    const NAME: &'static str = "DashboardState";
    const FIELDS: &'static [(&'static str, &'static str)] = &[("id","u64"),("provider","String"),("workspace","String"),("node","u64"),("revision","u64"),("status","String"),("rows","Vec<Row>"),("row_nodes","Vec<(String, u64)>"),("selected","Option<String>"),("handoff","Option<Handoff>"),("problem","Option<String>")];
}
fn rows(mut req: Request, world: &mut World) -> BrpResult {
    let params: DashboardRowsParams = req.parse()?;
    to_value(DashboardRows { rows: dashboard::rows(world,params.machine.as_deref()).map_err(invalid)? })
}
fn open(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: Open = req.parse()?;
    to_value(dashboard::open(world,params).map_err(invalid)?)
}
fn close(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: DashboardParams = req.parse()?;
    to_value(dashboard::close(world,params.id).map_err(invalid)?)
}
fn state(mut req: Request, world: &mut World) -> BrpResult {
    let params: DashboardParams = req.parse()?;
    to_value(dashboard::state(world,params.id).map_err(invalid)?)
}
fn input(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: DashboardInputParams = req.parse()?;
    to_value(dashboard::input(world,params.id,params.viewer,params.revision,params.provider_node,params.input).map_err(invalid)?)
}
fn returned(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: DashboardReturnParams = req.parse()?;
    to_value(dashboard::returned(world,params.id,params.handoff).map_err(invalid)?)
}
handler!(brp_rows,rows);
handler!(brp_open,open);
handler!(brp_close,close);
handler!(brp_state,state);
handler!(brp_input,input);
handler!(brp_return,returned);
pub const METHODS: &[MethodSpec] = &[
    spec!("zor/dashboard.rows",brp_rows,DashboardRowsParams,DashboardRows),
    spec!("zor/dashboard.open",brp_open,Open,State),
    spec!("zor/dashboard.close",brp_close,DashboardParams,State),
    spec!("zor/dashboard.state",brp_state,DashboardParams,State),
    spec!("zor/dashboard.input",brp_input,DashboardInputParams,State),
    spec!("zor/dashboard.return",brp_return,DashboardReturnParams,State),
];
