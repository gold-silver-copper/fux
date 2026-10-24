//! What a pane's program wrote, as waits read it (`crate::wait`): how many
//! bytes reached its screen, and their text, escape sequences and the
//! other control characters left out, read across every piece as one
//! stream. Output reaches the screen in pieces cut after each BEL and each
//! ST (`pieces`), so that what the journal records of a piece (a bell, a
//! title, a shell command's end: `crate::log`) comes at the piece's end,
//! after its text: each has its place in the bytes.

/// Where the text of the stream is: in text, or in a sequence it leaves
/// out, as ECMA-48 and xterm end them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Strip {
    #[default]
    Text,
    /// After ESC, and any intermediate bytes.
    Escape,
    /// In a CSI, until its final byte.
    Csi,
    /// In an OSC, until BEL or ST.
    Osc,
    /// In a DCS, SOS, PM or APC, until ST.
    String,
    /// After ESC in an OSC or one of those: ST if `\` follows, else ESC
    /// begins a sequence of its own.
    StringEscape,
}

impl Strip {
    /// Where byte `b` leaves it, and whether `b` is text: a line end as
    /// `\n`.
    fn step(self, b: u8) -> (Strip, Option<u8>) {
        match (self, b) {
            // CAN and SUB cancel a sequence anywhere.
            (_, 0x18 | 0x1a) => (Strip::Text, None),
            (Strip::StringEscape, b'\\') => (Strip::Text, None),
            (Strip::StringEscape, _) => Strip::Escape.step(b),
            (Strip::Osc, 0x07) => (Strip::Text, None),
            (Strip::Osc | Strip::String, 0x1b) => (Strip::StringEscape, None),
            (Strip::Osc | Strip::String, _) => (self, None),
            (_, 0x1b) => (Strip::Escape, None),
            (Strip::Escape, b'[') => (Strip::Csi, None),
            (Strip::Escape, b']') => (Strip::Osc, None),
            (Strip::Escape, b'P' | b'X' | b'^' | b'_') => (Strip::String, None),
            // Intermediates, and a control executed inside the sequence.
            (Strip::Escape, 0x00..=0x2f) => (Strip::Escape, None),
            (Strip::Escape, _) => (Strip::Text, None),
            (Strip::Csi, 0x40..=0x7e) => (Strip::Text, None),
            (Strip::Csi, _) => (Strip::Csi, None),
            (Strip::Text, b'\r' | b'\n') => (Strip::Text, Some(b'\n')),
            (Strip::Text, b'\t') => (Strip::Text, Some(b'\t')),
            (Strip::Text, 0x00..=0x1f | 0x7f) => (Strip::Text, None),
            (Strip::Text, _) => (Strip::Text, Some(b)),
        }
    }
}

/// What a pane's program wrote, as far as its screen has taken it.
#[derive(Debug, Default)]
pub struct Written {
    /// How many bytes its screen has taken.
    pub bytes: u64,
    /// How many bytes of text they made.
    pub text_len: u64,
    /// The text made since the server last let the waits read it.
    pub text: Vec<u8>,
    /// Where each piece taken since ended: in the text, and in the bytes.
    pub ends: Vec<(u64, u64)>,
    strip: Strip,
}

impl Written {
    /// `piece`, the next bytes the screen took.
    pub fn take(&mut self, piece: &[u8]) {
        if piece.is_empty() {
            return;
        }
        let before = self.text.len();
        for &b in piece {
            let (strip, text) = self.strip.step(b);
            self.strip = strip;
            self.text.extend(text);
        }
        let made = u64::try_from(self.text.len().saturating_sub(before)).unwrap_or(u64::MAX);
        let len = u64::try_from(piece.len()).unwrap_or(u64::MAX);
        self.text_len = self.text_len.saturating_add(made);
        self.bytes = self.bytes.saturating_add(len);
        self.ends.push((self.text_len, self.bytes));
    }

    /// The waits have read it: what came is let go.
    pub fn clear(&mut self) {
        self.text.clear();
        self.ends.clear();
    }
}

/// `bytes` in pieces, each ending after a BEL or an ST (`ESC \`), or at
/// the end.
pub fn pieces(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = bytes;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = (rest.iter().enumerate())
            .find(|(i, b)| {
                **b == 0x07
                    || (**b == b'\\' && i.checked_sub(1).and_then(|p| rest.get(p)) == Some(&0x1b))
            })
            .map_or(rest.len(), |(i, _)| i.saturating_add(1));
        let (piece, after) = rest.split_at_checked(end).unwrap_or((rest, &[]));
        rest = after;
        Some(piece)
    })
}

#[cfg(test)]
mod tests {
    use super::{Written, pieces};

    /// The text of what was written, however split, without its escape
    /// sequences: BEL ends an OSC alone, ST ends every string, ESC inside
    /// one begins a sequence of its own, CAN cancels.
    #[test]
    fn text_leaves_out_escape_sequences_however_split() {
        let output: &[u8] =
            b"\x1b[1mlis\x1b]0;ti\x07ten\x1bPq\x07x\x1b\\ing\x1b]2;a\x1b[0m on\x18 :80\r\nnext\t!";
        for split in 0..output.len() {
            let (a, b) = output.split_at_checked(split).unwrap_or((output, &[]));
            let mut written = Written::default();
            written.take(a);
            written.take(b);
            assert_eq!(
                written.text, b"listening on :80\n\nnext\t!",
                "split {split}"
            );
            assert_eq!(written.bytes, u64::try_from(output.len()).unwrap_or(0));
        }
    }

    /// Pieces end after each BEL and each ST.
    #[test]
    fn pieces_end_at_bells_and_string_ends() {
        let cut: Vec<&[u8]> = pieces(b"a\x07b\x1b]2;t\x1b\\c\x1b[m").collect();
        assert_eq!(cut, [&b"a\x07"[..], b"b\x1b]2;t\x1b\\", b"c\x1b[m"]);
    }
}
