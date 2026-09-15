//! Serde shape of a template node edit (`node.patch`): every field optional, unknown fields
//! rejected, `Val`s as CSS-like strings (`"auto"`, `"12px"`, `"50%"`, `"10vw"`), keywords as
//! lower-snake strings, colours as `"none" | "default" | {"rgb": [r, g, b]}`.

use bevy_color::Color;
use bevy_ui::{
    AlignContent, AlignItems, AlignSelf, BackgroundColor, BorderColor, Display, FlexDirection,
    FlexWrap, JustifyContent, JustifyItems, JustifySelf, Node, Overflow, OverflowAxis,
    PositionType, RepeatedGridTrack, UiRect, Val, ZIndex,
};
use serde::{Deserialize, Serialize};

use super::LayoutError;

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
    pub left: Option<String>,
    pub right: Option<String>,
    pub top: Option<String>,
    pub bottom: Option<String>,
    pub overflow_x: Option<String>,
    pub overflow_y: Option<String>,
    pub border: Option<RectPatch>,
    pub padding: Option<RectPatch>,
    pub margin: Option<RectPatch>,
    pub row_gap: Option<String>,
    pub column_gap: Option<String>,
    pub grid_template_columns: Option<Vec<GridTrackPatch>>,
    pub grid_template_rows: Option<Vec<GridTrackPatch>>,
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
            node.grid_template_columns = grid_tracks("grid_template_columns", tracks)?;
        }
        if let Some(tracks) = &self.grid_template_rows {
            node.grid_template_rows = grid_tracks("grid_template_rows", tracks)?;
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

fn grid_tracks(
    field: &str,
    tracks: &[GridTrackPatch],
) -> Result<Vec<RepeatedGridTrack>, LayoutError> {
    tracks
        .iter()
        .map(|t| {
            let s = t.track.trim();
            if t.repeat == 0 {
                return Err(invalid(field, s));
            }
            Ok(match s {
                "auto" => RepeatedGridTrack::auto(t.repeat),
                "min-content" => RepeatedGridTrack::min_content(t.repeat),
                "max-content" => RepeatedGridTrack::max_content(t.repeat),
                _ => {
                    if let Some(n) = s.strip_suffix("fr") {
                        RepeatedGridTrack::flex(t.repeat, number(field, n)?)
                    } else if let Some(n) = s.strip_suffix("px") {
                        RepeatedGridTrack::px(t.repeat, number(field, n)?)
                    } else if let Some(n) = s.strip_suffix('%') {
                        RepeatedGridTrack::percent(t.repeat, number(field, n)?)
                    } else {
                        return Err(invalid(field, s));
                    }
                }
            })
        })
        .collect()
}

fn number(field: &str, s: &str) -> Result<f32, LayoutError> {
    s.trim()
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or_else(|| invalid(field, s))
}
