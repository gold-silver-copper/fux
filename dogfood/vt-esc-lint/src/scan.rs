//! Splits a byte stream into text, controls and sequences, as ECMA-48
//! frames them (Williams's DEC parser model), keeping what is malformed.

/// What ends a control string (OSC, DCS, APC, PM, SOS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ender {
    /// BEL, which xterm takes for OSC.
    Bel,
    /// ST, ESC `\`.
    St,
    /// ESC and another byte: the string ends, and a new sequence begins.
    Esc,
    /// CAN or SUB: the string is thrown away.
    Cancelled(u8),
    /// The input ended first.
    Eof,
}

/// The introducer of a control string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Introducer {
    Osc,
    Dcs,
    Apc,
    Pm,
    Sos,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Printable bytes, UTF-8 or not.
    Text,
    /// A C0 control or DEL.
    Control(u8),
    /// ESC, its intermediates and final byte.
    Esc { intermediates: Vec<u8>, last: u8 },
    /// CSI: its parameter bytes (private marker included), intermediates
    /// and final byte, and the C0 controls found inside it, which still run.
    Csi {
        params: Vec<u8>,
        intermediates: Vec<u8>,
        last: u8,
        controls: Vec<u8>,
        disordered: bool,
    },
    /// A control string, its contents, and how it ended.
    String { introducer: Introducer, body: Vec<u8>, ender: Ender },
    /// Not a sequence: why.
    Broken(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
}

const ESC: u8 = 0x1b;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// The stream's tokens, in order, covering every byte.
pub fn tokens(bytes: &[u8]) -> Vec<Token> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        let (end, kind) = match byte {
            ESC => escape(bytes, at),
            0x00..=0x1f | 0x7f => (at + 1, Kind::Control(byte)),
            _ => {
                let len = bytes
                    .get(at..)
                    .unwrap_or_default()
                    .iter()
                    .take_while(|&&b| b >= 0x20 && b != 0x7f)
                    .count();
                (at + len, Kind::Text)
            }
        };
        out.push(Token { start: at, end, kind });
        at = end;
    }
    out
}

fn name(byte: u8) -> String {
    match byte {
        ESC => "ESC".into(),
        CAN => "CAN".into(),
        SUB => "SUB".into(),
        0x00..=0x1f | 0x7f => format!("control 0x{byte:02x}"),
        _ => format!("byte 0x{byte:02x}"),
    }
}

/// The sequence the ESC at `at` begins: where it ends, and what it is.
fn escape(bytes: &[u8], at: usize) -> (usize, Kind) {
    let introducer = match bytes.get(at + 1) {
        None => return (at + 1, Kind::Broken("ESC at the end of the input".into())),
        Some(b'[') => return csi(bytes, at + 2),
        Some(b']') => Introducer::Osc,
        Some(b'P') => Introducer::Dcs,
        Some(b'_') => Introducer::Apc,
        Some(b'^') => Introducer::Pm,
        Some(b'X') => Introducer::Sos,
        Some(_) => return plain_escape(bytes, at + 1),
    };
    string(bytes, at + 2, introducer)
}

fn plain_escape(bytes: &[u8], from: usize) -> (usize, Kind) {
    let mut intermediates = Vec::new();
    let mut at = from;
    loop {
        match bytes.get(at).copied() {
            None => return (at, Kind::Broken("escape sequence cut off by the end of the input".into())),
            Some(b @ 0x20..=0x2f) => intermediates.push(b),
            Some(last @ 0x30..=0x7e) => return (at + 1, Kind::Esc { intermediates, last }),
            Some(b @ (CAN | SUB)) => {
                return (at + 1, Kind::Broken(format!("escape sequence cancelled by {}", name(b))));
            }
            // Another ESC, a control or a non-ASCII byte: the sequence is
            // abandoned there, and that byte read afresh.
            Some(b) => {
                return (at, Kind::Broken(format!("escape sequence broken off by {}", name(b))));
            }
        }
        at += 1;
    }
}

fn csi(bytes: &[u8], from: usize) -> (usize, Kind) {
    let (mut params, mut intermediates, mut controls) = (Vec::new(), Vec::new(), Vec::new());
    let mut disordered = false;
    let mut at = from;
    loop {
        match bytes.get(at).copied() {
            None => return (at, Kind::Broken("CSI cut off by the end of the input".into())),
            Some(b @ 0x30..=0x3f) => {
                // A private marker past the first byte, or a parameter byte
                // after an intermediate, is out of order.
                let marker = matches!(b, b'<'..=b'?');
                disordered |= !intermediates.is_empty() || (marker && at != from);
                params.push(b);
            }
            Some(b @ 0x20..=0x2f) => intermediates.push(b),
            Some(last @ 0x40..=0x7e) => {
                let kind = Kind::Csi { params, intermediates, last, controls, disordered };
                return (at + 1, kind);
            }
            Some(b @ (CAN | SUB)) => {
                return (at + 1, Kind::Broken(format!("CSI cancelled by {}", name(b))));
            }
            Some(ESC) => return (at, Kind::Broken("CSI broken off by ESC".into())),
            Some(b @ (0x00..=0x1f | 0x7f)) => controls.push(b),
            Some(b) => return (at, Kind::Broken(format!("CSI broken off by {}", name(b)))),
        }
        at += 1;
    }
}

fn string(bytes: &[u8], from: usize, introducer: Introducer) -> (usize, Kind) {
    let mut at = from;
    let done = |end, body: &[u8], ender| (end, Kind::String { introducer, body: body.to_vec(), ender });
    loop {
        let body = bytes.get(from..at).unwrap_or_default();
        match bytes.get(at).copied() {
            None => return done(at, body, Ender::Eof),
            Some(0x07) if introducer == Introducer::Osc => return done(at + 1, body, Ender::Bel),
            Some(b @ (CAN | SUB)) => return done(at + 1, body, Ender::Cancelled(b)),
            Some(ESC) if bytes.get(at + 1) == Some(&b'\\') => return done(at + 2, body, Ender::St),
            Some(ESC) => return done(at, body, Ender::Esc),
            Some(_) => at += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_byte_is_in_one_token() {
        let input = b"a\x1b[1;31mb\x1b]2;t\x07\x1b[\x1b(0\x1b";
        let tokens = tokens(input);
        let mut at = 0;
        for token in &tokens {
            assert_eq!(token.start, at);
            at = token.end;
        }
        assert_eq!(at, input.len());
        assert!(matches!(tokens.get(4).map(|t| &t.kind), Some(Kind::Broken(_))));
    }
}
