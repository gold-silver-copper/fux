//! Cuts a part out of a sixel picture: the picture is read into the pixels of one rectangle of
//! it, and those are written as a sixel picture again. It knows nothing of terminals, of files
//! or of the rest of fux, and every step of it has a bound.

use std::fmt::Write as _;

/// The most pixels a part may have: a screen of 3840 by 2160 has fewer.
pub const PIXELS_MAX: usize = 1 << 23;
/// The most colours a part may have, and the highest colour register but one.
pub const COLOURS_MAX: usize = 256;
/// How many times over a part's pixels may be painted, on average.
const PAINTED_MAX: usize = 16;

/// What a pixel was painted with: red, green and blue in hundredths, as sixel says them, or a
/// register the picture never gave a colour (the terminal's own colour for it).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Colour {
    Rgb([u8; 3]),
    Register(u8),
}

/// A rectangle of a picture's pixels, row after row: 0 for a pixel never painted, else one more
/// than its colour's place in `colours`. A row is `stride` long, of which `w` are the picture's.
pub struct Part {
    pub w: usize,
    pub h: usize,
    pub stride: usize,
    pub colours: Vec<Colour>,
    pub pixels: Vec<u16>,
}

/// The part of a sixel picture (what is between `ESC P` and `ESC \`) that lies in the rectangle
/// `left top width height` of its pixels, as a sixel picture of its own, of at most `max` bytes
/// (nothing, if no pixel of it is there). Or why not, in a few words.
pub fn cut(data: &[u8], rectangle: [usize; 4], max: usize) -> Result<String, &'static str> {
    read(data, rectangle)?.write(max)
}

/// Reads the pixels of one rectangle of a picture. The rest of the picture is only measured:
/// what it paints and the colours it paints with are no part of the rectangle's bounds, and
/// reading it is one step for each byte.
pub fn read(data: &[u8], rectangle: [usize; 4]) -> Result<Part, &'static str> {
    scan(data, rectangle).1
}

/// The size of a picture in pixels: the larger of what its raster attributes say and what its
/// data paints. And whether its pixels are square: whether its raster attributes, if it has
/// any, give a pixel's height and width as the same number.
pub fn size(data: &[u8]) -> (usize, usize, bool) {
    scan(data, [0; 4]).0
}

/// One pass over a picture: its size and squareness, and the pixels of one rectangle of it (or
/// why they are not read: from there on the picture is only measured).
fn scan(data: &[u8], [left, top, width, height]: [usize; 4]) -> ((usize, usize, bool), Result<Part, &'static str>) {
    let area = width.checked_mul(height).filter(|area| *area <= PIXELS_MAX);
    let mut wrong = area.is_none().then_some("its part in the tile is over 8 million pixels");
    let area = area.unwrap_or(0);
    let (right, bottom) = (left.saturating_add(width), top.saturating_add(height));
    let mut pixels = vec![0u16; area];
    let mut paints = area.saturating_mul(PAINTED_MAX);
    // The colour each register has now, and the pen it paints with (0: not looked up since).
    let (mut registers, mut pens) = ([None::<[u8; 3]>; COLOURS_MAX], [0u16; COLOURS_MAX]);
    let mut colours: Vec<Colour> = Vec::new();
    let (mut register, mut x, mut band, mut size) = (0usize, 0usize, 0usize, (0usize, 0usize));
    // (A repeat count is the next command's, whatever that is.)
    let (mut repeat, mut square) = (1, true);
    let body = data.iter().position(|&b| b == b'q').and_then(|q| data.get(q + 1..)).unwrap_or_default();
    // Each command with the numbers after it: `!` a repeat count, `#` a colour, `"` the raster attributes.
    for command in body.chunk_by(|_, next| next.is_ascii_digit() || *next == b';') {
        let Some((&byte, numbers)) = command.split_first() else { continue };
        let digit = |n: usize, digit: &u8| n.saturating_mul(10).saturating_add(usize::from(digit - b'0')).min(1 << 20);
        let mut each = numbers.split(|&b| b == b';').map(|number| number.iter().fold(0, digit));
        let [n0, n1, n2, n3, n4] = std::array::from_fn(|_| each.next().unwrap_or(0));
        let repeat = std::mem::replace(&mut repeat, if byte == b'!' { n0.max(1) } else { 1 });
        match byte {
            b'"' => (size, square) = ((size.0.max(n2), size.1.max(n3)), square && n0 == n1),
            b'#' => {
                register = n0;
                // A colour given in hue, lightness and saturation (1) or in red, green and blue (2).
                if let Some(slot) = registers.get_mut(register)
                    && numbers.split(|&b| b == b';').count() == 5
                    && (1..=2).contains(&n1)
                {
                    *slot = Some(if n1 == 1 { hls([n2, n3, n4]) } else { [n2, n3, n4].map(|n| n.min(100) as u8) });
                    pens.get_mut(register).into_iter().for_each(|pen| *pen = 0);
                } else if register >= COLOURS_MAX {
                    wrong = wrong.or(Some("it names a colour register over 255"));
                }
            }
            b'?'..=b'~' => {
                let (bits, from, to) = (byte - b'?', x.max(left), x.saturating_add(repeat).min(right));
                x = x.saturating_add(repeat);
                // (As wide as its rows are long, and as high as its lowest painted pixel.)
                let low = band.saturating_mul(6).saturating_add(8 - bits.leading_zeros() as usize);
                size = (size.0.max(x), if bits == 0 { size.1 } else { size.1.max(low) });
                let rows = (0..6).filter(|k| bits >> k & 1 == 1).map(|k| band.saturating_mul(6).saturating_add(k));
                let rows = rows.filter(|row| (top..bottom).contains(row));
                let painted = (to.saturating_sub(from)).saturating_mul(rows.clone().count());
                if painted == 0 || wrong.is_some() {
                    continue;
                }
                let Some(left_to_paint) = paints.checked_sub(painted) else {
                    wrong = Some("it paints its part in the tile over too many times");
                    continue;
                };
                paints = left_to_paint;
                let mut pen = pens.get(register).copied().unwrap_or(0);
                if pen == 0 {
                    let colour = registers.get(register).copied().flatten();
                    let colour = colour.map_or(Colour::Register(register as u8), Colour::Rgb);
                    let known = colours.iter().position(|c| *c == colour);
                    let place = known.unwrap_or_else(|| {
                        colours.push(colour);
                        colours.len() - 1
                    });
                    if place >= COLOURS_MAX {
                        wrong = Some("its part in the tile has more than 256 colours");
                        continue;
                    }
                    pen = place as u16 + 1;
                    pens.get_mut(register).into_iter().for_each(|slot| *slot = pen);
                }
                for start in rows.map(|row| (row - top) * width) {
                    let cells = pixels.get_mut(start + (from - left)..start + (to - left));
                    cells.into_iter().for_each(|cells| cells.fill(pen));
                }
            }
            b'$' => x = 0,
            b'-' => (x, band) = (0, band.saturating_add(1)),
            _ => {}
        }
    }
    // The picture is as large as the larger of what it says and what it paints.
    let (w, h) = (size.0.min(right).saturating_sub(left), size.1.min(bottom).saturating_sub(top));
    ((size.0, size.1, square), wrong.map_or(Ok(Part { w, h, stride: width, colours, pixels }), Err))
}

/// Sixel's hue, lightness and saturation as red, green and blue: its hue starts at blue, and red
/// is at 120 degrees.
fn hls([h, l, s]: [usize; 3]) -> [u8; 3] {
    let (l, s) = (l.min(100) as f32 / 100.0, s.min(100) as f32 / 100.0);
    let reach = s * l.min(1.0 - l);
    [0.0, 8.0, 4.0].map(|n| {
        let k = (n + ((h % 360 + 240) % 360) as f32 / 30.0) % 12.0;
        ((l - reach * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)) * 100.0).round() as u8
    })
}

/// One colour's row of a band while it is written: its text, how many columns that and the run
/// not yet written cover, and that run (its six pixels, how many columns).
#[derive(Default)]
struct Row {
    text: String,
    end: usize,
    bits: u8,
    count: usize,
}

impl Row {
    fn run(&mut self, bits: u8, columns: usize) {
        if bits != self.bits {
            self.flush();
            self.bits = bits;
        }
        (self.count, self.end) = (self.count + columns, self.end + columns);
    }

    fn flush(&mut self) {
        let sixel = char::from(b'?' + (self.bits & 63));
        match std::mem::take(&mut self.count) {
            count @ 0..=3 => self.text.extend(std::iter::repeat_n(sixel, count)),
            count => _ = write!(self.text, "!{count}{sixel}"),
        }
    }
}

impl Part {
    /// The colour of a pixel as its number: 0 if it was never painted or is not in the part.
    fn pen(&self, x: usize, y: usize) -> u16 {
        let at = y.checked_mul(self.stride).filter(|_| x < self.w && y < self.h).and_then(|at| at.checked_add(x));
        at.and_then(|at| self.pixels.get(at)).copied().unwrap_or(0)
    }

    /// The part as a sixel picture of exactly its size, with square pixels. A pixel never
    /// painted is left unpainted, and the picture says that such a pixel keeps what is under it.
    pub fn write(&self, max: usize) -> Result<String, &'static str> {
        if self.w == 0 || self.h == 0 {
            return Ok(String::new());
        }
        let mut out = format!("0;1q\"1;1;{};{}", self.w, self.h);
        // A register the picture gave no colour keeps its number; the colours take the other numbers.
        let mut free = (0..=u8::MAX).filter(|n| !self.colours.contains(&Colour::Register(*n)));
        let mut names = Vec::new();
        for colour in &self.colours {
            names.push(match colour {
                Colour::Register(n) => *n,
                Colour::Rgb([r, g, b]) => {
                    let n = free.next().ok_or("its part in the tile has more than 256 colours")?;
                    let _ = write!(out, "#{n};2;{r};{g};{b}");
                    n
                }
            });
        }
        let mut rows: Vec<Row> = self.colours.iter().map(|_| Row::default()).collect();
        for band in 0..self.h.div_ceil(6) {
            for x in 0..self.w {
                let column: [u16; 6] = std::array::from_fn(|k| self.pen(x, band * 6 + k));
                for (k, pen) in column.iter().enumerate() {
                    // Each colour of the column once, with all its pixels there.
                    if *pen == 0 || column.iter().take(k).any(|above| above == pen) {
                        continue;
                    }
                    let bits = (0..6).zip(&column).filter(|(_, p)| *p == pen).fold(0, |bits, (k, _)| bits | 1 << k);
                    if let Some(row) = rows.get_mut(usize::from(*pen) - 1) {
                        if x > row.end {
                            row.run(0, x - row.end);
                        }
                        row.run(bits, 1);
                    }
                }
            }
            if band > 0 {
                out.push('-');
            }
            for (row, name) in rows.iter_mut().zip(&names).filter(|(row, _)| row.end > 0) {
                row.flush();
                let _ = write!(out, "#{name}{}$", row.text);
                row.text.clear();
                (row.end, row.bits) = (0, 0);
            }
            if out.len() > max {
                return Err("its part in the tile is over 4 MiB of sixel");
            }
        }
        Ok(out)
    }
}
