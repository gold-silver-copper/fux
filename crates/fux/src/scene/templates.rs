//! `bsn!` templates (prompt 3.7): typed construction of session shape. A template is an ordinary
//! `bevy_scene` [`Scene`] built from the helpers here; it is resolved and spawned into an inert
//! scratch `World`, normalised to the document shape [`super::apply`] validates, and committed
//! through the same validate-then-commit tail as a RON scene, so a template ends in exactly the
//! state `layout::ops` produces: `TemplateRoot`/`TemplateNode`/`NodeId`/`RootOf`/
//! `LayoutGeneration`/`Name` on the root, `NodeId` on every node, and behind each leaf carrying
//! a [`PaneTemplate`] a `Disabled`, `Process::Starting` pane with `Creation` that the `Requests`
//! phase materialises. Templates never spawn processes themselves.
//!
//! ## Syntax (Bevy 0.19.1)
//!
//! The prompt sketches `workspace("default") [ root("main") { Node {..} [ pane(shell()) {..} ] } ]`;
//! the shipped macro has no bracket/brace form for children or fields on an included scene.
//! Related entities are spelled `Children [ .. ]`, fields are patched by naming the component,
//! and a scene include is a plain call. The equivalent that compiles is:
//!
//! ```ignore
//! use bevy_scene::prelude::*;
//! use bevy_ui::{FlexDirection, Node, px};
//! use fux::scene::templates::{command, pane, root, shell, workspace};
//!
//! bsn! {
//!     workspace("default")
//!     Children [
//!         (
//!             root("main")
//!             Node { flex_direction: FlexDirection::Row }
//!             Children [
//!                 (pane(shell()) Node { flex_grow: 1.0 }),
//!                 (pane(command(&["htop"], Some("/tmp"))) Node { flex_grow: 1.0, min_width: px(20.0) }),
//!             ]
//!         ),
//!     ]
//! }
//! ```
//!
//! `root(..)` and `pane(..)` include a `Node` with sensible defaults (a root fills its viewport;
//! a leaf grows, shrinks and keeps the minimum pane size); a `Node { .. }` written after them
//! patches only the named fields. A scene whose root entity is a `root(..)` is a single tab; one
//! whose root is a `workspace(..)` lists its tabs as `Children`.

use bevy_asset::{AssetServer, Assets};
use bevy_ecs::prelude::*;
use bevy_ecs::template::template;
use bevy_scene::prelude::*;
use bevy_scene::{ResolvedSceneRoot, ScenePatch};
use bevy_ui::{Node, percent, px};

use super::{ApplyOptions, ApplyReport, R, SceneError, begin, empty_scratch, finish, roots_of};
use crate::layout::ops;
use crate::model::*;

// ---------------------------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------------------------

/// A workspace named `name`; its `Children` are its template roots in tab order.
pub fn workspace(name: &str) -> impl Scene {
    let name = name.to_owned();
    (
        bsn! { Workspace },
        template(move |_| Ok(WorkspaceName(name.clone()))),
    )
}

/// A template root (a tab) named `name`, filling its viewport; patch its `Node` for the flex
/// direction and list its nodes as `Children`.
pub fn root(name: &str) -> impl Scene {
    let name = name.to_owned();
    bsn! {
        TemplateRoot
        TemplateNode
        Name({name})
        Node { width: percent(100.0), height: percent(100.0) }
    }
}

/// A leaf that launches `launch` into itself: grows and shrinks with its siblings and keeps
/// the minimum pane size. Patch its `Node` for anything else.
pub fn pane(launch: PaneTemplate) -> impl Scene {
    bsn! {
        TemplateNode
        template_value(launch)
        Node {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            min_width: px(MIN_PANE_COLS),
            min_height: px(MIN_PANE_ROWS),
        }
    }
}

/// A container node (a nested row or column): grows and shrinks with its siblings; patch its
/// `Node` for the flex direction and list its nodes as `Children`.
pub fn node() -> impl Scene {
    bsn! {
        TemplateNode
        Node { flex_grow: 1.0, flex_shrink: 1.0 }
    }
}

/// The configured default command (`default-command`, else the login shell): an empty `argv`
/// that [`super::apply`] completes when the template is committed.
pub fn shell() -> PaneTemplate {
    PaneTemplate::default()
}

/// A specific command in an optional working directory.
pub fn command(argv: &[&str], cwd: Option<&str>) -> PaneTemplate {
    PaneTemplate {
        argv: argv.iter().map(|a| (*a).to_owned()).collect(),
        cwd: cwd.map(str::to_owned),
        ..PaneTemplate::default()
    }
}

/// The startup session: workspace `name` with one root `main` holding one pane running
/// `template`.
pub fn default_session(name: &str, template: PaneTemplate) -> impl Scene {
    bsn! {
        workspace(name)
        Children [
            (
                root("main")
                Children [
                    pane(template),
                ]
            ),
        ]
    }
}

// ---------------------------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------------------------

/// Creates the workspace a `workspace(..)` scene describes, with its roots and panes. A refusal
/// leaves the World untouched.
pub fn spawn_workspace(world: &mut World, scene: impl Scene) -> R<Entity> {
    let mut resolved = resolve(world, scene)?;
    let name = resolved
        .scratch
        .get::<WorkspaceName>(resolved.workspace)
        .map(|n| n.0.clone())
        .ok_or_else(|| SceneError::Template("workspace scene has no name".to_owned()))?;
    let ws = ops::new_workspace(world, &name)?;
    let options = ApplyOptions {
        allow_templates: true,
        ..ApplyOptions::default()
    };
    match finish(
        world,
        ws,
        &mut resolved.scratch,
        &resolved.entities,
        &[],
        &options,
        CreationKind::Spawn,
    ) {
        Ok(_) => Ok(ws),
        Err(error) => {
            world.despawn(ws);
            Err(error)
        }
    }
}

/// Appends the roots of `scene` (one `root(..)`, or every root of a `workspace(..)`) to
/// `workspace`; existing roots and their viewers are untouched. A refusal leaves the World
/// untouched.
pub fn spawn(world: &mut World, workspace: Entity, scene: impl Scene) -> R<ApplyReport> {
    let options = ApplyOptions {
        allow_templates: true,
        ..ApplyOptions::default()
    };
    begin(world, workspace, &options)?;
    let mut resolved = resolve(world, scene)?;
    finish(
        world,
        workspace,
        &mut resolved.scratch,
        &resolved.entities,
        &[],
        &options,
        CreationKind::Spawn,
    )
}

/// [`super::apply`] for a template: the roots of `scene` replace the workspace's, with
/// templates allowed whatever `options` says.
pub fn apply(
    world: &mut World,
    workspace: Entity,
    scene: impl Scene,
    options: &ApplyOptions,
) -> R<ApplyReport> {
    let options = ApplyOptions {
        allow_templates: true,
        ..options.clone()
    };
    begin(world, workspace, &options)?;
    let old_roots = roots_of(world, workspace);
    let mut resolved = resolve(world, scene)?;
    finish(
        world,
        workspace,
        &mut resolved.scratch,
        &resolved.entities,
        &old_roots,
        &options,
        CreationKind::Restore,
    )
}

/// A template spawned into an inert World and normalised to document shape: `workspace` holds
/// `RootOrder`, roots are parentless, `entities` lists the workspace and every node in
/// pre-order.
struct Resolved {
    scratch: World,
    entities: Vec<Entity>,
    workspace: Entity,
}

fn resolve(world: &World, scene: impl Scene) -> R<Resolved> {
    let assets = world
        .get_resource::<AssetServer>()
        .ok_or(SceneError::NoAssetServer)?;
    let patches = world
        .get_resource::<Assets<ScenePatch>>()
        .ok_or(SceneError::NoAssetServer)?;
    let template = |e: &dyn core::fmt::Display| SceneError::Template(e.to_string());
    let resolved =
        ResolvedSceneRoot::resolve(Box::new(scene), assets, patches).map_err(|e| template(&e))?;
    let mut scratch = empty_scratch(world);
    let top = resolved.spawn(&mut scratch).map_err(|e| template(&e))?.id();

    let (workspace, roots) = if scratch.get::<Workspace>(top).is_some() {
        let roots: Vec<Entity> = scratch
            .get::<Children>(top)
            .map(|c| c.iter().collect())
            .unwrap_or_default();
        for &root in &roots {
            scratch.entity_mut(root).remove::<ChildOf>();
        }
        (top, roots)
    } else if scratch.get::<TemplateRoot>(top).is_some() {
        (scratch.spawn(Workspace).id(), vec![top])
    } else {
        return Err(SceneError::Template(
            "scene root is neither a workspace nor a template root".to_owned(),
        ));
    };
    scratch
        .entity_mut(workspace)
        .insert(RootOrder(roots.clone()));

    let mut entities = vec![workspace];
    let mut stack: Vec<Entity> = roots.iter().rev().copied().collect();
    while let Some(entity) = stack.pop() {
        entities.push(entity);
        if let Some(children) = scratch.get::<Children>(entity) {
            stack.extend(children.iter().rev());
        }
    }
    Ok(Resolved {
        scratch,
        entities,
        workspace,
    })
}
