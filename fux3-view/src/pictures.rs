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

/// Whether `bytes` are one device control string and nothing else: they
/// start with `ESC P` and hold no other `ESC` but the closing `ESC \`, so
/// nothing in a picture file reaches the terminal as another sequence.
fn one_sixel(bytes: &[u8]) -> bool {
    let body = bytes
        .strip_prefix(b"\x1bP")
        .and_then(|rest| rest.strip_suffix(b"\x1b\\"));
    body.is_some_and(|body| !body.contains(&0x1b))
}

/// The bytes that draw `drawn`, the cursor kept where it was; a file that
/// cannot be read, or is not one sixel alone, is left out.
pub fn draw(drawn: &[Drawn], out: &mut Vec<u8>) {
    for one in drawn {
        let Ok(sixel) = fux3::files::read_small(&one.path, MAX_FILE) else {
            continue;
        };
        if !one_sixel(&sixel) {
            continue;
        }
        let (x, y) = one.at;
        out.extend_from_slice(
            format!("\x1b7\x1b[{};{}H", y.saturating_add(1), x.saturating_add(1)).as_bytes(),
        );
        out.extend_from_slice(&sixel);
        out.extend_from_slice(b"\x1b8");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_one_device_control_string_is_a_picture() {
        assert!(super::one_sixel(b"\x1bPq#0~-\x1b\\"));
        for wrong in [
            &b"q#0~\x1b\\"[..],
            b"\x1bPq#0~",
            b"\x1bPq\x1b[2J\x1b\\",
            b"\x1bPq\x1b\\\x1b]0;title\x07",
        ] {
            assert!(!super::one_sixel(wrong), "{wrong:?}");
        }
    }
}
