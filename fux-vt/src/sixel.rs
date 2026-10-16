//! Sixel graphics (DEC STD 070, the VT340's; xterm's ctlseqs, "Sixel
//! Graphics"): the data of a `DCS P1 ; P2 ; P3 q … ST`, decoded as it
//! comes into a picture of RGBA pixels. Pixels no sixel paints stay clear.

use std::io::Write;
use std::sync::Arc;

/// The most pixels across or down a picture may have: a 4K screen's.
pub const MAX_SIDE: u32 = 4096;
/// The most pixels a picture may have in all, which bounds its memory.
const MAX_PIXELS: usize = 4096 * 2160;

/// A picture a program drew: its size in pixels, and its pixels, RGBA, row
/// by row from the top.
#[derive(Debug, PartialEq, Eq)]
pub struct Picture {
    /// Its width in pixels.
    pub width: u32,
    /// Its height in pixels.
    pub height: u32,
    /// Its pixels, four bytes each: red, green, blue and alpha, 0 clear.
    pub rgba: Vec<u8>,
}

impl Picture {
    /// The RGBA of the pixel at `x`, `y`, if it is in the picture.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width {
            return None;
        }
        let at = offset(x, y, self.width)?;
        let bytes = self.rgba.get(at..at.checked_add(4)?)?;
        bytes.try_into().ok()
    }
}

/// The colour cube a picture of more than 256 colours is drawn in: 6
/// reds, 7 greens and 6 blues, numbered red first, then green.
const CUBE: (usize, usize, usize) = (6, 7, 6);

/// Where the pixel at `x`, `y` of a picture `width` wide starts in its RGBA
/// bytes.
fn offset(x: u32, y: u32, width: u32) -> Option<usize> {
    let at = u64::from(y).checked_mul(u64::from(width))?;
    usize::try_from(at.checked_add(u64::from(x))?.checked_mul(4)?).ok()
}

/// Where the decoder is in its data: a run of digits after `!`, `#` or
/// `"`, or between commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    None,
    /// `! Pn ch`: the next sixel `Pn` times.
    Repeat,
    /// `# Pc` selects a colour; `# Pc ; Pu ; Px ; Py ; Pz` defines it.
    Colour,
    /// `" Pan ; Pad ; Ph ; Pv`: the picture's size, which it may draw past.
    Raster,
}

/// Sixel data, decoded as it comes.
#[derive(Clone, Debug)]
pub(crate) struct Decoder {
    /// The colour registers: the VT340's sixteen to begin with.
    palette: [[u8; 3]; 256],
    colour: usize,
    command: Command,
    /// The numbers of the command being read, at most five.
    numbers: [u32; 5],
    count: usize,
    /// Where the next sixel goes: its column, and the top of its band.
    x: u32,
    y: u32,
    /// The size drawn so far, and the pixels, `width` across.
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    /// The data drew past what a picture may hold: it ends with nothing.
    spoilt: bool,
}

impl Decoder {
    pub(crate) fn new() -> Box<Decoder> {
        Box::new(Decoder {
            palette: vt340_palette(),
            colour: 0,
            command: Command::None,
            numbers: [0; 5],
            count: 0,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            pixels: Vec::new(),
            spoilt: false,
        })
    }

    /// Reads one byte of the data.
    pub(crate) fn push(&mut self, byte: u8) {
        if self.spoilt {
            return;
        }
        match byte {
            b'0'..=b'9' if self.command != Command::None => {
                let digit = u32::from(byte.wrapping_sub(b'0'));
                if let Some(n) = self.numbers.get_mut(self.count) {
                    *n = n.saturating_mul(10).saturating_add(digit).min(1 << 20);
                }
                return;
            }
            b';' if self.command != Command::None => {
                self.count = self.count.saturating_add(1);
                return;
            }
            _ => {}
        }
        let repeat = self.end_command();
        match byte {
            b'!' => self.begin(Command::Repeat),
            b'#' => self.begin(Command::Colour),
            b'"' => self.begin(Command::Raster),
            b'$' => self.x = 0,
            b'-' => {
                self.x = 0;
                self.y = self.y.saturating_add(6);
            }
            0x3f..=0x7e => self.sixel(byte.wrapping_sub(0x3f), repeat.unwrap_or(1).max(1)),
            _ => {}
        }
    }

    fn begin(&mut self, command: Command) {
        self.command = command;
        self.numbers = [0; 5];
        self.count = 0;
    }

    /// Carries out the command whose numbers were read: a repeat's count is
    /// for the sixel that follows.
    fn end_command(&mut self) -> Option<u32> {
        let command = std::mem::replace(&mut self.command, Command::None);
        let [a, b, c, d, e] = self.numbers;
        match command {
            Command::None | Command::Raster => None,
            Command::Repeat => Some(a.min(MAX_SIDE)),
            Command::Colour => {
                self.colour = usize::try_from(a).unwrap_or(0).min(255);
                if self.count >= 4 {
                    let rgb = match b {
                        1 => hls(c, d, e),
                        _ => [percent(c), percent(d), percent(e)],
                    };
                    if let Some(slot) = self.palette.get_mut(self.colour) {
                        *slot = rgb;
                    }
                }
                None
            }
        }
    }

    /// Paints `bits`, a sixel's six pixels from the top, `count` times
    /// across, in the current colour.
    fn sixel(&mut self, bits: u8, count: u32) {
        let right = self.x.saturating_add(count);
        if bits != 0 {
            // Down to its lowest pixel painted: the highest bit set.
            let lowest = 6u32.saturating_sub(bits.leading_zeros().saturating_sub(2));
            let bottom = self.y.saturating_add(lowest);
            if !self.grow(right, bottom) {
                self.spoilt = true;
                return;
            }
            let [r, g, b] = self.palette.get(self.colour).copied().unwrap_or([0; 3]);
            for bit in 0..6u32 {
                if bits & (1 << bit) == 0 {
                    continue;
                }
                let y = self.y.saturating_add(bit);
                for x in self.x..right {
                    let at = offset(x, y, self.width).unwrap_or(usize::MAX);
                    if let Some(pixel) = self.pixels.get_mut(at..at.saturating_add(4)) {
                        crate::copy_from(pixel, &[r, g, b, 255]);
                    }
                }
            }
        }
        self.x = right.min(MAX_SIDE);
    }

    /// Makes room for pixels to `right` and `bottom`, keeping those drawn;
    /// false if a picture may not be so large.
    fn grow(&mut self, right: u32, bottom: u32) -> bool {
        if right <= self.width && bottom <= self.height {
            return true;
        }
        let (width, height) = (right.max(self.width), bottom.max(self.height));
        let pixels = u64::from(width).saturating_mul(u64::from(height));
        let pixels = usize::try_from(pixels).unwrap_or(usize::MAX);
        if width > MAX_SIDE || height > MAX_SIDE || pixels > MAX_PIXELS {
            return false;
        }
        let mut grown = vec![0u8; pixels.saturating_mul(4)];
        let old = usize::try_from(self.width).unwrap_or(0).saturating_mul(4);
        let new = usize::try_from(width).unwrap_or(0).saturating_mul(4);
        let rows = usize::try_from(self.height).unwrap_or(0);
        for row in 0..rows {
            let (from, to) = (row.saturating_mul(old), row.saturating_mul(new));
            let source = self.pixels.get(from..from.saturating_add(old));
            if let (Some(source), Some(target)) =
                (source, grown.get_mut(to..to.saturating_add(old)))
            {
                crate::copy_from(target, source);
            }
        }
        (self.width, self.height, self.pixels) = (width, height, grown);
        true
    }

    /// The picture drawn, unless it is empty or ran past what one may be.
    pub(crate) fn finish(mut self: Box<Self>) -> Option<Arc<Picture>> {
        self.end_command();
        if self.spoilt || self.width == 0 || self.height == 0 {
            return None;
        }
        Some(Arc::new(Picture {
            width: self.width,
            height: self.height,
            rgba: std::mem::take(&mut self.pixels),
        }))
    }
}

/// A sixel colour's share, 0 to 100, as a byte.
fn percent(n: u32) -> u8 {
    let scaled = n.min(100).saturating_mul(255).checked_div(100).unwrap_or(0);
    u8::try_from(scaled).unwrap_or(u8::MAX)
}

/// A colour given as hue (0 to 360, blue at 0), lightness and saturation
/// (0 to 100), as the VT340 reads them, worked in thousandths.
fn hls(hue: u32, lightness: u32, saturation: u32) -> [u8; 3] {
    let (l, s) = (
        i64::from(lightness.min(100)),
        i64::from(saturation.min(100)),
    );
    // The VT340's hue 0 is blue; the usual wheel's 0 is red, 240° on.
    let h = i64::from(hue.checked_rem(360).unwrap_or(0)).saturating_add(240);
    let h = h.checked_rem(360).unwrap_or(0);
    // Chroma, and the second-largest channel, both of 0 to 100_000.
    let chroma = 100i64
        .saturating_sub(l.saturating_mul(2).saturating_sub(100).abs())
        .saturating_mul(s)
        .saturating_mul(10);
    let sector = h.checked_div(60).unwrap_or(0);
    let within = h
        .checked_rem(120)
        .unwrap_or(0)
        .saturating_mul(1000)
        .checked_div(60)
        .unwrap_or(0);
    let second = chroma
        .saturating_mul(1000i64.saturating_sub(within.saturating_sub(1000).abs()))
        .checked_div(1000)
        .unwrap_or(0);
    let (r, g, b) = match sector {
        0 => (chroma, second, 0),
        1 => (second, chroma, 0),
        2 => (0, chroma, second),
        3 => (0, second, chroma),
        4 => (second, 0, chroma),
        _ => (chroma, 0, second),
    };
    let base = l
        .saturating_mul(1000)
        .saturating_sub(chroma.checked_div(2).unwrap_or(0));
    let byte = |c: i64| {
        let v = c
            .saturating_add(base)
            .saturating_mul(255)
            .checked_div(100_000)
            .unwrap_or(0);
        u8::try_from(v.clamp(0, 255)).unwrap_or(0)
    };
    [byte(r), byte(g), byte(b)]
}

/// The VT340's sixteen colour registers as it starts, the rest black.
fn vt340_palette() -> [[u8; 3]; 256] {
    const SIXTEEN: [[u32; 3]; 16] = [
        [0, 0, 0],
        [20, 20, 80],
        [80, 13, 13],
        [20, 80, 20],
        [80, 20, 80],
        [20, 80, 80],
        [80, 80, 20],
        [53, 53, 53],
        [26, 26, 26],
        [33, 33, 60],
        [60, 26, 26],
        [33, 60, 33],
        [60, 33, 60],
        [33, 60, 60],
        [60, 60, 33],
        [80, 80, 80],
    ];
    let mut palette = [[0u8; 3]; 256];
    for (slot, [r, g, b]) in palette.iter_mut().zip(SIXTEEN) {
        *slot = [percent(r), percent(g), percent(b)];
    }
    palette
}

/// A picture as sixel data, to show it on a terminal that draws sixel:
/// its pixels from `x`, `y`, `width` by `height` of them, in at most 256
/// colours; clear pixels left alone.
pub fn encode(picture: &Picture, (x, y, width, height): (u32, u32, u32, u32), out: &mut Vec<u8>) {
    let width = width.min(picture.width.saturating_sub(x));
    let height = height.min(picture.height.saturating_sub(y));
    if width == 0 || height == 0 {
        return;
    }
    // Colours as they are if there are few enough, else to the cube.
    let at = |px: u32, py: u32| picture.pixel(x.saturating_add(px), y.saturating_add(py));
    let mut seen: Vec<[u8; 3]> = Vec::new();
    let mut exact = true;
    'scan: for py in 0..height {
        for px in 0..width {
            if let Some([r, g, b, a]) = at(px, py)
                && a >= 128
                && !seen.contains(&[r, g, b])
            {
                if seen.len() == 256 {
                    exact = false;
                    break 'scan;
                }
                seen.push([r, g, b]);
            }
        }
    }
    let (reds, greens, blues) = CUBE;
    let steps = |c: u8, of: usize| usize::from(c).saturating_mul(of) >> 8;
    let cube = |[r, g, b]: [u8; 3]| {
        let (r, g, b) = (steps(r, reds), steps(g, greens), steps(b, blues));
        (r.saturating_mul(greens).saturating_add(g))
            .saturating_mul(blues)
            .saturating_add(b)
    };
    let level = |n: usize, of: usize| {
        let n = n
            .saturating_mul(255)
            .checked_div(of.saturating_sub(1))
            .unwrap_or(0);
        u8::try_from(n).unwrap_or(u8::MAX)
    };
    let palette: Vec<[u8; 3]> = if exact {
        seen
    } else {
        // In `cube`'s order: blue fastest, then green, then red.
        let mut levels = Vec::new();
        for r in 0..reds {
            for g in 0..greens {
                for b in 0..blues {
                    levels.push([level(r, reds), level(g, greens), level(b, blues)]);
                }
            }
        }
        levels
    };
    let index = |rgb: [u8; 3]| -> usize {
        if exact {
            palette.iter().position(|c| *c == rgb).unwrap_or(0)
        } else {
            cube(rgb)
        }
    };
    // P2 = 1: clear pixels stay as they are. The raster attributes give
    // the size, square pixels.
    let _ = write!(out, "\x1bP0;1;0q\"1;1;{width};{height}");
    for (i, [r, g, b]) in palette.iter().enumerate() {
        let share = |c: u8| {
            u32::from(c)
                .saturating_mul(100)
                .checked_div(255)
                .unwrap_or(0)
        };
        let _ = write!(out, "#{i};2;{};{};{}", share(*r), share(*g), share(*b));
    }
    let colours = palette.len();
    let mut band = vec![0u8; usize::try_from(width).unwrap_or(0)];
    let mut top = 0;
    while top < height {
        // Each colour of the band in turn, from its left, `$` between.
        let mut first = true;
        for colour in 0..colours {
            let mut any = false;
            for (px, sixel) in (0..width).zip(band.iter_mut()) {
                *sixel = 0;
                for bit in 0..6u32 {
                    let py = top.saturating_add(bit);
                    if py >= height {
                        break;
                    }
                    if let Some([r, g, b, a]) = at(px, py)
                        && a >= 128
                        && index([r, g, b]) == colour
                    {
                        *sixel |= 1 << bit;
                        any = true;
                    }
                }
            }
            if !any {
                continue;
            }
            if !first {
                out.push(b'$');
            }
            first = false;
            let _ = write!(out, "#{colour}");
            for run in band.chunk_by(|a, b| a == b) {
                let ch = run.first().copied().unwrap_or(0).saturating_add(0x3f);
                match run.len() {
                    1..=3 => out.extend(std::iter::repeat_n(ch, run.len())),
                    n => {
                        let _ = write!(out, "!{n}");
                        out.push(ch);
                    }
                }
            }
        }
        out.push(b'-');
        top = top.saturating_add(6);
    }
    out.extend_from_slice(b"\x1b\\");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(data: &[u8]) -> Option<Arc<Picture>> {
        let mut decoder = Decoder::new();
        data.iter().for_each(|b| decoder.push(*b));
        decoder.finish()
    }

    /// Colours defined and selected, sixels repeated, bands below one
    /// another; and the picture encoded back decodes to itself.
    #[test]
    fn sixels_decode_and_encode_back() {
        // Red 2 wide by 6 high, then a band below of one blue column 3 high.
        let data = b"#1;2;100;0;0#2;2;0;0;100#1!2~-#2F";
        let picture = decode(data).unwrap_or_else(|| {
            Arc::new(Picture {
                width: 0,
                height: 0,
                rgba: vec![],
            })
        });
        assert_eq!((picture.width, picture.height), (2, 9));
        assert_eq!(picture.pixel(1, 5), Some([255, 0, 0, 255]));
        assert_eq!(picture.pixel(0, 8), Some([0, 0, 255, 255]));
        assert_eq!(
            picture.pixel(1, 8),
            Some([0, 0, 0, 0]),
            "clear where nothing was drawn"
        );
        let mut encoded = Vec::new();
        encode(&picture, (0, 0, 2, 9), &mut encoded);
        let body = encoded
            .strip_prefix(b"\x1bP0;1;0q")
            .and_then(|b| b.strip_suffix(b"\x1b\\"))
            .unwrap_or_default();
        assert_eq!(decode(body).as_deref(), Some(&*picture));
    }

    /// A picture of more than 256 colours is drawn in a cube of 6 reds, 7
    /// greens and 6 blues: each channel to its nearest step below, which
    /// the colour definitions give in percent.
    #[test]
    fn many_colours_are_drawn_in_the_cube() {
        let mut rgba = Vec::new();
        for c in [[0, 0, 0], [255, 255, 255], [128, 128, 128], [200, 100, 50]] {
            rgba.extend(c);
            rgba.push(255);
        }
        for i in 4..300u32 {
            let [low, high, ..] = i.to_le_bytes();
            rgba.extend([low, high.saturating_mul(128), 7, 255]);
        }
        let picture = Picture {
            width: 300,
            height: 1,
            rgba,
        };
        let mut encoded = Vec::new();
        encode(&picture, (0, 0, 300, 1), &mut encoded);
        let body = encoded
            .strip_prefix(b"\x1bP0;1;0q")
            .and_then(|b| b.strip_suffix(b"\x1b\\"))
            .unwrap_or_default();
        let back = decode(body);
        let pixel = |x| back.as_ref().and_then(|p| p.pixel(x, 0));
        assert_eq!(pixel(0), Some([0, 0, 0, 255]));
        assert_eq!(pixel(1), Some([255, 255, 255, 255]));
        // 128: red and blue 3 of 5 steps, 153, 60%; green 3 of 6, 127,
        // 49%, read back as 124.
        assert_eq!(pixel(2), Some([153, 124, 153, 255]));
        // 200 is red's step 4 (204, 80%); 100 green's 2 (85, 33%: 84); 50
        // blue's 1 (51, 20%).
        assert_eq!(pixel(3), Some([204, 84, 51, 255]));
    }

    /// Data that would draw past what a picture may hold makes none.
    #[test]
    fn a_picture_too_large_is_dropped() {
        assert!(decode(b"!4096~!4096~").is_none());
        assert!(decode(b"").is_none());
    }
}
