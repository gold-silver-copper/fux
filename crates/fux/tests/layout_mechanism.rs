//! Prompt section 5, first bullet: a root under a `UiTargetCamera` whose target info is 80x24
//! yields an 80x24 `ComputedNode` with no window and no renderer.

use bevy_app::prelude::*;
use bevy_asset::AssetApp;
use bevy_camera::{Camera, RenderTarget, RenderTargetInfo};
use bevy_ecs::prelude::*;
use bevy_math::UVec2;
use bevy_ui::prelude::*;
use bevy_ui::{UiGlobalTransform, UiPlugin};

fn headless_app() -> App {
    let mut app = App::new();
    app.add_plugins((
        bevy_app::TaskPoolPlugin::default(),
        bevy_time::TimePlugin,
        bevy_input::InputPlugin,
        bevy_transform::TransformPlugin,
        bevy_asset::AssetPlugin::default(),
        bevy_picking::PickingPlugin,
        UiPlugin,
    ));
    // `UiPlugin` registers text and image systems whose parameters must resolve even though
    // fux never spawns a text or image node.
    app.init_asset::<bevy_image::Image>()
        .init_asset::<bevy_image::TextureAtlasLayout>()
        .init_asset::<bevy_text::Font>()
        .init_resource::<bevy_text::FontAtlasSet>()
        .init_resource::<bevy_text::TextPipeline>()
        .init_resource::<bevy_text::FontCx>()
        .init_resource::<bevy_text::LayoutCx>()
        .init_resource::<bevy_text::ScaleCx>()
        .init_resource::<bevy_text::TextIterScratch>()
        .insert_resource(bevy_text::RemSize(16.0));
    app
}

fn cell_camera(world: &mut World, cols: u32, rows: u32) -> Entity {
    let mut camera = Camera::default();
    camera.computed.target_info = Some(RenderTargetInfo {
        physical_size: UVec2::new(cols, rows),
        scale_factor: 1.0,
    });
    world
        .spawn((
            camera,
            RenderTarget::None {
                size: UVec2::new(cols, rows),
            },
        ))
        .id()
}

#[test]
fn root_fills_camera_target_in_cells() {
    let mut app = headless_app();
    let camera = cell_camera(app.world_mut(), 80, 24);
    let root = app
        .world_mut()
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                ..Default::default()
            },
            UiTargetCamera(camera),
        ))
        .with_children(|p| {
            p.spawn(Node {
                flex_grow: 1.0,
                ..Default::default()
            });
            p.spawn(Node {
                flex_grow: 1.0,
                min_width: Val::Px(20.0),
                ..Default::default()
            });
        })
        .id();
    app.update();
    let world = app.world_mut();
    let computed = world.get::<ComputedNode>(root).unwrap();
    assert_eq!(computed.size.as_uvec2(), UVec2::new(80, 24));
    let children: Vec<Entity> = world.get::<Children>(root).unwrap().iter().collect();
    let sizes: Vec<(UVec2, UVec2)> = children
        .iter()
        .map(|&c| {
            let n = world.get::<ComputedNode>(c).unwrap();
            let t = world.get::<UiGlobalTransform>(c).unwrap();
            (n.size.as_uvec2(), t.translation.as_uvec2())
        })
        .collect();
    assert_eq!(sizes[0].0, UVec2::new(40, 24));
    assert_eq!(sizes[1].0, UVec2::new(40, 24));
    // UiGlobalTransform translation is the node centre.
    assert_eq!(sizes[0].1, UVec2::new(20, 12));
    assert_eq!(sizes[1].1, UVec2::new(60, 12));
}

#[test]
fn resize_relayouts_next_update() {
    let mut app = headless_app();
    let camera = cell_camera(app.world_mut(), 80, 24);
    let root = app
        .world_mut()
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..Default::default()
            },
            UiTargetCamera(camera),
        ))
        .id();
    app.update();
    app.world_mut()
        .get_mut::<Camera>(camera)
        .unwrap()
        .computed
        .target_info = Some(RenderTargetInfo {
        physical_size: UVec2::new(120, 40),
        scale_factor: 1.0,
    });
    app.update();
    let computed = app.world().get::<ComputedNode>(root).unwrap();
    assert_eq!(computed.size.as_uvec2(), UVec2::new(120, 40));
}
