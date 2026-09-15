//! Derived sizes: viewer cameras follow `Viewport`; `PaneSize` is the minimum content size over
//! a pane's `ShownBy` instances (prompt 3.4).

use bevy_camera::{Camera, RenderTarget, RenderTargetInfo};
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::prelude::*;
use bevy_ecs::query::Allow;
use bevy_math::{Rect, UVec2};
use bevy_ui::{CalculatedClip, ComputedNode, Display, Node, UiGlobalTransform};

use crate::model::{
    InstanceNode, MIN_PANE_COLS, MIN_PANE_ROWS, Pane, PaneSize, ShownBy, ViewerCamera, Viewport,
    clamp_dims,
};

/// The camera target for a viewport, in cells. Zero-sized viewports are laid out at the
/// smallest emulator size so nodes keep sane geometry until the viewer reports a real size.
pub(super) fn target_size(viewport: Viewport) -> UVec2 {
    let (rows, cols) = clamp_dims(viewport.rows, viewport.cols);
    UVec2::new(u32::from(cols), u32::from(rows))
}

pub(super) fn set_camera_size(camera: &mut Camera, size: UVec2) {
    let current = camera
        .computed
        .target_info
        .as_ref()
        .map(|info| info.physical_size);
    if current != Some(size) {
        camera.computed.target_info = Some(RenderTargetInfo {
            physical_size: size,
            scale_factor: 1.0,
        });
    }
}

pub(super) fn set_render_target(target: &mut RenderTarget, size: UVec2) {
    if !matches!(*target, RenderTarget::None { size: current } if current == size) {
        *target = RenderTarget::None { size };
    }
}

pub(super) fn apply_viewport(camera: &mut Camera, target: &mut RenderTarget, viewport: Viewport) {
    let size = target_size(viewport);
    set_camera_size(camera, size);
    set_render_target(target, size);
}

/// `PostUpdate` before `UiSystems::Prepare`: a changed `Viewport` reaches the camera before
/// `propagate_ui_target_cameras` reads it.
pub fn sync_cameras(
    viewers: Query<(&Viewport, &ViewerCamera), Changed<Viewport>>,
    mut cameras: Query<(&mut Camera, &mut RenderTarget)>,
) {
    for (viewport, camera) in &viewers {
        if let Ok((mut camera, mut target)) = cameras.get_mut(camera.0) {
            let size = target_size(*viewport);
            // Nothing reads `Changed<Camera>`/`Changed<RenderTarget>` (`RenderTarget` has no
            // `PartialEq`); both are written through only when they differ.
            set_camera_size(camera.bypass_change_detection(), size);
            set_render_target(target.bypass_change_detection(), size);
        }
    }
}

/// `PostUpdate` after `bevy_ui` layout ([`super::LayoutSystems::SizeFold`]): `PaneSize` is the
/// minimum over the pane's shown instances of the visible content box in cells — `ComputedNode`
/// minus its borders and padding, cut to the ancestors' `CalculatedClip` (an `Overflow::scroll`/`clip`
/// container or a `Display::None` subtree) — clamped to the emulator limits. Instances that are
/// hidden, clipped away or smaller than a usable pane do not count; a pane with no usable
/// instance keeps its size. Includes `Disabled` (`Starting`) panes so a process is spawned at
/// its laid-out size.
pub fn fold_pane_sizes(
    leaves: Query<
        (
            &ComputedNode,
            &Node,
            &UiGlobalTransform,
            Option<&CalculatedClip>,
        ),
        With<InstanceNode>,
    >,
    mut panes: Query<(&mut PaneSize, &ShownBy), (With<Pane>, Allow<Disabled>)>,
) {
    for (mut size, shown_by) in &mut panes {
        let mut best: Option<(u16, u16)> = None;
        for leaf in shown_by.iter() {
            let Ok((computed, node, transform, clip)) = leaves.get(leaf) else {
                continue;
            };
            if node.display == Display::None {
                continue;
            }
            let Some((rows, cols)) = visible_content_cells(computed, transform, clip) else {
                continue;
            };
            best = Some(match best {
                Some((r, c)) => (r.min(rows), c.min(cols)),
                None => (rows, cols),
            });
        }
        if let Some((rows, cols)) = best {
            let (rows, cols) = clamp_dims(rows, cols);
            size.set_if_neq(PaneSize { rows, cols });
        }
    }
}

/// `(rows, cols)` of the visible content box of a laid-out node: its border box in viewport
/// cells inset by its borders and padding (`ComputedNode::content_inset`), intersected with
/// the inherited clip rect; `None` below the minimum usable pane.
pub fn visible_content_cells(
    computed: &ComputedNode,
    transform: &UiGlobalTransform,
    clip: Option<&CalculatedClip>,
) -> Option<(u16, u16)> {
    let inset = computed.content_inset();
    let mut content = Rect::from_center_size(transform.translation, computed.size);
    content.min += inset.min_inset;
    content.max -= inset.max_inset;
    if let Some(clip) = clip {
        content = content.intersect(clip.clip);
    }
    let cols = content.width().floor();
    let rows = content.height().floor();
    if cols < f32::from(MIN_PANE_COLS) || rows < f32::from(MIN_PANE_ROWS) {
        return None;
    }
    // Bounded by `MAX_DIM` at the caller; the float is a whole number of cells here.
    Some((
        rows.min(f32::from(u16::MAX)) as u16,
        cols.min(f32::from(u16::MAX)) as u16,
    ))
}
