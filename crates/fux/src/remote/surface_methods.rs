//! `fux/surface.*` (prompt 3.13): a template leaf hosting a provider's scene. `open` is a
//! template edit (generation-checked); `update` is revision-checked against the surface itself
//! so a provider never tracks the layout generation its updates bump; `close` despawns the
//! subtree. The delta contract and limits live in [`crate::surface`].

use bevy_ecs::prelude::*;
use bevy_remote::{BrpError, BrpResult};
use serde_json::json;

use super::methods::{
    Request, check_generation, codes, generation, handler, invalid, node_id, spec, to_value,
};
use super::schema::described;
use crate::surface::{self, SurfaceError, SurfaceState};

described!(
    pub struct SurfaceOpenParams {
        pub workspace: String,
        pub node: u64,
        pub provider: String,
        pub generation: u64,
    }
);
described!(
    pub struct SurfaceOpened {
        pub surface: u64,
        pub revision: u64,
        pub generation: u64,
    }
);
described!(
    pub struct SurfaceUpdateParams {
        pub surface: u64,
        pub expected_provider: String,
        pub revision: u64,
        /// A full update replaces the subtree: entities it omits are despawned and sibling
        /// order is set exactly. A partial one adds, reparents and rewrites components.
        pub full: bool,
        /// RON `DynamicWorld` over `Node`, `Name`, `ZIndex`, `BackgroundColor`, `BorderColor`,
        /// `ScrollPosition`, `Text`, `ChildOf`, `Children`.
        pub delta: String,
    }
);
described!(
    pub struct SurfaceUpdated {
        pub revision: u64,
        pub nodes: usize,
        pub generation: u64,
    }
);
described!(
    pub struct SurfaceCloseParams {
        pub surface: u64,
        /// Refuse cleanup if another provider has replaced this surface.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub expected_provider: Option<String>,
    }
);
described!(
    pub struct SurfaceClosed {
        pub generation: u64,
    }
);

described!(
    pub struct SurfaceScrollParams {
        pub surface: u64,
        pub expected_provider: String,
        pub revision: u64,
        pub viewer: u64,
        pub provider_node: u64,
        pub rows: i32,
        #[serde(default)]
        pub absolute: bool,
    }
);

fn surface_error(e: SurfaceError) -> BrpError {
    match e {
        SurfaceError::StaleRevision { current, .. } => BrpError {
            code: codes::STALE_GENERATION,
            message: e.to_string(),
            data: Some(json!({ "revision": current })),
        },
        SurfaceError::NoSuchEntity(_) | SurfaceError::NotASurface(_) => BrpError {
            code: codes::NOT_FOUND,
            message: e.to_string(),
            data: None,
        },
        _ => invalid(e.to_string()),
    }
}

/// A surface leaf addressed by node id, inside the token's grant.
fn surface_node(req: &Request, world: &World, id: u64) -> Result<(Entity, Entity), BrpError> {
    let (node, root) = req.node(world, id)?;
    if world.get::<SurfaceState>(node).is_none() {
        return Err(surface_error(SurfaceError::NotASurface(node)));
    }
    Ok((node, root))
}

fn surface_open(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: SurfaceOpenParams = req.parse()?;
    let workspace = req.workspace(world, &params.workspace)?;
    let (node, root) = req.node(world, params.node)?;
    let in_workspace = world
        .get::<crate::model::RootOf>(root)
        .is_some_and(|r| r.0 == workspace);
    if !in_workspace {
        return Err(invalid(format!(
            "node {} is not in workspace `{}`",
            params.node, params.workspace
        )));
    }
    check_generation(world, root, params.generation)?;
    surface::open(world, node, &params.provider).map_err(surface_error)?;
    to_value(SurfaceOpened {
        surface: node_id(world, node)?,
        revision: 0,
        generation: generation(world, root),
    })
}

fn surface_update(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: SurfaceUpdateParams = req.parse()?;
    let (node, root) = surface_node(&req, world, params.surface)?;
    if world.get::<SurfaceState>(node).is_none_or(|state| state.provider != params.expected_provider) {
        return Err(invalid("surface provider changed; refusing stale update"));
    }
    let nodes = surface::update(world, node, params.revision, params.full, &params.delta)
        .map_err(surface_error)?;
    to_value(SurfaceUpdated {
        revision: params.revision,
        nodes,
        generation: generation(world, root),
    })
}

fn surface_close(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: SurfaceCloseParams = req.parse()?;
    let (node, root) = surface_node(&req, world, params.surface)?;
    if params.expected_provider.as_ref().is_some_and(|expected| {
        world.get::<SurfaceState>(node).is_none_or(|state| &state.provider != expected)
    }) {
        return Err(invalid("surface provider changed; refusing stale cleanup"));
    }
    surface::close(world, node).map_err(surface_error)?;
    to_value(SurfaceClosed {
        generation: generation(world, root),
    })
}
fn surface_scroll(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: SurfaceScrollParams = req.parse()?;
    let (surface, _) = surface_node(&req, world, params.surface)?;
    let viewer = req.viewer(world, params.viewer)?;
    let state = world.get::<SurfaceState>(surface).ok_or_else(|| invalid("not a surface"))?;
    if state.provider != params.expected_provider || state.revision != params.revision {
        return Err(invalid("surface changed; refusing stale scroll"));
    }
    let source = Entity::try_from_bits(params.provider_node).ok_or_else(|| invalid("invalid provider node"))?;
    let node = state.entity(source).ok_or_else(|| invalid("provider node is no longer present"))?;
    let rows = if params.absolute {
        let id = world.get::<crate::model::NodeId>(node).ok_or_else(|| invalid("node identity absent"))?;
        let current = world.get::<crate::layout::ViewState>(viewer)
            .and_then(|view| view.scroll.get(id)).copied().unwrap_or(0.0);
        params.rows.max(0).saturating_sub(current as i32)
    } else {
        params.rows
    };
    crate::layout::ops::scroll(world, viewer, node, rows)
        .map_err(|error| invalid(error.to_string()))?;
    to_value(super::methods::Done {})
}

handler!(brp_surface_scroll, surface_scroll);

handler!(brp_surface_open, surface_open);
handler!(brp_surface_update, surface_update);
handler!(brp_surface_close, surface_close);

pub const METHODS: &[super::methods::MethodSpec] = &[
    spec!("fux/surface.scroll", brp_surface_scroll, SurfaceScrollParams, super::methods::Done),
    spec!(
        "fux/surface.open",
        brp_surface_open,
        SurfaceOpenParams,
        SurfaceOpened
    ),
    spec!(
        "fux/surface.update",
        brp_surface_update,
        SurfaceUpdateParams,
        SurfaceUpdated
    ),
    spec!(
        "fux/surface.close",
        brp_surface_close,
        SurfaceCloseParams,
        SurfaceClosed
    ),
];
