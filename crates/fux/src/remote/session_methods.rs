//! `fux/session.*` (prompt 3.8): the persisted session over BRP. `save` writes the session
//! document now (mutate capability, no World change); `status` lists the file state and the
//! panes still awaiting a `--restore ask` decision; `restore` and `skip` decide one pane each
//! (mutations guarded by the instance nonce).

use bevy_ecs::prelude::*;
use bevy_remote::{BrpError, BrpResult};

use super::methods::{MethodSpec, NoParams, Request, codes, handler, invalid, spec, to_value};
use super::schema::described;
use super::token::Capabilities;
use crate::model::{LaunchAttribution, PaneId, Title};
use crate::session::{self, SessionError, SessionFile, SessionState};

described!(
    pub struct SessionSaved {
        pub path: String,
        pub bytes: usize,
    }
);

described!(
    /// A restored pane awaiting `fux/session.restore` or `fux/session.skip`.
    pub struct PendingPane {
        pub pane: u64,
        pub workspace: String,
        pub stream: String,
        pub argv: Vec<String>,
        pub cwd: Option<String>,
        pub title: String,
        pub history_lines: usize,
    }
);
described!(
    pub struct SessionStatus {
        pub path: String,
        pub exists: bool,
        /// Clock of the last write attempt (0: none yet).
        pub last_saved_ms: u64,
        pub saves: u64,
        pub last_error: Option<String>,
        pub pending: Vec<PendingPane>,
    }
);

described!(
    pub struct SessionDecisionParams {
        pub pane: u64,
    }
);
described!(
    pub struct SessionDecided {
        pub pane: u64,
        /// Panes still awaiting a decision.
        pub pending: usize,
    }
);

fn session_error(e: SessionError) -> BrpError {
    match &e {
        SessionError::NoSessionFile => BrpError {
            code: codes::NOT_FOUND,
            message: e.to_string(),
            data: None,
        },
        _ => invalid(e.to_string()),
    }
}

fn session_save(mut req: Request, world: &mut World) -> BrpResult {
    req.require(Capabilities::MUTATE)?;
    let NoParams {} = req.parse()?;
    let (path, bytes) = session::save(world).map_err(session_error)?;
    let now = crate::lifecycle::now_ms(world);
    session::note_save(world, now);
    to_value(SessionSaved {
        path: path.display().to_string(),
        bytes,
    })
}

fn session_status(mut req: Request, world: &mut World) -> BrpResult {
    let NoParams {} = req.parse()?;
    let file = world
        .get_resource::<SessionFile>()
        .ok_or_else(|| session_error(SessionError::NoSessionFile))?;
    let path = file.path();
    let state = world
        .get_resource::<SessionState>()
        .cloned()
        .unwrap_or_default();
    let pending = session::pending(world)
        .into_iter()
        .filter_map(|pane| {
            let id = world.get::<PaneId>(pane)?.0;
            let attribution = world.get::<LaunchAttribution>(pane)?;
            req.cover_name(&attribution.workspace_name).ok()?;
            Some(PendingPane {
                pane: id,
                workspace: attribution.workspace_name.clone(),
                stream: attribution.stream.clone(),
                argv: attribution.argv.clone(),
                cwd: attribution.cwd.clone(),
                title: world
                    .get::<Title>(pane)
                    .map(|t| t.0.clone())
                    .unwrap_or_default(),
                history_lines: world
                    .get::<session::Historical>(pane)
                    .map_or(0, |h| h.lines.len()),
            })
        })
        .collect();
    to_value(SessionStatus {
        path: path.display().to_string(),
        exists: path.is_file(),
        last_saved_ms: state.last_save_ms,
        saves: state.saves,
        last_error: state.last_error,
        pending,
    })
}

fn decide(
    mut req: Request,
    world: &mut World,
    decision: fn(&mut World, Entity) -> Result<(), SessionError>,
) -> BrpResult {
    req.mutation(world)?;
    let params: SessionDecisionParams = req.parse()?;
    let pane = req.pane(world, params.pane)?;
    decision(world, pane).map_err(session_error)?;
    to_value(SessionDecided {
        pane: params.pane,
        pending: session::pending(world).len(),
    })
}

fn session_restore(req: Request, world: &mut World) -> BrpResult {
    decide(req, world, session::decide_restore)
}

fn session_skip(req: Request, world: &mut World) -> BrpResult {
    decide(req, world, session::decide_skip)
}

handler!(brp_session_save, session_save);
handler!(brp_session_status, session_status);
handler!(brp_session_restore, session_restore);
handler!(brp_session_skip, session_skip);

pub static METHODS: &[MethodSpec] = &[
    spec!("fux/session.save", brp_session_save, NoParams, SessionSaved),
    spec!(
        "fux/session.status",
        brp_session_status,
        NoParams,
        SessionStatus
    ),
    spec!(
        "fux/session.restore",
        brp_session_restore,
        SessionDecisionParams,
        SessionDecided
    ),
    spec!(
        "fux/session.skip",
        brp_session_skip,
        SessionDecisionParams,
        SessionDecided
    ),
];
