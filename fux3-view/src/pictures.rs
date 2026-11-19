//! Pictures: the files `images/N.sixel` a screen names, read back into
//! pixels by a fux-vt terminal of their own, and drawn in sixel over their
//! tiles, cropped to the cells that show, when this terminal draws sixel.
use crate::render::Tile;
use fux_vt::sixel::Picture;
use fux_vt::{CellSize, Feature, Graphics, Options, Parser, Size};
use fux3::files::{MAX_COLS, MAX_ROWS, Placed};
use std::path::Path;
use std::sync::Arc;

/// The largest picture file read, and the pixels a terminal reading one
/// keeps.
const MAX_FILE: usize = 16 << 20;
const MAX_PIXEL_BYTES: usize = 64 << 20;

/// The pixels of the picture file `path`, laid out as `placed` says.
pub fn load(path: &Path, placed: &Placed) -> Option<Arc<Picture>> {
    let bytes = fux3::files::read_small(path, MAX_FILE).ok()?;
    let cell = CellSize::new(placed.cell.0, placed.cell.1)?;
    let rows = placed
        .rows
        .saturating_add(placed.cropped)
        .saturating_add(1)
        .min(MAX_ROWS);
    let size = Size::new(rows, placed.cols.saturating_add(1).min(MAX_COLS)).ok()?;
    let mut parser = Parser::with_options(size, 0, Options::new().with(Feature::Pictures)).ok()?;
    parser.set_picture_bytes(MAX_PIXEL_BYTES);
    parser.set_host_graphics(Graphics::Sixel(cell));
    parser.process_all(&bytes, &mut ());
    parser
        .screen()
        .pictures()
        .next()
        .map(|shown| Arc::clone(shown.picture))
}

/// A picture as drawn: where (the cell of its top left), which pixels,
/// and from which file of which pane.
#[derive(Clone)]
pub struct Drawn {
    pub at: (u16, u16),
    pub crop: (u32, u32, u32, u32),
    pub file: String,
    pub picture: Arc<Picture>,
}

impl PartialEq for Drawn {
    fn eq(&self, other: &Drawn) -> bool {
        (self.at, self.crop, &self.file) == (other.at, other.crop, &other.file)
            && Arc::ptr_eq(&self.picture, &other.picture)
    }
}

/// Where `placed` shows in `tile`, its pane's rows from `first` on shown
/// and no row at or past `bottom` (the bar's) drawn on: none if no cell of
/// it shows.
pub fn place(
    tile: Tile,
    first: u16,
    bottom: u16,
    placed: &Placed,
    picture: &Arc<Picture>,
) -> Option<Drawn> {
    let (cw, ch) = (u32::from(placed.cell.0), u32::from(placed.cell.1));
    let shown_top = i64::from(first);
    let last_row = i64::from(bottom.saturating_sub(tile.y))
        .min(i64::from(tile.h))
        .saturating_add(shown_top);
    let top = placed
        .row
        .saturating_add(placed.cropped.into())
        .max(shown_top)
        .max(0);
    let end = placed.row.saturating_add(placed.rows.into()).min(last_row);
    let right = placed.col.saturating_add(placed.cols).min(tile.w);
    if top >= end || placed.col >= right {
        return None;
    }
    let small = |n: i64| u32::try_from(n).unwrap_or(0);
    let crop = (
        0,
        small(top.saturating_sub(placed.row)).saturating_mul(ch),
        u32::from(right.saturating_sub(placed.col)).saturating_mul(cw),
        small(end.saturating_sub(top)).saturating_mul(ch),
    );
    let y = tile
        .y
        .saturating_add(u16::try_from(top.saturating_sub(shown_top)).unwrap_or(0));
    let file = placed.file.clone();
    let picture = Arc::clone(picture);
    Some(Drawn {
        at: (tile.x.saturating_add(placed.col), y),
        crop,
        file,
        picture,
    })
}

/// The bytes that draw `drawn`, the cursor kept where it was.
pub fn draw(drawn: &[Drawn], out: &mut Vec<u8>) {
    for one in drawn {
        let (x, y) = one.at;
        out.extend_from_slice(
            format!("\x1b7\x1b[{};{}H", y.saturating_add(1), x.saturating_add(1)).as_bytes(),
        );
        fux_vt::sixel::encode(&one.picture, one.crop, out);
        out.extend_from_slice(b"\x1b8");
    }
}
