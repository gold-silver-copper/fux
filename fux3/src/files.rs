//! The names and formats of a pane's files, and how to read them back: the
//! one place the holder writes them from and a client, the viewer among
//! them, reads them by.
use crate::Failure;

/// FIFO: raw bytes for the program.
pub const IN: &str = "in";
/// FIFO: commands, a line each.
pub const CTL: &str = "ctl";
/// Line 1 ([`Head`]), then the screen as plain text.
pub const SCREEN: &str = "screen";
/// Line 1 ([`Head`]), then the screen in colour, then its pictures.
pub const STYLED: &str = "screen.styled";
/// One fact a line.
pub const STATUS: &str = "status";
/// Plain text of the lines that scrolled off.
pub const HISTORY: &str = "history";
/// The history before `history` last reached its cap.
pub const HISTORY_OLDER: &str = "history.1";
/// The last refusals, a line each.
pub const ERRORS: &str = "errors";
/// One file per picture shown.
pub const IMAGES: &str = "images";

/// The most columns and rows a pane may have.
pub const MAX_COLS: u16 = 500;
pub const MAX_ROWS: u16 = 200;
/// The longest line `ctl` takes: one write of at most this many bytes
/// reaches a FIFO whole (POSIX's least `PIPE_BUF`).
pub const PIPE_BUF: usize = 512;
/// The most bytes a published screen file may hold; one past it is not
/// written, and `status` says so.
pub const MAX_SCREEN_FILE: usize = 8 << 20;

/// Line 1 of `screen` and `screen.styled`, the same in both: which pane
/// (its id, new at every start) and which of its screens (the version),
/// the size, and the cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub id: String,
    pub version: u64,
    pub cols: u16,
    pub rows: u16,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub cursor_shown: bool,
}

impl Head {
    /// As line 1 is written: `id=… version=… size=CxR cursor=ROW,COL
    /// cursor-shown=yes|no`, rows and columns counted from 0.
    pub fn line(&self) -> String {
        let shown = if self.cursor_shown { "yes" } else { "no" };
        format!(
            "id={} version={} size={}x{} cursor={},{} cursor-shown={shown}",
            self.id, self.version, self.cols, self.rows, self.cursor_row, self.cursor_col
        )
    }

    /// Reads line 1 back.
    pub fn parse(line: &str) -> Result<Head, Failure> {
        let bad = || Failure::new(format!("not a fux3 screen's first line: {line:?}"));
        let mut head = Head {
            id: String::new(),
            version: 0,
            cols: 0,
            rows: 0,
            cursor_row: 0,
            cursor_col: 0,
            cursor_shown: false,
        };
        let mut seen = 0u8;
        for word in line.split(' ') {
            let (key, value) = word.split_once('=').ok_or_else(bad)?;
            match key {
                "id" => head.id = value.to_owned(),
                "version" => head.version = value.parse().map_err(|_| bad())?,
                "size" => (head.cols, head.rows) = pair(value, 'x').ok_or_else(bad)?,
                "cursor" => {
                    (head.cursor_row, head.cursor_col) = pair(value, ',').ok_or_else(bad)?
                }
                "cursor-shown" => head.cursor_shown = value == "yes",
                // A later field is skipped, so a reader outlives an addition.
                _ => continue,
            }
            seen = seen.saturating_add(1);
        }
        match seen >= 5 && !head.id.is_empty() {
            true => Ok(head),
            false => Err(bad()),
        }
    }
}

fn pair<T: std::str::FromStr>(text: &str, sep: char) -> Option<(T, T)> {
    let (a, b) = text.split_once(sep)?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// A picture shown on a published screen, as `screen.styled`'s lines after
/// the screen's rows give it: `picture N.sixel at=ROW,COL cells=CxR
/// cropped=K cell=WxH`. Its top row may be above the screen (negative);
/// the file is in `images/`, its pixels laid out in cells `cell` big.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub file: String,
    pub row: i64,
    pub col: u16,
    pub cols: u16,
    pub rows: u16,
    pub cropped: u16,
    pub cell: (u16, u16),
}

impl Placed {
    pub fn line(&self) -> String {
        format!(
            "picture {} at={},{} cells={}x{} cropped={} cell={}x{}",
            self.file,
            self.row,
            self.col,
            self.cols,
            self.rows,
            self.cropped,
            self.cell.0,
            self.cell.1
        )
    }

    pub fn parse(line: &str) -> Option<Placed> {
        let mut words = line.strip_prefix("picture ")?.split(' ');
        let file = words.next()?.to_owned();
        let good = file
            .strip_suffix(".sixel")?
            .bytes()
            .all(|b| b.is_ascii_digit());
        if !good {
            return None;
        }
        let mut placed = Placed {
            file,
            row: 0,
            col: 0,
            cols: 0,
            rows: 0,
            cropped: 0,
            cell: (0, 0),
        };
        for word in words {
            let (key, value) = word.split_once('=')?;
            match key {
                "at" => {
                    let (row, col) = value.split_once(',')?;
                    (placed.row, placed.col) = (row.parse().ok()?, col.parse().ok()?);
                }
                "cells" => (placed.cols, placed.rows) = pair(value, 'x')?,
                "cropped" => placed.cropped = value.parse().ok()?,
                "cell" => placed.cell = pair(value, 'x')?,
                _ => {}
            }
        }
        Some(placed)
    }
}

/// A published screen read back: line 1, the rows, and with
/// `screen.styled` the pictures.
#[derive(Clone, Debug)]
pub struct Published {
    pub head: Head,
    pub rows: Vec<String>,
    pub pictures: Vec<Placed>,
}

/// Reads a `screen` or `screen.styled` file's text: line 1, then exactly
/// the rows its size says, then picture lines.
pub fn parse_screen(text: &str) -> Result<Published, Failure> {
    let mut lines = text.split('\n');
    let head = Head::parse(lines.next().unwrap_or_default())?;
    let rows: Vec<String> = lines
        .by_ref()
        .take(usize::from(head.rows))
        .map(str::to_owned)
        .collect();
    if rows.len() != usize::from(head.rows) {
        let why = format!(
            "{} rows of {}: the file is cut short",
            rows.len(),
            head.rows
        );
        return Err(Failure::new(why));
    }
    let pictures = lines.filter_map(Placed::parse).collect();
    Ok(Published {
        head,
        rows,
        pictures,
    })
}

/// `status` read back: each line's first word and the rest.
pub fn parse_status(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            (!key.is_empty()).then(|| (key.to_owned(), value.to_owned()))
        })
        .collect()
}

/// The first value of `key` in a read-back `status`.
pub fn status_value<'a>(status: &'a [(String, String)], key: &str) -> Option<&'a str> {
    status
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Reads a regular file of at most `most` bytes, never through a link, a
/// FIFO or a device: what a client reads a pane's files with.
pub fn read_small(path: &std::path::Path, most: usize) -> Result<Vec<u8>, Failure> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::File::options()
        .read(true)
        .custom_flags(fuxix::file::NOFOLLOW | fuxix::file::NONBLOCK)
        .open(path)
        .map_err(|e| crate::io(path.display(), e))?;
    let meta = file.metadata().map_err(|e| crate::io(path.display(), e))?;
    if !meta.is_file() {
        return Err(Failure::new(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    let limit = u64::try_from(most).unwrap_or(u64::MAX).saturating_add(1);
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|e| crate::io(path.display(), e))?;
    if bytes.len() > most {
        let why = format!("{} is over {most} bytes", path.display());
        return Err(Failure::new(why));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_one_reads_back_as_written() -> Result<(), Failure> {
        let head = Head {
            id: "0123456789abcdef".to_owned(),
            version: 42,
            cols: 80,
            rows: 24,
            cursor_row: 3,
            cursor_col: 7,
            cursor_shown: true,
        };
        assert_eq!(Head::parse(&head.line())?, head);
        let placed = Placed {
            file: "3.sixel".to_owned(),
            row: -2,
            col: 4,
            cols: 10,
            rows: 5,
            cropped: 1,
            cell: (10, 20),
        };
        assert_eq!(Placed::parse(&placed.line()), Some(placed));
        assert!(Placed::parse("picture ../x.sixel at=0,0").is_none());
        Ok(())
    }
}
