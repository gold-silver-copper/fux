//! An image previewer's test-bed on fux-vt: a sixel encoder for PPM files,
//! and a preview that feeds a terminal's output to `fux_vt::Parser` at a
//! cell size, then reports what pictures it kept, where, at what size, and
//! which were lost or cut on the way.

use fux_vt::sixel::Picture;
use fux_vt::{CellSize, Feature, Graphics, Options, Parser, Size};
use std::fmt::Write as _;
use std::sync::Arc;

pub type Res<T> = Result<T, String>;

/// An RGB image, row by row from the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<[u8; 3]>,
}

/// The most pixels a PPM may have here: what a sixel picture keeps.
const MAX_PIXELS: u64 = 4096 * 2160;

/// Reads a PPM: binary (`P6`) or plain (`P3`), any maxval up to 65535.
pub fn parse_ppm(data: &[u8]) -> Res<Image> {
    let mut at = 0usize;
    // The header's next token, skipping blanks and `#` comments.
    let mut token = |data: &[u8]| -> Res<String> {
        loop {
            match data.get(at) {
                Some(b'#') => {
                    while data.get(at).is_some_and(|b| *b != b'\n') {
                        at = at.saturating_add(1);
                    }
                }
                Some(b) if b.is_ascii_whitespace() => at = at.saturating_add(1),
                Some(_) => break,
                None => return Err("PPM ends inside its header".to_owned()),
            }
        }
        let start = at;
        while data.get(at).is_some_and(|b| !b.is_ascii_whitespace()) {
            at = at.saturating_add(1);
        }
        Ok(String::from_utf8_lossy(data.get(start..at).unwrap_or_default()).into_owned())
    };
    let magic = token(data)?;
    let number = |s: String, what: &str| -> Res<u32> {
        s.parse()
            .map_err(|_| format!("PPM {what} {s:?} is not a number"))
    };
    let width = number(token(data)?, "width")?;
    let height = number(token(data)?, "height")?;
    let maxval = number(token(data)?, "maxval")?;
    if width == 0 || height == 0 || !(1..=65535).contains(&maxval) {
        return Err(format!("PPM {width}x{height} maxval {maxval} is no image"));
    }
    let count = u64::from(width).saturating_mul(u64::from(height));
    if count > MAX_PIXELS {
        return Err(format!("PPM {width}x{height} is past {MAX_PIXELS} pixels"));
    }
    let count = usize::try_from(count).map_err(|e| e.to_string())?;
    let scale =
        |v: u32| u8::try_from(v.min(maxval).saturating_mul(255) / maxval).unwrap_or(u8::MAX);
    let mut samples: Vec<u32> = Vec::with_capacity(count.saturating_mul(3));
    match magic.as_str() {
        "P6" => {
            // One blank after maxval, then the raster.
            let body = data.get(at.saturating_add(1)..).unwrap_or_default();
            let wide = maxval > 255;
            let each = if wide { 2 } else { 1 };
            if body.len() < count.saturating_mul(3).saturating_mul(each) {
                return Err(format!("PPM raster is short: {} bytes", body.len()));
            }
            if wide {
                let (pairs, _) = body.as_chunks::<2>();
                samples.extend(pairs.iter().map(|c| u32::from(u16::from_be_bytes(*c))));
            } else {
                samples.extend(body.iter().map(|b| u32::from(*b)));
            }
        }
        "P3" => {
            for _ in 0..count.saturating_mul(3) {
                samples.push(number(token(data)?, "sample")?);
            }
        }
        other => return Err(format!("not a PPM (P3 or P6): magic {other:?}")),
    }
    let (triples, _) = samples.as_chunks::<3>();
    let rgb = triples.iter().take(count).map(|c| c.map(scale)).collect();
    Ok(Image { width, height, rgb })
}

/// The image as a sixel string, `DCS 0;1;0 q … ST`, square pixels: its own
/// colours if 256 or fewer, else a 6×7×6 cube.
pub fn encode_sixel(image: &Image) -> Vec<u8> {
    let mut colours: Vec<[u8; 3]> = image.rgb.clone();
    colours.sort_unstable();
    colours.dedup();
    let exact = colours.len() <= 256;
    let (r_n, g_n, b_n) = (6u32, 7u32, 6u32);
    let cube = |[r, g, b]: [u8; 3]| -> usize {
        let step = |c: u8, n: u32| (u32::from(c) * n / 256).min(n - 1);
        usize::try_from((step(r, r_n) * g_n + step(g, g_n)) * b_n + step(b, b_n)).unwrap_or(0)
    };
    let palette: Vec<[u8; 3]> = if exact {
        colours
    } else {
        let level = |i: u32, n: u32| u8::try_from(i * 255 / (n - 1)).unwrap_or(u8::MAX);
        (0..r_n)
            .flat_map(|r| {
                (0..g_n).flat_map(move |g| {
                    (0..b_n).map(move |b| [level(r, r_n), level(g, g_n), level(b, b_n)])
                })
            })
            .collect()
    };
    let index = |rgb: [u8; 3]| {
        if exact {
            palette.binary_search(&rgb).unwrap_or(0)
        } else {
            cube(rgb)
        }
    };
    let mut out = format!("\x1bP0;1;0q\"1;1;{};{}", image.width, image.height);
    for (i, [r, g, b]) in palette.iter().enumerate() {
        let pct = |c: u8| (u32::from(c) * 100 + 127) / 255;
        let _ = write!(out, "#{i};2;{};{};{}", pct(*r), pct(*g), pct(*b));
    }
    let width = usize::try_from(image.width).unwrap_or(0);
    let height = usize::try_from(image.height).unwrap_or(0);
    let mut row_bits = vec![0u8; width];
    for top in (0..height).step_by(6) {
        // The colours in this band, each drawn in one pass, `$` between.
        let mut used: Vec<usize> = (top..(top + 6).min(height))
            .flat_map(|y| (0..width).filter_map(move |x| image.rgb.get(y * width + x)))
            .map(|p| index(*p))
            .collect();
        used.sort_unstable();
        used.dedup();
        for (n, colour) in used.iter().enumerate() {
            row_bits.fill(0);
            for bit in 0..6 {
                let y = top + bit;
                for (x, bits) in row_bits.iter_mut().enumerate() {
                    if image
                        .rgb
                        .get(y * width + x)
                        .is_some_and(|p| index(*p) == *colour)
                        && y < height
                    {
                        *bits |= 1 << bit;
                    }
                }
            }
            if n > 0 {
                out.push('$');
            }
            let _ = write!(out, "#{colour}");
            push_runs(&mut out, &row_bits);
        }
        out.push('-');
    }
    out.push_str("\x1b\\");
    out.into_bytes()
}

/// Sixel characters for `bits`, runs of four or more as `!count`.
fn push_runs(out: &mut String, bits: &[u8]) {
    let mut i = 0;
    while let Some(&b) = bits.get(i) {
        let run = bits
            .get(i..)
            .unwrap_or_default()
            .iter()
            .take_while(|x| **x == b)
            .count();
        let ch = char::from(b'?' + b);
        if run >= 4 {
            let _ = write!(out, "!{run}{ch}");
        } else {
            out.extend(std::iter::repeat_n(ch, run));
        }
        i += run;
    }
}

/// What a sink heard while the output was fed.
#[derive(Default)]
struct Heard {
    placed: u64,
    lost_or_cut: u64,
    replies: u64,
}

impl fux_vt::Sink for Heard {
    fn picture(&mut self, placed: bool) {
        if placed {
            self.placed += 1;
        } else {
            self.lost_or_cut += 1;
        }
    }
    fn reply(&mut self, _bytes: &[u8]) {
        self.replies += 1;
    }
}

/// The terminal a preview feeds.
#[derive(Clone, Copy, Debug)]
pub struct Setup {
    pub rows: u16,
    pub cols: u16,
    pub cell: (u16, u16),
    pub history_bytes: usize,
}

/// A picture as the screen shows it, copied out of a `Shown`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub number: usize,
    pub row: i64,
    pub col: u16,
    pub cols: u16,
    pub rows: u16,
    pub cropped: u16,
    pub width: u32,
    pub height: u32,
}

/// What a preview found.
pub struct Report {
    pub text: String,
    pub shown: Vec<Placed>,
    pub placed: u64,
    pub lost_or_cut: u64,
    pub let_go: usize,
    pub parser: Parser,
}

/// Picture strings of protocols fux-vt keeps none of, found by their
/// introducers: the host is told nothing of them.
fn other_protocols(data: &[u8]) -> Vec<(&'static str, usize)> {
    let kinds: [(&str, &[u8]); 3] = [
        ("kitty graphics (APC G)", b"\x1b_G"),
        ("iTerm2 image (OSC 1337;File=)", b"\x1b]1337;File="),
        ("ReGIS (DCS p)", b"\x1bPp"),
    ];
    kinds
        .iter()
        .map(|(name, intro)| {
            (
                *name,
                data.windows(intro.len()).filter(|w| w == intro).count(),
            )
        })
        .filter(|(_, n)| *n > 0)
        .collect()
}

/// Feeds `data` to a parser set up as `setup` says, a line or string at a
/// time, noting each picture as it appears and as it is let go.
pub fn preview(data: &[u8], setup: Setup) -> Res<Report> {
    let size = Size::new(setup.rows, setup.cols).map_err(|e| e.to_string())?;
    let cell = CellSize::new(setup.cell.0, setup.cell.1).ok_or(format!(
        "cell {}x{} is under the least, {:?}",
        setup.cell.0,
        setup.cell.1,
        CellSize::LEAST
    ))?;
    let options = Options::from(Feature::Pictures);
    let mut parser =
        Parser::with_options(size, setup.history_bytes, options).map_err(|e| e.to_string())?;
    parser.set_host_graphics(Graphics::Sixel(cell));
    let mut heard = Heard::default();
    let mut text = String::new();
    let _ = writeln!(
        text,
        "terminal {}x{} cells of {}x{} px, {} bytes of history; input {} bytes",
        setup.rows,
        setup.cols,
        setup.cell.0,
        setup.cell.1,
        setup.history_bytes,
        data.len()
    );
    // Each live picture by the `Arc` of its pixels, held here so that no
    // newer picture can take its address, numbered as first seen.
    let mut live: Vec<(usize, Arc<Picture>)> = Vec::new();
    let mut numbered = 0usize;
    let mut let_go = 0usize;
    let mut offset = 0usize;
    for chunk in chunks(data) {
        offset += chunk.len();
        let before = heard.lost_or_cut;
        let _ = parser.process_all(chunk, &mut heard); // counted in Parser::refused
        if heard.lost_or_cut > before {
            let _ = writeln!(
                text,
                "@{offset}: a picture ended not placed whole (lost or cut)"
            );
        }
        let now: Vec<(usize, fux_vt::Shown<'_>)> = parser
            .screen()
            .pictures()
            .map(|s| {
                let held = live.iter().find(|(_, p)| Arc::ptr_eq(p, s.picture));
                let n = held.map_or_else(
                    || {
                        numbered += 1;
                        numbered
                    },
                    |(n, _)| *n,
                );
                (n, s)
            })
            .collect();
        for (n, s) in &now {
            if !live.iter().any(|(m, _)| m == n) {
                let _ = writeln!(
                    text,
                    "@{offset}: picture #{n} shown at row {} col {}, {}x{} cells, {}x{} px",
                    s.row,
                    s.col,
                    s.cols,
                    s.rows,
                    s.picture.width(),
                    s.picture.height()
                );
            }
        }
        for (n, _) in &live {
            if !now.iter().any(|(m, _)| m == n) {
                let_go += 1;
                let _ = writeln!(
                    text,
                    "@{offset}: picture #{n} let go (text, an erase, scrolled out, or covered)"
                );
            }
        }
        live = now
            .iter()
            .map(|(n, s)| (*n, Arc::clone(s.picture)))
            .collect();
    }
    for (name, n) in other_protocols(data) {
        let _ = writeln!(
            text,
            "dropped: {n} {name} string(s); fux-vt keeps sixel alone"
        );
    }
    let shown: Vec<Placed> = parser
        .screen()
        .pictures()
        .map(|s| Placed {
            number: (live.iter())
                .find(|(_, p)| Arc::ptr_eq(p, s.picture))
                .map_or(0, |(n, _)| *n),
            row: s.row,
            col: s.col,
            cols: s.cols,
            rows: s.rows,
            cropped: s.cropped,
            width: s.picture.width(),
            height: s.picture.height(),
        })
        .collect();
    let _ = writeln!(
        text,
        "pictures ended: {} placed whole, {} lost or cut (Sink::picture); {} let go since; {} shown now",
        heard.placed,
        heard.lost_or_cut,
        let_go,
        shown.len()
    );
    for p in &shown {
        // Shown says neither: a host works them out.
        let full = p.width.div_ceil(u32::from(setup.cell.0));
        let clipped = if u32::from(p.cols) < full {
            format!(", clipped to {} of {full} columns", p.cols)
        } else {
            String::new()
        };
        let place = if p.row + i64::from(p.rows) <= 0 {
            ", all in history"
        } else if p.row < 0 {
            ", partly in history"
        } else {
            ""
        };
        let _ = writeln!(
            text,
            "  #{}: row {} col {}, {}x{} cells, top {} rows cropped, {}x{} px{clipped}{place}",
            p.number, p.row, p.col, p.cols, p.rows, p.cropped, p.width, p.height
        );
    }
    let (r, c) = parser.screen().cursor_position();
    let _ = writeln!(
        text,
        "cursor at row {r} col {c}; replies {}; refused room {} times",
        heard.replies,
        parser.refused()
    );
    Ok(Report {
        text,
        shown,
        placed: heard.placed,
        lost_or_cut: heard.lost_or_cut,
        let_go,
        parser,
    })
}

/// The input cut after each line feed and each ST, so that pictures are
/// looked for between strings, not inside one.
fn chunks(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = data;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = rest
            .windows(2)
            .position(|w| w == b"\x1b\\")
            .map(|i| i + 2)
            .into_iter()
            .chain(rest.iter().position(|b| *b == b'\n').map(|i| i + 1))
            .min()
            .unwrap_or(rest.len());
        let (chunk, tail) = rest.split_at(end);
        rest = tail;
        Some(chunk)
    })
}

/// The screen as a map: `#` where a picture's cell is, the text elsewhere.
pub fn map(parser: &Parser) -> String {
    let mut out = String::new();
    for (_, row) in parser.screen().window().iter() {
        let line: String = row
            .cells()
            .map(|c| match c.contents() {
                fux_vt::PLACEHOLDER => "#".to_owned(),
                "" => " ".to_owned(),
                s => s.to_owned(),
            })
            .collect();
        out.push('|');
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Each shown picture drawn with half blocks in 24-bit colour, a cell for
/// a cell, sampled from `Picture::pixel`: for a terminal without sixel.
pub fn blocks(parser: &Parser) -> String {
    let mut out = String::new();
    for s in parser.screen().pictures() {
        let (cw, ch) = (u32::from(s.cell.0), u32::from(s.cell.1));
        let _ = writeln!(out, "picture at row {} col {}:", s.row, s.col);
        let sample = |x: u32, y: u32| s.picture.pixel(x, y).filter(|p| p[3] != 0);
        for row in u32::from(s.cropped)..u32::from(s.rows) {
            for col in 0..u32::from(s.cols) {
                let x = col * cw + cw / 2;
                let (top, bottom) = (
                    sample(x, row * ch + ch / 4),
                    sample(x, row * ch + ch * 3 / 4),
                );
                match (top, bottom) {
                    (Some(t), Some(b)) => {
                        let _ = write!(
                            out,
                            "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m▀",
                            t[0], t[1], t[2], b[0], b[1], b[2]
                        );
                    }
                    (Some(t), None) => {
                        let _ = write!(out, "\x1b[49m\x1b[38;2;{};{};{}m▀", t[0], t[1], t[2]);
                    }
                    (None, Some(b)) => {
                        let _ = write!(out, "\x1b[49m\x1b[38;2;{};{};{}m▄", b[0], b[1], b[2]);
                    }
                    (None, None) => out.push_str("\x1b[m "),
                }
            }
            out.push_str("\x1b[m\n");
        }
    }
    out
}

/// Each shown picture again as sixel, its cropped rows left out, through
/// fux-vt's own encoder: what a host forwards to a sixel terminal.
pub fn resixel(parser: &Parser) -> Vec<u8> {
    let mut out = Vec::new();
    for s in parser.screen().pictures() {
        let top = u32::from(s.cropped) * u32::from(s.cell.1);
        fux_vt::sixel::encode(
            s.picture,
            (0, top, s.picture.width(), s.picture.height()),
            &mut out,
        );
        out.push(b'\n');
    }
    out
}

/// A gradient image, `width` by `height`.
pub fn gradient(width: u32, height: u32) -> Image {
    let rgb = (0..height)
        .flat_map(|y| {
            (0..width).map(move |x| {
                let c = |v: u32, of: u32| u8::try_from(v * 255 / of.max(1)).unwrap_or(u8::MAX);
                [c(x, width), c(y, height), 128]
            })
        })
        .collect();
    Image { width, height, rgb }
}

/// A test-bed stream for a terminal of `cell`-sized cells: a picture kept,
/// one text writes over, one scrolled partly off, one cut at the screen's
/// right edge, one cancelled, and the two other protocols.
pub fn demo(rows: u16, cols: u16, cell: (u16, u16)) -> Vec<u8> {
    let (cw, ch) = (u32::from(cell.0), u32::from(cell.1));
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b[H\x1b[2J# kept: 4x2 cells\r\n");
    out.extend(encode_sixel(&gradient(4 * cw, 2 * ch)));
    out.extend_from_slice(b"\r\n# overwritten by text next\r\n");
    out.extend(encode_sixel(&gradient(3 * cw, ch)));
    out.extend_from_slice(b"\x1b[A\rtext over the picture\r\n");
    out.extend_from_slice(b"# cut at the right edge\r\n");
    out.extend_from_slice(format!("\x1b[{}G", cols.saturating_sub(2)).as_bytes());
    out.extend(encode_sixel(&gradient(6 * cw, ch)));
    out.extend_from_slice(b"\r\n# cancelled with CAN\r\n\x1bPq#1;2;100;0;0~~~~\x18\r\n");
    out.extend_from_slice(
        b"# cut: 5000 sixels wide, past 4096\r\n\x1bP0;1;0q#1;2;100;0;0!5000~\x1b\\\r\n",
    );
    out.extend_from_slice(b"# kitty and iTerm2 pictures\r\n\x1b_Gf=24,s=1,v=1;AAAA\x1b\\\x1b]1337;File=inline=1:AAAA\x07\r\n");
    // Enough lines to push the first picture's top off the screen.
    for i in 0..rows.saturating_sub(4) {
        out.extend_from_slice(format!("line {i}\r\n").as_bytes());
    }
    out
}
