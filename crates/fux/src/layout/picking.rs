//! The fux picking backend (prompt 3.4): one `PointerId::Custom` per viewer, located in viewport
//! cells on `NormalizedRenderTarget::None`; hits are that viewer's instance nodes containing the
//! cell, deepest in the UI stack first, each tagged with the [`HitRegion`] the cell falls in.
//! Modelled on `bevy_picking::window::update_window_hits` for the pointer side and on
//! `bevy_ui::picking_backend::ui_picking` for the geometry (`ComputedNode::contains_point`,
//! `UiGlobalTransform`, `ComputedStackIndex` as the `UiStack` order, `CalculatedClip` as the
//! precomputed `clip_check_recursive`); like both, no `normal` is reported.
//!
//! `bevy_picking`'s hover map keeps only the topmost blocking hit per pointer, so the observers
//! in [`crate::pointer`] see one original target per event and its ancestors by bubbling.

use std::sync::Arc;

use bevy_camera::NormalizedRenderTarget;
use bevy_ecs::prelude::*;
use bevy_math::Vec2;
use bevy_picking::backend::{HitData, HitDataExtra, PointerHits};
use bevy_picking::pointer::{Location, PointerId, PointerLocation};
use bevy_ui::{
    CalculatedClip, ComputedNode, ComputedStackIndex, ComputedUiTargetCamera, UiGlobalTransform,
};

use crate::model::{InstanceNode, Viewer, ViewerCamera, ViewerPointer};

pub use super::Side;

/// Layer of fux hits; nothing else emits hits in this app.
const ORDER: f32 = 10.0;

/// Where inside a node's border box a hit cell lies, from `ComputedNode.border`. Reported in
/// `HitData::extra` (read with `hit.extra_as::<HitRegion>()`): a cell on a border edge is
/// `Border(side)`; on a corner the side whose edge is nearer the cell wins, ties going to
/// `Left` over `Right`, `Top` over `Bottom`, and `Left`/`Top` over the perpendicular side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitRegion {
    Content,
    Border(Side),
}

impl HitRegion {
    /// Classifies `point` (viewport cells) against a laid-out node. `None` when the transform
    /// cannot be inverted (a degenerate node).
    pub fn classify(
        computed: &ComputedNode,
        transform: &UiGlobalTransform,
        point: Vec2,
    ) -> Option<Self> {
        let local = transform.try_inverse()?.transform_point2(point);
        let half = computed.size * 0.5;
        // Distance from each edge, inward positive.
        let from_left = local.x + half.x;
        let from_right = half.x - local.x;
        let from_top = local.y + half.y;
        let from_bottom = half.y - local.y;
        let border = computed.border;
        let candidates = [
            (Side::Left, from_left, border.min_inset.x),
            (Side::Top, from_top, border.min_inset.y),
            (Side::Right, from_right, border.max_inset.x),
            (Side::Bottom, from_bottom, border.max_inset.y),
        ];
        let mut best: Option<(Side, f32)> = None;
        for (side, distance, width) in candidates {
            if distance < width && best.is_none_or(|(_, d)| distance < d) {
                best = Some((side, distance));
            }
        }
        Some(best.map_or(Self::Content, |(side, _)| Self::Border(side)))
    }
}

/// The five possible regions as shared `HitData::extra` payloads, so a hit never allocates.
pub struct RegionExtras {
    content: Arc<dyn HitDataExtra>,
    left: Arc<dyn HitDataExtra>,
    right: Arc<dyn HitDataExtra>,
    top: Arc<dyn HitDataExtra>,
    bottom: Arc<dyn HitDataExtra>,
}

impl Default for RegionExtras {
    fn default() -> Self {
        Self {
            content: Arc::new(HitRegion::Content),
            left: Arc::new(HitRegion::Border(Side::Left)),
            right: Arc::new(HitRegion::Border(Side::Right)),
            top: Arc::new(HitRegion::Border(Side::Top)),
            bottom: Arc::new(HitRegion::Border(Side::Bottom)),
        }
    }
}

impl RegionExtras {
    fn get(&self, region: HitRegion) -> Arc<dyn HitDataExtra> {
        Arc::clone(match region {
            HitRegion::Content => &self.content,
            HitRegion::Border(Side::Left) => &self.left,
            HitRegion::Border(Side::Right) => &self.right,
            HitRegion::Border(Side::Top) => &self.top,
            HitRegion::Border(Side::Bottom) => &self.bottom,
        })
    }
}

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
    extras: Local<RegionExtras>,
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
                    && computed.size != Vec2::ZERO
                    && clip.is_none_or(|clip| clip.clip.contains(position))
                    && computed.contains_point(**transform, position)
            })
            .filter_map(|(entity, computed, transform, stack, _, _)| {
                let region = HitRegion::classify(computed, transform, position)?;
                Some((
                    entity,
                    HitData {
                        camera,
                        depth: -(stack.0 as f32),
                        position: Some(position.extend(0.0)),
                        normal: None,
                        extra: Some(extras.get(region)),
                    },
                ))
            })
            .collect();
        if !picks.is_empty() {
            hits.write(PointerHits::new(*id, picks, ORDER));
        }
    }
}
