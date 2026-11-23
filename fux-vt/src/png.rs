//! Bounded, zero-dependency PNG decoder and 8-bit RGBA encoder for Kitty graphics.

use crate::inflate;

/// Maximum width or height accepted for a PNG image (matches Kitty and Ghostty).
pub const DIMENSION_MAX: u32 = 10_000;
const SIGNATURE: [u8; 8] = *b"\x89PNG\r\n\x1a\n";
const ADAM7: [(usize, usize, usize, usize); 7] =
    [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)];

/// Fast header check: returns `(width, height)` if `data` starts with a valid PNG signature and IHDR.
pub fn dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.get(..8) != Some(&SIGNATURE) || data.len() < 33 {
        return None;
    }
    let len = u32::from_be_bytes([*data.get(8)?, *data.get(9)?, *data.get(10)?, *data.get(11)?]);
    if len != 13 || data.get(12..16) != Some(b"IHDR") {
        return None;
    }
    let w = u32::from_be_bytes([*data.get(16)?, *data.get(17)?, *data.get(18)?, *data.get(19)?]);
    let h = u32::from_be_bytes([*data.get(20)?, *data.get(21)?, *data.get(22)?, *data.get(23)?]);
    (w > 0 && h > 0 && w <= DIMENSION_MAX && h <= DIMENSION_MAX).then_some((w, h))
}

/// Decodes a PNG image into `(width, height, rgba)` where `rgba.len() == width * height * 4`.
/// One of more than `max_pixels` pixels, a side over [`DIMENSION_MAX`], or more than
/// [`crate::PICTURE_MAX`] (4 MiB) of compressed image data is refused.
pub fn decode(data: &[u8], max_pixels: usize) -> Result<(u32, u32, Vec<u8>), &'static str> {
    if data.get(..8) != Some(&SIGNATURE) {
        return Err("invalid PNG signature");
    }
    let mut pos = 8usize;
    let mut header: Option<Header> = None;
    let mut palette: Option<&[u8]> = None;
    let mut trns: Option<&[u8]> = None;
    let mut idat = Vec::new();
    let mut saw_end = false;

    while pos < data.len() {
        let len = read_u32(data, pos)? as usize;
        let kind = data.get(pos + 4..pos + 8).ok_or("truncated PNG chunk")?;
        let payload_start = pos.checked_add(8).ok_or("PNG chunk overflow")?;
        let payload_end = payload_start.checked_add(len).ok_or("PNG chunk overflow")?;
        let crc_end = payload_end.checked_add(4).ok_or("PNG chunk overflow")?;
        let payload = data.get(payload_start..payload_end).ok_or("truncated PNG chunk payload")?;
        let expected_crc = read_u32(data, payload_end)?;
        let crc_input = data.get(pos + 4..payload_end).ok_or("truncated PNG chunk")?;
        if crc32(crc_input) != expected_crc {
            return Err("PNG chunk CRC mismatch");
        }
        pos = crc_end;

        match kind {
            b"IHDR" => {
                if header.is_some() || len != 13 {
                    return Err("invalid PNG IHDR");
                }
                header = Some(Header::parse(payload, max_pixels)?);
            }
            b"PLTE" => {
                if header.is_none() || !len.is_multiple_of(3) || len == 0 || len > 768 {
                    return Err("invalid PNG PLTE");
                }
                palette = Some(payload);
            }
            b"tRNS" => {
                if header.is_none() {
                    return Err("tRNS before IHDR");
                }
                trns = Some(payload);
            }
            b"IDAT" => {
                if header.is_none() {
                    return Err("IDAT before IHDR");
                }
                if idat.len().checked_add(len).is_none_or(|total| total > crate::PICTURE_MAX) {
                    return Err("PNG IDAT exceeds limit");
                }
                idat.extend_from_slice(payload);
            }
            b"IEND" => {
                if len != 0 {
                    return Err("invalid PNG IEND");
                }
                saw_end = true;
                break;
            }
            _ => {
                // Critical unknown chunk (bit 5 of first byte is 0) must be rejected per spec.
                if header.is_none() || kind.first().is_some_and(|b| b & 0x20 == 0) {
                    return Err("unsupported critical PNG chunk");
                }
            }
        }
    }

    let hdr = header.ok_or("missing PNG IHDR")?;
    if !saw_end || idat.is_empty() {
        return Err("incomplete PNG stream");
    }
    if hdr.color_type == 3 && palette.is_none() {
        return Err("missing PNG palette");
    }

    let expected_raw = hdr.raw_len()?;
    let raw = inflate::zlib(&idat, expected_raw)?;
    if raw.len() != expected_raw {
        return Err("PNG image data length mismatch");
    }

    let mut rgba = vec![0u8; (hdr.w as usize) * (hdr.h as usize) * 4];
    let mut offset = 0usize;
    let passes: &[(usize, usize, usize, usize)] = if hdr.interlace == 0 { &[(0, 0, 1, 1)] } else { &ADAM7 };
    for &(x0, y0, dx, dy) in passes {
        let (pw, ph) = pass_dims(hdr.w as usize, hdr.h as usize, x0, y0, dx, dy);
        if pw == 0 || ph == 0 {
            continue;
        }
        let stride = hdr.row_bytes(pw)?;
        let mut prev = vec![0u8; stride];
        let mut curr = vec![0u8; stride];
        for py in 0..ph {
            let filter = *raw.get(offset).ok_or("truncated PNG scanline")?;
            offset += 1;
            let src = raw.get(offset..offset + stride).ok_or("truncated PNG scanline")?;
            offset += stride;
            unfilter(filter, src, &prev, &mut curr, hdr.bpp)?;
            let y = y0 + py * dy;
            expand_row(&hdr, &curr, palette, trns, pw, |px, pixel| {
                let x = x0 + px * dx;
                let dst_idx = (y * (hdr.w as usize) + x) * 4;
                if let Some(dst) = rgba.get_mut(dst_idx..dst_idx + 4) {
                    dst.copy_from_slice(&pixel);
                }
            })?;
            std::mem::swap(&mut prev, &mut curr);
        }
    }
    Ok((hdr.w, hdr.h, rgba))
}

/// Encodes 8-bit RGBA pixels (`rgba.len() == w * h * 4`) into a standard valid PNG
/// using stored zlib blocks, failing if the result would exceed `max` bytes. Nothing is
/// compressed: the file is `(4 * w + 1) * h` bytes and a few hundred, so `max` bounds the pixels
/// (4 MiB holds 1024 by 1023 of them, not 1024 by 1024).
pub fn encode_rgba(w: u32, h: u32, rgba: &[u8], max: usize) -> Result<Vec<u8>, &'static str> {
    if w == 0 || h == 0 || w > DIMENSION_MAX || h > DIMENSION_MAX {
        return Err("invalid PNG dimensions");
    }
    let row_bytes = (w as usize).checked_mul(4).ok_or("PNG size overflow")?;
    let expected = row_bytes.checked_mul(h as usize).ok_or("PNG size overflow")?;
    if rgba.len() != expected {
        return Err("RGBA buffer length mismatch");
    }
    let raw_len = (row_bytes + 1).checked_mul(h as usize).ok_or("PNG size overflow")?;
    if raw_len + 64 > max {
        return Err("PNG exceeds size limit");
    }
    #[cfg(test)]
    ENCODED.with(|encoded| encoded.set(encoded.get() + 1));
    let mut raw = Vec::with_capacity(raw_len);
    for row in rgba.chunks_exact(row_bytes) {
        raw.push(0); // Filter type 0 (None)
        raw.extend_from_slice(row);
    }
    let idat = inflate::zlib_store(&raw);
    let total = 8 + (12 + 13) + (12 + idat.len()) + 12;
    if total > max {
        return Err("PNG exceeds size limit");
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&SIGNATURE);
    let mut ihdr = [0u8; 13];
    if let Some(slot) = ihdr.get_mut(0..4) {
        slot.copy_from_slice(&w.to_be_bytes());
    }
    if let Some(slot) = ihdr.get_mut(4..8) {
        slot.copy_from_slice(&h.to_be_bytes());
    }
    if let Some(slot) = ihdr.get_mut(8..13) {
        slot.copy_from_slice(&[8, 6, 0, 0, 0]);
    }
    write_chunk(&mut out, b"IHDR", &ihdr);
    write_chunk(&mut out, b"IDAT", &idat);
    write_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

#[cfg(test)]
thread_local!(pub(crate) static ENCODED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });

struct Header {
    w: u32,
    h: u32,
    bit_depth: u8,
    color_type: u8,
    interlace: u8,
    channels: usize,
    bpp: usize,
}

impl Header {
    fn parse(data: &[u8], max_pixels: usize) -> Result<Self, &'static str> {
        let w = read_u32(data, 0)?;
        let h = read_u32(data, 4)?;
        let bit_depth = *data.get(8).ok_or("truncated PNG IHDR")?;
        let color_type = *data.get(9).ok_or("truncated PNG IHDR")?;
        let compression = *data.get(10).ok_or("truncated PNG IHDR")?;
        let filter = *data.get(11).ok_or("truncated PNG IHDR")?;
        let interlace = *data.get(12).ok_or("truncated PNG IHDR")?;
        if w == 0 || h == 0 || w > DIMENSION_MAX || h > DIMENSION_MAX {
            return Err("invalid PNG dimensions");
        }
        let pixels = (w as usize).checked_mul(h as usize).ok_or("PNG dimensions overflow")?;
        if pixels > max_pixels {
            return Err("PNG exceeds pixel limit");
        }
        if compression != 0 || filter != 0 || interlace > 1 {
            return Err("unsupported PNG header options");
        }
        let channels = match (color_type, bit_depth) {
            (0, 1 | 2 | 4 | 8 | 16) => 1,
            (2, 8 | 16) => 3,
            (3, 1 | 2 | 4 | 8) => 1,
            (4, 8 | 16) => 2,
            (6, 8 | 16) => 4,
            _ => return Err("unsupported PNG color type or bit depth"),
        };
        let bpp = (channels * usize::from(bit_depth)).div_ceil(8).max(1);
        Ok(Self { w, h, bit_depth, color_type, interlace, channels, bpp })
    }

    fn row_bytes(&self, width: usize) -> Result<usize, &'static str> {
        let bits = width.checked_mul(self.channels * usize::from(self.bit_depth)).ok_or("PNG row overflow")?;
        Ok(bits.div_ceil(8))
    }

    fn raw_len(&self) -> Result<usize, &'static str> {
        let passes: &[(usize, usize, usize, usize)] = if self.interlace == 0 { &[(0, 0, 1, 1)] } else { &ADAM7 };
        let mut total = 0usize;
        for &(x0, y0, dx, dy) in passes {
            let (pw, ph) = pass_dims(self.w as usize, self.h as usize, x0, y0, dx, dy);
            if pw == 0 || ph == 0 {
                continue;
            }
            let row = self.row_bytes(pw)?.checked_add(1).ok_or("PNG raw size overflow")?;
            let pass = row.checked_mul(ph).ok_or("PNG raw size overflow")?;
            total = total.checked_add(pass).ok_or("PNG raw size overflow")?;
        }
        Ok(total)
    }
}

fn pass_dims(w: usize, h: usize, x0: usize, y0: usize, dx: usize, dy: usize) -> (usize, usize) {
    let pw = if w > x0 { (w - x0).div_ceil(dx) } else { 0 };
    let ph = if h > y0 { (h - y0).div_ceil(dy) } else { 0 };
    (pw, ph)
}

fn unfilter(filter: u8, src: &[u8], prev: &[u8], dst: &mut [u8], bpp: usize) -> Result<(), &'static str> {
    match filter {
        0 => dst.copy_from_slice(src),
        1 => {
            for i in 0..src.len() {
                let s = src.get(i).copied().unwrap_or(0);
                let a = if i >= bpp { dst.get(i - bpp).copied().unwrap_or(0) } else { 0 };
                if let Some(slot) = dst.get_mut(i) {
                    *slot = s.wrapping_add(a);
                }
            }
        }
        2 => {
            for i in 0..src.len() {
                let s = src.get(i).copied().unwrap_or(0);
                let b = prev.get(i).copied().unwrap_or(0);
                if let Some(slot) = dst.get_mut(i) {
                    *slot = s.wrapping_add(b);
                }
            }
        }
        3 => {
            for i in 0..src.len() {
                let s = src.get(i).copied().unwrap_or(0);
                let a = u16::from(if i >= bpp { dst.get(i - bpp).copied().unwrap_or(0) } else { 0 });
                let b = u16::from(prev.get(i).copied().unwrap_or(0));
                if let Some(slot) = dst.get_mut(i) {
                    *slot = s.wrapping_add(((a + b) / 2) as u8);
                }
            }
        }
        4 => {
            for i in 0..src.len() {
                let s = src.get(i).copied().unwrap_or(0);
                let a = if i >= bpp { dst.get(i - bpp).copied().unwrap_or(0) } else { 0 };
                let b = prev.get(i).copied().unwrap_or(0);
                let c = if i >= bpp { prev.get(i - bpp).copied().unwrap_or(0) } else { 0 };
                if let Some(slot) = dst.get_mut(i) {
                    *slot = s.wrapping_add(paeth(a, b, c));
                }
            }
        }
        _ => return Err("invalid PNG filter type"),
    }
    Ok(())
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (i32::from(a), i32::from(b), i32::from(c));
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

fn expand_row(
    hdr: &Header,
    row: &[u8],
    palette: Option<&[u8]>,
    trns: Option<&[u8]>,
    width: usize,
    mut put: impl FnMut(usize, [u8; 4]),
) -> Result<(), &'static str> {
    let sample_at = |x: usize| -> u16 {
        match hdr.bit_depth {
            1 | 2 | 4 => {
                let bits = usize::from(hdr.bit_depth);
                let per_byte = 8 / bits;
                let byte = row.get(x / per_byte).copied().unwrap_or(0);
                let shift = 8 - bits * ((x % per_byte) + 1);
                u16::from((byte >> shift) & ((1u8 << bits) - 1))
            }
            8 => u16::from(row.get(x).copied().unwrap_or(0)),
            16 => {
                let hi = row.get(x * 2).copied().unwrap_or(0);
                let lo = row.get(x * 2 + 1).copied().unwrap_or(0);
                u16::from_be_bytes([hi, lo])
            }
            _ => 0,
        }
    };
    let scale_gray = |v: u16| -> u8 {
        match hdr.bit_depth {
            1 => (v as u8) * 255,
            2 => (v as u8) * 85,
            4 => (v as u8) * 17,
            8 => v as u8,
            16 => (v >> 8) as u8,
            _ => 0,
        }
    };
    let trns_u16 = |off: usize| -> Option<u16> {
        let t = trns?;
        Some(u16::from_be_bytes([*t.get(off)?, *t.get(off + 1)?]))
    };

    match hdr.color_type {
        0 => {
            let key = trns_u16(0);
            for x in 0..width {
                let raw = sample_at(x);
                let g = scale_gray(raw);
                let a = if key == Some(raw) { 0 } else { 255 };
                put(x, [g, g, g, a]);
            }
        }
        2 => {
            let key = trns_u16(0).zip(trns_u16(2)).zip(trns_u16(4)).map(|((r, g), b)| (r, g, b));
            let step = if hdr.bit_depth == 16 { 6 } else { 3 };
            for x in 0..width {
                let i = x * step;
                let (r16, g16, b16) = if hdr.bit_depth == 16 {
                    (
                        u16::from_be_bytes([row.get(i).copied().unwrap_or(0), row.get(i + 1).copied().unwrap_or(0)]),
                        u16::from_be_bytes([
                            row.get(i + 2).copied().unwrap_or(0),
                            row.get(i + 3).copied().unwrap_or(0),
                        ]),
                        u16::from_be_bytes([
                            row.get(i + 4).copied().unwrap_or(0),
                            row.get(i + 5).copied().unwrap_or(0),
                        ]),
                    )
                } else {
                    (
                        u16::from(row.get(i).copied().unwrap_or(0)),
                        u16::from(row.get(i + 1).copied().unwrap_or(0)),
                        u16::from(row.get(i + 2).copied().unwrap_or(0)),
                    )
                };
                let (r, g, b) = if hdr.bit_depth == 16 {
                    ((r16 >> 8) as u8, (g16 >> 8) as u8, (b16 >> 8) as u8)
                } else {
                    (r16 as u8, g16 as u8, b16 as u8)
                };
                let a = if key == Some((r16, g16, b16)) { 0 } else { 255 };
                put(x, [r, g, b, a]);
            }
        }
        3 => {
            let pal = palette.ok_or("missing PNG palette")?;
            for x in 0..width {
                let idx = usize::from(sample_at(x));
                let r = *pal.get(idx * 3).ok_or("PNG palette index out of range")?;
                let g = *pal.get(idx * 3 + 1).ok_or("PNG palette index out of range")?;
                let b = *pal.get(idx * 3 + 2).ok_or("PNG palette index out of range")?;
                let a = trns.and_then(|t| t.get(idx).copied()).unwrap_or(255);
                put(x, [r, g, b, a]);
            }
        }
        4 => {
            let step = if hdr.bit_depth == 16 { 4 } else { 2 };
            for x in 0..width {
                let i = x * step;
                let (g, a) = if hdr.bit_depth == 16 {
                    (row.get(i).copied().unwrap_or(0), row.get(i + 2).copied().unwrap_or(0))
                } else {
                    (row.get(i).copied().unwrap_or(0), row.get(i + 1).copied().unwrap_or(0))
                };
                put(x, [g, g, g, a]);
            }
        }
        6 => {
            let step = if hdr.bit_depth == 16 { 8 } else { 4 };
            for x in 0..width {
                let i = x * step;
                let pixel = if hdr.bit_depth == 16 {
                    [
                        row.get(i).copied().unwrap_or(0),
                        row.get(i + 2).copied().unwrap_or(0),
                        row.get(i + 4).copied().unwrap_or(0),
                        row.get(i + 6).copied().unwrap_or(0),
                    ]
                } else {
                    [
                        row.get(i).copied().unwrap_or(0),
                        row.get(i + 1).copied().unwrap_or(0),
                        row.get(i + 2).copied().unwrap_or(0),
                        row.get(i + 3).copied().unwrap_or(0),
                    ]
                };
                put(x, pixel);
            }
        }
        _ => return Err("unsupported PNG color type"),
    }
    Ok(())
}

fn read_u32(data: &[u8], pos: usize) -> Result<u32, &'static str> {
    let slice = data.get(pos..pos + 4).ok_or("truncated PNG u32")?;
    Ok(u32::from_be_bytes([
        *slice.first().ok_or("truncated PNG u32")?,
        *slice.get(1).ok_or("truncated PNG u32")?,
        *slice.get(2).ok_or("truncated PNG u32")?,
        *slice.get(3).ok_or("truncated PNG u32")?,
    ]))
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    let crc = crc32(out.get(start..).unwrap_or_default());
    out.extend_from_slice(&crc.to_be_bytes());
}

/// What eight steps of the CRC make of each byte.
static CRC_TABLE: std::sync::LazyLock<[u32; 256]> = std::sync::LazyLock::new(|| {
    let step = |crc: u32, _| if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
    std::array::from_fn(|byte| (0..8).fold(byte as u32, step))
});

pub(crate) fn crc32(data: &[u8]) -> u32 {
    let table = &*CRC_TABLE;
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        let step = table.get(usize::from(crc as u8 ^ byte)).copied().unwrap_or(0);
        crc = step ^ (crc >> 8);
    }
    !crc
}
