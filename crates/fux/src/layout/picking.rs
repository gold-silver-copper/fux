//! The fux picking backend (prompt 3.4): one `PointerId::Custom` per viewer, located in viewport
//! cells on `NormalizedRenderTarget::None`; hits are that viewer's instance nodes containing the
//! cell, deepest in the UI stack first. Modelled on `bevy_picking::window::update_window_hits`;
//! like it, no `normal` is reported.

use bevy_camera::NormalizedRenderTarget;
use bevy_ecs::prelude::*;
use bevy_picking::backend::{HitData, PointerHits};
use bevy_picking::pointer::{Location, PointerId, PointerLocation};
use bevy_ui::{
    CalculatedClip, ComputedNode, ComputedStackIndex, ComputedUiTargetCamera, UiGlobalTransform,
};

use crate::model::{InstanceNode, Viewer, ViewerCamera, ViewerPointer};

/// Layer of fux hits; nothing else emits hits in this app.
const ORDER: f32 = 10.0;

pub fn cell_backend(
    pointers: Query<(Entity, &PointerId, &PointerLocation)>,
    viewers: Query<(&ViewerPointer, &ViewerCamera), With<Viewer>>,
    nodes: Query<
        (
            Entity,
            &ComputedNode,
            &UiGlobalTransform,
            &ComputedStackIndex,
            &ComputedUiTargetCamera,
            Option<&CalculatedClip>,
        ),
        With<InstanceNode>,
    >,
    mut hits: MessageWriter<PointerHits>,
) {
    for (pointer, id, location) in &pointers {
        let Some(Location {
            target: NormalizedRenderTarget::None { .. },
            position,
        }) = location.location
        else {
            continue;
        };
        let Some((_, camera)) = viewers.iter().find(|(p, _)| p.0 == pointer) else {
            continue;
        };
        let camera = camera.0;
        // One allocation per located pointer per update: `PointerHits` owns its picks.
        let picks: Vec<(Entity, HitData)> = nodes
            .iter()
            .filter(|(_, computed, transform, _, target, clip)| {
                target.get() == Some(camera)
                    && clip.is_none_or(|clip| clip.clip.contains(position))
                    && computed.contains_point(**transform, position)
            })
            .map(|(entity, _, _, stack, _, _)| {
                (
                    entity,
                    HitData::new(camera, -(stack.0 as f32), Some(position.extend(0.0)), None),
                )
            })
            .collect();
        if !picks.is_empty() {
            hits.write(PointerHits::new(*id, picks, ORDER));
        }
    }
}
