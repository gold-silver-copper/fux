#[cfg(test)]
mod tests;

use bevy_app::{App, TaskPoolPlugin};
use bevy_asset::{AssetApp, AssetPlugin};
use bevy_camera::NormalizedRenderTarget;
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
use bevy_math::{URect, UVec2, Vec2};
use bevy_mesh::{Mesh, skinning::SkinnedMeshInverseBindposes};
use bevy_picking::{
    backend::PointerHits,
    events::PointerState,
    hover::HoverMap,
    pointer::{Location, PointerId, PointerInput, PointerLocation, PointerMap},
};
use bevy_reflect::FromReflect;
use bevy_text::TextPlugin;
use bevy_time::{Real, Time};
use bevy_ui::{FocusPolicy, UiPlugin, UiStack, picking_backend::ui_picking, prelude::*};
use bevy_window::{PrimaryWindow, Window, WindowRef};
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

#[derive(Component)]
pub struct Presentation {
    world: World,
    pub(crate) last: String,
    pub(crate) clipboard: Vec<String>,
    pub(crate) next_paint: std::time::Instant,
    pub(crate) paint_wake_pending: bool,
    window: Entity,
    camera: Entity,
    container: Entity,
    /// One synthetic mouse pointer, moved to the requested cell before the UI
    /// picking backend is run on demand.
    pointer: Entity,
    source_to_local: EntityHashMap<Entity>,
    local_to_source: EntityHashMap<Entity>,
    scene_key: Option<(u32, Entity, Option<Entity>, Option<Entity>)>,
    viewport: UVec2,
    rects: Vec<PaneRect>,
    separators: BTreeMap<(u16, u16), u8>,
    chrome: Vec<(Entity, URect)>,
    chrome_nodes: Vec<Entity>,
}

impl Presentation {
    fn build(registry: AppTypeRegistry) -> (App, Entity, Entity, Entity, Entity) {
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
            // This adapter runs that backend on demand for one synthetic pointer,
            // so no continuous picking pipeline or event synthesizer is necessary.
            .init_resource::<HoverMap>()
            .init_resource::<PointerState>()
            // `PointerId`'s insert hook records the pointer in `PointerMap`.
            .init_resource::<PointerMap>()
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
        let pointer = app
            .world_mut()
            .spawn((PointerId::Mouse, PointerLocation::default()))
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
        (app, window, camera, container, pointer)
    }

    pub fn new(registry: AppTypeRegistry) -> Self {
        let (mut app, window, camera, container, pointer) = Self::build(registry);
        Self {
            world: std::mem::take(app.world_mut()),
            last: String::new(),
            clipboard: Vec::new(),
            next_paint: std::time::Instant::now(),
            paint_wake_pending: false,
            window,
            camera,
            container,
            pointer,
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
        (tab, focus): (Option<Entity>, Option<Entity>),
    ) -> Result<(), String> {
        let zoom = focus.filter(|_| viewer.zoom);
        let key = (revision, root, zoom, tab);
        let rebuild = self.scene_key != Some(key);
        if rebuild {
            self.scene_key = None;
            self.rects.clear();
            let world = &mut self.world;
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
                    .insert(FocusPolicy::Block)
                    .insert_if_new(TabIndex(0));
            }
            // Hide inactive branches in this inert projection only. Native focus
            // navigation does not inspect Display, so remove their tab indices too.
            let inactive: Vec<_> = world
                .query_filtered::<Entity, With<Tab>>()
                .iter(world)
                .filter(|local| self.local_to_source.get(local).copied() != tab)
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
            let world = &mut self.world;
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
        let desired = focus
            .and_then(|source| self.source_to_local.get(&source).copied())
            .filter(|local| self.world.get::<PaneView>(*local).is_some());
        let world = &mut self.world;
        let mut focus = world.resource_mut::<InputFocus>();
        let focus_changed = focus.get() != desired;
        if focus_changed {
            match desired {
                Some(local) => focus.set(local, FocusCause::Navigated),
                None => focus.clear(),
            }
        }
        if rebuild || resized {
            world
                .get_mut::<Node>(self.container)
                .ok_or("presentation container is missing")?
                .height = Val::Px(viewport.y as f32);
            self.world.run_schedule(bevy_app::Main);
            self.world.clear_trackers();
            self.collect_rects();
        } else if focus_changed {
            self.world
                .run_system_cached(process_recorded_focus_changes)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn chrome(&mut self, hits: Vec<(Entity, URect)>) {
        if self.chrome == hits {
            return;
        }
        let world = &mut self.world;
        for entity in self.chrome_nodes.drain(..) {
            world.despawn(entity);
        }
        for &(target, bounds) in &hits {
            let entity = world
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(bounds.min.x as f32),
                        top: Val::Px(bounds.min.y as f32),
                        width: Val::Px(bounds.width() as f32),
                        height: Val::Px(bounds.height() as f32),
                        ..Default::default()
                    },
                    ChromeTarget(target),
                    FocusPolicy::Block,
                    UiTargetCamera(self.camera),
                ))
                .id();
            self.chrome_nodes.push(entity);
        }
        self.chrome = hits;
        self.world.run_schedule(bevy_app::Main);
        self.world.clear_trackers();
        self.collect_rects();
    }

    pub fn neighbor(&self, leaf: Entity, direction: crate::protocol::Direction) -> Option<Entity> {
        crate::frame::directional_neighbor(&self.rects, leaf, direction)
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
        let world = &self.world;
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
            let clamp = |v: Vec2| {
                UVec2::new(
                    v.x.round().clamp(0.0, self.viewport.x as f32) as u32,
                    v.y.round().clamp(0.0, self.viewport.y as f32) as u32,
                )
            };
            let rect = URect::from_corners(clamp(min), clamp(max));
            if !rect.is_empty() {
                self.rects.push(PaneRect {
                    leaf: *leaf,
                    pane: *pane,
                    rect,
                });
            }
        }
        // Only actual one-cell gaps between visible siblings of a Split are chrome.
        // User-scene margins, padding, and arbitrary empty grid areas remain blank.
        let viewport = self.viewport;
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
                min.x.round().clamp(0.0, viewport.x as f32) as u16,
                min.y.round().clamp(0.0, viewport.y as f32) as u16,
                max.x.round().clamp(0.0, viewport.x as f32) as u16,
                max.y.round().clamp(0.0, viewport.y as f32) as u16,
            ))
        };
        // Flex layout rounds every edge on its own, and a nested container's
        // leaves can end one cell past their container's rounded box. The
        // edge that matters is where the leaves actually are, so a sibling's
        // extent on the container's axis is taken from the leaf rectangles
        // beneath it, and only its cross-axis range from its own box.
        let extent = |child: Entity, rects: &[PaneRect], vertical: bool| -> Option<(u16, u16)> {
            let mut span: Option<(u32, u32)> = None;
            for local in crate::navigation::leaves(world, child) {
                let Some(source) = self.local_to_source.get(&local) else {
                    continue;
                };
                let Some(r) = rects.iter().find(|r| r.leaf == *source) else {
                    continue;
                };
                let (start, end) = if vertical {
                    (r.rect.min.y, r.rect.max.y)
                } else {
                    (r.rect.min.x, r.rect.max.x)
                };
                span = Some(span.map_or((start, end), |(s, e)| (s.min(start), e.max(end))));
            }
            span.map(|(s, e)| (s as u16, e as u16))
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
            let kids: Vec<_> = children
                .iter()
                .filter_map(|child| {
                    let b = bounds(child)?;
                    let columns = extent(child, &self.rects, false).unwrap_or((b.0, b.2));
                    let rows = extent(child, &self.rects, true).unwrap_or((b.1, b.3));
                    Some((b, columns, rows))
                })
                .collect();
            for (a, a_columns, a_rows) in &kids {
                for (b, b_columns, b_rows) in &kids {
                    // Two siblings may touch with no gap cell, or sit two cells
                    // apart, once their edges are rounded. The documented
                    // separator must still exist: a missing gap is carved out
                    // of the later sibling's leading cells, and a surplus cell
                    // is given to the earlier sibling's trailing edge, in every
                    // leaf rectangle beneath them.
                    if node.column_gap == Val::Px(1.0) && a.1.max(b.1) < a.3.min(b.3) {
                        let (a_end, b_start) = (a_columns.1, b_columns.0);
                        // Touching siblings give up one cell for the gap: the
                        // later one unless that would take its second cell
                        // while the earlier one has more than two.
                        let gap = if a_end.checked_add(1) == Some(b_start) {
                            Some(a_end)
                        } else if a_end == b_start
                            && b_columns.1 <= b_start + 2
                            && a_end > a_columns.0 + 2
                        {
                            for r in &mut self.rects {
                                if r.rect.max.x == u32::from(a_end)
                                    && r.rect.min.y >= u32::from(a.1)
                                    && r.rect.max.y <= u32::from(a.3)
                                    && r.rect.width() > 1
                                {
                                    r.rect.max.x -= 1;
                                }
                            }
                            Some(a_end - 1)
                        } else if a_end == b_start && b_columns.1 > b_start + 1 {
                            for r in &mut self.rects {
                                if r.rect.min.x == u32::from(b_start)
                                    && r.rect.min.y >= u32::from(b.1)
                                    && r.rect.max.y <= u32::from(b.3)
                                    && r.rect.width() > 1
                                {
                                    r.rect.min.x += 1;
                                }
                            }
                            Some(a_end)
                        } else if a_end.checked_add(2) == Some(b_start) {
                            for r in &mut self.rects {
                                if r.rect.max.x == u32::from(a_end)
                                    && r.rect.min.y >= u32::from(a.1)
                                    && r.rect.max.y <= u32::from(a.3)
                                {
                                    r.rect.max.x += 1;
                                }
                            }
                            Some(a_end + 1)
                        } else {
                            None
                        };
                        if let Some(x) = gap {
                            for y in a.1.max(b.1)..a.3.min(b.3) {
                                self.separators.insert((x, y), 3);
                            }
                        }
                    }
                    if node.row_gap == Val::Px(1.0) && a.0.max(b.0) < a.2.min(b.2) {
                        let (a_end, b_start) = (a_rows.1, b_rows.0);
                        let gap = if a_end.checked_add(1) == Some(b_start) {
                            Some(a_end)
                        } else if a_end == b_start
                            && b_rows.1 <= b_start + 2
                            && a_end > a_rows.0 + 2
                        {
                            for r in &mut self.rects {
                                if r.rect.max.y == u32::from(a_end)
                                    && r.rect.min.x >= u32::from(a.0)
                                    && r.rect.max.x <= u32::from(a.2)
                                    && r.rect.height() > 1
                                {
                                    r.rect.max.y -= 1;
                                }
                            }
                            Some(a_end - 1)
                        } else if a_end == b_start && b_rows.1 > b_start + 1 {
                            for r in &mut self.rects {
                                if r.rect.min.y == u32::from(b_start)
                                    && r.rect.min.x >= u32::from(b.0)
                                    && r.rect.max.x <= u32::from(b.2)
                                    && r.rect.height() > 1
                                {
                                    r.rect.min.y += 1;
                                }
                            }
                            Some(a_end)
                        } else if a_end.checked_add(2) == Some(b_start) {
                            for r in &mut self.rects {
                                if r.rect.max.y == u32::from(a_end)
                                    && r.rect.min.x >= u32::from(a.0)
                                    && r.rect.max.x <= u32::from(a.2)
                                {
                                    r.rect.max.y += 1;
                                }
                            }
                            Some(a_end + 1)
                        } else {
                            None
                        };
                        if let Some(y) = gap {
                            for x in a.0.max(b.0)..a.2.min(b.2) {
                                self.separators.insert((x, y), 12);
                            }
                        }
                    }
                }
            }
        }
        // Overlapping custom scenes may put a pane over a split gap.
        self.separators
            .retain(|&(x, y), _| !self.rects.iter().any(|r| r.covers(x, y)));
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
            self.world.entity_mut(local).remove::<TabIndex>();
        }
        for rect in &self.rects {
            if let Some(local) = self.source_to_local.get(&rect.leaf) {
                self.world.entity_mut(*local).insert_if_new(TabIndex(0));
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
            // A separator touches the focused pane when it lies on its one-cell rim.
            let touches = focused.is_some_and(|r| {
                r.rect
                    .inflate(1)
                    .contains(UVec2::new(u32::from(x), u32::from(y)))
            });
            let style = if touches { "\x1b[0;1m" } else { "\x1b[0;90m" };
            at(out, x, y, format_args!("{style}{glyph}\x1b[0m"));
        }
    }

    pub fn focus(&self) -> Option<Entity> {
        self.world
            .resource::<InputFocus>()
            .get()
            .and_then(|local| self.local_to_source.get(&local).copied())
    }

    pub fn focus_step(&mut self, previous: bool) -> Option<Entity> {
        self.world
            .run_system_cached_with(advance_focus, previous)
            .ok()?;
        self.world
            .run_system_cached(process_recorded_focus_changes)
            .ok()?;
        self.focus()
    }

    /// Coordinates are terminal cells, including the global chrome row. The
    /// virtual window uses cells as physical pixels; native UI does the picking.
    pub fn pointer(&mut self, x: u16, y: u16, pressed: bool) -> Option<Entity> {
        let world = &mut self.world;
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
        // `ui_picking` reads the pointer's own location rather than the window's
        // cursor, and writes its hits as messages. It walks `UiStack` from the
        // top down and stops at the first node that blocks, which is every node
        // here, so the first pick is the topmost one.
        let target =
            NormalizedRenderTarget::Window(WindowRef::Primary.normalize(Some(self.window))?);
        let position = Vec2::new(f32::from(x) + 0.5, f32::from(y) + 0.5);
        let inside = u32::from(x) < self.viewport.x && u32::from(y) <= self.viewport.y;
        world.entity_mut(self.pointer).insert(PointerLocation {
            location: inside.then(|| Location { target, position }),
        });
        world.run_system_cached(ui_picking).ok()?;
        let hits: Vec<PointerHits> = world
            .resource_mut::<Messages<PointerHits>>()
            .drain()
            .collect();
        let local = hits
            .iter()
            .flat_map(|hit| hit.picks.iter())
            .map(|(entity, _)| *entity)
            .find(|entity| {
                world.get::<PaneView>(*entity).is_some()
                    || world.get::<ChromeTarget>(*entity).is_some()
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
