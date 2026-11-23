//! Bounded, zero-dependency RFC 1950 (zlib) and RFC 1951 (DEFLATE) decompression,
//! plus a stored-block zlib writer for encoding uncompressed PNGs.

const MAX_BITS: usize = 15;
const CL_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

/// Decompresses an RFC 1950 zlib stream, verifying header and Adler-32 checksum,
/// failing if output would exceed `max_out` bytes.
pub fn zlib(data: &[u8], max_out: usize) -> Result<Vec<u8>, &'static str> {
    if data.len() < 6 {
        return Err("truncated zlib stream");
    }
    let cmf = *data.first().ok_or("truncated zlib stream")?;
    let flg = *data.get(1).ok_or("truncated zlib stream")?;
    if cmf & 0x0f != 8 || (cmf >> 4) > 7 {
        return Err("unsupported zlib compression");
    }
    if !(u16::from(cmf) * 256 + u16::from(flg)).is_multiple_of(31) {
        return Err("invalid zlib header");
    }
    if flg & 0x20 != 0 {
        return Err("unsupported zlib dictionary");
    }
    let body = data.get(2..data.len() - 4).ok_or("truncated zlib stream")?;
    let trailer = data.get(data.len() - 4..).ok_or("truncated zlib stream")?;
    let expected = u32::from_be_bytes([
        *trailer.first().ok_or("truncated zlib trailer")?,
        *trailer.get(1).ok_or("truncated zlib trailer")?,
        *trailer.get(2).ok_or("truncated zlib trailer")?,
        *trailer.get(3).ok_or("truncated zlib trailer")?,
    ]);
    let out = deflate(body, max_out)?;
    if adler32(&out) != expected {
        return Err("zlib checksum mismatch");
    }
    Ok(out)
}

/// Decompresses a raw RFC 1951 DEFLATE stream, failing if output would exceed `max_out` bytes.
pub fn deflate(data: &[u8], max_out: usize) -> Result<Vec<u8>, &'static str> {
    let mut reader = Bits::new(data);
    let mut out = Vec::new();
    // (The fixed trees are the same for every block that uses them: built for the first, then kept.)
    let mut fixed = None;
    loop {
        let final_block = reader.take(1)? != 0;
        match reader.take(2)? {
            0 => read_stored(&mut reader, &mut out, max_out)?,
            1 => {
                let (lit, dist) = match &fixed {
                    Some(trees) => trees,
                    None => fixed.insert(fixed_trees()?),
                };
                decode_block(&mut reader, lit, dist, &mut out, max_out)?;
            }
            2 => {
                let (lit, dist) = dynamic_trees(&mut reader)?;
                decode_block(&mut reader, &lit, &dist, &mut out, max_out)?;
            }
            _ => return Err("invalid deflate block type"),
        }
        if final_block {
            return Ok(out);
        }
    }
}

/// Wraps `data` in a valid RFC 1950 zlib stream using stored (uncompressed) DEFLATE blocks.
pub fn zlib_store(data: &[u8]) -> Vec<u8> {
    let blocks = data.len().div_ceil(65535).max(1);
    let mut out = Vec::with_capacity(data.len() + 6 + blocks * 5);
    out.extend_from_slice(&[0x78, 0x01]);
    if data.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    } else {
        let mut chunks = data.chunks(65535).peekable();
        while let Some(chunk) = chunks.next() {
            let last = u8::from(chunks.peek().is_none());
            let len = chunk.len() as u16;
            out.push(last);
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(chunk);
        }
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// RFC 1950 Adler-32 checksum.
pub fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    bits: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, buf: 0, bits: 0 }
    }

    fn fill(&mut self) {
        while self.bits <= 56 {
            let Some(&byte) = self.data.get(self.pos) else { break };
            self.buf |= u64::from(byte) << self.bits;
            self.bits += 8;
            self.pos += 1;
        }
    }

    fn take(&mut self, n: u32) -> Result<u32, &'static str> {
        if n == 0 {
            return Ok(0);
        }
        if self.bits < n {
            self.fill();
            if self.bits < n {
                return Err("unexpected end of deflate stream");
            }
        }
        let val = (self.buf & ((1u64 << n) - 1)) as u32;
        self.buf >>= n;
        self.bits -= n;
        Ok(val)
    }

    fn align_byte(&mut self) {
        let drop = self.bits & 7;
        self.buf >>= drop;
        self.bits -= drop;
    }

    fn read_byte(&mut self) -> Result<u8, &'static str> {
        if self.bits >= 8 {
            let byte = (self.buf & 0xff) as u8;
            self.buf >>= 8;
            self.bits -= 8;
            Ok(byte)
        } else {
            let byte = *self.data.get(self.pos).ok_or("unexpected end of deflate stream")?;
            self.pos += 1;
            Ok(byte)
        }
    }
}

struct Tree {
    counts: [u16; MAX_BITS + 1],
    symbols: Vec<u16>,
}

#[cfg(test)]
thread_local!(pub(crate) static TREES_BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });

impl Tree {
    fn build(lengths: &[u8]) -> Result<Self, &'static str> {
        #[cfg(test)]
        TREES_BUILT.with(|built| built.set(built.get() + 1));
        let mut counts = [0u16; MAX_BITS + 1];
        for &len in lengths {
            let len = usize::from(len);
            if len > MAX_BITS {
                return Err("invalid huffman code length");
            }
            if let Some(slot) = counts.get_mut(len) {
                *slot = slot.saturating_add(1);
            }
        }
        if counts.first().copied().unwrap_or(0) as usize == lengths.len() {
            return Ok(Self { counts: [0; MAX_BITS + 1], symbols: Vec::new() });
        }
        let mut left = 1i32;
        for len in 1..=MAX_BITS {
            left <<= 1;
            left -= i32::from(counts.get(len).copied().unwrap_or(0));
            if left < 0 {
                return Err("oversubscribed huffman tree");
            }
        }
        let mut offsets = [0u16; MAX_BITS + 1];
        for len in 1..MAX_BITS {
            let prev_off = offsets.get(len).copied().unwrap_or(0);
            let prev_cnt = counts.get(len).copied().unwrap_or(0);
            if let Some(next) = offsets.get_mut(len + 1) {
                *next = prev_off.saturating_add(prev_cnt);
            }
        }
        let total: usize = counts.iter().skip(1).map(|&c| usize::from(c)).sum();
        let mut symbols = vec![0u16; total];
        for (sym, &len) in lengths.iter().enumerate() {
            let len = usize::from(len);
            if len == 0 {
                continue;
            }
            let Some(off) = offsets.get_mut(len) else { return Err("invalid huffman code length") };
            let idx = usize::from(*off);
            *off = off.saturating_add(1);
            if let Some(slot) = symbols.get_mut(idx) {
                *slot = sym as u16;
            }
        }
        Ok(Self { counts, symbols })
    }

    fn decode(&self, reader: &mut Bits<'_>) -> Result<u16, &'static str> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0usize);
        for len in 1..=MAX_BITS {
            code |= reader.take(1)? as i32;
            let count = i32::from(self.counts.get(len).copied().unwrap_or(0));
            if code - count < first {
                let sym_idx = index + (code - first) as usize;
                return self.symbols.get(sym_idx).copied().ok_or("invalid huffman symbol");
            }
            index += count as usize;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("invalid huffman code")
    }
}

fn read_stored(reader: &mut Bits<'_>, out: &mut Vec<u8>, max_out: usize) -> Result<(), &'static str> {
    reader.align_byte();
    let len = u16::from_le_bytes([reader.read_byte()?, reader.read_byte()?]);
    let nlen = u16::from_le_bytes([reader.read_byte()?, reader.read_byte()?]);
    if len != !nlen {
        return Err("invalid stored deflate block length");
    }
    let len = usize::from(len);
    if out.len().checked_add(len).is_none_or(|total| total > max_out) {
        return Err("decompressed data exceeds limit");
    }
    out.reserve(len);
    for _ in 0..len {
        out.push(reader.read_byte()?);
    }
    Ok(())
}

fn fixed_trees() -> Result<(Tree, Tree), &'static str> {
    let mut lit = [0u8; 288];
    for (i, slot) in lit.iter_mut().enumerate() {
        *slot = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    let dist = [5u8; 32];
    Ok((Tree::build(&lit)?, Tree::build(&dist)?))
}

fn dynamic_trees(reader: &mut Bits<'_>) -> Result<(Tree, Tree), &'static str> {
    let hlit = reader.take(5)? as usize + 257;
    let hdist = reader.take(5)? as usize + 1;
    let hclen = reader.take(4)? as usize + 4;
    if hlit > 286 || hdist > 30 {
        return Err("invalid dynamic huffman header");
    }
    let mut cl_lengths = [0u8; 19];
    for &idx in CL_ORDER.iter().take(hclen) {
        if let Some(slot) = cl_lengths.get_mut(idx) {
            *slot = reader.take(3)? as u8;
        }
    }
    let cl_tree = Tree::build(&cl_lengths)?;
    let total = hlit + hdist;
    let mut lengths = Vec::with_capacity(total);
    while lengths.len() < total {
        let sym = cl_tree.decode(reader)?;
        match sym {
            0..=15 => lengths.push(sym as u8),
            16 => {
                let prev = *lengths.last().ok_or("invalid huffman length repeat")?;
                let repeat = reader.take(2)? as usize + 3;
                if lengths.len() + repeat > total {
                    return Err("huffman length repeat overflow");
                }
                lengths.extend(std::iter::repeat_n(prev, repeat));
            }
            17 => {
                let repeat = reader.take(3)? as usize + 3;
                if lengths.len() + repeat > total {
                    return Err("huffman zero repeat overflow");
                }
                lengths.extend(std::iter::repeat_n(0, repeat));
            }
            18 => {
                let repeat = reader.take(7)? as usize + 11;
                if lengths.len() + repeat > total {
                    return Err("huffman long zero repeat overflow");
                }
                lengths.extend(std::iter::repeat_n(0, repeat));
            }
            _ => return Err("invalid huffman length symbol"),
        }
    }
    let (lit_lens, dist_lens) = lengths.split_at_checked(hlit).ok_or("invalid huffman table split")?;
    if lit_lens.get(256).copied().unwrap_or(0) == 0 {
        return Err("missing end-of-block symbol");
    }
    Ok((Tree::build(lit_lens)?, Tree::build(dist_lens)?))
}

fn decode_block(
    reader: &mut Bits<'_>,
    lit: &Tree,
    dist: &Tree,
    out: &mut Vec<u8>,
    max_out: usize,
) -> Result<(), &'static str> {
    loop {
        let sym = lit.decode(reader)?;
        match sym {
            0..=255 => {
                if out.len() >= max_out {
                    return Err("decompressed data exceeds limit");
                }
                out.push(sym as u8);
            }
            256 => return Ok(()),
            257..=285 => {
                let li = usize::from(sym - 257);
                let base = usize::from(*LEN_BASE.get(li).ok_or("invalid deflate length")?);
                let extra = u32::from(*LEN_EXTRA.get(li).ok_or("invalid deflate length")?);
                let length = base + reader.take(extra)? as usize;

                let dsym = usize::from(dist.decode(reader)?);
                let dbase = usize::from(*DIST_BASE.get(dsym).ok_or("invalid deflate distance")?);
                let dextra = u32::from(*DIST_EXTRA.get(dsym).ok_or("invalid deflate distance")?);
                let distance = dbase + reader.take(dextra)? as usize;

                if distance == 0 || distance > out.len() {
                    return Err("deflate distance out of range");
                }
                if out.len().checked_add(length).is_none_or(|total| total > max_out) {
                    return Err("decompressed data exceeds limit");
                }
                for _ in 0..length {
                    let src = out.len() - distance;
                    let byte = *out.get(src).ok_or("deflate back-reference out of range")?;
                    out.push(byte);
                }
            }
            _ => return Err("invalid deflate literal/length symbol"),
        }
    }
}
