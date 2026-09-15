//! Serde shape of a template node edit (`node.patch`): every field optional, unknown fields
//! rejected, `Val`s as CSS-like strings (`"auto"`, `"12px"`, `"50%"`, `"10vw"`, `"10vh"`,
//! `"10vmin"`, `"10vmax"`), keywords as lower-snake strings, colours as
//! `"none" | "default" | {"rgb": [r, g, b]}`. Every field is validated before anything is
//! written; the first invalid one is reported as [`LayoutError::InvalidPatch`].

use bevy_color::Color;
use bevy_ui::{
    AlignContent, AlignItems, AlignSelf, BackgroundColor, BorderColor, Display, FlexDirection,
    FlexWrap, GridAutoFlow, GridPlacement, GridTrack, JustifyContent, JustifyItems, JustifySelf,
    Node, Overflow, OverflowAxis, OverflowClipMargin, PositionType, RepeatedGridTrack, UiRect, Val,
    VisualBox, ZIndex,
};
use serde::{Deserialize, Serialize};

use super::LayoutError;

/// A partial edit of a template node's `Node`, `ZIndex`, `BackgroundColor` and `BorderColor`.
///
/// String forms:
/// * `display`: `flex | grid | block | none`.
/// * `position_type`: `relative | absolute`.
/// * `flex_direction`: `row | column | row_reverse | column_reverse`.
/// * `flex_wrap`: `no_wrap | wrap | wrap_reverse`.
/// * `flex_grow`, `flex_shrink`: finite, non-negative numbers.
/// * `flex_basis`, `width`, `height`, `min_*`, `max_*`, `left`, `right`, `top`, `bottom`,
///   `row_gap`, `column_gap` and every side of `inset`, `border`, `padding`, `margin`: `Val`
///   strings (`auto`, `<n>px`, `<n>%`, `<n>vw`, `<n>vh`, `<n>vmin`, `<n>vmax`); `px` are cells.
/// * `inset`: shorthand for `left`/`right`/`top`/`bottom`, applied before the named fields.
/// * `aspect_ratio`: `auto` (none) or a positive number (`width / height`).
/// * `overflow_x`, `overflow_y`: `visible | clip | hidden | scroll`.
/// * `overflow_clip_margin`: `{visual_box: content_box | padding_box | border_box, margin: n}`.
/// * `grid_template_columns`, `grid_template_rows`: `[{repeat?: n, track: <track>}]`.
/// * `grid_auto_columns`, `grid_auto_rows`: `[<track>]`; a track is `auto | min-content |
///   max-content | <n>fr | <n>px | <n>%`.
/// * `grid_auto_flow`: `row | column | row_dense | column_dense`.
/// * `grid_row`, `grid_column`: `{start?: line, span?: n, end?: line}`; lines are 1-based,
///   negative counts from the end, zero is invalid, all three together is over-constrained.
/// * `justify_content`, `align_content`: `default | start | end | flex_start | flex_end | center
///   | stretch | space_between | space_evenly | space_around`.
/// * `justify_items`, `align_items`: `default | start | end | (flex_start | flex_end for align)
///   | center | baseline | stretch`.
/// * `justify_self`, `align_self`: `auto | start | end | (flex_start | flex_end for align) |
///   center | baseline | stretch`.
/// * `z_index`: integer; `background_color`, `border_color`: [`ColorPatch`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NodePatch {
    pub display: Option<String>,
    pub position_type: Option<String>,
    pub flex_direction: Option<String>,
    pub flex_wrap: Option<String>,
    pub flex_grow: Option<f32>,
    pub flex_shrink: Option<f32>,
    pub flex_basis: Option<String>,
    pub width: Option<String>,
    pub height: Option<String>,
    pub min_width: Option<String>,
    pub min_height: Option<String>,
    pub max_width: Option<String>,
    pub max_height: Option<String>,
    pub aspect_ratio: Option<String>,
    pub inset: Option<RectPatch>,
    pub left: Option<String>,
    pub right: Option<String>,
    pub top: Option<String>,
    pub bottom: Option<String>,
    pub overflow_x: Option<String>,
    pub overflow_y: Option<String>,
    pub overflow_clip_margin: Option<OverflowClipMarginPatch>,
    pub border: Option<RectPatch>,
    pub padding: Option<RectPatch>,
    pub margin: Option<RectPatch>,
    pub row_gap: Option<String>,
    pub column_gap: Option<String>,
    pub grid_template_columns: Option<Vec<GridTrackPatch>>,
    pub grid_template_rows: Option<Vec<GridTrackPatch>>,
    pub grid_auto_columns: Option<Vec<String>>,
    pub grid_auto_rows: Option<Vec<String>>,
    pub grid_auto_flow: Option<String>,
    pub grid_row: Option<GridPlacementPatch>,
    pub grid_column: Option<GridPlacementPatch>,
    pub justify_content: Option<String>,
    pub align_content: Option<String>,
    pub justify_items: Option<String>,
    pub align_items: Option<String>,
    pub justify_self: Option<String>,
    pub align_self: Option<String>,
    pub z_index: Option<i32>,
    pub background_color: Option<ColorPatch>,
    pub border_color: Option<ColorPatch>,
}

/// Four sides; `all` is applied first, named sides override it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RectPatch {
    pub all: Option<String>,
    pub left: Option<String>,
    pub right: Option<String>,
    pub top: Option<String>,
    pub bottom: Option<String>,
}

/// `repeat` copies of one track: `"1fr"`, `"12px"`, `"50%"`, `"auto"`, `"min-content"`,
/// `"max-content"`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GridTrackPatch {
    #[serde(default = "one")]
    pub repeat: u16,
    pub track: String,
}

fn one() -> u16 {
    1
}

/// A whole `GridPlacement`: the fields given are set, the others are automatic (`span`
/// defaults to 1 unless both lines are given).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GridPlacementPatch {
    pub start: Option<i16>,
    pub span: Option<u16>,
    pub end: Option<i16>,
}

/// A whole `OverflowClipMargin`: `visual_box` defaults to the padding box, `margin` to 0 cells.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OverflowClipMarginPatch {
    pub visual_box: Option<String>,
    pub margin: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorPatch {
    None,
    Default,
    Rgb(u8, u8, u8),
}

impl ColorPatch {
    fn color(self, default: Color) -> Color {
        match self {
            Self::None => Color::NONE,
            Self::Default => default,
            Self::Rgb(r, g, b) => Color::srgb_u8(r, g, b),
        }
    }
}

impl NodePatch {
    /// Applies the patch to copies of the components; the caller commits them only if every
    /// field parsed.
    pub fn apply(
        &self,
        node: &mut Node,
        z_index: &mut ZIndex,
        background: &mut BackgroundColor,
        border_color: &mut BorderColor,
    ) -> Result<(), LayoutError> {
        if let Some(s) = &self.display {
            node.display = match s.as_str() {
                "flex" => Display::Flex,
                "grid" => Display::Grid,
                "block" => Display::Block,
                "none" => Display::None,
                other => return Err(invalid("display", other)),
            };
        }
        if let Some(s) = &self.position_type {
            node.position_type = match s.as_str() {
                "relative" => PositionType::Relative,
                "absolute" => PositionType::Absolute,
                other => return Err(invalid("position_type", other)),
            };
        }
        if let Some(s) = &self.flex_direction {
            node.flex_direction = match s.as_str() {
                "row" => FlexDirection::Row,
                "column" => FlexDirection::Column,
                "row_reverse" => FlexDirection::RowReverse,
                "column_reverse" => FlexDirection::ColumnReverse,
                other => return Err(invalid("flex_direction", other)),
            };
        }
        if let Some(s) = &self.flex_wrap {
            node.flex_wrap = match s.as_str() {
                "no_wrap" => FlexWrap::NoWrap,
                "wrap" => FlexWrap::Wrap,
                "wrap_reverse" => FlexWrap::WrapReverse,
                other => return Err(invalid("flex_wrap", other)),
            };
        }
        if let Some(v) = self.flex_grow {
            node.flex_grow = finite("flex_grow", v)?;
        }
        if let Some(v) = self.flex_shrink {
            node.flex_shrink = finite("flex_shrink", v)?;
        }
        val(&self.flex_basis, "flex_basis", &mut node.flex_basis)?;
        val(&self.width, "width", &mut node.width)?;
        val(&self.height, "height", &mut node.height)?;
        val(&self.min_width, "min_width", &mut node.min_width)?;
        val(&self.min_height, "min_height", &mut node.min_height)?;
        val(&self.max_width, "max_width", &mut node.max_width)?;
        val(&self.max_height, "max_height", &mut node.max_height)?;
        if let Some(s) = &self.aspect_ratio {
            node.aspect_ratio = match s.trim() {
                "auto" => None,
                n => Some(positive("aspect_ratio", n)?),
            };
        }
        if let Some(r) = &self.inset {
            let mut inset = UiRect {
                left: node.left,
                right: node.right,
                top: node.top,
                bottom: node.bottom,
            };
            r.apply("inset", &mut inset)?;
            node.left = inset.left;
            node.right = inset.right;
            node.top = inset.top;
            node.bottom = inset.bottom;
        }
        val(&self.left, "left", &mut node.left)?;
        val(&self.right, "right", &mut node.right)?;
        val(&self.top, "top", &mut node.top)?;
        val(&self.bottom, "bottom", &mut node.bottom)?;
        val(&self.row_gap, "row_gap", &mut node.row_gap)?;
        val(&self.column_gap, "column_gap", &mut node.column_gap)?;
        let mut overflow = Overflow {
            x: node.overflow.x,
            y: node.overflow.y,
        };
        if let Some(s) = &self.overflow_x {
            overflow.x = overflow_axis("overflow_x", s)?;
        }
        if let Some(s) = &self.overflow_y {
            overflow.y = overflow_axis("overflow_y", s)?;
        }
        node.overflow = overflow;
        if let Some(m) = &self.overflow_clip_margin {
            node.overflow_clip_margin = m.parse()?;
        }
        if let Some(r) = &self.border {
            r.apply("border", &mut node.border)?;
        }
        if let Some(r) = &self.padding {
            r.apply("padding", &mut node.padding)?;
        }
        if let Some(r) = &self.margin {
            r.apply("margin", &mut node.margin)?;
        }
        if let Some(tracks) = &self.grid_template_columns {
            node.grid_template_columns = repeated_tracks("grid_template_columns", tracks)?;
        }
        if let Some(tracks) = &self.grid_template_rows {
            node.grid_template_rows = repeated_tracks("grid_template_rows", tracks)?;
        }
        if let Some(tracks) = &self.grid_auto_columns {
            node.grid_auto_columns = tracks
                .iter()
                .map(|t| grid_track("grid_auto_columns", t))
                .collect::<Result<_, _>>()?;
        }
        if let Some(tracks) = &self.grid_auto_rows {
            node.grid_auto_rows = tracks
                .iter()
                .map(|t| grid_track("grid_auto_rows", t))
                .collect::<Result<_, _>>()?;
        }
        if let Some(s) = &self.grid_auto_flow {
            node.grid_auto_flow = match s.as_str() {
                "row" => GridAutoFlow::Row,
                "column" => GridAutoFlow::Column,
                "row_dense" => GridAutoFlow::RowDense,
                "column_dense" => GridAutoFlow::ColumnDense,
                other => return Err(invalid("grid_auto_flow", other)),
            };
        }
        if let Some(p) = &self.grid_row {
            node.grid_row = p.parse("grid_row")?;
        }
        if let Some(p) = &self.grid_column {
            node.grid_column = p.parse("grid_column")?;
        }
        if let Some(s) = &self.justify_content {
            node.justify_content = match s.as_str() {
                "default" => JustifyContent::Default,
                "start" => JustifyContent::Start,
                "end" => JustifyContent::End,
                "flex_start" => JustifyContent::FlexStart,
                "flex_end" => JustifyContent::FlexEnd,
                "center" => JustifyContent::Center,
                "stretch" => JustifyContent::Stretch,
                "space_between" => JustifyContent::SpaceBetween,
                "space_evenly" => JustifyContent::SpaceEvenly,
                "space_around" => JustifyContent::SpaceAround,
                other => return Err(invalid("justify_content", other)),
            };
        }
        if let Some(s) = &self.align_content {
            node.align_content = match s.as_str() {
                "default" => AlignContent::Default,
                "start" => AlignContent::Start,
                "end" => AlignContent::End,
                "flex_start" => AlignContent::FlexStart,
                "flex_end" => AlignContent::FlexEnd,
                "center" => AlignContent::Center,
                "stretch" => AlignContent::Stretch,
                "space_between" => AlignContent::SpaceBetween,
                "space_evenly" => AlignContent::SpaceEvenly,
                "space_around" => AlignContent::SpaceAround,
                other => return Err(invalid("align_content", other)),
            };
        }
        if let Some(s) = &self.justify_items {
            node.justify_items = match s.as_str() {
                "default" => JustifyItems::Default,
                "start" => JustifyItems::Start,
                "end" => JustifyItems::End,
                "center" => JustifyItems::Center,
                "baseline" => JustifyItems::Baseline,
                "stretch" => JustifyItems::Stretch,
                other => return Err(invalid("justify_items", other)),
            };
        }
        if let Some(s) = &self.align_items {
            node.align_items = match s.as_str() {
                "default" => AlignItems::Default,
                "start" => AlignItems::Start,
                "end" => AlignItems::End,
                "flex_start" => AlignItems::FlexStart,
                "flex_end" => AlignItems::FlexEnd,
                "center" => AlignItems::Center,
                "baseline" => AlignItems::Baseline,
                "stretch" => AlignItems::Stretch,
                other => return Err(invalid("align_items", other)),
            };
        }
        if let Some(s) = &self.justify_self {
            node.justify_self = match s.as_str() {
                "auto" => JustifySelf::Auto,
                "start" => JustifySelf::Start,
                "end" => JustifySelf::End,
                "center" => JustifySelf::Center,
                "baseline" => JustifySelf::Baseline,
                "stretch" => JustifySelf::Stretch,
                other => return Err(invalid("justify_self", other)),
            };
        }
        if let Some(s) = &self.align_self {
            node.align_self = match s.as_str() {
                "auto" => AlignSelf::Auto,
                "start" => AlignSelf::Start,
                "end" => AlignSelf::End,
                "flex_start" => AlignSelf::FlexStart,
                "flex_end" => AlignSelf::FlexEnd,
                "center" => AlignSelf::Center,
                "baseline" => AlignSelf::Baseline,
                "stretch" => AlignSelf::Stretch,
                other => return Err(invalid("align_self", other)),
            };
        }
        if let Some(z) = self.z_index {
            *z_index = ZIndex(z);
        }
        if let Some(c) = self.background_color {
            *background = BackgroundColor(c.color(BackgroundColor::DEFAULT.0));
        }
        if let Some(c) = self.border_color {
            *border_color = BorderColor::all(c.color(BorderColor::DEFAULT.top));
        }
        Ok(())
    }
}

impl RectPatch {
    fn apply(&self, field: &str, rect: &mut UiRect) -> Result<(), LayoutError> {
        if let Some(all) = &self.all {
            let v = parse_val(field, all)?;
            *rect = UiRect::all(v);
        }
        val(&self.left, field, &mut rect.left)?;
        val(&self.right, field, &mut rect.right)?;
        val(&self.top, field, &mut rect.top)?;
        val(&self.bottom, field, &mut rect.bottom)
    }
}

impl GridPlacementPatch {
    fn parse(&self, field: &str) -> Result<GridPlacement, LayoutError> {
        let line = |v: Option<i16>| match v {
            Some(0) => Err(invalid(field, "0")),
            other => Ok(other),
        };
        let start = line(self.start)?;
        let end = line(self.end)?;
        let span = match self.span {
            Some(0) => return Err(invalid(field, "span 0")),
            other => other,
        };
        // The constructors below panic only on zero, which is excluded above.
        Ok(match (start, span, end) {
            (None, None, None) => GridPlacement::auto(),
            (Some(s), None, None) => GridPlacement::start(s),
            (None, Some(n), None) => GridPlacement::span(n),
            (None, None, Some(e)) => GridPlacement::end(e),
            (Some(s), Some(n), None) => GridPlacement::start_span(s, n),
            (Some(s), None, Some(e)) => GridPlacement::start_end(s, e),
            (None, Some(n), Some(e)) => GridPlacement::end_span(e, n),
            (Some(_), Some(_), Some(_)) => return Err(invalid(field, "start, span and end")),
        })
    }
}

impl OverflowClipMarginPatch {
    fn parse(&self) -> Result<OverflowClipMargin, LayoutError> {
        let visual_box = match self.visual_box.as_deref() {
            None | Some("padding_box") => VisualBox::PaddingBox,
            Some("content_box") => VisualBox::ContentBox,
            Some("border_box") => VisualBox::BorderBox,
            Some(other) => return Err(invalid("overflow_clip_margin.visual_box", other)),
        };
        let margin = match self.margin {
            Some(m) => finite("overflow_clip_margin.margin", m)?,
            None => 0.0,
        };
        Ok(OverflowClipMargin { visual_box, margin })
    }
}

fn invalid(field: &str, value: &str) -> LayoutError {
    LayoutError::InvalidPatch(format!("{field}: {value:?}"))
}

fn finite(field: &str, v: f32) -> Result<f32, LayoutError> {
    if v.is_finite() && v >= 0.0 {
        Ok(v)
    } else {
        Err(LayoutError::InvalidPatch(format!("{field}: {v}")))
    }
}

fn positive(field: &str, s: &str) -> Result<f32, LayoutError> {
    number(field, s).and_then(|v| {
        if v > 0.0 {
            Ok(v)
        } else {
            Err(invalid(field, s))
        }
    })
}

fn parse_val(field: &str, s: &str) -> Result<Val, LayoutError> {
    s.parse::<Val>().map_err(|_| invalid(field, s))
}

fn val(src: &Option<String>, field: &str, dst: &mut Val) -> Result<(), LayoutError> {
    if let Some(s) = src {
        *dst = parse_val(field, s)?;
    }
    Ok(())
}

fn overflow_axis(field: &str, s: &str) -> Result<OverflowAxis, LayoutError> {
    Ok(match s {
        "visible" => OverflowAxis::Visible,
        "clip" => OverflowAxis::Clip,
        "hidden" => OverflowAxis::Hidden,
        "scroll" => OverflowAxis::Scroll,
        other => return Err(invalid(field, other)),
    })
}

fn repeated_tracks(
    field: &str,
    tracks: &[GridTrackPatch],
) -> Result<Vec<RepeatedGridTrack>, LayoutError> {
    tracks
        .iter()
        .map(|t| {
            if t.repeat == 0 {
                return Err(invalid(field, "repeat 0"));
            }
            let track: GridTrack = grid_track(field, &t.track)?;
            Ok(RepeatedGridTrack::repeat_many(t.repeat, vec![track]))
        })
        .collect()
}

fn grid_track(field: &str, s: &str) -> Result<GridTrack, LayoutError> {
    let s = s.trim();
    Ok(match s {
        "auto" => GridTrack::auto(),
        "min-content" => GridTrack::min_content(),
        "max-content" => GridTrack::max_content(),
        _ => {
            if let Some(n) = s.strip_suffix("fr") {
                GridTrack::flex(number(field, n)?)
            } else if let Some(n) = s.strip_suffix("px") {
                GridTrack::px(number(field, n)?)
            } else if let Some(n) = s.strip_suffix('%') {
                GridTrack::percent(number(field, n)?)
            } else {
                return Err(invalid(field, s));
            }
        }
    })
}

fn number(field: &str, s: &str) -> Result<f32, LayoutError> {
    s.trim()
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or_else(|| invalid(field, s))
}
