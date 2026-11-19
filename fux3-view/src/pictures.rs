//! Pictures: the files `images/N.sixel` a screen names are sixel already.
//! One that shows whole in its tile is written as it is, at its place,
//! when this terminal draws sixel; one cut by its tile's edge, the bar or
//! the top of what shows is left blank, as its cells are.
use crate::render::Tile;
use fux3::files::Placed;
use std::path::{Path, PathBuf};

/// The largest picture file drawn.
const MAX_FILE: usize = 16 << 20;

/// A picture as drawn: the cell of its top left, and its file.
#[derive(Clone, PartialEq, Eq)]
pub struct Drawn {
    pub at: (u16, u16),
    pub path: PathBuf,
}

/// Where `placed`, a picture of the pane in `dir`, shows whole in `tile`,
/// the pane's rows from `first` on shown and nothing drawn at or past row
/// `bottom` (the bar's).
pub fn place(tile: Tile, first: u16, bottom: u16, dir: &Path, placed: &Placed) -> Option<Drawn> {
    let shown = tile.h.min(bottom.saturating_sub(tile.y));
    let top = u16::try_from(placed.row).ok()?.checked_sub(first)?;
    let whole = placed.cropped == 0
        && top.checked_add(placed.rows).is_some_and(|end| end <= shown)
        && placed
            .col
            .checked_add(placed.cols)
            .is_some_and(|end| end <= tile.w);
    whole.then(|| Drawn {
        at: (
            tile.x.saturating_add(placed.col),
            tile.y.saturating_add(top),
        ),
        path: dir.join(fux3::files::IMAGES).join(&placed.file),
    })
}

/// The bytes that draw `drawn`, the cursor kept where it was; a file that
/// cannot be read is left out.
pub fn draw(drawn: &[Drawn], out: &mut Vec<u8>) {
    for one in drawn {
        let Ok(sixel) = fux3::files::read_small(&one.path, MAX_FILE) else {
            continue;
        };
        let (x, y) = one.at;
        out.extend_from_slice(
            format!("\x1b7\x1b[{};{}H", y.saturating_add(1), x.saturating_add(1)).as_bytes(),
        );
        out.extend_from_slice(&sixel);
        out.extend_from_slice(b"\x1b8");
    }
}
