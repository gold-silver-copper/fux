//! Kitty terminal graphics protocol: APC command parsing, image storage,
//! chunked direct transmission, placement/deletion, and Kitty/sixel pixel encoders.

use std::fmt::Write as _;

pub use crate::base64::{decode_base64, encode_base64};
use crate::sixel::{self, Colour, Part};
use crate::{PICTURE_MAX, PICTURES_MAX, Picture, PictureFormat, SIZE_MAX, png};

const STORED_IMAGES_MAX: usize = 16;
/// The most a screen keeps of images, all together: each one's pixels as RGBA, and a PNG's own bytes.
const STORED_BYTES_MAX: usize = 16 << 20;
const TOO_LARGE: &str = "ENOMEM: image is over the 16 MiB a screen keeps";
/// The log's line for a transfer in parts that something other than its next part broke off.
pub(crate) const BROKEN_OFF: &str = "a kitty picture that was not sent whole was dropped";
const UNREADABLE: &str = "a kitty graphics command that could not be read was dropped";
/// The most a screen keeps of the PNGs it made of images' pixels for the pane's folder, to use them again.
const MADE_BYTES_MAX: usize = PICTURE_MAX;
const PIXELS_MAX: usize = sixel::PIXELS_MAX;
const CHUNK_RAW_BYTES: usize = 3072; // 3072 raw bytes == 4096 base64 characters

/// Encodes a PNG byte slice into chunked Kitty graphics APC sequences (`a=T,f=100,C=1,q=2`).
pub fn encode_png(
    png_bytes: &[u8],
    crop: Option<[usize; 4]>,
    cells: Option<(usize, usize)>,
    max: usize,
) -> Result<String, &'static str> {
    let mut header = String::from("a=T,f=100,C=1,q=2");
    if let Some([x, y, w, h]) = crop {
        let _ = write!(header, ",x={x},y={y},w={w},h={h}");
    }
    if let Some((c, r)) = cells {
        if c > 0 {
            let _ = write!(header, ",c={c}");
        }
        if r > 0 {
            let _ = write!(header, ",r={r}");
        }
    }
    encode_chunks(&header, png_bytes, max)
}

/// Encodes raw 32-bit RGBA pixels into chunked Kitty graphics APC sequences (`a=T,f=32,C=1,q=2`).
pub fn encode_rgba(
    w: usize,
    h: usize,
    rgba: &[u8],
    cells: Option<(usize, usize)>,
    max: usize,
) -> Result<String, &'static str> {
    if w == 0 || h == 0 || rgba.len() != w.saturating_mul(h).saturating_mul(4) {
        return Err("invalid RGBA dimensions");
    }
    let mut header = format!("a=T,f=32,s={w},v={h},C=1,q=2");
    if let Some((c, r)) = cells {
        if c > 0 {
            let _ = write!(header, ",c={c}");
        }
        if r > 0 {
            let _ = write!(header, ",r={r}");
        }
    }
    encode_chunks(&header, rgba, max)
}

fn encode_chunks(header: &str, payload: &[u8], max: usize) -> Result<String, &'static str> {
    if payload.is_empty() {
        return Ok(String::new());
    }
    let b64_len = payload.len().div_ceil(3).saturating_mul(4);
    let n_chunks = payload.len().div_ceil(CHUNK_RAW_BYTES);
    let est = b64_len + header.len() + n_chunks * 16;
    if est > max {
        return Err("its part in the tile is over the picture byte limit");
    }
    let mut out = String::with_capacity(est);
    let mut chunks = payload.chunks(CHUNK_RAW_BYTES).enumerate().peekable();
    while let Some((idx, chunk)) = chunks.next() {
        let more = chunks.peek().is_some();
        out.push_str("\x1b_G");
        if idx == 0 {
            out.push_str(header);
            if more {
                out.push_str(",m=1");
            }
        } else if more {
            out.push_str("m=1");
        } else {
            out.push_str("m=0");
        }
        out.push(';');
        encode_base64(chunk, &mut out);
        out.push_str("\x1b\\");
        if out.len() > max {
            return Err("its part in the tile is over the picture byte limit");
        }
    }
    Ok(out)
}

/// Converts a decoded `sixel::Part` into 32-bit RGBA (`part.w * part.h * 4` bytes).
/// Unpainted pixels (pen 0) become transparent (`[0, 0, 0, 0]`).
pub fn sixel_to_rgba(part: &Part) -> Vec<u8> {
    let mut rgba = vec![0u8; part.w.saturating_mul(part.h).saturating_mul(4)];
    let palette: Vec<[u8; 4]> = part
        .colours
        .iter()
        .map(|c| match *c {
            Colour::Rgb([r, g, b]) => [
                ((u16::from(r) * 255 + 50) / 100) as u8,
                ((u16::from(g) * 255 + 50) / 100) as u8,
                ((u16::from(b) * 255 + 50) / 100) as u8,
                255,
            ],
            Colour::Register(n) => {
                let [r, g, b] = xterm_256(n);
                [r, g, b, 255]
            }
        })
        .collect();
    for y in 0..part.h {
        let row_start = y.saturating_mul(part.stride);
        for x in 0..part.w {
            let pen = part.pixels.get(row_start + x).copied().unwrap_or(0);
            if pen == 0 {
                continue;
            }
            if let Some(color) = palette.get(usize::from(pen - 1)) {
                let dst = (y * part.w + x) * 4;
                if let Some(slot) = rgba.get_mut(dst..dst + 4) {
                    slot.copy_from_slice(color);
                }
            }
        }
    }
    rgba
}

/// Crops `[left, top, wide, high]` from an RGBA buffer and encodes it as sixel (quantized to the 6x6x6 RGB cube).
pub fn rgba_to_sixel(
    w: usize,
    h: usize,
    rgba: &[u8],
    [left, top, wide, high]: [usize; 4],
    max: usize,
) -> Result<String, &'static str> {
    let cw = w.saturating_sub(left).min(wide);
    let ch = h.saturating_sub(top).min(high);
    if cw == 0 || ch == 0 {
        return Ok(String::new());
    }
    let area =
        cw.checked_mul(ch).filter(|a| *a <= PIXELS_MAX).ok_or("its part in the tile is over 8 million pixels")?;
    let mut pixels = vec![0u16; area];
    let mut colours: Vec<Colour> = Vec::new();
    let mut map = [0u16; 216];
    for y in 0..ch {
        for x in 0..cw {
            let src = ((top + y) * w + (left + x)) * 4;
            let Some([r, g, b, a]) = rgba.get(src..src + 4).and_then(|s| <[u8; 4]>::try_from(s).ok()) else {
                continue;
            };
            if a < 128 {
                continue;
            }
            let ri = ((u16::from(r) * 5 + 127) / 255) as usize;
            let gi = ((u16::from(g) * 5 + 127) / 255) as usize;
            let bi = ((u16::from(b) * 5 + 127) / 255) as usize;
            let idx = ri * 36 + gi * 6 + bi;
            let Some(slot) = map.get_mut(idx) else { continue };
            if *slot == 0 {
                let pct = |i: usize| (i * 20) as u8;
                colours.push(Colour::Rgb([pct(ri), pct(gi), pct(bi)]));
                *slot = colours.len() as u16;
            }
            if let Some(dst) = pixels.get_mut(y * cw + x) {
                *dst = *slot;
            }
        }
    }
    Part { w: cw, h: ch, stride: cw, colours, pixels }.write(max)
}

fn xterm_256(n: u8) -> [u8; 3] {
    const ANSI16: [[u8; 3]; 16] = [
        [0, 0, 0],
        [205, 0, 0],
        [0, 205, 0],
        [205, 205, 0],
        [0, 0, 238],
        [205, 0, 205],
        [0, 205, 205],
        [229, 229, 229],
        [127, 127, 127],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [92, 92, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    match n {
        0..=15 => ANSI16.get(usize::from(n)).copied().unwrap_or([0, 0, 0]),
        16..=231 => {
            let v = n - 16;
            let c = |i: u8| if i == 0 { 0 } else { 55 + i * 40 };
            [c(v / 36), c((v / 6) % 6), c(v % 6)]
        }
        232..=255 => {
            let g = 8 + (n - 232) * 10;
            [g, g, g]
        }
    }
}

// ---- Kitty APC command parser & screen storage --------------------------------

/// The keys of the protocol, as of Kitty 0.50: each is one letter.
const KEYS: &[u8] = b"aqeftsvSOiIpomNxywhXYcrCUzPQHVd";

/// Dense key-value table for single-ASCII-letter Kitty control keys (`a`..`z`, `A`..`Z`).
#[derive(Clone, Copy)]
struct Kv {
    values: [u32; 52],
    present: u64,
}

impl Default for Kv {
    fn default() -> Self {
        Self { values: [0; 52], present: 0 }
    }
}

impl Kv {
    fn index(key: u8) -> Option<usize> {
        match key {
            b'a'..=b'z' => Some(usize::from(key - b'a')),
            b'A'..=b'Z' => Some(26 + usize::from(key - b'A')),
            _ => None,
        }
    }

    fn get(&self, key: u8) -> Option<u32> {
        let idx = Self::index(key)?;
        (self.present & (1u64 << idx) != 0).then(|| self.values.get(idx).copied().unwrap_or(0))
    }

    /// The table without `keys`.
    fn without(mut self, keys: &[u8]) -> Self {
        for idx in keys.iter().filter_map(|key| Self::index(*key)) {
            self.present &= !(1u64 << idx);
            if let Some(slot) = self.values.get_mut(idx) {
                *slot = 0;
            }
        }
        self
    }

    /// Whether each key of the table is in `other` too, with the same value.
    fn within(&self, other: &Self) -> bool {
        let said = |idx: &usize| self.present & (1u64 << idx) != 0;
        self.present & !other.present == 0
            && (0..self.values.len()).filter(said).all(|idx| self.values.get(idx) == other.values.get(idx))
    }

    fn put(&mut self, key: u8, val: u32) {
        if let Some(idx) = Self::index(key)
            && let Some(slot) = self.values.get_mut(idx)
        {
            *slot = val;
            self.present |= 1u64 << idx;
        }
    }

    /// Reads `key=value` pairs, each key one of the protocol's. A key that names one of several things
    /// (`a`, `t`, `o`, `d`, `e`) takes one letter; `z`, `H` and `V` a number that may have a `-`; every
    /// other key a number without sign. Anything else makes the command none, a key that is not the
    /// protocol's too, as in Kitty: no pair of it is believed. (Passed over, such a key leaves the
    /// command another one: `a=d,D=i,i=1`, with its `d` mistyped, is `a=d`, and deletes everything.)
    fn parse(control: &[u8]) -> Option<Self> {
        let mut kv = Self::default();
        if control.is_empty() {
            return Some(kv);
        }
        let number = |digits: &[u8]| {
            let digits = std::str::from_utf8(digits).ok().filter(|text| text.bytes().all(|b| b.is_ascii_digit()))?;
            digits.parse::<u32>().ok()
        };
        for pair in control.split(|&b| b == b',') {
            let [key, b'=', value @ ..] = pair else { return None };
            if !KEYS.contains(key) {
                return None;
            }
            let value = match (key, value) {
                (b'a' | b't' | b'o' | b'd' | b'e', [letter]) if letter.is_ascii_alphabetic() => u32::from(*letter),
                (b'a' | b't' | b'o' | b'd' | b'e', _) => return None,
                (b'z' | b'H' | b'V', [b'-', digits @ ..]) => {
                    0i32.checked_sub_unsigned(number(digits)?)?.cast_unsigned()
                }
                (b'z' | b'H' | b'V', digits) => i32::try_from(number(digits)?).ok()?.cast_unsigned(),
                (_, digits) => number(digits)?,
            };
            kv.put(*key, value);
        }
        Some(kv)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quiet {
    No,
    Ok,
    Failures,
}

impl Quiet {
    fn from_val(v: Option<u32>) -> Self {
        match v {
            None | Some(0) => Self::No,
            Some(1) => Self::Ok,
            Some(_) => Self::Failures,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Rgb,
    Rgba,
    Png,
    Unsupported,
}

impl Format {
    fn from_val(v: Option<u32>) -> Self {
        match v {
            None | Some(32) => Self::Rgba,
            Some(24) => Self::Rgb,
            Some(100) => Self::Png,
            Some(_) => Self::Unsupported,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Medium {
    Direct,
    Unsupported,
}

impl Medium {
    fn from_val(v: Option<u32>) -> Self {
        match v {
            None => Self::Direct,
            Some(x) if x == u32::from(b'd') => Self::Direct,
            Some(_) => Self::Unsupported,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Compression {
    None,
    Zlib,
    Unsupported,
}

impl Compression {
    fn from_val(v: Option<u32>) -> Self {
        match v {
            None => Self::None,
            Some(x) if x == u32::from(b'z') => Self::Zlib,
            Some(_) => Self::Unsupported,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct DisplaySpec {
    image_id: u32,
    image_number: u32,
    placement_id: u32,
    src_x: u32,
    src_y: u32,
    src_w: u32,
    src_h: u32,
    cols: u32,
    rows: u32,
    move_cursor: bool,
    z: i32,
    /// It is one for Unicode placeholders (`U=1`) or relative to another (`P`, `Q`): fux draws neither.
    not_drawn: bool,
}

impl DisplaySpec {
    fn from_kv(kv: &Kv) -> Self {
        Self {
            image_id: kv.get(b'i').unwrap_or(0),
            image_number: kv.get(b'I').unwrap_or(0),
            placement_id: kv.get(b'p').unwrap_or(0),
            src_x: kv.get(b'x').unwrap_or(0),
            src_y: kv.get(b'y').unwrap_or(0),
            src_w: kv.get(b'w').unwrap_or(0),
            src_h: kv.get(b'h').unwrap_or(0),
            cols: kv.get(b'c').unwrap_or(0),
            rows: kv.get(b'r').unwrap_or(0),
            move_cursor: kv.get(b'C') != Some(1),
            z: kv.get(b'z').map_or(0, u32::cast_signed),
            not_drawn: b"UPQ".iter().any(|key| kv.get(*key).is_some_and(|n| n > 0)),
        }
    }
}

#[derive(Clone, Copy)]
struct Response {
    id: u32,
    number: u32,
    placement_id: u32,
    message: &'static str,
}

impl Response {
    fn ok(id: u32, number: u32, placement_id: u32) -> Self {
        Self { id, number, placement_id, message: "OK" }
    }

    fn err(id: u32, number: u32, placement_id: u32, message: &'static str) -> Self {
        Self { id, number, placement_id, message }
    }

    fn encode(self, quiet: Quiet) -> Option<String> {
        let is_ok = self.message == "OK";
        match quiet {
            Quiet::Failures => return None,
            Quiet::Ok if is_ok => return None,
            _ => {}
        }
        if self.id == 0 && self.number == 0 {
            return None;
        }
        let mut out = String::from("\x1b_G");
        let mut prior = false;
        if self.id > 0 {
            let _ = write!(out, "i={}", self.id);
            prior = true;
        }
        if self.number > 0 {
            if prior {
                out.push(',');
            }
            let _ = write!(out, "I={}", self.number);
            prior = true;
        }
        if self.placement_id > 0 {
            if prior {
                out.push(',');
            }
            let _ = write!(out, "p={}", self.placement_id);
        }
        let _ = write!(out, ";{}\x1b\\", self.message);
        Some(out)
    }
}

struct StoredImage {
    id: u32,
    number: u32,
    w: u32,
    h: u32,
    rgba: Vec<u8>,
    /// The PNG that was sent, if one was: the file of a placement of the whole image.
    png: Option<Vec<u8>>,
    /// The PNG last made of its pixels, and of which of them (`[x, y, w, h]`): placed so again, it is used again.
    made: Option<([u32; 4], Vec<u8>)>,
}

impl StoredImage {
    fn byte_len(&self) -> usize {
        self.rgba.len() + self.png.as_ref().map_or(0, Vec::len)
    }
}

struct Loading {
    /// What its first part said, but for `m` and `q`, with the `a` and `t` it may have left out
    /// (`a=t`, `t=d`): a later part may say any of it again.
    header: Kv,
    id: u32,
    number: u32,
    implicit_id: bool,
    replaces: Option<u32>,
    format: Format,
    compression: Compression,
    width: u32,
    height: u32,
    display: Option<DisplaySpec>,
    quiet: Quiet,
    placement_id: u32,
    data: Vec<u8>,
}

/// Cursor movement requested after placing a Kitty image (when `C=0`).
pub(crate) struct CursorAdvance {
    pub linefeeds: usize,
    pub target_col: usize,
}

/// Result of executing one Kitty graphics APC command.
#[derive(Default)]
pub(crate) struct Outcome {
    pub reply: Option<String>,
    pub cursor: Option<CursorAdvance>,
    pub refused: Option<&'static str>,
    /// Whether the command broke off a transfer in parts: the log is told that as well.
    pub broke: bool,
}

/// Per-screen Kitty graphics image store and chunked-upload state.
pub(crate) struct Storage {
    images: Vec<StoredImage>,
    loading: Option<Loading>,
    next_id: u32,
}

impl Default for Storage {
    fn default() -> Self {
        Self { images: Vec::new(), loading: None, next_id: 1 << 31 }
    }
}

impl Storage {
    pub(crate) fn clear(&mut self) {
        self.images.clear();
        self.loading = None;
    }

    pub(crate) fn execute(
        &mut self,
        data: &[u8],
        cell: (usize, usize),
        screen_cols: usize,
        cursor: (usize, usize),
        pictures: &mut Vec<Picture>,
        next_picture: &mut u32,
    ) -> Outcome {
        let (control, payload) = match data.iter().position(|&b| b == b';') {
            Some(sep) => (data.get(..sep).unwrap_or_default(), data.get(sep + 1..).unwrap_or_default()),
            None => (data, &[][..]),
        };
        // (A command that cannot be read names no image to answer under: the log says it was dropped.)
        let Some(kv) = Kv::parse(control) else {
            return Outcome { refused: Some(UNREADABLE), broke: self.abandon(pictures), ..Outcome::default() };
        };
        let mut quiet = Quiet::from_val(kv.get(b'q'));
        let id = kv.get(b'i').unwrap_or(0);
        let number = kv.get(b'I').unwrap_or(0);
        let placement_id = kv.get(b'p').unwrap_or(0);
        let action = kv.get(b'a').and_then(|v| u8::try_from(v).ok()).unwrap_or(b't');

        // A transfer in parts goes on with a part that says, `m` and `q` aside, nothing the first part
        // did not: nothing at all, as the protocol has it, or some of the first part's keys again, each
        // with its value, as programs send and Kitty takes (`kitten icat` says `a=T,q=2,m=1` where it
        // began with `a=T`: without that no picture of its comes through). A part that says `a=t` or
        // `a=T` and no more goes on with it too, whichever of the two the first part said: Kitty takes
        // it, and the first part's stands. A query stores nothing and leaves the transfer be. Anything
        // else is not part of it, be it a key the first part had not or another value for one it had:
        // the transfer is dropped, and the command is itself.
        let goes_on = self.loading.as_ref().is_some_and(|loading| {
            let said = kv.without(b"mq");
            let action_alone = matches!(action, b't' | b'T') && said.without(b"a").present == 0;
            action_alone || said.within(&loading.header)
        });
        let broke = !goes_on && action != b'q' && self.abandon(pictures);
        let mut outcome = if id > 0 && number > 0 {
            let both = "EINVAL: image ID and number are mutually exclusive";
            Outcome { reply: Response::err(id, number, placement_id, both).encode(quiet), ..Outcome::default() }
        } else {
            match action {
                b'q' => self.query(&kv, payload, quiet),
                b't' | b'T' => {
                    // A later part that says `q=1` or `q=2` makes the transfer that quiet from there on,
                    // less quiet than it was too, as in Kitty; one that says neither is as quiet as it was.
                    if let Some(loading) = self.loading.as_mut() {
                        match quiet {
                            Quiet::No => quiet = loading.quiet,
                            Quiet::Ok | Quiet::Failures => loading.quiet = quiet,
                        }
                    }
                    let show = action == b'T';
                    self.transmit(&kv, show, payload, quiet, cell, screen_cols, cursor, pictures, next_picture)
                }
                b'p' => {
                    let spec = DisplaySpec::from_kv(&kv);
                    self.place(&spec, quiet, cell, screen_cols, cursor, pictures, next_picture, false)
                }
                b'd' => {
                    self.delete(&kv, cursor, pictures);
                    Outcome::default()
                }
                b'f' | b'a' | b'c' => {
                    let no = "ENOTSUPPORTED: animation not supported";
                    Outcome { reply: Response::err(id, number, placement_id, no).encode(quiet), ..Outcome::default() }
                }
                _ => {
                    let no = "EINVAL: unsupported action";
                    Outcome { reply: Response::err(id, number, placement_id, no).encode(quiet), ..Outcome::default() }
                }
            }
        };
        outcome.broke = broke;
        outcome
    }

    fn query(&self, kv: &Kv, payload: &[u8], quiet: Quiet) -> Outcome {
        let id = kv.get(b'i').unwrap_or(0);
        let placement_id = kv.get(b'p').unwrap_or(0);
        // (A query names its image by `i`, as the protocol says and Kitty holds it: one that gives
        // only a number, `I`, is not answered. One that gives both never comes here.)
        if id == 0 {
            return Outcome::default();
        }
        // Feature query (`e=` key, Kitty 0.50+): report supported non-default keys without loading data.
        if let Some(feat) = kv.get(b'e').and_then(|v| u8::try_from(v).ok()) {
            if quiet == Quiet::Ok || quiet == Quiet::Failures {
                return Outcome::default();
            }
            let mut reply = format!("\x1b_Gi={id}");
            if matches!(feat, b'q' | b't' | b'T' | b'p' | b'd') {
                let _ = write!(reply, ",e={}", char::from(feat));
                if matches!(feat, b't' | b'T' | b'q') {
                    if let Some(f @ (24 | 100)) = kv.get(b'f') {
                        let _ = write!(reply, ",f={f}");
                    }
                    if kv.get(b'o') == Some(u32::from(b'z')) {
                        reply.push_str(",o=z");
                    }
                }
                if matches!(feat, b'T' | b'p') && kv.get(b'C') == Some(1) {
                    reply.push_str(",C=1");
                }
            }
            reply.push_str(";OK\x1b\\");
            return Outcome { reply: Some(reply), ..Outcome::default() };
        }

        let resp = match decode_image(
            Format::from_val(kv.get(b'f')),
            Medium::from_val(kv.get(b't')),
            Compression::from_val(kv.get(b'o')),
            kv.get(b's').unwrap_or(0),
            kv.get(b'v').unwrap_or(0),
            payload,
            true,
        ) {
            Ok(_) => Response::ok(id, 0, placement_id),
            Err(msg) => Response::err(id, 0, placement_id, msg),
        };
        Outcome { reply: resp.encode(quiet), ..Outcome::default() }
    }

    #[allow(clippy::too_many_arguments)]
    fn transmit(
        &mut self,
        kv: &Kv,
        and_display: bool,
        payload: &[u8],
        quiet: Quiet,
        cell: (usize, usize),
        screen_cols: usize,
        cursor: (usize, usize),
        pictures: &mut Vec<Picture>,
        next_picture: &mut u32,
    ) -> Outcome {
        let more = kv.get(b'm') == Some(1);
        if let Some(loading) = self.loading.as_ref() {
            // (What goes wrong with a later part is told as what goes wrong with the whole is: under the
            // name the transfer gave its image, and not at all if it gave none.)
            let said = (!loading.implicit_id).then_some((loading.id, loading.number, loading.placement_id));
            let fail =
                |message| said.and_then(|(id, number, pid)| Response::err(id, number, pid, message).encode(quiet));
            let chunk = match decode_base64(payload, PICTURE_MAX) {
                Ok(chunk) if loading.data.len().saturating_add(chunk.len()) <= PICTURE_MAX => chunk,
                Ok(_) => {
                    self.abandon(pictures);
                    return Outcome {
                        reply: fail("ENOMEM: image payload exceeds 4 MiB"),
                        refused: Some("a kitty picture sent as more than 4 MiB of data was dropped"),
                        ..Outcome::default()
                    };
                }
                Err(_) => {
                    self.abandon(pictures);
                    return Outcome { reply: fail("EINVAL: invalid base64"), ..Outcome::default() };
                }
            };
            if let Some(loading) = self.loading.as_mut() {
                loading.data.extend_from_slice(&chunk);
            }
            if more {
                return Outcome::default();
            }
            let Some(loading) = self.loading.take() else { return Outcome::default() };
            return self.finish_load(loading, quiet, cell, screen_cols, cursor, pictures, next_picture);
        }

        // First (or only) chunk of a new transmission.
        let medium = Medium::from_val(kv.get(b't'));
        let req_id = kv.get(b'i').unwrap_or(0);
        let number = kv.get(b'I').unwrap_or(0);
        let placement_id = kv.get(b'p').unwrap_or(0);
        if medium != Medium::Direct {
            return Outcome {
                reply: Response::err(req_id, number, placement_id, "EINVAL: unsupported medium").encode(quiet),
                ..Outcome::default()
            };
        }
        let format = Format::from_val(kv.get(b'f'));
        if format == Format::Unsupported {
            return Outcome {
                reply: Response::err(req_id, number, placement_id, "EINVAL: unsupported format").encode(quiet),
                ..Outcome::default()
            };
        }
        let compression = Compression::from_val(kv.get(b'o'));
        if compression == Compression::Unsupported {
            return Outcome {
                reply: Response::err(req_id, number, placement_id, "EINVAL: unsupported compression").encode(quiet),
                ..Outcome::default()
            };
        }
        let raw = match decode_base64(payload, PICTURE_MAX) {
            Ok(raw) => raw,
            Err(_) => {
                if req_id > 0 && number == 0 {
                    self.remove_image_and_placements(req_id, pictures);
                }
                return Outcome {
                    reply: Response::err(req_id, number, placement_id, "EINVAL: invalid base64").encode(quiet),
                    ..Outcome::default()
                };
            }
        };
        let (id, implicit_id, replaces) = if req_id > 0 {
            (req_id, false, Some(req_id))
        } else if number > 0 {
            (self.alloc_id(), false, None)
        } else {
            (self.alloc_id(), true, None)
        };
        let display = and_display.then(|| {
            let mut spec = DisplaySpec::from_kv(kv);
            spec.image_id = id;
            spec.image_number = number;
            spec
        });
        let mut header = kv.without(b"mq");
        header.put(b'a', u32::from(if and_display { b'T' } else { b't' }));
        header.put(b't', u32::from(b'd'));
        let loading = Loading {
            header,
            id,
            number,
            implicit_id,
            replaces,
            format,
            compression,
            width: kv.get(b's').unwrap_or(0),
            height: kv.get(b'v').unwrap_or(0),
            display,
            quiet,
            placement_id,
            data: raw,
        };
        if more {
            self.loading = Some(loading);
            return Outcome::default();
        }
        self.finish_load(loading, quiet, cell, screen_cols, cursor, pictures, next_picture)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_load(
        &mut self,
        loading: Loading,
        quiet: Quiet,
        cell: (usize, usize),
        screen_cols: usize,
        cursor: (usize, usize),
        pictures: &mut Vec<Picture>,
        next_picture: &mut u32,
    ) -> Outcome {
        // (The image this one was sent to take the place of goes, whether or not this one is kept.)
        if let Some(old_id) = loading.replaces {
            self.remove_image_and_placements(old_id, pictures);
        }
        let (format, compression, data) = (loading.format, loading.compression, &loading.data);
        let kept = decode_image(format, Medium::Direct, compression, loading.width, loading.height, data, false)
            .and_then(|decoded| {
                let DecodedImage { w, h, rgba, png } = decoded;
                let img = StoredImage { id: loading.id, number: loading.number, w, h, rgba, png, made: None };
                self.store_image(img, pictures)
            });
        if let Err(msg) = kept {
            return Outcome {
                reply: (!loading.implicit_id)
                    .then(|| Response::err(loading.id, loading.number, loading.placement_id, msg).encode(quiet))
                    .flatten(),
                refused: (msg == TOO_LARGE).then_some("a kitty picture over the 16 MiB a screen keeps was dropped"),
                ..Outcome::default()
            };
        }

        if let Some(spec) = loading.display {
            return self.place(&spec, quiet, cell, screen_cols, cursor, pictures, next_picture, loading.implicit_id);
        }
        let reply = (!loading.implicit_id)
            .then(|| Response::ok(loading.id, loading.number, loading.placement_id).encode(quiet))
            .flatten();
        Outcome { reply, ..Outcome::default() }
    }

    #[allow(clippy::too_many_arguments)]
    fn place(
        &mut self,
        spec: &DisplaySpec,
        quiet: Quiet,
        cell: (usize, usize),
        screen_cols: usize,
        cursor: (usize, usize),
        pictures: &mut Vec<Picture>,
        next_picture: &mut u32,
        implicit_id: bool,
    ) -> Outcome {
        if spec.image_id == 0 && spec.image_number == 0 {
            return Outcome {
                reply: Response::err(0, 0, spec.placement_id, "EINVAL: image ID or number required").encode(quiet),
                ..Outcome::default()
            };
        }
        let img = if spec.image_id > 0 {
            self.images.iter().find(|img| img.id == spec.image_id)
        } else {
            self.images.iter().rev().find(|img| img.number == spec.image_number)
        };
        let Some(img) = img else {
            return Outcome {
                reply: Response::err(spec.image_id, spec.image_number, spec.placement_id, "ENOENT: image not found")
                    .encode(quiet),
                ..Outcome::default()
            };
        };
        let img_id = img.id;
        // (Refused, not drawn at the cursor as a plain placement: that would cover text the program
        // meant to keep and move its cursor.)
        if spec.not_drawn {
            let no = "ENOTSUPPORTED: Unicode placeholders and relative placements are not supported";
            return Outcome {
                reply: (!implicit_id)
                    .then(|| Response::err(img_id, spec.image_number, spec.placement_id, no).encode(quiet))
                    .flatten(),
                ..Outcome::default()
            };
        }
        let sx = spec.src_x.min(img.w);
        let sy = spec.src_y.min(img.h);
        let sw = if spec.src_w > 0 { spec.src_w } else { img.w }.min(img.w - sx);
        let sh = if spec.src_h > 0 { spec.src_h } else { img.h }.min(img.h - sy);
        if sw == 0 || sh == 0 {
            return Outcome {
                reply: (!implicit_id)
                    .then(|| {
                        Response::err(img_id, spec.image_number, spec.placement_id, "EINVAL: empty source rectangle")
                            .encode(quiet)
                    })
                    .flatten(),
                ..Outcome::default()
            };
        }
        let (cell_w, cell_h) = (cell.0.max(1), cell.1.max(1));
        let (cols, rows) = grid_size(sw as usize, sh as usize, spec.cols as usize, spec.rows as usize, cell_w, cell_h);
        if cols == 0 || rows == 0 || cols > SIZE_MAX || rows > SIZE_MAX {
            return Outcome {
                reply: (!implicit_id)
                    .then(|| {
                        Response::err(img_id, spec.image_number, spec.placement_id, "EINVAL: image too large")
                            .encode(quiet)
                    })
                    .flatten(),
                refused: Some("a picture over 500 cells wide or high was dropped"),
                ..Outcome::default()
            };
        }

        // The PNG for the pane's folder: the one that was sent, for the whole image; else one made of the
        // pixels, which is kept and used again while the same part of the image is placed.
        let rect = [sx, sy, sw, sh];
        let full = rect == [0, 0, img.w, img.h];
        let kept = match (&img.png, &img.made) {
            (Some(sent), _) if full && sent.len() <= PICTURE_MAX => Some(sent.clone()),
            (_, Some((of, made))) if *of == rect => Some(made.clone()),
            _ => None,
        };
        let png_bytes = match kept {
            Some(bytes) => bytes,
            None => {
                let made = if full {
                    png::encode_rgba(img.w, img.h, &img.rgba, PICTURE_MAX)
                } else {
                    let part = crop_rgba(img.w as usize, &img.rgba, sx as usize, sy as usize, sw as usize, sh as usize);
                    png::encode_rgba(sw, sh, &part, PICTURE_MAX)
                };
                // (The PNG is not compressed: four bytes a pixel and one a row, in a file of 4 MiB at most.)
                let Ok(made) = made else {
                    let no = "ENOMEM: a PNG of these pixels would be over 4 MiB (about a million pixels fit)";
                    return Outcome {
                        reply: (!implicit_id)
                            .then(|| Response::err(img_id, spec.image_number, spec.placement_id, no).encode(quiet))
                            .flatten(),
                        refused: Some(
                            "a kitty picture of more pixels than its 4 MiB file holds (about a million) was dropped",
                        ),
                        ..Outcome::default()
                    };
                };
                self.keep_made(img_id, rect, &made);
                made
            }
        };

        // Explicit placement ID replaces any existing placement with the same (image_id, placement_id).
        if spec.placement_id > 0 {
            pictures.retain(|p| !(p.kitty_id == img_id && p.placement_id == spec.placement_id));
        }
        let mut refused = None;
        if pictures.len() >= PICTURES_MAX {
            pictures.remove(0);
            refused = Some("a screen keeps 8 pictures: the oldest was dropped for a new one");
        }
        let pic_id = *next_picture;
        *next_picture = next_picture.wrapping_add(1);
        pictures.push(Picture {
            id: pic_id,
            format: PictureFormat::Png,
            row: cursor.0,
            col: cursor.1,
            cols,
            rows,
            off: 0,
            cell,
            z: spec.z,
            kitty_id: img_id,
            placement_id: spec.placement_id,
            bytes: Some(png_bytes),
        });

        let cursor_advance = spec.move_cursor.then(|| {
            let target_x = cursor.1.saturating_add(cols);
            let wraps = target_x >= screen_cols;
            let linefeeds = rows.saturating_sub(1).saturating_add(usize::from(wraps));
            CursorAdvance { linefeeds, target_col: if wraps { 0 } else { target_x } }
        });
        let reply =
            (!implicit_id).then(|| Response::ok(img_id, spec.image_number, spec.placement_id).encode(quiet)).flatten();
        Outcome { reply, cursor: cursor_advance, refused, broke: false }
    }

    fn delete(&mut self, kv: &Kv, cursor: (usize, usize), pictures: &mut Vec<Picture>) {
        let what = kv.get(b'd').and_then(|v| u8::try_from(v).ok()).unwrap_or(b'a');
        let free = what.is_ascii_uppercase();
        let mut touched_ids: Vec<u32> = Vec::new();
        let intersects = |p: &Picture, row: usize, col: usize| {
            p.format == PictureFormat::Png
                && row >= p.row
                && row < p.row.saturating_add(p.rows)
                && col >= p.col
                && col < p.col.saturating_add(p.cols)
        };
        match what.to_ascii_lowercase() {
            b'a' => {
                pictures.retain(|p| {
                    if p.format == PictureFormat::Png {
                        touched_ids.push(p.kitty_id);
                        false
                    } else {
                        true
                    }
                });
                if free {
                    self.images.clear();
                    return;
                }
            }
            b'i' => {
                let id = kv.get(b'i').unwrap_or(0);
                let pid = kv.get(b'p').unwrap_or(0);
                if id == 0 {
                    return;
                }
                pictures.retain(|p| {
                    let hit = p.format == PictureFormat::Png && p.kitty_id == id && (pid == 0 || p.placement_id == pid);
                    !hit
                });
                if free && !pictures.iter().any(|p| p.format == PictureFormat::Png && p.kitty_id == id) {
                    self.images.retain(|img| img.id != id);
                }
                return;
            }
            b'n' => {
                let num = kv.get(b'I').unwrap_or(0);
                let pid = kv.get(b'p').unwrap_or(0);
                if num == 0 {
                    return;
                }
                let Some(id) = self.images.iter().rev().find(|img| img.number == num).map(|img| img.id) else {
                    return;
                };
                pictures.retain(|p| {
                    !(p.format == PictureFormat::Png && p.kitty_id == id && (pid == 0 || p.placement_id == pid))
                });
                if free && !pictures.iter().any(|p| p.format == PictureFormat::Png && p.kitty_id == id) {
                    self.images.retain(|img| img.id != id);
                }
                return;
            }
            b'c' => {
                pictures.retain(|p| {
                    if intersects(p, cursor.0, cursor.1) {
                        touched_ids.push(p.kitty_id);
                        false
                    } else {
                        true
                    }
                });
            }
            b'p' | b'q' => {
                let x = kv.get(b'x').unwrap_or(0);
                let y = kv.get(b'y').unwrap_or(0);
                if x == 0 || y == 0 {
                    return;
                }
                let (col, row) = ((x - 1) as usize, (y - 1) as usize);
                let z_filter = what.eq_ignore_ascii_case(&b'q').then(|| kv.get(b'z').map_or(0, u32::cast_signed));
                pictures.retain(|p| {
                    let hit = intersects(p, row, col) && z_filter.is_none_or(|z| p.z == z);
                    if hit {
                        touched_ids.push(p.kitty_id);
                    }
                    !hit
                });
            }
            b'r' => {
                let first = kv.get(b'x').unwrap_or(0);
                let last = kv.get(b'y').unwrap_or(0);
                if first == 0 || first > last {
                    return;
                }
                pictures.retain(|p| {
                    let hit = p.format == PictureFormat::Png && (first..=last).contains(&p.kitty_id);
                    !hit
                });
                if free {
                    self.images.retain(|img| !(first..=last).contains(&img.id));
                }
                return;
            }
            b'x' => {
                let x = kv.get(b'x').unwrap_or(0);
                if x == 0 {
                    return;
                }
                let col = (x - 1) as usize;
                pictures.retain(|p| {
                    let hit = p.format == PictureFormat::Png && col >= p.col && col < p.col.saturating_add(p.cols);
                    if hit {
                        touched_ids.push(p.kitty_id);
                    }
                    !hit
                });
            }
            b'y' => {
                let y = kv.get(b'y').unwrap_or(0);
                if y == 0 {
                    return;
                }
                let row = (y - 1) as usize;
                pictures.retain(|p| {
                    let hit = p.format == PictureFormat::Png && row >= p.row && row < p.row.saturating_add(p.rows);
                    if hit {
                        touched_ids.push(p.kitty_id);
                    }
                    !hit
                });
            }
            b'z' => {
                let z = kv.get(b'z').map_or(0, u32::cast_signed);
                pictures.retain(|p| {
                    let hit = p.format == PictureFormat::Png && p.z == z;
                    if hit {
                        touched_ids.push(p.kitty_id);
                    }
                    !hit
                });
            }
            _ => return,
        }
        if free {
            for id in touched_ids {
                if !pictures.iter().any(|p| p.format == PictureFormat::Png && p.kitty_id == id) {
                    self.images.retain(|img| img.id != id);
                }
            }
        }
    }

    /// Keeps the PNG made of `rect` of image `id`. Of the PNGs made of other images, the newest
    /// images' stay, as many as fit beside it.
    fn keep_made(&mut self, id: u32, rect: [u32; 4], png: &[u8]) {
        let mut room = MADE_BYTES_MAX.saturating_sub(png.len());
        for img in self.images.iter_mut().rev() {
            let len = img.made.as_ref().map_or(0, |(_, made)| made.len());
            if img.id == id {
                img.made = Some((rect, png.to_vec()));
            } else if len > room {
                img.made = None;
            } else {
                room -= len;
            }
        }
    }

    /// Drops the transfer that is under way, with the image it was to take the place of; whether there was one.
    pub(crate) fn abandon(&mut self, pictures: &mut Vec<Picture>) -> bool {
        let Some(loading) = self.loading.take() else { return false };
        if let Some(old_id) = loading.replaces {
            self.remove_image_and_placements(old_id, pictures);
        }
        true
    }

    fn remove_image_and_placements(&mut self, id: u32, pictures: &mut Vec<Picture>) {
        pictures.retain(|p| !(p.format == PictureFormat::Png && p.kitty_id == id));
        self.images.retain(|img| img.id != id);
    }

    fn alloc_id(&mut self) -> u32 {
        loop {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1);
            if id != 0 && !self.images.iter().any(|img| img.id == id) {
                return id;
            }
        }
    }

    /// Keeps `img`. While there are too many, or they are too large together, another goes for it: the
    /// oldest that is not placed, or the oldest of all when all are. One that is too large alone is not
    /// kept, and none goes for it here: the image it was sent to replace is gone by then, `finish_load`
    /// removes that first. (`decode_image` refuses such a one sooner, before its pixels are unpacked;
    /// the bound is held here whatever comes.)
    fn store_image(&mut self, img: StoredImage, pictures: &mut Vec<Picture>) -> Result<(), &'static str> {
        let needed = img.byte_len();
        if needed > STORED_BYTES_MAX {
            return Err(TOO_LARGE);
        }
        let total = |imgs: &[StoredImage]| imgs.iter().map(StoredImage::byte_len).sum::<usize>();
        while self.images.len() >= STORED_IMAGES_MAX || total(&self.images).saturating_add(needed) > STORED_BYTES_MAX {
            // Prefer evicting oldest unplaced image first, then oldest placed image (matches Kitty/Ghostty).
            let pos = self
                .images
                .iter()
                .position(|cand| !pictures.iter().any(|p| p.format == PictureFormat::Png && p.kitty_id == cand.id))
                .or_else(|| (!self.images.is_empty()).then_some(0));
            let Some(pos) = pos else { break };
            let evicted = self.images.remove(pos);
            pictures.retain(|p| !(p.format == PictureFormat::Png && p.kitty_id == evicted.id));
        }
        self.images.push(img);
        Ok(())
    }
}

struct DecodedImage {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
    png: Option<Vec<u8>>,
}

fn decode_image(
    format: Format,
    medium: Medium,
    compression: Compression,
    width: u32,
    height: u32,
    payload: &[u8],
    is_base64: bool,
) -> Result<DecodedImage, &'static str> {
    if medium != Medium::Direct {
        return Err("EINVAL: unsupported medium");
    }
    if format == Format::Unsupported {
        return Err("EINVAL: unsupported format");
    }
    if compression == Compression::Unsupported {
        return Err("EINVAL: unsupported compression");
    }
    let raw = if is_base64 {
        decode_base64(payload, PICTURE_MAX).map_err(|_| "EINVAL: invalid base64")?
    } else {
        payload.to_vec()
    };
    // An image is kept as RGBA, and a PNG's own bytes with it: one that a screen's store has no room
    // for, even empty, is refused before its pixels are unpacked. (Pixels that were sent, by the
    // width and height sent with them, before anything is; a PNG by its header, which is read once
    // the file is whole, and so after `o=z` is undone: that is 4 MiB at most.)
    let fits = |pixels: usize, more: usize| pixels.saturating_mul(4).saturating_add(more) <= STORED_BYTES_MAX;
    let max_uncompressed = match format {
        Format::Png => PICTURE_MAX,
        Format::Rgb | Format::Rgba => {
            let bpp = if format == Format::Rgb { 3usize } else { 4usize };
            if width == 0 || height == 0 {
                return Err("EINVAL: dimensions required");
            }
            if width > png::DIMENSION_MAX || height > png::DIMENSION_MAX {
                return Err("EINVAL: dimensions too large");
            }
            let pixels = (width as usize).checked_mul(height as usize).ok_or("EINVAL: dimensions too large")?;
            if pixels > PIXELS_MAX {
                return Err("EINVAL: dimensions too large");
            }
            if !fits(pixels, 0) {
                return Err(TOO_LARGE);
            }
            pixels.checked_mul(bpp).ok_or("EINVAL: dimensions too large")?
        }
        Format::Unsupported => return Err("EINVAL: unsupported format"),
    };
    let data = match compression {
        Compression::None => raw,
        Compression::Zlib => {
            crate::inflate::zlib(&raw, max_uncompressed).map_err(|_| "EINVAL: decompression failed")?
        }
        Compression::Unsupported => return Err("EINVAL: unsupported compression"),
    };

    match format {
        Format::Png => {
            if png::dimensions(&data).is_some_and(|(w, h)| !fits(w as usize * h as usize, data.len())) {
                return Err(TOO_LARGE);
            }
            let (w, h, rgba) = png::decode(&data, PIXELS_MAX).map_err(|_| "EINVAL: invalid PNG data")?;
            Ok(DecodedImage { w, h, rgba, png: Some(data) })
        }
        Format::Rgba => {
            if data.len() != max_uncompressed {
                return Err("EINVAL: invalid data");
            }
            Ok(DecodedImage { w: width, h: height, rgba: data, png: None })
        }
        Format::Rgb => {
            if data.len() != max_uncompressed {
                return Err("EINVAL: invalid data");
            }
            let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);
            for &[r, g, b] in data.as_chunks::<3>().0 {
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
            Ok(DecodedImage { w: width, h: height, rgba, png: None })
        }
        Format::Unsupported => Err("EINVAL: unsupported format"),
    }
}

fn grid_size(src_w: usize, src_h: usize, cols: usize, rows: usize, cell_w: usize, cell_h: usize) -> (usize, usize) {
    if cols > 0 && rows > 0 {
        return (cols, rows);
    }
    if cols > 0 {
        let px_w = cols.saturating_mul(cell_w);
        let px_h = scale_dim(px_w, src_h, src_w);
        return (cols, px_h.div_ceil(cell_h).max(1));
    }
    if rows > 0 {
        let px_h = rows.saturating_mul(cell_h);
        let px_w = scale_dim(px_h, src_w, src_h);
        return (px_w.div_ceil(cell_w).max(1), rows);
    }
    (src_w.div_ceil(cell_w).max(1), src_h.div_ceil(cell_h).max(1))
}

fn scale_dim(target: usize, num: usize, den: usize) -> usize {
    if den == 0 {
        return 0;
    }
    ((target as u128 * num as u128) / (den as u128)).min(usize::MAX as u128) as usize
}

fn crop_rgba(img_w: usize, rgba: &[u8], sx: usize, sy: usize, sw: usize, sh: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(sw.saturating_mul(sh).saturating_mul(4));
    for y in sy..sy + sh {
        let start = (y * img_w + sx) * 4;
        let end = start + sw * 4;
        if let Some(row) = rgba.get(start..end) {
            out.extend_from_slice(row);
        }
    }
    out
}

/// (Here and not with the other tests: nothing outside this file can hand `store_image` such an image.)
#[cfg(test)]
#[test]
fn an_image_over_what_a_screen_keeps_is_not_stored_whoever_asks() {
    let image = |id, bytes| StoredImage { id, number: 0, w: 1, h: 1, rgba: vec![0; bytes], png: None, made: None };
    let ids = |storage: &Storage| storage.images.iter().map(|img| img.id).collect::<Vec<_>>();
    let (mut storage, mut pictures) = (Storage::default(), Vec::new());
    assert_eq!(storage.store_image(image(1, 4), &mut pictures), Ok(()));
    assert_eq!(storage.store_image(image(2, STORED_BYTES_MAX + 1), &mut pictures), Err(TOO_LARGE));
    assert_eq!(ids(&storage), [1]);
    assert_eq!(storage.store_image(image(3, STORED_BYTES_MAX), &mut pictures), Ok(()));
    assert_eq!(ids(&storage), [3]);
}
