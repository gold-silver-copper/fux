//! Layout (prompt 3.4): workspaces own inert template roots (`bevy_ui` `Node` trees); every
//! viewer showing a root gets one **instance** clone of it laid out against the viewer's camera.
//!
//! * [`ops`]: validated, all-or-nothing transitions on `&mut World` (template edits, viewer
//!   attachment, per-viewer instance state).
//! * [`instances`]: `Update`/[`Phase::Layout`]: make each viewer's instance tree equal in shape
//!   to the template it shows, re-cloning with `EntityCloner` when the template's
//!   [`LayoutGeneration`] moves.
//! * [`size`]: viewer cameras track `Viewport`; `PostUpdate` folds instance geometry into
//!   [`PaneSize`] (minimum over `ShownBy`).
//! * [`picking`]: the cell backend turning per-viewer `PointerId::Custom` locations into
//!   `PointerHits` against instance nodes.
//! * [`patch`]: the serde shape of a template node patch.

pub mod instances;
pub mod ops;
pub mod patch;
pub mod picking;
pub mod size;

use bevy_app::prelude::*;
use bevy_asset::AssetApp;
use bevy_ecs::prelude::*;
use bevy_reflect::prelude::*;
use bevy_ui::UiSystems;

use crate::model::{Ids, Limits, NodeId, Phase};

pub use patch::{ColorPatch, GridTrackPatch, NodePatch, RectPatch};

/// Public ordering handles for other plugins.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LayoutSystems {
    /// `Update`, inside [`Phase::Layout`]: instance trees are equal in shape to their templates
    /// after this set.
    Instances,
    /// `PostUpdate`, inside [`Phase::Projection`], after `bevy_ui` layout: [`PaneSize`] is folded
    /// after this set.
    SizeFold,
}

/// The template [`LayoutGeneration`] an instance root was cloned from; a mismatch re-clones.
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[reflect(Component)]
pub struct InstanceGeneration(pub u64);

/// Per-viewer instance state keyed by template [`NodeId`] so it survives re-cloning (prompt 3.4).
#[derive(Component, Debug, Default)]
pub struct ViewState {
    pub zoom: Option<NodeId>,
    /// Vertical scroll offset in cells per scrolled node.
    pub scroll: bevy_platform::collections::HashMap<NodeId, f32>,
    /// Transient `Display` overrides; the template's value is restored when an entry is removed.
    pub display: bevy_platform::collections::HashMap<NodeId, bevy_ui::Display>,
}

/// Which edge of a target node an existing node is placed beside ([`ops::place_beside`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

/// Which way a viewer navigates between the panes it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavDirection {
    Left,
    Right,
    Up,
    Down,
    /// Next pane in tree order (wraps).
    Next,
    /// Previous pane in tree order (wraps).
    Prev,
}

/// Typed refusal of a layout transition. Every variant leaves the World untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    NoSuchEntity(Entity),
    NotAWorkspace(Entity),
    WorkspaceRetiring(Entity),
    DuplicateWorkspace(String),
    NotATemplateNode(Entity),
    NotATemplateRoot(Entity),
    IsATemplateRoot(Entity),
    NotAPane(Entity),
    NotAViewer(Entity),
    /// The node is a placing leaf and cannot take children.
    LeafHasChildren(Entity),
    /// The node is a surface leaf; its subtree belongs to its provider.
    SurfaceSubtree(Entity),
    PaneLive(Entity),
    PaneNotPlaced(Entity),
    /// Reparenting a node into its own subtree.
    Cycle(Entity),
    IndexOutOfRange {
        index: usize,
        len: usize,
    },
    DepthExceeded {
        depth: usize,
        max: usize,
    },
    TooManyNodes {
        count: usize,
        max: usize,
    },
    TooManyPanes {
        count: usize,
        max: usize,
    },
    TooManyWorkspaces {
        count: usize,
        max: usize,
    },
    TooManyViewers {
        count: usize,
        max: usize,
    },
    CrossWorkspace,
    RootOrderMismatch,
    /// The viewer is an exact attachment and cannot change what it targets or shows.
    ExactTarget(Entity),
    /// The viewer shows no root or the node is not in the root it shows.
    NotShown(Entity),
    NoNeighbour,
    InvalidPatch(String),
    InvalidName(String),
}

impl core::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoSuchEntity(e) => write!(f, "{e} does not exist"),
            Self::NotAWorkspace(e) => write!(f, "{e} is not a workspace"),
            Self::WorkspaceRetiring(e) => write!(f, "workspace {e} is retiring"),
            Self::DuplicateWorkspace(n) => write!(f, "workspace {n:?} already exists"),
            Self::NotATemplateNode(e) => write!(f, "{e} is not a template node"),
            Self::NotATemplateRoot(e) => write!(f, "{e} is not a template root"),
            Self::IsATemplateRoot(e) => write!(f, "{e} is a template root"),
            Self::NotAPane(e) => write!(f, "{e} is not a pane"),
            Self::NotAViewer(e) => write!(f, "{e} is not a viewer"),
            Self::LeafHasChildren(e) => write!(f, "{e} places a pane and cannot have children"),
            Self::SurfaceSubtree(e) => {
                write!(
                    f,
                    "{e} is a surface leaf; its subtree belongs to its provider"
                )
            }
            Self::PaneLive(e) => write!(f, "pane {e} is still live"),
            Self::PaneNotPlaced(e) => write!(f, "pane {e} is not placed by a template leaf"),
            Self::Cycle(e) => write!(f, "{e} cannot be moved into its own subtree"),
            Self::IndexOutOfRange { index, len } => {
                write!(f, "index {index} out of range 0..={len}")
            }
            Self::DepthExceeded { depth, max } => write!(f, "template depth {depth} exceeds {max}"),
            Self::TooManyNodes { count, max } => write!(f, "{count} template nodes exceed {max}"),
            Self::TooManyPanes { count, max } => write!(f, "{count} panes exceed {max}"),
            Self::TooManyWorkspaces { count, max } => write!(f, "{count} workspaces exceed {max}"),
            Self::TooManyViewers { count, max } => write!(f, "{count} viewers exceed {max}"),
            Self::CrossWorkspace => write!(f, "targets belong to different workspaces"),
            Self::RootOrderMismatch => {
                write!(f, "order is not a permutation of the workspace roots")
            }
            Self::ExactTarget(e) => write!(f, "viewer {e} is an exact attachment"),
            Self::NotShown(e) => write!(f, "{e} is not in the root the viewer shows"),
            Self::NoNeighbour => write!(f, "no neighbour in that direction"),
            Self::InvalidPatch(s) => write!(f, "invalid patch: {s}"),
            Self::InvalidName(s) => write!(f, "invalid name {s:?}"),
        }
    }
}

impl std::error::Error for LayoutError {}

/// The headless `bevy_ui` stack fux and its viewer share: input, transforms, picking with the
/// window backend off (fux pointers target `NormalizedRenderTarget::None`), `UiPlugin` and the
/// text/image assets and resources its systems validate even though nothing is ever laid out as
/// text. Each plugin is added only if the app lacks it; `TaskPoolPlugin`, `TimePlugin` and
/// `AssetPlugin` must already be present.
pub fn add_ui_stack(app: &mut App) {
    if !app.is_plugin_added::<bevy_input::InputPlugin>() {
        app.add_plugins(bevy_input::InputPlugin);
    }
    if !app.is_plugin_added::<bevy_transform::TransformPlugin>() {
        app.add_plugins(bevy_transform::TransformPlugin);
    }
    if !app.is_plugin_added::<bevy_picking::PickingPlugin>() {
        app.insert_resource(bevy_picking::PickingSettings {
            is_window_picking_enabled: false,
            ..Default::default()
        })
        .add_plugins(bevy_picking::PickingPlugin);
    }
    if !app.is_plugin_added::<bevy_picking::InteractionPlugin>() {
        app.add_plugins(bevy_picking::InteractionPlugin);
    }
    if !app.is_plugin_added::<bevy_ui::UiPlugin>() {
        app.add_plugins(bevy_ui::UiPlugin);
    }
    app.init_asset::<bevy_image::Image>()
        .init_asset::<bevy_image::TextureAtlasLayout>()
        .init_asset::<bevy_text::Font>()
        .init_resource::<bevy_text::FontAtlasSet>()
        .init_resource::<bevy_text::TextPipeline>()
        .init_resource::<bevy_text::FontCx>()
        .init_resource::<bevy_text::LayoutCx>()
        .init_resource::<bevy_text::ScaleCx>()
        .init_resource::<bevy_text::TextIterScratch>();
    if !app.world().contains_resource::<bevy_text::RemSize>() {
        app.insert_resource(bevy_text::RemSize(16.0));
    }
}

/// Templates, instances, the fux picking backend and the derived writes ([`PaneSize`], camera
/// target sizes) over [`add_ui_stack`]. Requires `ModelPlugin`, `TaskPoolPlugin`, `TimePlugin`
/// and `AssetPlugin`.
pub struct LayoutPlugin;

impl Plugin for LayoutPlugin {
    fn build(&self, app: &mut App) {
        add_ui_stack(app);
        app.init_resource::<Ids>()
            .init_resource::<Limits>()
            .register_type::<InstanceGeneration>()
            .configure_sets(Update, LayoutSystems::Instances.in_set(Phase::Layout))
            .configure_sets(
                PostUpdate,
                LayoutSystems::SizeFold
                    .in_set(Phase::Projection)
                    .after(UiSystems::PostLayout),
            )
            .add_systems(
                PreUpdate,
                picking::cell_backend.in_set(bevy_picking::PickingSystems::Backend),
            )
            .add_systems(
                Update,
                instances::sync_instances.in_set(LayoutSystems::Instances),
            )
            .add_systems(
                PostUpdate,
                (
                    size::sync_cameras.before(UiSystems::Prepare),
                    size::fold_pane_sizes.in_set(LayoutSystems::SizeFold),
                ),
            );
    }
}
