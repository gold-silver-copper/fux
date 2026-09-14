//! Byte-exact input classification for the viewer: the configured prefix, command keys, literal
//! prefix, bracketed paste passthrough, complete CSI/SS3 sequences and SGR mouse reports.
//! Ordinary input is never altered; only command-mode keys are consumed.

use crate::commands::{Action, ClientBindings};
use crate::proto::attach::MouseEvent;

const PASTE_BEGIN: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";
/// Longest escape sequence kept together before being forwarded as ordinary bytes.
const MAX_SEQUENCE: usize = 64;

/// A scroll request for the command column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollBy {
    Rows(i32),
    Screens(i32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// Ordinary pane input, byte-exact (paste delimiters included).
    Bytes(Vec<u8>),
    /// A disambiguated lone Escape, distinct from paste and literal-prefix bytes.
    Escape,
    /// A configured command key was pressed after the prefix.
    Command(Action),
    /// A complete SGR mouse report outside command mode.
    Mouse(MouseEvent),
    /// A mouse report owned by the visible command popup.
    PopupMouse(MouseEvent),
    /// Prefix then an unbound key: command mode stays active and the popup is revealed.
    Unknown,
    /// Prefix then Esc, or Esc while the popup is open.
    Cancel,
    /// Up/Down (one row) or PageUp/PageDown (one screenful) while the popup is open.
    Scroll(ScrollBy),
}

/// Consume a paste delimiter incrementally, retaining overlapping ESC prefixes.
/// Returned bytes are literal paste content; the delimiter itself is reported separately.
pub(super) fn paste_byte(pending: &mut Vec<u8>, byte: u8) -> (Vec<u8>, bool) {
    pending.push(byte);
    if pending == PASTE_END {
        pending.clear();
        return (Vec::new(), true);
    }
    let mut literal = Vec::new();
    while !PASTE_END.starts_with(pending) {
        literal.push(pending.remove(0));
    }
    (literal, false)
}

/// A stateful classifier over one viewer's raw terminal input.
#[derive(Clone, Debug)]
pub struct PrefixFilter {
    bindings: ClientBindings,
    command_pending: bool,
    reveal: bool,
    paste: bool,
    sequence: Vec<u8>,
}

impl PrefixFilter {
    #[must_use]
    pub fn new(bindings: ClientBindings) -> Self {
        Self {
            bindings,
            command_pending: false,
            reveal: false,
            paste: false,
            sequence: Vec::new(),
        }
    }

    pub fn bindings(&self) -> &ClientBindings {
        &self.bindings
    }

    /// Replaces the registry. A pending command is cancelled: the popup shown for the old
    /// bindings must not dispatch under new ones.
    pub fn configure(&mut self, bindings: ClientBindings) {
        if bindings != self.bindings {
            self.bindings = bindings;
            self.command_pending = false;
            self.reveal = false;
        }
    }

    #[must_use]
    pub fn command_pending(&self) -> bool {
        self.command_pending
    }

    /// The column is shown while command mode is pending; `reveal` distinguishes an unknown key
    /// from a plain prefix press (both show it immediately).
    #[must_use]
    pub fn popup_visible(&self) -> bool {
        self.command_pending
    }

    #[must_use]
    pub fn revealed(&self) -> bool {
        self.reveal
    }

    /// Opens the popup as if the prefix had been pressed (used when a mode backs out).
    pub fn show_commands(&mut self) {
        self.command_pending = true;
        self.reveal = true;
    }

    pub fn cancel(&mut self) {
        self.command_pending = false;
        self.reveal = false;
    }

    /// An unfinished escape sequence is waiting for more bytes or a timeout.
    #[must_use]
    pub fn escape_pending(&self) -> bool {
        self.sequence.first() == Some(&0x1b)
            && !self.paste
            && (self.sequence.len() == 1 || self.sequence.last() == Some(&0x1b))
    }

    /// Resolves a lone Escape (or a trailing one) after the disambiguation delay.
    pub fn resolve_escape(&mut self) -> Vec<InputEvent> {
        self.resolve_history_escape(false)
    }

    /// Escape dismisses active history even when Escape is also the configured prefix.
    /// A subsequent Escape with no history follows the configured prefix normally.
    pub fn resolve_history_escape(&mut self, history: bool) -> Vec<InputEvent> {
        if history && !self.command_pending && !self.paste && self.sequence == [27] {
            self.sequence.clear();
            return vec![InputEvent::Escape];
        }
        if !self.escape_pending() {
            return Vec::new();
        }
        let pending = std::mem::take(&mut self.sequence);
        let mut events = Vec::new();
        for byte in pending {
            self.plain(byte, &mut events, true);
        }
        coalesce(events)
    }

    /// Classifies a chunk of terminal input in order.
    pub fn feed(&mut self, input: &[u8]) -> Vec<InputEvent> {
        let mut events = Vec::new();
        for &byte in input {
            self.feed_byte(byte, &mut events);
        }
        coalesce(events)
    }

    fn feed_byte(&mut self, byte: u8, events: &mut Vec<InputEvent>) {
        if self.paste {
            let (literal, ended) = paste_byte(&mut self.sequence, byte);
            if !literal.is_empty() {
                events.push(InputEvent::Bytes(literal));
            }
            if ended {
                events.push(InputEvent::Bytes(PASTE_END.to_vec()));
                self.paste = false;
            }
            return;
        }
        if !self.sequence.is_empty() || byte == 0x1b {
            self.sequence.push(byte);
            let complete = match self.sequence.as_slice() {
                [0x1b] => false,
                [0x1b, 0x1b] => false,
                [0x1b, b'[' | b'O'] => false,
                [0x1b, b'[' | b'O', rest @ ..] => {
                    rest.last().is_some_and(|last| (0x40..=0x7e).contains(last))
                        || self.sequence.len() >= MAX_SEQUENCE
                }
                _ => true,
            };
            if !complete {
                return;
            }
            let sequence = std::mem::take(&mut self.sequence);
            if sequence == PASTE_BEGIN {
                self.paste = true;
                if self.command_pending {
                    // A paste cannot be a command key; leave command mode without leaking it.
                    self.cancel();
                }
                events.push(InputEvent::Bytes(sequence));
                return;
            }
            if sequence.len() == 2
                && sequence.first() == Some(&0x1b)
                && sequence.get(1) != Some(&0x1b)
            {
                // Esc followed by an ordinary byte: Alt-key or a fast Esc+key, not a command.
                for byte in sequence {
                    self.plain(byte, events, false);
                }
                return;
            }
            if self.command_pending {
                self.command_sequence(&sequence, events);
                return;
            }
            if let Some(mouse) = MouseEvent::parse(&sequence) {
                events.push(InputEvent::Mouse(mouse));
                return;
            }
            events.push(InputEvent::Bytes(sequence));
            return;
        }
        self.plain(byte, events, false);
    }

    /// A complete escape sequence while the popup is open never reaches the pane.
    fn command_sequence(&mut self, sequence: &[u8], events: &mut Vec<InputEvent>) {
        if sequence == [0x1b, 0x1b] {
            // Prefix twice when the prefix is Escape forwards one Escape.
            self.cancel();
            events.push(if self.bindings.prefix() == 0x1b {
                InputEvent::Bytes(vec![0x1b])
            } else {
                InputEvent::Cancel
            });
            return;
        }
        self.reveal = true;
        if let Some(mouse) = MouseEvent::parse(sequence) {
            events.push(InputEvent::PopupMouse(mouse));
            return;
        }
        events.push(match sequence {
            b"\x1b[B" | b"\x1bOB" => InputEvent::Scroll(ScrollBy::Rows(1)),
            b"\x1b[A" | b"\x1bOA" => InputEvent::Scroll(ScrollBy::Rows(-1)),
            b"\x1b[6~" => InputEvent::Scroll(ScrollBy::Screens(1)),
            b"\x1b[5~" => InputEvent::Scroll(ScrollBy::Screens(-1)),
            _ => InputEvent::Unknown,
        });
    }

    fn plain(&mut self, byte: u8, events: &mut Vec<InputEvent>, resolved_escape: bool) {
        let prefix = self.bindings.prefix();
        if self.command_pending {
            // Literal prefix takes precedence over cancellation and bindings.
            if byte == prefix {
                self.cancel();
                events.push(InputEvent::Bytes(vec![byte]));
                return;
            }
            if byte == 0x1b {
                self.cancel();
                events.push(InputEvent::Cancel);
                return;
            }
            match self.bindings.action(byte) {
                Some(action) => {
                    self.cancel();
                    events.push(InputEvent::Command(action));
                }
                None => {
                    self.reveal = true;
                    events.push(InputEvent::Unknown);
                }
            }
            return;
        }
        if byte == prefix && (byte != 0x1b || resolved_escape) {
            self.command_pending = true;
            self.reveal = false;
            return;
        }
        events.push(if byte == 27 && resolved_escape {
            InputEvent::Escape
        } else {
            InputEvent::Bytes(vec![byte])
        });
    }
}

fn coalesce(events: Vec<InputEvent>) -> Vec<InputEvent> {
    let mut output: Vec<InputEvent> = Vec::with_capacity(events.len());
    for event in events {
        match (output.last_mut(), event) {
            (Some(InputEvent::Bytes(previous)), InputEvent::Bytes(bytes)) => previous.extend(bytes),
            (_, event) => output.push(event),
        }
    }
    output
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn fresh() -> PrefixFilter {
        PrefixFilter::new(ClientBindings::default())
    }

    #[test]
    fn escape_prefix_dismisses_history_before_entering_commands() {
        let mut filter = PrefixFilter::new(ClientBindings::new(27, [(b'x', Action::ClosePane)]));
        assert!(filter.feed(b"\x1b").is_empty());
        assert_eq!(
            filter.resolve_history_escape(true),
            vec![InputEvent::Escape]
        );
        assert!(!filter.command_pending());
        filter.feed(b"\x1b");
        assert!(filter.resolve_history_escape(false).is_empty());
        assert!(filter.command_pending());
    }

    #[test]
    fn overlapping_paste_end_prefixes_preserve_bytes_and_release_ownership() {
        let payload = b"\x1b[200~a\x1b\x1b[201~";
        for split in 0..=payload.len() {
            let mut filter = fresh();
            let mut events = filter.feed(&payload[..split]);
            events.extend(filter.feed(&payload[split..]));
            let bytes = events
                .into_iter()
                .flat_map(|event| match event {
                    InputEvent::Bytes(bytes) => bytes,
                    other => panic!("unexpected {other:?}"),
                })
                .collect::<Vec<_>>();
            assert_eq!(bytes, payload);
            assert_eq!(
                filter.feed(b"\x01|"),
                vec![InputEvent::Command(Action::SplitSide)]
            );
        }
    }

    #[test]
    fn only_a_disambiguated_escape_can_dismiss_history() {
        let mut filter = fresh();
        assert!(filter.feed(b"\x1b").is_empty());
        assert_eq!(filter.resolve_escape(), vec![InputEvent::Escape]);
        assert_eq!(
            filter.feed(b"\x1bx"),
            vec![InputEvent::Bytes(b"\x1bx".to_vec())]
        );
        let paste = b"\x1b[200~\x1bx\x01\x1b[201~";
        assert_eq!(filter.feed(paste), vec![InputEvent::Bytes(paste.to_vec())]);
    }

    #[test]
    fn ordinary_input_is_byte_exact_including_paste() {
        let mut filter = fresh();
        let payload = b"plain \x1b[200~\x01x\x1b[201~ tail";
        let events = filter.feed(payload);
        assert_eq!(events, vec![InputEvent::Bytes(payload.to_vec())]);
        // Split across reads at every position.
        for split in 1..payload.len() {
            let mut filter = fresh();
            let mut events = filter.feed(&payload[..split]);
            events.extend(filter.feed(&payload[split..]));
            events.extend(filter.resolve_escape());
            let bytes: Vec<u8> = events
                .iter()
                .flat_map(|event| match event {
                    InputEvent::Bytes(bytes) => bytes.clone(),
                    other => panic!("unexpected {other:?} at split {split}"),
                })
                .collect();
            assert_eq!(bytes, payload.to_vec(), "split {split}");
        }
    }

    #[test]
    fn prefix_commands_literal_and_unknown_keys() {
        let mut filter = fresh();
        assert_eq!(
            filter.feed(b"\x01|"),
            vec![InputEvent::Command(Action::SplitSide)]
        );
        assert!(!filter.command_pending());
        assert_eq!(filter.feed(b"\x01\x01"), vec![InputEvent::Bytes(vec![1])]);
        assert_eq!(filter.feed(b"\x010"), vec![InputEvent::Unknown]);
        assert!(filter.command_pending() && filter.revealed());
        assert_eq!(filter.feed(b"0"), vec![InputEvent::Unknown]);
        assert_eq!(
            filter.feed(b"\x1b[B"),
            vec![InputEvent::Scroll(ScrollBy::Rows(1))]
        );
        assert_eq!(
            filter.feed(b"\x1bOA"),
            vec![InputEvent::Scroll(ScrollBy::Rows(-1))]
        );
        assert_eq!(
            filter.feed(b"\x1b[6~"),
            vec![InputEvent::Scroll(ScrollBy::Screens(1))]
        );
        assert!(filter.command_pending());
        assert_eq!(filter.feed(b"\x1b"), vec![]);
        assert_eq!(filter.resolve_escape(), vec![InputEvent::Cancel]);
        assert!(!filter.command_pending());
        assert_eq!(filter.feed(b"x"), vec![InputEvent::Bytes(b"x".to_vec())]);
        // A paste while the popup is open leaves command mode without reaching a command.
        assert_eq!(filter.feed(b"\x01"), vec![]);
        assert_eq!(
            filter.feed(b"\x1b[200~t\x1b[201~"),
            vec![InputEvent::Bytes(b"\x1b[200~t\x1b[201~".to_vec())]
        );
        assert!(!filter.command_pending());
    }

    #[test]
    fn escape_prefix_forwards_exactly_one_literal_and_alt_keys_pass_through() {
        let mut filter = PrefixFilter::new(ClientBindings::new(
            27,
            crate::commands::DEFAULT_BINDINGS
                .iter()
                .map(|spec| (spec.key, spec.action)),
        ));
        let mut events = filter.feed(b"\x1b\x1b");
        events.extend(filter.resolve_escape());
        assert_eq!(events, vec![InputEvent::Bytes(vec![27])]);
        let mut events = filter.feed(b"\x1b");
        events.extend(filter.resolve_escape());
        assert!(events.is_empty() && filter.command_pending());
        assert_eq!(
            filter.feed(b"|"),
            vec![InputEvent::Command(Action::SplitSide)]
        );
        let mut plain = fresh();
        assert_eq!(
            plain.feed(b"\x1bx"),
            vec![InputEvent::Bytes(b"\x1bx".to_vec())]
        );
        assert_eq!(
            plain.feed(b"\x1b[A"),
            vec![InputEvent::Bytes(b"\x1b[A".to_vec())]
        );
        assert_eq!(
            plain.feed(b"\x1b[<0;3;4M"),
            vec![InputEvent::Mouse(MouseEvent {
                code: 0,
                column: 3,
                row: 4,
                release: false
            })]
        );
    }

    #[test]
    fn terminal_escape_parameters_do_not_activate_printable_prefixes() {
        for prefix in *b"A2P" {
            let mut filter =
                PrefixFilter::new(ClientBindings::new(prefix, [(b'x', Action::ClosePane)]));
            for sequence in [
                b"\x1b[A".as_slice(),
                b"\x1b[1;2A",
                b"\x1bOP",
                b"\x1b[<0;2;2M",
            ] {
                let events = filter.feed(sequence);
                assert!(!filter.command_pending(), "{prefix} {sequence:?}");
                assert!(
                    events
                        .iter()
                        .all(|event| matches!(event, InputEvent::Bytes(_) | InputEvent::Mouse(..)))
                );
            }
        }
    }

    #[test]
    fn terminal_sequences_and_commands_survive_every_read_boundary() {
        for prefix in [1, 2, b'P', 27] {
            let make =
                || PrefixFilter::new(ClientBindings::new(prefix, [(b'x', Action::ClosePane)]));
            let mut paste = b"\x1b[200~".to_vec();
            paste.extend([prefix, b'x']);
            paste.extend("界\x1b\x1b[201~".as_bytes());
            for payload in [
                b"\x1b[1;2A".to_vec(),
                b"\x1bOP".to_vec(),
                b"\x1bx".to_vec(),
                "界é".as_bytes().to_vec(),
                b"\x1b[<0;3;4M\x1b[<0;3;4m".to_vec(),
                paste,
            ] {
                for split in 0..=payload.len() {
                    let (head, tail) = payload.split_at(split);
                    let mut filter = make();
                    let mut events = filter.feed(head);
                    events.extend(filter.feed(tail));
                    events.extend(filter.resolve_escape());
                    let mut bytes = Vec::new();
                    for event in events {
                        match event {
                            InputEvent::Bytes(raw) => bytes.extend(raw),
                            InputEvent::Mouse(mouse) => {
                                bytes.extend(mouse.sgr(mouse.column, mouse.row));
                            }
                            other => panic!(
                                "unexpected {other:?}, prefix {prefix}, split {split}, payload {payload:?}"
                            ),
                        }
                    }
                    assert_eq!(bytes, payload, "prefix {prefix}, split {split}");
                    assert!(!filter.command_pending());
                }
            }
            if prefix != 27 {
                for split in 0..=2 {
                    let mut filter = make();
                    let command = [prefix, b'x'];
                    let (head, tail) = command.split_at(split);
                    let mut events = filter.feed(head);
                    events.extend(filter.feed(tail));
                    assert_eq!(events, vec![InputEvent::Command(Action::ClosePane)]);
                    let literal = [prefix, prefix];
                    let (head, tail) = literal.split_at(split);
                    let mut events = filter.feed(head);
                    events.extend(filter.feed(tail));
                    assert_eq!(events, vec![InputEvent::Bytes(vec![prefix])]);
                }
            }
        }
    }

    #[test]
    fn oversized_sequences_are_forwarded_rather_than_retained() {
        let mut filter = fresh();
        let mut long = b"\x1b[".to_vec();
        long.extend(std::iter::repeat_n(b'1', 100));
        let events = filter.feed(&long);
        let forwarded: usize = events
            .iter()
            .map(|event| match event {
                InputEvent::Bytes(bytes) => bytes.len(),
                _ => 0,
            })
            .sum();
        assert!(forwarded >= MAX_SEQUENCE);
    }
}
