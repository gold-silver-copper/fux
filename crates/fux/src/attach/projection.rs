//! `PostUpdate`/`Projection`: each viewer's frame is a delta against its `ProjectionBaseline`
//! (extraction pattern, prompt 3.10): the allowlisted components of this viewer's instance
//! entities that are new or changed since the last projection, the instance entities that
//! vanished, root order and names when they changed, the target and shown root when they
//! changed, and one `TerminalDelta` per shown pane whose emulator advanced.

use bevy_ecs::entity::EntityHashSet;
use bevy_ecs::lifecycle::RemovedComponents;
use bevy_ecs::prelude::*;
use bevy_ecs::reflect::AppTypeRegistry;
use bevy_ecs::system::SystemState;
use bevy_state::prelude::State;
use bevy_ui::{
    BackgroundColor, BorderColor, ComputedNode, Node, ScrollPosition, UiTargetCamera, ZIndex,
};
use bevy_world_serialization::DynamicWorldBuilder;

use crate::model::Effect;
use crate::model::{
    Detaching, ExactTarget, InstanceNode, NodeId, Notice, PaneBaseline, PaneId, Process,
    ProjectionBaseline, Retiring, RootOrder, ServerMode, Showing, Shows, Surface, Targets,
    TemplateRoot, Viewer, ViewerCamera, Viewing, Zoomed,
};
use crate::surface::Text;
use crate::terminal::Terminal;
use crate::wire::{ByeReason, ProcessSummary, RootEntry, SceneFrame, ServerFrame, TerminalDelta};

/// Private per-viewer memory of what was projected: the instance entities the viewer has been
/// told about, so `despawned` can name them after they are gone (a despawned entity cannot be
/// re-queried, which is what makes this a legitimate snapshot). Everything else the frame
/// carries is keyed on change ticks and `ProjectionBaseline`.
#[derive(Component, Default)]
pub(crate) struct Projected {
    known: EntityHashSet,
    input_revision: u64,
    input_state: InputState,
}

/// The inputs that can change hit testing before the next projection has run.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct InputState {
    showing: Option<Entity>,
    generation: Option<u64>,
    viewport: Option<crate::model::Viewport>,
    view_state_tick: Option<u32>,
}

impl InputState {
    pub(crate) fn capture(world: &World, viewer: Entity) -> Self {
        let showing = world.get::<Showing>(viewer).map(|s| s.0);
        Self {
            showing,
            generation: showing.and_then(|root| {
                world
                    .get::<crate::model::LayoutGeneration>(root)
                    .map(|g| g.0)
            }),
            viewport: world.get::<crate::model::Viewport>(viewer).copied(),
            view_state_tick: world.get_entity(viewer).ok().and_then(|e| {
                e.get_ref::<crate::layout::ViewState>()
                    .map(|state| state.last_changed().get())
            }),
        }
    }
}

/// A painted revision remains usable through terminal-output-only frames, but never
/// through a scene replacement or an unprojected layout/view-state mutation.
pub(crate) fn admits_input(world: &World, viewer: Entity, revision: u64) -> bool {
    let Some(projected) = world.get::<Projected>(viewer) else {
        return false;
    };
    admits_captured_input(world, viewer, revision, &projected.input_state)
}

/// Deferred pointer policy can retain the state after its own resize, without admitting
/// an older painted revision or any intervening mutation by another operation.
pub(crate) fn admits_captured_input(
    world: &World,
    viewer: Entity,
    revision: u64,
    input_state: &InputState,
) -> bool {
    let Some(projected) = world.get::<Projected>(viewer) else {
        return false;
    };
    let Some(baseline) = world.get::<ProjectionBaseline>(viewer) else {
        return false;
    };
    revision != 0
        && revision >= projected.input_revision
        && revision <= baseline.scene_revision
        && *input_state == InputState::capture(world, viewer)
}

/// An instance node whose allowlisted components changed since the last projection.
type ChangedInstance = Or<(
    Added<InstanceNode>,
    Changed<Node>,
    Changed<ComputedNode>,
    Changed<ZIndex>,
    Changed<BackgroundColor>,
    Changed<BorderColor>,
    Changed<Name>,
    Changed<Children>,
    Changed<ChildOf>,
    Changed<Shows>,
    Changed<NodeId>,
    Changed<Zoomed>,
    Changed<Text>,
    Changed<Surface>,
    Changed<ScrollPosition>,
)>;

type Viewers<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static ViewerCamera,
        &'static Viewing,
        Option<Ref<'static, Showing>>,
        Option<Ref<'static, Targets>>,
        &'static ProjectionBaseline,
        &'static Projected,
        Option<Ref<'static, Notice>>,
    ),
    (With<Viewer>, Without<Detaching>),
>;

type Params<'w, 's> = (
    Viewers<'w, 's>,
    Query<'w, 's, (Entity, &'static UiTargetCamera), (With<InstanceNode>, Without<ChildOf>)>,
    Query<'w, 's, &'static Children>,
    Query<'w, 's, Entity, (With<InstanceNode>, ChangedInstance)>,
    Query<'w, 's, &'static Shows, With<InstanceNode>>,
    Query<
        'w,
        's,
        (
            &'static PaneId,
            &'static Terminal,
            Option<Ref<'static, Process>>,
        ),
    >,
    Query<'w, 's, Ref<'static, RootOrder>>,
    Query<'w, 's, (&'static NodeId, Ref<'static, Name>), With<TemplateRoot>>,
    Query<
        'w,
        's,
        (
            Entity,
            Has<ExactTarget>,
            Option<&'static Targets>,
            &'static Viewing,
        ),
        (With<Viewer>, Added<Detaching>),
    >,
    RemovedComponents<'w, 's, Viewer>,
    RemovedComponents<'w, 's, Showing>,
    RemovedComponents<'w, 's, Targets>,
    Res<'w, AppTypeRegistry>,
    Option<Res<'w, State<ServerMode>>>,
);

/// One viewer's frame plus the baseline writes it implies; buffers persist across updates.
struct Outgoing {
    viewer: Entity,
    frame: SceneFrame,
    revision: u64,
    /// The instance entities in this frame's view; swapped into `Projected::known`.
    current: EntityHashSet,
    panes: Vec<(Entity, PaneBaseline)>,
}

impl Default for Outgoing {
    fn default() -> Self {
        Self {
            viewer: Entity::PLACEHOLDER,
            frame: SceneFrame::default(),
            revision: 0,
            current: EntityHashSet::default(),
            panes: Vec::new(),
        }
    }
}

#[derive(Default)]
pub(crate) struct Scratch {
    outgoing: Vec<Outgoing>,
    /// `Outgoing` values not used this update, kept for their buffers.
    spare: Vec<Outgoing>,
    extract: Vec<Entity>,
    closing: Vec<(Entity, ByeReason)>,
    gone: Vec<Entity>,
    /// Viewers whose `Showing` or `Targets` was removed since the last projection: a removal
    /// has no change tick, so it is the one focus change a `Ref` cannot report.
    unfocused: EntityHashSet,
}

pub(crate) fn project(
    world: &mut World,
    state: &mut SystemState<Params<'static, 'static>>,
    mut scratch: Local<Scratch>,
) {
    let scratch = &mut *scratch;
    collect(&*world, state, scratch);
    commit(world, scratch);
}

fn collect(
    world: &World,
    state: &mut SystemState<Params<'static, 'static>>,
    scratch: &mut Scratch,
) {
    let Ok((
        viewers,
        instance_roots,
        children,
        changed,
        shows,
        terminals,
        root_orders,
        root_names,
        detaching,
        mut removed,
        mut unshown,
        mut untargeted,
        registry,
        mode,
    )) = state.get(world)
    else {
        return;
    };
    scratch.unfocused.clear();
    scratch.unfocused.extend(unshown.read());
    scratch.unfocused.extend(untargeted.read());
    let registry = registry.read();
    let shutting_down = mode.is_some_and(|mode| *mode.get() == ServerMode::ShuttingDown);

    for (viewer, camera, viewing, showing, targets, baseline, projected, notice) in &viewers {
        let mut out = scratch.spare.pop().unwrap_or_default();
        out.viewer = viewer;
        out.current.clear();
        out.panes.clear();
        let full = baseline.scene_revision == 0;

        // This viewer's instance tree: the root whose camera is the viewer's, plus descendants.
        let root = instance_roots
            .iter()
            .find(|(_, target)| target.entity() == camera.0)
            .map(|(entity, _)| entity);
        if let Some(root) = root {
            out.current.insert(root);
            out.current
                .extend(children.iter_descendants::<Children>(root));
        }

        // Scene: new or changed instance entities, and the pane entity behind a changed leaf
        // (only its `PaneId` passes the allowlist) so the viewer can key terminal deltas.
        scratch.extract.clear();
        for &entity in &out.current {
            if full || !projected.known.contains(&entity) || changed.contains(entity) {
                scratch.extract.push(entity);
                if let Ok(leaf) = shows.get(entity)
                    && world.get_entity(leaf.0).is_ok()
                {
                    scratch.extract.push(leaf.0);
                }
            }
        }
        out.frame.scene = if scratch.extract.is_empty() {
            String::new()
        } else {
            let dynamic = DynamicWorldBuilder::from_world(world, &registry)
                .deny_all()
                .allow_component::<Node>()
                .allow_component::<ComputedNode>()
                .allow_component::<ZIndex>()
                .allow_component::<BackgroundColor>()
                .allow_component::<BorderColor>()
                .allow_component::<Name>()
                .allow_component::<PaneId>()
                .allow_component::<NodeId>()
                .allow_component::<Zoomed>()
                .allow_component::<ChildOf>()
                .allow_component::<Children>()
                .allow_component::<Shows>()
                .allow_component::<InstanceNode>()
                .allow_component::<Text>()
                .allow_component::<Surface>()
                .allow_component::<ScrollPosition>()
                .extract_entities(scratch.extract.iter().copied())
                .remove_empty_entities()
                .build();
            match dynamic.serialize(&registry) {
                Ok(ron) => ron,
                Err(error) => {
                    bevy_log::warn!("scene for viewer {viewer} failed to serialize: {error}");
                    String::new()
                }
            }
        };
        out.frame.despawned.clear();
        out.frame.despawned.extend(
            projected
                .known
                .iter()
                .filter(|entity| !out.current.contains(*entity))
                .map(|entity| entity.to_bits()),
        );

        // Terminals: one delta per shown pane whose emulator moved past the baseline; a
        // process transition without new rows still yields an (empty) delta so the viewer
        // learns the exit.
        out.frame.terminals.clear();
        for &leaf in &out.current {
            let Ok(shown) = shows.get(leaf) else {
                continue;
            };
            let Ok((pane_id, terminal, process)) = terminals.get(shown.0) else {
                continue;
            };
            let pane_baseline = baseline.panes.get(&shown.0).copied();
            let process_changed = full || process.as_ref().is_some_and(Ref::is_changed);
            // The frame owns its rows: it crosses the effect boundary into the adapter.
            let mut delta = TerminalDelta {
                pane: *pane_id,
                seq: 0,
                rows: 0,
                cols: 0,
                full: false,
                lines: Vec::new(),
                cursor: Default::default(),
                modes: Default::default(),
                title: None,
                clipboard: None,
                process: summary(process.as_deref().copied()),
            };
            if terminal.write_delta(pane_baseline, &mut delta) {
                out.frame.terminals.push(delta);
            } else if process_changed {
                delta.seq = terminal.seq();
                delta.rows = terminal.rows();
                delta.cols = terminal.cols();
                delta.cursor = terminal.cursor();
                delta.modes = terminal.modes();
                out.frame.terminals.push(delta);
            }
            out.panes.push((
                shown.0,
                PaneBaseline {
                    seq: terminal.seq(),
                    rows: terminal.rows(),
                    cols: terminal.cols(),
                },
            ));
        }

        // Roots for the tab strip when order or a name changed.
        out.frame.roots = root_orders.get(viewing.0).ok().and_then(|order| {
            let names_changed = order.0.iter().any(|root| {
                root_names
                    .get(*root)
                    .is_ok_and(|(_, name)| name.is_changed())
            });
            (full || order.is_changed() || names_changed).then(|| {
                order
                    .0
                    .iter()
                    .filter_map(|root| root_names.get(*root).ok())
                    .map(|(node, name)| RootEntry {
                        node: *node,
                        name: name.as_str().to_owned(),
                    })
                    .collect()
            })
        });

        out.frame.target = targets
            .as_ref()
            .and_then(|t| world.get::<PaneId>(t.0).copied());
        out.frame.showing = showing
            .as_ref()
            .and_then(|s| world.get::<NodeId>(s.0).copied());
        let focus_changed = full
            || showing.is_some_and(|showing| showing.is_changed())
            || targets.is_some_and(|targets| targets.is_changed())
            || scratch.unfocused.contains(&viewer);
        out.frame.notice = notice
            .filter(|notice| full || notice.is_changed())
            .map(|notice| notice.text.clone());

        let carries = !out.frame.scene.is_empty()
            || !out.frame.despawned.is_empty()
            || out.frame.roots.is_some()
            || !out.frame.terminals.is_empty()
            || focus_changed
            || out.frame.notice.is_some();
        if carries {
            out.revision = baseline.scene_revision + 1;
            out.frame.revision = out.revision;
            out.frame.full = full;
            scratch.outgoing.push(out);
        } else {
            scratch.spare.push(out);
        }
    }

    scratch.closing.clear();
    for (viewer, exact, targets, viewing) in &detaching {
        let reason = if shutting_down {
            ByeReason::ServerShutdown
        } else if world.get::<Retiring>(viewing.0).is_some() {
            ByeReason::WorkspaceRetired
        } else if exact
            && targets.is_none_or(|t| {
                world
                    .get::<Process>(t.0)
                    .is_none_or(|p| matches!(p, Process::Exited { .. }))
            })
        {
            ByeReason::ExactTargetLost
        } else {
            ByeReason::Detached
        };
        scratch.closing.push((viewer, reason));
    }
    scratch.gone.clear();
    scratch.gone.extend(removed.read());
}

fn commit(world: &mut World, scratch: &mut Scratch) {
    for mut out in scratch.outgoing.drain(..) {
        let input_state = InputState::capture(world, out.viewer);
        let input_changed = out.frame.full
            || !out.frame.scene.is_empty()
            || !out.frame.despawned.is_empty()
            || out.frame.roots.is_some()
            || world
                .get::<Projected>(out.viewer)
                .is_none_or(|projected| projected.input_state != input_state);
        let Ok(mut viewer) = world.get_entity_mut(out.viewer) else {
            scratch.spare.push(out);
            continue;
        };
        if let Some(mut baseline) = viewer.get_mut::<ProjectionBaseline>() {
            baseline.scene_revision = out.revision;
            baseline.panes.clear();
            baseline.panes.extend(out.panes.iter().copied());
        }
        if let Some(mut projected) = viewer.get_mut::<Projected>() {
            if input_changed {
                projected.input_revision = out.revision;
            }
            projected.input_state = input_state;
            core::mem::swap(&mut projected.known, &mut out.current);
        }
        let frame = core::mem::take(&mut out.frame);
        world.write_message(Effect::SendFrame {
            viewer: out.viewer,
            frame: ServerFrame::Scene(frame),
        });
        scratch.spare.push(out);
    }
    for (viewer, reason) in scratch.closing.drain(..) {
        world.write_message(Effect::SendFrame {
            viewer,
            frame: ServerFrame::Bye {
                reason,
                message: String::new(),
            },
        });
        world.write_message(Effect::CloseViewer { viewer });
    }
    for viewer in scratch.gone.drain(..) {
        world.write_message(Effect::CloseViewer { viewer });
    }
}

fn summary(process: Option<Process>) -> ProcessSummary {
    match process {
        None | Some(Process::Starting) => ProcessSummary::Starting,
        Some(Process::Live { .. } | Process::Eof { .. } | Process::Terminating { .. }) => {
            ProcessSummary::Live
        }
        Some(Process::Exited { code }) => ProcessSummary::Exited { code },
    }
}
