//! Base64 (RFC 4648), as a clipboard is said in it (OSC 52): read strictly and to a bound, and written with its padding.

const B64_ENC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Decodes RFC 4648 base64 bytes, failing if invalid or if decoded size exceeds `max_out`: the
/// reading of a clipboard. Padding may be left out; if it is there it is the padding of that
/// length, and what is left over of the last character is nothing.
pub fn decode_base64(input: &[u8], max_out: usize) -> Result<Vec<u8>, &'static str> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity((input.len() / 4).saturating_mul(3).min(max_out));
    let mut buf = 0u32;
    let mut bits = 0u8;
    let mut pad = 0u8;
    for &b in input {
        if b == b'=' {
            pad += 1;
            if pad > 2 {
                return Err("invalid base64 padding");
            }
            continue;
        }
        if pad > 0 {
            return Err("data after base64 padding");
        }
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err("invalid base64 byte"),
        };
        buf = (buf << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            if out.len() >= max_out {
                return Err("decoded payload exceeds limit");
            }
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    if bits == 6 || (pad == 1 && bits != 2) || (pad == 2 && bits != 4) || buf & ((1 << bits) - 1) != 0 {
        return Err("truncated base64 payload");
    }
    Ok(out)
}

/// Appends RFC 4648 base64 encoding of `input` to `out`.
pub fn encode_base64(input: &[u8], out: &mut String) {
    let (chunks, rem) = input.as_chunks::<3>();
    for &[a, b, d] in chunks {
        let n = (u32::from(a) << 16) | (u32::from(b) << 8) | u32::from(d);
        for shift in [18, 12, 6, 0] {
            if let Some(&ch) = B64_ENC.get(((n >> shift) & 63) as usize) {
                out.push(char::from(ch));
            }
        }
    }
    match rem {
        [a] => {
            let n = u32::from(*a) << 16;
            for shift in [18, 12] {
                if let Some(&ch) = B64_ENC.get(((n >> shift) & 63) as usize) {
                    out.push(char::from(ch));
                }
            }
            out.push_str("==");
        }
        [a, b] => {
            let n = (u32::from(*a) << 16) | (u32::from(*b) << 8);
            for shift in [18, 12, 6] {
                if let Some(&ch) = B64_ENC.get(((n >> shift) & 63) as usize) {
                    out.push(char::from(ch));
                }
            }
            out.push('=');
        }
        _ => {}
    }
}
