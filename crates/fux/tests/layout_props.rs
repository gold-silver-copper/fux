//! Prompt section 5: random template-edit sequences interleaved with viewer attach/detach/show/
//! resize/zoom against the real `bevy_ui` layout systems. After every `update` the section 3.5
//! invariants hold and every viewer's instance equals its template in shape.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "integration-test helpers outside #[test] fns; clippy.toml only relaxes test fns"
)]

use bevy_app::prelude::*;
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::prelude::*;
use bevy_ecs::query::Allow;
use bevy_ui::UiTargetCamera;
use bevy_ui::prelude::*;
use fux::layout::{GridTrackPatch, LayoutPlugin, NodePatch, ops};
use fux::model::invariants::check_invariants;
use fux::model::*;
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Op {
    NewRoot,
    Spawn {
        parent: usize,
        pane: bool,
        kind: u8,
    },
    Split {
        pane: usize,
        below: bool,
    },
    Exit {
        pane: usize,
    },
    RemoveLeaf {
        pane: usize,
    },
    Despawn {
        node: usize,
    },
    Reparent {
        node: usize,
        parent: usize,
        index: Option<u8>,
    },
    Reorder {
        node: usize,
        index: u8,
    },
    Patch {
        node: usize,
        kind: u8,
    },
    Attach {
        rows: u16,
        cols: u16,
        exact: bool,
    },
    Detach {
        viewer: usize,
    },
    Show {
        viewer: usize,
        root: usize,
    },
    Resize {
        viewer: usize,
        rows: u16,
        cols: u16,
    },
    Zoom {
        viewer: usize,
        node: usize,
    },
    Unzoom {
        viewer: usize,
    },
    Target {
        viewer: usize,
        pane: usize,
    },
}

fn op() -> impl Strategy<Value = Op> {
    let idx = 0..64usize;
    prop_oneof![
        1 => Just(Op::NewRoot),
        6 => (idx.clone(), any::<bool>(), 0..4u8).prop_map(|(parent, pane, kind)| Op::Spawn { parent, pane, kind }),
        6 => (idx.clone(), any::<bool>()).prop_map(|(pane, below)| Op::Split { pane, below }),
        3 => idx.clone().prop_map(|pane| Op::Exit { pane }),
        3 => idx.clone().prop_map(|pane| Op::RemoveLeaf { pane }),
        3 => idx.clone().prop_map(|node| Op::Despawn { node }),
        4 => (idx.clone(), idx.clone(), proptest::option::of(0..8u8)).prop_map(|(node, parent, index)| Op::Reparent { node, parent, index }),
        3 => (idx.clone(), 0..8u8).prop_map(|(node, index)| Op::Reorder { node, index }),
        4 => (idx.clone(), 0..5u8).prop_map(|(node, kind)| Op::Patch { node, kind }),
        4 => (2..60u16, 2..200u16, any::<bool>()).prop_map(|(rows, cols, exact)| Op::Attach { rows, cols, exact }),
        2 => idx.clone().prop_map(|viewer| Op::Detach { viewer }),
        3 => (idx.clone(), idx.clone()).prop_map(|(viewer, root)| Op::Show { viewer, root }),
        3 => (idx.clone(), 1..80u16, 1..300u16).prop_map(|(viewer, rows, cols)| Op::Resize { viewer, rows, cols }),
        3 => (idx.clone(), idx.clone()).prop_map(|(viewer, node)| Op::Zoom { viewer, node }),
        2 => idx.clone().prop_map(|viewer| Op::Unzoom { viewer }),
        3 => (idx.clone(), idx).prop_map(|(viewer, pane)| Op::Target { viewer, pane }),
    ]
}

fn app() -> App {
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        bevy_state::app::StatesPlugin,
        bevy_time::TimePlugin,
        bevy_asset::AssetPlugin::default(),
        ModelPlugin,
        LayoutPlugin,
    ));
    app.insert_resource(Limits {
        nodes_per_workspace: 48,
        panes_per_workspace: 16,
        viewers: 6,
        ..Default::default()
    });
    app
}

fn shell() -> PaneTemplate {
    PaneTemplate {
        argv: vec!["sh".to_owned()],
        ..Default::default()
    }
}

fn node_of_kind(kind: u8) -> Node {
    match kind {
        0 => Node {
            flex_grow: 1.0,
            ..Default::default()
        },
        1 => Node {
            flex_direction: FlexDirection::Column,
            flex_grow: 1.0,
            border: UiRect::all(Val::Px(1.0)),
            ..Default::default()
        },
        2 => Node {
            display: Display::Grid,
            grid_template_columns: vec![RepeatedGridTrack::flex(2, 1.0)],
            width: Val::Percent(100.0),
            ..Default::default()
        },
        _ => Node {
            position_type: PositionType::Absolute,
            left: Val::Px(2.0),
            top: Val::Px(1.0),
            width: Val::Percent(50.0),
            height: Val::Percent(50.0),
            ..Default::default()
        },
    }
}

fn patch_of_kind(kind: u8) -> NodePatch {
    match kind {
        0 => NodePatch {
            flex_direction: Some("column".to_owned()),
            ..Default::default()
        },
        1 => NodePatch {
            width: Some("30%".to_owned()),
            min_width: Some("4px".to_owned()),
            ..Default::default()
        },
        2 => NodePatch {
            display: Some("grid".to_owned()),
            grid_template_columns: Some(vec![GridTrackPatch {
                repeat: 3,
                track: "1fr".to_owned(),
            }]),
            ..Default::default()
        },
        3 => NodePatch {
            position_type: Some("absolute".to_owned()),
            left: Some("0px".to_owned()),
            top: Some("0px".to_owned()),
            width: Some("50%".to_owned()),
            height: Some("auto".to_owned()),
            ..Default::default()
        },
        _ => NodePatch {
            overflow_y: Some("scroll".to_owned()),
            border: Some(fux::layout::RectPatch {
                all: Some("1px".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        },
    }
}

fn pick<T: Copy>(items: &[T], index: usize) -> Option<T> {
    if items.is_empty() {
        None
    } else {
        items.get(index % items.len()).copied()
    }
}

struct Live {
    nodes: Vec<Entity>,
    roots: Vec<Entity>,
    panes: Vec<Entity>,
    viewers: Vec<Entity>,
}

fn live(world: &mut World) -> Live {
    let mut nodes: Vec<Entity> = world
        .query_filtered::<Entity, With<TemplateNode>>()
        .iter(world)
        .collect();
    nodes.sort();
    let mut roots: Vec<Entity> = world
        .query_filtered::<Entity, With<TemplateRoot>>()
        .iter(world)
        .collect();
    roots.sort();
    let mut panes: Vec<Entity> = world
        .query_filtered::<Entity, (With<Pane>, Allow<Disabled>)>()
        .iter(world)
        .collect();
    panes.sort();
    let mut viewers: Vec<Entity> = world
        .query_filtered::<Entity, With<Viewer>>()
        .iter(world)
        .collect();
    viewers.sort();
    Live {
        nodes,
        roots,
        panes,
        viewers,
    }
}

fn apply(world: &mut World, ws: Entity, op: &Op) {
    let live = live(world);
    match *op {
        Op::NewRoot => {
            let _ = ops::new_root(world, ws, "root");
        }
        Op::Spawn { parent, pane, kind } => {
            if let Some(parent) = pick(&live.nodes, parent) {
                let _ = ops::spawn_node(world, parent, None, node_of_kind(kind), pane.then(shell));
            }
        }
        Op::Split { pane, below } => {
            if let Some(pane) = pick(&live.panes, pane) {
                let direction = if below {
                    SplitDirection::Below
                } else {
                    SplitDirection::Right
                };
                let _ = ops::split(world, pane, direction, shell());
            }
        }
        Op::Exit { pane } => {
            if let Some(pane) = pick(&live.panes, pane) {
                world
                    .entity_mut(pane)
                    .insert(Process::Exited { code: 0 })
                    .remove::<Disabled>();
            }
        }
        Op::RemoveLeaf { pane } => {
            if let Some(pane) = pick(&live.panes, pane)
                && ops::remove_leaf(world, pane).is_ok()
            {
                // The lifecycle despawns the pane in the same step.
                world.despawn(pane);
            }
        }
        Op::Despawn { node } => {
            if let Some(node) = pick(&live.nodes, node) {
                let _ = ops::despawn_node(world, node);
            }
        }
        Op::Reparent {
            node,
            parent,
            index,
        } => {
            if let (Some(node), Some(parent)) = (pick(&live.nodes, node), pick(&live.nodes, parent))
            {
                let _ = ops::reparent_node(world, node, parent, index.map(usize::from));
            }
        }
        Op::Reorder { node, index } => {
            if let Some(node) = pick(&live.nodes, node) {
                let _ = ops::reorder_node(world, node, usize::from(index));
            }
        }
        Op::Patch { node, kind } => {
            if let Some(node) = pick(&live.nodes, node) {
                let _ = ops::patch_node(world, node, &patch_of_kind(kind));
            }
        }
        Op::Attach { rows, cols, exact } => {
            let exact = exact
                .then(|| pick(&live.panes, usize::from(rows)))
                .flatten();
            let _ = ops::attach_viewer(world, ws, Viewport { rows, cols }, exact);
        }
        Op::Detach { viewer } => {
            if let Some(viewer) = pick(&live.viewers, viewer) {
                let _ = ops::detach_viewer(world, viewer);
            }
        }
        Op::Show { viewer, root } => {
            if let (Some(viewer), Some(root)) =
                (pick(&live.viewers, viewer), pick(&live.roots, root))
            {
                let _ = ops::show_root(world, viewer, root);
            }
        }
        Op::Resize { viewer, rows, cols } => {
            if let Some(viewer) = pick(&live.viewers, viewer) {
                let _ = ops::resize_viewer(world, viewer, Viewport { rows, cols });
            }
        }
        Op::Zoom { viewer, node } => {
            if let (Some(viewer), Some(node)) =
                (pick(&live.viewers, viewer), pick(&live.nodes, node))
            {
                let _ = ops::zoom(world, viewer, node);
            }
        }
        Op::Unzoom { viewer } => {
            if let Some(viewer) = pick(&live.viewers, viewer) {
                let _ = ops::unzoom(world, viewer);
            }
        }
        Op::Target { viewer, pane } => {
            if let (Some(viewer), Some(pane)) =
                (pick(&live.viewers, viewer), pick(&live.panes, pane))
            {
                let _ = ops::target(world, viewer, pane);
            }
        }
    }
}

fn kids(world: &World, e: Entity) -> Vec<Entity> {
    world
        .get::<Children>(e)
        .map(|c| c.iter().collect())
        .unwrap_or_default()
}

fn same_shape(
    world: &World,
    template: Entity,
    instance: Entity,
    zoomed: bool,
) -> Result<(), String> {
    if world.get::<InstanceOf>(instance).map(|i| i.0) != Some(template) {
        return Err(format!("{instance} is not an instance of {template}"));
    }
    if world.get::<InstanceNode>(instance).is_none()
        || world.get::<TemplateNode>(instance).is_some()
    {
        return Err(format!("{instance} is not marked as an instance"));
    }
    if world.get::<NodeId>(instance) != world.get::<NodeId>(template) {
        return Err(format!("{instance} has a different NodeId than {template}"));
    }
    if world.get::<Places>(template).map(|p| p.0) != world.get::<Shows>(instance).map(|s| s.0) {
        return Err(format!(
            "{instance} shows a different pane than {template} places"
        ));
    }
    if !zoomed && world.get::<Node>(instance) != world.get::<Node>(template) {
        return Err(format!("{instance} has a different Node than {template}"));
    }
    if world.get::<ZIndex>(instance) != world.get::<ZIndex>(template)
        || world.get::<Name>(instance) != world.get::<Name>(template)
    {
        return Err(format!(
            "{instance} differs from {template} in ZIndex or Name"
        ));
    }
    let t = kids(world, template);
    let i = kids(world, instance);
    if t.len() != i.len() {
        return Err(format!(
            "{instance} has {} children, template {template} has {}",
            i.len(),
            t.len()
        ));
    }
    for (t, i) in t.iter().zip(i.iter()) {
        same_shape(world, *t, *i, zoomed)?;
    }
    Ok(())
}

fn check_instances(world: &mut World) -> Result<(), String> {
    let viewers: Vec<(Entity, Entity, Option<Entity>)> = world
        .query_filtered::<(Entity, &ViewerCamera, Option<&Showing>), With<Viewer>>()
        .iter(world)
        .map(|(v, c, s)| (v, c.0, s.map(|s| s.0)))
        .collect();
    let roots: Vec<(Entity, Entity, Option<Entity>)> = world
        .query_filtered::<(Entity, &UiTargetCamera, Option<&InstanceOf>), (With<InstanceNode>, Without<ChildOf>)>()
        .iter(world)
        .map(|(e, c, of)| (e, c.entity(), of.map(|o| o.0)))
        .collect();
    for (instance, camera, template) in &roots {
        let owner = viewers.iter().find(|v| v.1 == *camera);
        if owner.is_none() {
            return Err(format!("instance root {instance} has no viewer"));
        }
        if template.is_none() {
            return Err(format!("instance root {instance} points at no template"));
        }
    }
    for (viewer, camera, showing) in viewers {
        let mine: Vec<Entity> = roots
            .iter()
            .filter(|r| r.1 == camera)
            .map(|r| r.0)
            .collect();
        match showing.filter(|root| world.get::<TemplateRoot>(*root).is_some()) {
            Some(root) => {
                if mine.len() != 1 {
                    return Err(format!("viewer {viewer} has {} instance roots", mine.len()));
                }
                let zoomed = world.get::<Zoomed>(mine[0]).is_some();
                same_shape(world, root, mine[0], zoomed)?;
            }
            None => {
                if !mine.is_empty() {
                    return Err(format!("viewer {viewer} shows nothing but has instances"));
                }
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, max_shrink_iters: 200, ..ProptestConfig::default() })]

    #[test]
    fn random_edits_keep_invariants_and_instances_equal_templates(steps in proptest::collection::vec(op(), 1..40)) {
        let mut app = app();
        let ws = ops::new_workspace(app.world_mut(), "prop").unwrap();
        let root = ops::new_root(app.world_mut(), ws, "main").unwrap();
        ops::spawn_node(app.world_mut(), root, None, node_of_kind(0), Some(shell())).unwrap();
        for (i, step) in steps.iter().enumerate() {
            apply(app.world_mut(), ws, step);
            app.update();
            check_invariants(app.world_mut()).map_err(|e| TestCaseError::fail(format!("step {i} {step:?}: {e}")))?;
            check_instances(app.world_mut()).map_err(|e| TestCaseError::fail(format!("step {i} {step:?}: {e}")))?;
        }
    }
}
