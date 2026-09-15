//! `fux/scene.*` (prompt 3.9): a workspace's layout as a RON `DynamicWorld` document over BRP.
//! `export` and `list` are reads; `save` writes a file (mutate capability, no World change);
//! `apply` and `restore` are template edits guarded by the instance nonce and, optionally, the
//! `(root, generation)` pairs the caller last saw.

use bevy_ecs::prelude::*;
use bevy_remote::{BrpError, BrpResult};
use serde_json::json;

use super::methods::{MethodSpec, NoParams, Request, codes, handler, invalid, spec, to_value};
use super::schema::described;
use super::token::Capabilities;
use crate::model::{NodeId, PaneId};
use crate::scene::{self, ApplyOptions, ApplyReport, LayoutDir, SceneError};

// `RootState`: a root as the caller saw it; `fux/scene.export` returns these and
// `apply`/`restore` take them back as `expected`.
described!(
    pub struct RootState {
        pub root: u64,
        pub generation: u64,
    }
);

described!(
    pub struct SceneExportParams {
        pub workspace: String,
    }
);
described!(
    pub struct SceneExported {
        pub document: String,
        pub roots: Vec<RootState>,
    }
);

described!(
    pub struct SceneApplyParams {
        pub workspace: String,
        pub document: String,
        pub expected: Option<Vec<RootState>>,
        /// Leaves may launch processes (default false).
        pub allow_templates: Option<bool>,
        /// Unreferenced live panes fill launching leaves first (default false).
        pub adopt: Option<bool>,
        /// Unreferenced live panes are closed instead of refusing (default false).
        pub close_unplaced: Option<bool>,
    }
);
described!(
    pub struct AppliedRoot {
        pub root: u64,
        pub name: String,
        pub generation: u64,
    }
);
described!(
    pub struct SceneApplied {
        pub roots: Vec<AppliedRoot>,
        pub launched: Vec<u64>,
        pub adopted: Vec<u64>,
        pub closed: Vec<u64>,
    }
);

described!(
    pub struct SceneListed {
        pub saved: Vec<String>,
        pub builtin: Vec<String>,
    }
);

described!(
    pub struct SceneSaveParams {
        pub workspace: String,
        pub name: String,
    }
);
described!(
    pub struct SceneSaved {
        pub name: String,
        pub path: String,
        pub bytes: usize,
    }
);

described!(
    pub struct SceneRestoreParams {
        pub workspace: String,
        pub name: String,
        pub expected: Option<Vec<RootState>>,
        /// Default true: a restored layout takes over the panes already there.
        pub adopt: Option<bool>,
        /// Default false.
        pub close_unplaced: Option<bool>,
    }
);

fn scene_error(e: SceneError) -> BrpError {
    match &e {
        SceneError::StaleGeneration { current, .. } => BrpError {
            code: codes::STALE_GENERATION,
            message: e.to_string(),
            data: Some(json!({ "roots": root_states(current) })),
        },
        SceneError::NotFound(_) | SceneError::PaneNotFound(_) => BrpError {
            code: codes::NOT_FOUND,
            message: e.to_string(),
            data: None,
        },
        _ => invalid(e.to_string()),
    }
}

fn root_states(states: &[(NodeId, u64)]) -> Vec<RootState> {
    states
        .iter()
        .map(|&(id, generation)| RootState {
            root: id.0,
            generation,
        })
        .collect()
}

fn expected(states: Option<Vec<RootState>>) -> Option<Vec<(NodeId, u64)>> {
    states.map(|s| {
        s.into_iter()
            .map(|r| (NodeId(r.root), r.generation))
            .collect()
    })
}

fn layout_dir(world: &World) -> Result<std::path::PathBuf, BrpError> {
    world
        .get_resource::<LayoutDir>()
        .map(|d| d.0.clone())
        .ok_or_else(|| invalid(SceneError::NoLayoutDir.to_string()))
}

fn pane_ids(world: &World, panes: &[Entity]) -> Vec<u64> {
    panes
        .iter()
        .filter_map(|&p| world.get::<PaneId>(p).map(|id| id.0))
        .collect()
}

fn applied(world: &World, report: ApplyReport) -> BrpResult {
    let roots = report
        .roots
        .iter()
        .map(|&(root, generation)| AppliedRoot {
            root: world.get::<NodeId>(root).map_or(0, |id| id.0),
            name: world
                .get::<Name>(root)
                .map(|n| n.as_str().to_owned())
                .unwrap_or_default(),
            generation,
        })
        .collect();
    to_value(SceneApplied {
        roots,
        launched: pane_ids(world, &report.launched),
        adopted: pane_ids(world, &report.adopted),
        closed: pane_ids(world, &report.closed),
    })
}

fn scene_export(mut req: Request, world: &mut World) -> BrpResult {
    let params: SceneExportParams = req.parse()?;
    let workspace = req.workspace(world, &params.workspace)?;
    let export = scene::export(world, workspace).map_err(scene_error)?;
    let roots: Vec<(NodeId, u64)> = export.roots.iter().map(|r| (r.1, r.2)).collect();
    to_value(SceneExported {
        document: export.document,
        roots: root_states(&roots),
    })
}

fn scene_apply(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: SceneApplyParams = req.parse()?;
    let workspace = req.workspace(world, &params.workspace)?;
    let options = ApplyOptions {
        expected: expected(params.expected),
        allow_templates: params.allow_templates.unwrap_or(false),
        adopt: params.adopt.unwrap_or(false),
        close_unplaced: params.close_unplaced.unwrap_or(false),
    };
    let report = scene::apply(world, workspace, &params.document, &options).map_err(scene_error)?;
    applied(world, report)
}

fn scene_list(mut req: Request, world: &mut World) -> BrpResult {
    let NoParams {} = req.parse()?;
    let saved = scene::list(&layout_dir(world)?).map_err(scene_error)?;
    to_value(SceneListed {
        saved,
        builtin: scene::builtin::NAMES
            .iter()
            .map(|n| (*n).to_owned())
            .collect(),
    })
}

fn scene_save(mut req: Request, world: &mut World) -> BrpResult {
    req.require(Capabilities::MUTATE)?;
    let params: SceneSaveParams = req.parse()?;
    let workspace = req.workspace(world, &params.workspace)?;
    let dir = layout_dir(world)?;
    let export = scene::export(world, workspace).map_err(scene_error)?;
    let path = scene::save(&dir, &params.name, &export.document).map_err(scene_error)?;
    to_value(SceneSaved {
        name: params.name,
        path: path.display().to_string(),
        bytes: export.document.len(),
    })
}

fn scene_restore(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: SceneRestoreParams = req.parse()?;
    let workspace = req.workspace(world, &params.workspace)?;
    let dir = layout_dir(world)?;
    let options = ApplyOptions {
        expected: expected(params.expected),
        allow_templates: true,
        adopt: params.adopt.unwrap_or(true),
        close_unplaced: params.close_unplaced.unwrap_or(false),
    };
    let report =
        scene::restore(world, workspace, &dir, &params.name, &options).map_err(scene_error)?;
    applied(world, report)
}

handler!(brp_scene_export, scene_export);
handler!(brp_scene_apply, scene_apply);
handler!(brp_scene_list, scene_list);
handler!(brp_scene_save, scene_save);
handler!(brp_scene_restore, scene_restore);

pub static METHODS: &[MethodSpec] = &[
    spec!(
        "fux/scene.export",
        brp_scene_export,
        SceneExportParams,
        SceneExported
    ),
    spec!(
        "fux/scene.apply",
        brp_scene_apply,
        SceneApplyParams,
        SceneApplied
    ),
    spec!("fux/scene.list", brp_scene_list, NoParams, SceneListed),
    spec!(
        "fux/scene.save",
        brp_scene_save,
        SceneSaveParams,
        SceneSaved
    ),
    spec!(
        "fux/scene.restore",
        brp_scene_restore,
        SceneRestoreParams,
        SceneApplied
    ),
];
