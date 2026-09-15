//! The cell painter (prompt 3.11): one pass over the viewer's own `UiStack` in stacking order,
//! drawing each node's fill (its resolved `CellStyle` for chrome, `BackgroundColor` for
//! replicated nodes), `BorderColor`/theme borders, then the node's content (the pane grid for
//! replicated leaves, `Text` for chrome) into a cell buffer that is diffed against the last
//! painted screen; only changed cells become termina commands. No allocation per cell: both
//! screens and the output buffer are reused across frames.

use std::io::Write as _;

use bevy_app::prelude::*;
use bevy_asset::{AssetEvent, AssetEventSystems, Assets};
use bevy_color::Color;
use bevy_ecs::prelude::*;
use bevy_input_focus::InputFocus;
use bevy_math::{IVec2, Rect};
use bevy_ui::{
    BackgroundColor, BorderColor, CalculatedClip, ComputedNode, UiGlobalTransform, UiStack,
    UiSystems,
};
use termina::OneBased;
use termina::escape::csi::{
    Csi, Cursor as CsiCursor, DecPrivateMode, DecPrivateModeCode, Edit, EraseInDisplay, Mode, Sgr,
    SgrAttributes, SgrModifiers,
};
use termina::style::{ColorSpec, RgbaColor};

use super::chrome::Text;
use super::copy_mode::CopyView;
use super::prompts::CursorAt;
use super::replicate::{ClipboardWrite, Grid};
use super::theme::{BorderStyles, CellStyle};
use super::{Mode as ViewerMode, Viewport};
use crate::assets::{ConfigAsset, ConfigHandle};
use crate::config::ClipboardPolicy;
use crate::model::Shows;
use crate::surface::Text as SurfaceText;
use crate::wire::{Color as WireColor, Style};

/// Inline cell text: a grapheme cluster of up to fifteen UTF-8 bytes. Longer clusters keep their
/// leading scalar values; storage stays fixed so screens never allocate per cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CellText {
    bytes: [u8; 15],
    len: u8,
}

impl CellText {
    pub const SPACE: Self = Self {
        bytes: [b' ', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        len: 1,
    };

    pub fn new(text: &str) -> Self {
        let mut cell = Self {
            bytes: [0; 15],
            len: 0,
        };
        for ch in text.chars() {
            let width = ch.len_utf8();
            let end = usize::from(cell.len) + width;
            let Some(slot) = cell.bytes.get_mut(usize::from(cell.len)..end) else {
                break;
            };
            ch.encode_utf8(slot);
            cell.len = end as u8;
        }
        if cell.len == 0 {
            return Self::SPACE;
        }
        cell
    }

    pub fn from_char(ch: char) -> Self {
        let mut buf = [0u8; 4];
        Self::new(ch.encode_utf8(&mut buf))
    }

    pub fn as_str(&self) -> &str {
        self.bytes
            .get(..usize::from(self.len))
            .and_then(|b| core::str::from_utf8(b).ok())
            .unwrap_or(" ")
    }
}

/// One painted cell. `width == 0` marks the trailing column of a wide glyph.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScreenCell {
    pub text: CellText,
    pub width: u8,
    pub style: Style,
}

impl ScreenCell {
    pub const BLANK: Self = Self {
        text: CellText::SPACE,
        width: 1,
        style: Style {
            fg: WireColor::Default,
            bg: WireColor::Default,
            attrs: 0,
        },
    };

    pub fn blank(style: Style) -> Self {
        Self {
            text: CellText::SPACE,
            width: 1,
            style,
        }
    }

    pub fn glyph(ch: char, style: Style) -> Self {
        Self {
            text: CellText::from_char(ch),
            width: 1,
            style,
        }
    }
}

/// A cell buffer the size of the terminal.
#[derive(Clone, Debug)]
pub struct Screen {
    cols: u16,
    rows: u16,
    cells: Vec<ScreenCell>,
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            cells: vec![ScreenCell::BLANK; usize::from(cols) * usize::from(rows)],
        }
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.cells.clear();
        self.cells
            .resize(usize::from(cols) * usize::from(rows), ScreenCell::BLANK);
    }

    pub fn clear(&mut self) {
        self.cells.fill(ScreenCell::BLANK);
    }

    fn index(&self, col: i32, row: i32) -> Option<usize> {
        if col < 0 || row < 0 || col >= i32::from(self.cols) || row >= i32::from(self.rows) {
            return None;
        }
        Some(row as usize * usize::from(self.cols) + col as usize)
    }

    pub fn get(&self, col: u16, row: u16) -> Option<&ScreenCell> {
        self.index(i32::from(col), i32::from(row))
            .and_then(|i| self.cells.get(i))
    }

    fn set(&mut self, col: i32, row: i32, cell: ScreenCell) {
        if let Some(i) = self.index(col, row)
            && let Some(slot) = self.cells.get_mut(i)
        {
            *slot = cell;
        }
    }
}

/// Integer cell rectangle: `min` inclusive, `max` exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRect {
    pub min: IVec2,
    pub max: IVec2,
}

impl CellRect {
    pub fn from_node(node: &ComputedNode, transform: &UiGlobalTransform) -> Self {
        let centre = transform.translation;
        let half = node.size / 2.0;
        let min = (centre - half).round();
        let max = (centre + half).round();
        Self {
            min: IVec2::new(min.x as i32, min.y as i32),
            max: IVec2::new(max.x as i32, max.y as i32),
        }
    }

    pub fn intersect(self, other: Self) -> Self {
        Self {
            min: self.min.max(other.min),
            max: self.max.min(other.max),
        }
    }

    fn from_rect(rect: Rect) -> Self {
        Self {
            min: IVec2::new(rect.min.x.round() as i32, rect.min.y.round() as i32),
            max: IVec2::new(rect.max.x.round() as i32, rect.max.y.round() as i32),
        }
    }

    pub fn width(self) -> i32 {
        (self.max.x - self.min.x).max(0)
    }

    pub fn height(self) -> i32 {
        (self.max.y - self.min.y).max(0)
    }

    pub fn contains(self, col: i32, row: i32) -> bool {
        col >= self.min.x && col < self.max.x && row >= self.min.y && row < self.max.y
    }

    fn inset(self, node: &ComputedNode) -> Self {
        Self {
            min: self.min
                + IVec2::new(
                    node.border.min_inset.x.round() as i32,
                    node.border.min_inset.y.round() as i32,
                ),
            max: self.max
                - IVec2::new(
                    node.border.max_inset.x.round() as i32,
                    node.border.max_inset.y.round() as i32,
                ),
        }
    }
}

/// The painter state: the last painted screen, the one being composed, the pending terminal
/// bytes and the cursor position last requested.
#[derive(Resource, Debug)]
pub struct Painter {
    current: Screen,
    next: Screen,
    /// Terminal bytes produced by the last paint; the runner writes and clears them.
    pub out: Vec<u8>,
    cursor: Option<(u16, u16)>,
    /// Forces a full repaint (first frame, resize).
    full: bool,
}

impl Painter {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            current: Screen::new(cols, rows),
            next: Screen::new(cols, rows),
            out: Vec::with_capacity(64 * 1024),
            cursor: None,
            full: true,
        }
    }

    /// The screen as last painted.
    pub fn screen(&self) -> &Screen {
        &self.current
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.current.resize(cols, rows);
        self.next.resize(cols, rows);
        self.full = true;
    }

    pub fn cursor(&self) -> Option<(u16, u16)> {
        self.cursor
    }
}

pub fn wire_color(color: Color) -> WireColor {
    let s = color.to_srgba();
    if s.alpha <= 0.0 {
        return WireColor::Default;
    }
    WireColor::Rgb(
        (s.red * 255.0).round() as u8,
        (s.green * 255.0).round() as u8,
        (s.blue * 255.0).round() as u8,
    )
}

fn color_spec(color: WireColor) -> ColorSpec {
    match color {
        WireColor::Default => ColorSpec::Reset,
        WireColor::Indexed(i) => ColorSpec::PaletteIndex(i),
        WireColor::Rgb(r, g, b) => ColorSpec::TrueColor(RgbaColor {
            red: r,
            green: g,
            blue: b,
            alpha: 255,
        }),
    }
}

fn sgr(style: Style) -> Csi {
    let mut modifiers = SgrModifiers::RESET;
    if style.attrs & Style::BOLD != 0 {
        modifiers |= SgrModifiers::INTENSITY_BOLD;
    }
    if style.attrs & Style::DIM != 0 {
        modifiers |= SgrModifiers::INTENSITY_DIM;
    }
    if style.attrs & Style::ITALIC != 0 {
        modifiers |= SgrModifiers::ITALIC;
    }
    if style.attrs & Style::UNDERLINE != 0 {
        modifiers |= SgrModifiers::UNDERLINE_SINGLE;
    }
    if style.attrs & Style::INVERSE != 0 {
        modifiers |= SgrModifiers::REVERSE;
    }
    if style.attrs & Style::STRIKE != 0 {
        modifiers |= SgrModifiers::STRIKE_THROUGH;
    }
    Csi::Sgr(Sgr::Attributes(SgrAttributes {
        foreground: (style.fg != WireColor::Default).then(|| color_spec(style.fg)),
        background: (style.bg != WireColor::Default).then(|| color_spec(style.bg)),
        underline_color: None,
        modifiers,
        ..SgrAttributes::default()
    }))
}

fn cup(col: u16, row: u16) -> Csi {
    Csi::Cursor(CsiCursor::Position {
        line: OneBased::from_zero_based(row),
        col: OneBased::from_zero_based(col),
    })
}

type NodeItem<'a> = (
    Entity,
    &'a ComputedNode,
    &'a UiGlobalTransform,
    Option<&'a BackgroundColor>,
    Option<&'a BorderColor>,
    Option<&'a CellStyle>,
    Option<&'a CalculatedClip>,
    Option<&'a Shows>,
    Option<&'a Text>,
    Option<&'a SurfaceText>,
    Option<&'a CopyView>,
    Option<&'a CursorAt>,
);

/// Composes the screen from the UI stack and diffs it against the previous paint, then appends
/// the frame's pane clipboard writes as OSC 52 when the configured policy is `write-only` (the
/// viewer sets the outer terminal's clipboard and never queries it).
pub fn paint(
    stack: Res<UiStack>,
    nodes: Query<NodeItem<'_>>,
    grids: Query<&Grid>,
    (focus, mode, viewport, borders, policy): (
        Res<InputFocus>,
        Res<bevy_state::prelude::State<ViewerMode>>,
        Res<Viewport>,
        Res<BorderStyles>,
        Res<ClipboardPolicy>,
    ),
    mut clipboard: MessageReader<ClipboardWrite>,
    mut painter: ResMut<Painter>,
) {
    let painter = &mut *painter;
    if painter.next.cols != viewport.cols || painter.next.rows != viewport.rows {
        painter.resize(viewport.cols, viewport.rows);
    }
    painter.next.clear();
    let screen_rect = CellRect {
        min: IVec2::ZERO,
        max: IVec2::new(i32::from(viewport.cols), i32::from(viewport.rows)),
    };
    let focused = focus.get();
    let mut cursor = None;
    for &entity in &stack.uinodes {
        let Ok((
            entity,
            node,
            transform,
            bg_color,
            border_color,
            cell_style,
            clip,
            shows,
            text,
            surface_text,
            copy_view,
            cursor_at,
        )) = nodes.get(entity)
        else {
            continue;
        };
        if node.size.x < 0.5 || node.size.y < 0.5 {
            continue;
        }
        let rect = CellRect::from_node(node, transform);
        let mut visible = rect.intersect(screen_rect);
        if let Some(clip) = clip {
            visible = visible.intersect(CellRect::from_rect(clip.clip));
        }
        if visible.width() <= 0 || visible.height() <= 0 {
            continue;
        }
        let style = cell_style.map(|s| s.0).unwrap_or_default();
        let bg = match cell_style {
            Some(style) => style.0.bg,
            None => bg_color.map(|b| wire_color(b.0)).unwrap_or_default(),
        };
        if bg != WireColor::Default {
            let cell = ScreenCell::blank(Style {
                fg: WireColor::Default,
                bg,
                attrs: 0,
            });
            for row in visible.min.y..visible.max.y {
                for col in visible.min.x..visible.max.x {
                    painter.next.set(col, row, cell);
                }
            }
        }
        let has_border = node.border.min_inset.max_element() >= 0.5
            || node.border.max_inset.max_element() >= 0.5;
        if has_border && rect.width() >= 2 && rect.height() >= 2 {
            let style = if focused == Some(entity) {
                Style {
                    bg,
                    ..borders.focused
                }
            } else {
                let own = border_color
                    .map(|b| wire_color(b.top))
                    .filter(|c| *c != WireColor::Default);
                Style {
                    fg: own.unwrap_or(borders.pane.fg),
                    bg,
                    attrs: borders.pane.attrs,
                }
            };
            draw_border(&mut painter.next, rect, visible, style);
        }
        let content = rect.inset(node).intersect(visible);
        if let Some(grid) = shows.and_then(|s| grids.get(s.0).ok()) {
            let origin = rect.inset(node);
            if let Some(view) = copy_view {
                draw_copy_view(&mut painter.next, view, grid, origin, content, bg);
                let (row, col) = view.cursor();
                if row >= view.offset() {
                    let col = origin.min.x + col as i32;
                    let row = origin.min.y + (row - view.offset()) as i32;
                    if content.contains(col, row) {
                        cursor = Some((col as u16, row as u16));
                    }
                }
            } else {
                draw_grid(&mut painter.next, grid, origin, content, bg);
                if focused == Some(entity)
                    && grid.cursor.visible
                    && matches!(**mode, ViewerMode::Normal | ViewerMode::Prefix)
                {
                    let col = origin.min.x + i32::from(grid.cursor.col);
                    let row = origin.min.y + i32::from(grid.cursor.row);
                    if content.contains(col, row) {
                        cursor = Some((col as u16, row as u16));
                    }
                }
            }
        }
        if let Some(text) = text {
            draw_text(
                &mut painter.next,
                &text.0,
                style,
                rect.inset(node),
                content,
                bg,
            );
            if let Some(at) = cursor_at
                && **mode == ViewerMode::Prompt
            {
                let col = rect.inset(node).min.x + i32::from(at.0);
                let row = rect.inset(node).min.y;
                if content.contains(col, row) {
                    cursor = Some((col as u16, row as u16));
                }
            }
        }
        // A surface's text leaf: the provider styles through colours on the node itself.
        if let Some(text) = surface_text {
            draw_text(
                &mut painter.next,
                &text.0,
                Style::default(),
                rect.inset(node),
                content,
                bg,
            );
        }
    }
    emit(painter, cursor);
    match *policy {
        ClipboardPolicy::WriteOnly => {
            for ClipboardWrite(payload) in clipboard.read() {
                // `Vec<u8>` never fails to write.
                let _ = write!(painter.out, "\x1b]52;c;{payload}\x07");
            }
        }
        ClipboardPolicy::Off => clipboard.clear(),
    }
}

/// Follows `clipboard` in the loaded `fux.toml`.
fn apply_clipboard_policy(
    mut events: MessageReader<AssetEvent<ConfigAsset>>,
    handle: Res<ConfigHandle>,
    configs: Res<Assets<ConfigAsset>>,
    mut policy: ResMut<ClipboardPolicy>,
) {
    if !events.read().any(|event| handle.changed(event)) {
        return;
    }
    if let Some(ConfigAsset(config)) = configs.get(&handle.0) {
        policy.set_if_neq(config.clipboard);
    }
}

fn draw_border(screen: &mut Screen, rect: CellRect, visible: CellRect, style: Style) {
    let (x0, y0, x1, y1) = (rect.min.x, rect.min.y, rect.max.x - 1, rect.max.y - 1);
    let mut put = |col: i32, row: i32, ch: char| {
        if visible.contains(col, row) {
            screen.set(col, row, ScreenCell::glyph(ch, style));
        }
    };
    for col in (x0 + 1)..x1 {
        put(col, y0, '─');
        put(col, y1, '─');
    }
    for row in (y0 + 1)..y1 {
        put(x0, row, '│');
        put(x1, row, '│');
    }
    put(x0, y0, '┌');
    put(x1, y0, '┐');
    put(x0, y1, '└');
    put(x1, y1, '┘');
}

fn draw_grid(screen: &mut Screen, grid: &Grid, origin: CellRect, visible: CellRect, bg: WireColor) {
    let blank = ScreenCell::blank(Style {
        fg: WireColor::Default,
        bg,
        attrs: 0,
    });
    for row in visible.min.y..visible.max.y {
        let grid_row = row - origin.min.y;
        for col in visible.min.x..visible.max.x {
            let grid_col = col - origin.min.x;
            let cell = if grid_row >= 0 && grid_col >= 0 {
                grid.cell(grid_col as u16, grid_row as u16)
                    .copied()
                    .unwrap_or(blank)
            } else {
                blank
            };
            screen.set(col, row, cell);
        }
    }
}

/// Copy mode: the history rows above the live grid, then the selection (inverse) and search
/// matches (underlined) as highlights over whatever the row painted.
fn draw_copy_view(
    screen: &mut Screen,
    view: &CopyView,
    grid: &Grid,
    origin: CellRect,
    visible: CellRect,
    bg: WireColor,
) {
    let plain = Style {
        fg: WireColor::Default,
        bg,
        attrs: 0,
    };
    let blank = ScreenCell::blank(plain);
    let history = view.history_len();
    for row in visible.min.y..visible.max.y {
        let index = view.offset() + (row - origin.min.y).max(0) as usize;
        if let Some(line) = view.history_row(index) {
            let mut chars = line.chars();
            for col in origin.min.x..visible.max.x {
                let cell = chars
                    .next()
                    .map_or(blank, |ch| ScreenCell::glyph(ch, plain));
                if visible.contains(col, row) {
                    screen.set(col, row, cell);
                }
            }
        } else {
            let grid_row = index.saturating_sub(history);
            for col in visible.min.x..visible.max.x {
                let grid_col = col - origin.min.x;
                let cell = u16::try_from(grid_row)
                    .ok()
                    .filter(|_| grid_col >= 0)
                    .and_then(|r| grid.cell(grid_col as u16, r))
                    .copied()
                    .unwrap_or(blank);
                screen.set(col, row, cell);
            }
        }
    }
    let mut highlight = |index: usize, from: usize, to: usize, attr: u8| {
        if index < view.offset() {
            return;
        }
        let row = origin.min.y + (index - view.offset()) as i32;
        for col in from..to {
            let col = origin.min.x + col as i32;
            if visible.contains(col, row)
                && let Some(i) = screen.index(col, row)
                && let Some(cell) = screen.cells.get_mut(i)
            {
                cell.style.attrs |= attr;
            }
        }
    };
    let width = usize::from(grid.cols());
    for &(row, col, len) in view.matches() {
        highlight(row, col, col + len, Style::UNDERLINE);
    }
    if let Some(((r0, c0), (r1, c1))) = view.selection() {
        for row in r0..=r1 {
            let from = if row == r0 { c0 } else { 0 };
            let to = if row == r1 { c1 + 1 } else { width };
            highlight(row, from, to, Style::INVERSE);
        }
    }
}

/// Text keeps whatever background is already painted underneath unless it names its own, so
/// labels sit on their bar's fill. One line, left-aligned, clipped to `visible`.
fn draw_text(
    screen: &mut Screen,
    text: &str,
    style: Style,
    origin: CellRect,
    visible: CellRect,
    bg: WireColor,
) {
    let own_bg = if style.bg == WireColor::Default {
        bg
    } else {
        style.bg
    };
    let row = origin.min.y;
    for (col, ch) in (origin.min.x..visible.max.x).zip(text.chars()) {
        if !visible.contains(col, row) {
            continue;
        }
        let bg = if own_bg == WireColor::Default {
            screen
                .index(col, row)
                .and_then(|i| screen.cells.get(i))
                .map_or(WireColor::Default, |c| c.style.bg)
        } else {
            own_bg
        };
        screen.set(col, row, ScreenCell::glyph(ch, Style { bg, ..style }));
    }
}

/// Writes the difference between `next` and `current` as terminal commands, then makes `next`
/// the current screen. `out` is rebuilt from scratch each paint (the runner hands the buffer
/// back after writing it, so only its capacity survives).
fn emit(painter: &mut Painter, cursor: Option<(u16, u16)>) {
    let out = &mut painter.out;
    out.clear();
    let full = painter.full;
    painter.full = false;
    let mut wrote = false;
    let mut style: Option<Style> = None;
    let mut pos: Option<(i32, i32)> = None;
    if full {
        // Full repaints reset everything the terminal remembers.
        let _ = write!(
            out,
            "{}{}{}",
            Csi::Mode(Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::ShowCursor
            ))),
            Csi::Sgr(Sgr::Reset),
            Csi::Edit(Edit::EraseInDisplay(EraseInDisplay::EraseDisplay)),
        );
        wrote = true;
    }
    let cols = i32::from(painter.next.cols);
    let rows = i32::from(painter.next.rows);
    for row in 0..rows {
        let mut col = 0;
        while col < cols {
            let (Some(next), Some(current)) = (
                painter
                    .next
                    .index(col, row)
                    .and_then(|i| painter.next.cells.get(i)),
                painter
                    .current
                    .index(col, row)
                    .and_then(|i| painter.current.cells.get(i)),
            ) else {
                break;
            };
            if next.width == 0 {
                col += 1;
                continue;
            }
            if !full && next == current {
                col += 1;
                continue;
            }
            if !wrote {
                let _ = write!(
                    out,
                    "{}",
                    Csi::Mode(Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                        DecPrivateModeCode::ShowCursor
                    )))
                );
                wrote = true;
            }
            if pos != Some((col, row)) {
                let _ = write!(out, "{}", cup(col as u16, row as u16));
            }
            if style != Some(next.style) {
                let _ = write!(out, "{}", sgr(next.style));
                style = Some(next.style);
            }
            out.extend_from_slice(next.text.as_str().as_bytes());
            col += i32::from(next.width.max(1));
            pos = Some((col, row));
        }
    }
    if wrote {
        let _ = write!(out, "{}", Csi::Sgr(Sgr::Reset));
    }
    if wrote || cursor != painter.cursor {
        match cursor {
            Some((c, r)) => {
                let _ = write!(
                    out,
                    "{}{}",
                    cup(c, r),
                    Csi::Mode(Mode::SetDecPrivateMode(DecPrivateMode::Code(
                        DecPrivateModeCode::ShowCursor
                    )))
                );
            }
            None => {
                let _ = write!(
                    out,
                    "{}",
                    Csi::Mode(Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                        DecPrivateModeCode::ShowCursor
                    )))
                );
            }
        }
        painter.cursor = cursor;
    }
    core::mem::swap(&mut painter.current, &mut painter.next);
}

pub struct PaintPlugin;

impl Plugin for PaintPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ClipboardPolicy>().add_systems(
            PostUpdate,
            (
                apply_clipboard_policy.after(AssetEventSystems),
                paint
                    .after(UiSystems::PostLayout)
                    .after(UiSystems::Stack)
                    .after(bevy_input_focus::InputFocusSystems::FocusChangeEvents),
            )
                .chain(),
        );
    }
}
