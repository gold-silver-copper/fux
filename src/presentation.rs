use bevy_app::{App, TaskPoolPlugin};
use bevy_asset::{AssetApp, AssetPlugin};
use bevy_camera::{
    Camera, ComputedCameraValues, RenderTargetInfo,
    visibility::{InheritedVisibility, Visibility, VisibilityPlugin},
};
use bevy_ecs::{entity::EntityHashMap, prelude::*};
use bevy_image::{ImagePlugin, TextureAtlasLayout};
use bevy_input::{ButtonInput, mouse::MouseButton, touch::Touches};
use bevy_input_focus::{
    FocusCause, InputFocus, InputFocusPlugin, process_recorded_focus_changes,
    tab_navigation::{NavAction, TabGroup, TabIndex, TabNavigation},
};
use bevy_math::{UVec2, Vec2};
use bevy_mesh::{Mesh, skinning::SkinnedMeshInverseBindposes};
use bevy_picking::{
    backend::PointerHits, events::PointerState, hover::HoverMap, pointer::PointerInput,
};
use bevy_reflect::FromReflect;
use bevy_text::TextPlugin;
use bevy_time::{Real, Time};
use bevy_ui::{FocusPolicy, UiPlugin, UiStack, prelude::*, ui_focus_system};
use bevy_window::{PrimaryWindow, Window};
use bevy_world_serialization::DynamicWorld;

use crate::{
    chrome::at,
    model::{PaneView, Split, Tab, Viewer},
    protocol::PaneRect,
};
use std::collections::BTreeMap;

/// Registers ordinary UI scene types, not a scene component allowlist. Additional
/// registered components are instantiated unchanged by DynamicWorld.
pub fn register_types(app: &mut App) {
    app.register_type::<Node>()
        .register_type::<ChildOf>()
        .register_type::<Children>()
        .register_type::<UiTransform>()
        .register_type::<Visibility>()
        .register_type::<BackgroundColor>()
        .register_type::<BorderColor>()
        .register_type::<BorderRadius>()
        .register_type::<Outline>()
        .register_type::<BoxShadow>()
        .register_type::<ScrollPosition>()
        .register_type::<ZIndex>()
        .register_type::<GlobalZIndex>()
        .register_type::<FocusPolicy>()
        .register_type::<Interaction>()
        .register_type::<LayoutConfig>()
        .register_type::<TabIndex>()
        .register_type::<TabGroup>()
        .register_type::<Text>()
        .register_type::<ImageNode>()
        .register_type::<bevy_text::TextFont>()
        .register_type::<bevy_text::TextColor>()
        .register_type::<bevy_text::TextLayout>();
}

/// An inert per-viewer scene instance: UI, focus and a virtual Window/Camera only.
/// No OS window, renderer, process or event-loop plugin is installed.
#[derive(Component)]
struct ChromeTarget(Entity);

pub struct Presentation {
    app: App,
    window: Entity,
    camera: Entity,
    container: Entity,
    source_to_local: EntityHashMap<Entity>,
    local_to_source: EntityHashMap<Entity>,
    scene_key: Option<(u32, Entity, Option<Entity>, Option<Entity>)>,
    viewport: UVec2,
    rects: Vec<PaneRect>,
    separators: BTreeMap<(u16, u16), u8>,
    chrome: Vec<(Entity, crate::chrome::Bounds)>,
    chrome_nodes: Vec<Entity>,
}

impl Presentation {
    pub fn new(registry: AppTypeRegistry) -> Self {
        let mut app = App::new();
        app.insert_resource(registry);
        register_types(&mut app);
        app.add_plugins(TaskPoolPlugin::default())
            .add_plugins(AssetPlugin {
                watch_for_changes_override: Some(false),
                ..Default::default()
            })
            .init_asset::<Mesh>()
            .init_asset::<SkinnedMeshInverseBindposes>()
            .init_asset::<TextureAtlasLayout>()
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<Touches>()
            .init_resource::<Time<Real>>()
            // UiPlugin includes its picking backend and viewport forwarding systems.
            // This adapter uses ui_focus_system/Interaction, so no pointer event
            // synthesizer or redundant picking pipeline is necessary.
            .init_resource::<HoverMap>()
            .init_resource::<PointerState>()
            .add_message::<PointerInput>()
            .add_message::<PointerHits>()
            .add_plugins((
                ImagePlugin::default(),
                TextPlugin,
                InputFocusPlugin,
                VisibilityPlugin,
                UiPlugin,
            ));
        let window = app
            .world_mut()
            .spawn((Window::default(), PrimaryWindow))
            .id();
        let camera = app
            .world_mut()
            .spawn((
                Camera {
                    computed: ComputedCameraValues {
                        target_info: Some(RenderTargetInfo::default()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                IsDefaultUiCamera,
            ))
            .id();
        let container = app
            .world_mut()
            .spawn((
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..Default::default()
                },
                TabGroup::default(),
                UiTargetCamera(camera),
            ))
            .id();
        app.finish();
        app.cleanup();
        app.update();
        Self {
            app,
            window,
            camera,
            container,
            source_to_local: EntityHashMap::default(),
            local_to_source: EntityHashMap::default(),
            scene_key: None,
            viewport: UVec2::ZERO,
            rects: Vec::new(),
            separators: BTreeMap::new(),
            chrome: Vec::new(),
            chrome_nodes: Vec::new(),
        }
    }

    pub fn sync(
        &mut self,
        scene: &DynamicWorld,
        revision: u32,
        root: Entity,
        viewer: &Viewer,
    ) -> Result<(), String> {
        let zoom = viewer.focus.filter(|_| viewer.zoom);
        let key = (revision, root, zoom, viewer.tab);
        let rebuild = self.scene_key != Some(key);
        if rebuild {
            self.scene_key = None;
            self.rects.clear();
            let world = self.app.world_mut();
            world.resource_mut::<InputFocus>().clear();
            for local in self.source_to_local.values() {
                // Relationships may already have recursively removed children;
                // external scene references intentionally need not be live entities.
                if let Ok(entity) = world.get_entity_mut(*local) {
                    entity.despawn();
                }
            }
            self.source_to_local.clear();
            self.local_to_source.clear();
            // Relationship targets must exist before the scene inserts PaneView.
            // These inert entities represent external live-process references.
            for component in scene.entities.iter().flat_map(|entity| &entity.components) {
                if component.represents::<PaneView>() {
                    let view = PaneView::from_reflect(component.as_partial_reflect())
                        .ok_or("invalid pane relationship")?;
                    self.source_to_local
                        .entry(view.pane)
                        .or_insert_with(|| world.spawn_empty().id());
                }
            }
            scene
                .write_to_world(world, &mut self.source_to_local)
                .map_err(|e| e.to_string())?;
            self.local_to_source.extend(
                self.source_to_local
                    .iter()
                    .map(|(source, local)| (*local, *source)),
            );
            let local_root = *self
                .source_to_local
                .get(&root)
                .ok_or("layout root is absent from scene")?;
            if world.get::<Node>(local_root).is_none() {
                return Err("layout root has no Node".into());
            }
            world.entity_mut(local_root).insert(ChildOf(self.container));
            let mut leaves = world.query_filtered::<Entity, With<PaneView>>();
            let leaves: Vec<_> = leaves.iter(world).collect();
            for leaf in leaves {
                world
                    .entity_mut(leaf)
                    .insert((Interaction::None, FocusPolicy::Block))
                    .insert_if_new(TabIndex(0));
            }
            // Hide inactive branches in this inert projection only. Native focus
            // navigation does not inspect Display, so remove their tab indices too.
            let inactive: Vec<_> = world
                .query_filtered::<Entity, With<Tab>>()
                .iter(world)
                .filter(|local| self.local_to_source.get(local).copied() != viewer.tab)
                .collect();
            for tab in inactive {
                if let Some(mut node) = world.get_mut::<Node>(tab) {
                    node.display = Display::None;
                }
                let descendants = crate::navigation::leaves(world, tab);
                for entity in descendants {
                    world.entity_mut(entity).remove::<TabIndex>();
                }
            }
            if let Some(source) = zoom {
                let local = *self
                    .source_to_local
                    .get(&source)
                    .ok_or("zoom target is absent from scene")?;
                if world.get::<PaneView>(local).is_none() {
                    return Err("zoom target is not a pane view".into());
                }
                // Reparent only this projection. The authoritative hierarchy and
                // the referenced process entity are never changed.
                if local != local_root {
                    world
                        .get_mut::<Node>(local_root)
                        .ok_or("layout root has no Node")?
                        .display = Display::None;
                    world.entity_mut(local).insert(ChildOf(self.container));
                }
                let mut node = world
                    .get_mut::<Node>(local)
                    .ok_or("zoom target has no Node")?;
                node.width = Val::Percent(100.0);
                node.height = Val::Percent(100.0);
                node.min_width = Val::ZERO;
                node.min_height = Val::ZERO;
                node.max_width = Val::Auto;
                node.max_height = Val::Auto;
                node.margin = UiRect::ZERO;
                // Native tab navigation does not inspect Display. The hidden
                // branch must not participate in this presentation's tab group.
                let mut tabs = world.query_filtered::<Entity, With<TabIndex>>();
                let hidden: Vec<_> = tabs.iter(world).filter(|entity| *entity != local).collect();
                for entity in hidden {
                    world.entity_mut(entity).remove::<TabIndex>();
                }
            }
            self.scene_key = Some(key);
        }
        let viewport = UVec2::new(
            u32::from(viewer.cols),
            u32::from(viewer.rows.saturating_sub(1)),
        );
        let resized = self.viewport != viewport;
        if resized {
            self.viewport = viewport;
            let world = self.app.world_mut();
            world
                .get_mut::<Window>(self.window)
                .ok_or("presentation window is missing")?
                .resolution
                .set_physical_resolution(viewport.x, u32::from(viewer.rows));
            world
                .get_mut::<Camera>(self.camera)
                .ok_or("presentation camera is missing")?
                .computed
                .target_info = Some(RenderTargetInfo {
                physical_size: UVec2::new(viewport.x, u32::from(viewer.rows)),
                scale_factor: 1.0,
            });
        }
        let desired = viewer
            .focus
            .and_then(|source| self.source_to_local.get(&source).copied())
            .filter(|local| self.app.world().get::<PaneView>(*local).is_some());
        let world = self.app.world_mut();
        let mut focus = world.resource_mut::<InputFocus>();
        let focus_changed = focus.get() != desired;
        if focus_changed {
            match desired {
                Some(local) => focus.set(local, FocusCause::Navigated),
                None => focus.clear(),
            }
        }
        if rebuild || resized {
            world.get_mut::<Node>(self.container).unwrap().height = Val::Px(viewport.y as f32);
            self.app.update();
            self.collect_rects();
        } else if focus_changed {
            self.app
                .world_mut()
                .run_system_cached(process_recorded_focus_changes)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn chrome(&mut self, hits: Vec<(Entity, crate::chrome::Bounds)>) {
        if self.chrome == hits {
            return;
        }
        let world = self.app.world_mut();
        for entity in self.chrome_nodes.drain(..) {
            world.despawn(entity);
        }
        for &(target, bounds) in &hits {
            let entity = world
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(f32::from(bounds.x)),
                        top: Val::Px(f32::from(bounds.y)),
                        width: Val::Px(f32::from(bounds.width)),
                        height: Val::Px(f32::from(bounds.height)),
                        ..Default::default()
                    },
                    ChromeTarget(target),
                    Interaction::None,
                    FocusPolicy::Block,
                    UiTargetCamera(self.camera),
                ))
                .id();
            self.chrome_nodes.push(entity);
        }
        self.chrome = hits;
        self.app.update();
        self.collect_rects();
    }

    pub fn neighbor(&self, leaf: Entity, direction: &str) -> Option<Entity> {
        crate::server::directional_neighbor(&self.rects, leaf, direction)
    }

    pub fn rects(&self) -> &[PaneRect] {
        &self.rects
    }

    fn collect_rects(&mut self) {
        self.rects.clear();
        self.separators.clear();
        if self.viewport.min_element() == 0 {
            return;
        }
        let world = self.app.world();
        for local in &world.resource::<UiStack>().uinodes {
            let (Some(view), Some(node), Some(transform), Some(visible)) = (
                world.get::<PaneView>(*local),
                world.get::<ComputedNode>(*local),
                world.get::<UiGlobalTransform>(*local),
                world.get::<InheritedVisibility>(*local),
            ) else {
                continue;
            };
            if !visible.get() || node.size().min_element() <= 0.0 {
                continue;
            }
            let (Some(leaf), Some(pane)) = (
                self.local_to_source.get(local),
                self.local_to_source.get(&view.pane),
            ) else {
                continue;
            };
            let min = transform.translation - node.size() * 0.5;
            let max = min + node.size();
            let x = min.x.round().clamp(0.0, self.viewport.x as f32) as u16;
            let y = min.y.round().clamp(0.0, self.viewport.y as f32) as u16;
            let right = max.x.round().clamp(0.0, self.viewport.x as f32) as u16;
            let bottom = max.y.round().clamp(0.0, self.viewport.y as f32) as u16;
            if right > x && bottom > y {
                self.rects.push(PaneRect {
                    leaf: *leaf,
                    pane: *pane,
                    x,
                    y,
                    width: right - x,
                    height: bottom - y,
                });
            }
        }
        // Only actual one-cell gaps between visible siblings of a Split are chrome.
        // User-scene margins, padding, and arbitrary empty grid areas remain blank.
        let bounds = |entity| {
            let node = world.get::<ComputedNode>(entity)?;
            let transform = world.get::<UiGlobalTransform>(entity)?;
            if !world.get::<InheritedVisibility>(entity)?.get() || node.size().min_element() <= 0.0
            {
                return None;
            }
            let min = transform.translation - node.size() * 0.5;
            let max = min + node.size();
            Some((
                min.x.round().clamp(0.0, self.viewport.x as f32) as u16,
                min.y.round().clamp(0.0, self.viewport.y as f32) as u16,
                max.x.round().clamp(0.0, self.viewport.x as f32) as u16,
                max.y.round().clamp(0.0, self.viewport.y as f32) as u16,
            ))
        };
        for entity in &world.resource::<UiStack>().uinodes {
            let (Some(_), Some(node), Some(children)) = (
                world.get::<Split>(*entity),
                world.get::<Node>(*entity),
                world.get::<Children>(*entity),
            ) else {
                continue;
            };
            if bounds(*entity).is_none() {
                continue;
            }
            for a in children.iter().filter_map(&bounds) {
                for b in children.iter().filter_map(&bounds) {
                    if node.column_gap == Val::Px(1.0) && a.2.checked_add(1) == Some(b.0) {
                        for y in a.1.max(b.1)..a.3.min(b.3) {
                            self.separators.insert((a.2, y), 3);
                        }
                    }
                    if node.row_gap == Val::Px(1.0) && a.3.checked_add(1) == Some(b.1) {
                        for x in a.0.max(b.0)..a.2.min(b.2) {
                            self.separators.insert((x, a.3), 12);
                        }
                    }
                }
            }
        }
        // Overlapping custom scenes may put a pane over a split gap.
        self.separators.retain(|&(x, y), _| {
            !self
                .rects
                .iter()
                .any(|r| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
        });
        let hidden: Vec<_> = self
            .source_to_local
            .iter()
            .filter(|(source, local)| {
                world.get::<PaneView>(**local).is_some()
                    && !self.rects.iter().any(|r| r.leaf == **source)
            })
            .map(|(_, local)| *local)
            .collect();
        for local in hidden {
            self.app.world_mut().entity_mut(local).remove::<TabIndex>();
        }
        for rect in &self.rects {
            if let Some(local) = self.source_to_local.get(&rect.leaf) {
                self.app
                    .world_mut()
                    .entity_mut(*local)
                    .insert_if_new(TabIndex(0));
            }
        }
    }

    pub fn paint_separators(&self, out: &mut String, focus: Option<Entity>) {
        let focused = self.rects.iter().find(|r| Some(r.leaf) == focus);
        for (&(x, y), &axis) in &self.separators {
            let has = |x, y| self.separators.contains_key(&(x, y));
            let mask = axis
                | if y > 0 && has(x, y - 1) { 1 } else { 0 }
                | if has(x, y + 1) { 2 } else { 0 }
                | if x > 0 && has(x - 1, y) { 4 } else { 0 }
                | if has(x + 1, y) { 8 } else { 0 };
            let glyph = match mask {
                15 => "┼",
                7 => "┤",
                11 => "├",
                13 => "┴",
                14 => "┬",
                12 => "─",
                _ => "│",
            };
            let touches = focused.is_some_and(|r| {
                x + 1 >= r.x && x <= r.x + r.width && y + 1 >= r.y && y <= r.y + r.height
            });
            let style = if touches { "\x1b[0;1m" } else { "\x1b[0;90m" };
            at(out, x, y, format_args!("{style}{glyph}\x1b[0m"));
        }
    }

    pub fn focus(&self) -> Option<Entity> {
        self.app
            .world()
            .resource::<InputFocus>()
            .get()
            .and_then(|local| self.local_to_source.get(&local).copied())
    }

    pub fn focus_next(&mut self) -> Option<Entity> {
        self.focus_step(false)
    }

    pub fn focus_step(&mut self, previous: bool) -> Option<Entity> {
        self.app
            .world_mut()
            .run_system_cached_with(advance_focus, previous)
            .ok()?;
        self.app
            .world_mut()
            .run_system_cached(process_recorded_focus_changes)
            .ok()?;
        self.focus()
    }

    /// Coordinates are terminal cells, including the global chrome row. The
    /// virtual window uses cells as physical pixels; native UI does the picking.
    pub fn pointer(&mut self, x: u16, y: u16, pressed: bool) -> Option<Entity> {
        let world = self.app.world_mut();
        world.get_mut::<Window>(self.window)?.set_cursor_position(
            (u32::from(x) < self.viewport.x && u32::from(y) <= self.viewport.y)
                .then_some(Vec2::new(f32::from(x) + 0.5, f32::from(y) + 0.5)),
        );
        {
            let mut buttons = world.resource_mut::<ButtonInput<MouseButton>>();
            buttons.clear();
            buttons.release(MouseButton::Left);
            if pressed {
                buttons.press(MouseButton::Left);
            }
        }
        world.run_system_cached(ui_focus_system).ok()?;
        let mut query = world
            .query_filtered::<(Entity, &Interaction), Or<(With<PaneView>, With<ChromeTarget>)>>();
        let local = query.iter(world).find_map(|(entity, interaction)| {
            (*interaction != Interaction::None).then_some(entity)
        });
        if pressed
            && let Some(local) = local
            && world.get::<PaneView>(local).is_some()
        {
            world
                .resource_mut::<InputFocus>()
                .set(local, FocusCause::Pressed);
            world
                .run_system_cached(process_recorded_focus_changes)
                .ok()?;
        }
        world.resource_mut::<ButtonInput<MouseButton>>().clear();
        local.and_then(|local| {
            world
                .get::<ChromeTarget>(local)
                .map(|t| t.0)
                .or_else(|| self.local_to_source.get(&local).copied())
        })
    }
}

fn advance_focus(In(previous): In<bool>, nav: TabNavigation, mut focus: ResMut<InputFocus>) {
    if let Ok(next) = nav.navigate(
        &focus,
        if previous {
            NavAction::Previous
        } else {
            NavAction::Next
        },
    ) {
        focus.set(next, FocusCause::Navigated);
    }
}
