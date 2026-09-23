//! A bounded bracketed-paste envelope around Termina's native input decoder.
//! The start event lets the server retain ownership even if a modal is cancelled
//! by another control request before the terminal sends the end marker.
use crate::protocol::Input;
use crate::{
    actions::Target,
    interaction::{Mode, Overlay},
    model::{Notice, Viewer},
};
use bevy_ecs::{lifecycle::HookContext, prelude::*, world::DeferredWorld};

#[derive(Component, Default)]
pub struct Ownership {
    pub serial: u64,
    pending: Option<Owner>,
}

/// Every newly inserted overlay gets a fresh serial, whichever code path opened
/// it, so a paste captured for an earlier overlay can never land in this one.
pub(crate) fn overlay_opened(mut world: DeferredWorld, context: HookContext) {
    if let Some(mut ownership) = world.get_mut::<Ownership>(context.entity) {
        ownership.serial = ownership.serial.wrapping_add(1);
        let serial = ownership.serial;
        if let Some(mut overlay) = world.get_mut::<Overlay>(context.entity) {
            overlay.serial = serial;
        }
    }
}
enum Owner {
    Text(u64),
    Pane(Target),
    Discard,
}

pub fn input(world: &mut World, id: Entity, input: &Input) -> bool {
    if matches!(input, Input::PasteBegin) {
        if world.get::<Viewer>(id).is_none() {
            return true;
        }
        let owner = if let Some(overlay) = world.get::<Overlay>(id) {
            if matches!(overlay.mode, Mode::Text { .. }) {
                Owner::Text(overlay.serial)
            } else {
                Owner::Discard
            }
        } else if crate::interaction::modal(world, id) {
            Owner::Discard
        } else {
            match Target::of(world, id) {
                Some(target) => Owner::Pane(target),
                None => Owner::Discard,
            }
        };
        if let Some(mut ownership) = world.get_mut::<Ownership>(id) {
            ownership.pending = Some(owner);
        }
        crate::model::notify(world, id, Notice::info("pasting..."));
        return true;
    }
    let Input::Paste { text } = input else {
        return false;
    };
    let owner = world
        .get_mut::<Ownership>(id)
        .and_then(|mut state| state.pending.take());
    if let Some(mut v) = world.get_mut::<Viewer>(id)
        && v.notice.as_ref().is_some_and(|n| n.text == "pasting...")
    {
        v.notice = None;
    }
    let valid = match owner {
        None => true,
        Some(Owner::Discard) => return true,
        Some(Owner::Text(serial)) => world.get::<Overlay>(id).is_some_and(|o| o.serial == serial),
        Some(Owner::Pane(target)) => {
            Target::of(world, id) == Some(target) && !crate::interaction::modal(world, id)
        }
    };
    if !valid || text.len() > LIMIT {
        let reason = if valid {
            "paste exceeds 64 KiB; discarded"
        } else {
            "paste owner changed; discarded"
        };
        crate::model::notify(world, id, Notice::error(reason));
        return true;
    }
    false
}

const START: &[u8] = b"\x1b[200~";
const END: &[u8] = b"\x1b[201~";
pub const LIMIT: usize = 64 * 1024;
/// Bytes fux adds around an accepted paste when the application requested
/// bracketed-paste mode; the PTY transport budget covers `LIMIT + ENVELOPE`.
pub const ENVELOPE: usize = START.len() + END.len();

/// Frames an accepted paste for an application that requested bracketed paste.
pub fn bracketed(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() + ENVELOPE);
    bytes.extend_from_slice(START);
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(END);
    bytes
}

#[derive(Default)]
pub struct Decoder {
    parser: termina::Parser,
    boundary: Vec<u8>,
    paste: Option<Vec<u8>>,
    incomplete: bool,
}
impl Decoder {
    pub fn deadline_needed(&self) -> bool {
        self.paste.is_none() && (self.incomplete || !self.boundary.is_empty())
    }
    fn ordinary(&mut self, bytes: &[u8], more: bool, emit: &mut impl FnMut(Input)) {
        self.parser.parse(bytes, more);
        self.incomplete = more;
        while let Some(event) = self.parser.pop() {
            self.incomplete = false;
            if let Some(input) = crate::viewer::convert(event) {
                emit(input);
            }
        }
    }
    pub fn bytes(&mut self, bytes: &[u8], mut emit: impl FnMut(Input)) {
        for &byte in bytes {
            self.boundary.push(byte);
            let delimiter = if self.paste.is_some() { END } else { START };
            while !delimiter.starts_with(&self.boundary) {
                let byte = self.boundary.remove(0);
                if let Some(paste) = &mut self.paste {
                    if paste.len() <= LIMIT {
                        paste.push(byte);
                    }
                } else {
                    self.ordinary(&[byte], true, &mut emit);
                }
            }
            if self.boundary == delimiter {
                self.boundary.clear();
                if let Some(paste) = self.paste.take() {
                    emit(Input::Paste {
                        text: String::from_utf8_lossy(&paste).into_owned(),
                    });
                } else {
                    self.paste = Some(Vec::new());
                    self.incomplete = false;
                    emit(Input::PasteBegin);
                }
            }
        }
    }
    pub fn timeout(&mut self, mut emit: impl FnMut(Input)) {
        if self.paste.is_some() {
            return;
        }
        let boundary = std::mem::take(&mut self.boundary);
        self.ordinary(&boundary, false, &mut emit);
        self.incomplete = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Key;
    use crate::testing::*;
    #[test]
    fn every_fragment_boundary_preserves_paste_ownership_and_embedded_escape()
    -> crate::testing::Outcome {
        let bytes = b"\x02r\x1b[200~one\x1btwo\x1b[201~\x1b";
        for chunk in 1..=bytes.len() {
            let mut decoder = Decoder::default();
            let mut events = Vec::new();
            for bytes in bytes.chunks(chunk) {
                decoder.bytes(bytes, |event| events.push(event));
            }
            decoder.timeout(|event| events.push(event));
            assert!(matches!(events.get(2), Some(Input::PasteBegin)));
            assert!(matches!(events.get(3), Some(Input::Paste { text }) if text == "one\x1btwo"));
            assert!(matches!(events.get(4), Some(Input::Key { key, .. }) if *key == Key::Escape));
            assert_eq!(events.len(), 5);
        }
        Ok(())
    }
    #[test]
    fn largest_accepted_paste_fits_the_transport_with_its_envelope() {
        let text = "x".repeat(LIMIT);
        let framed = bracketed(&text);
        assert!(framed.starts_with(START) && framed.ends_with(END));
        assert_eq!(framed.len(), LIMIT + ENVELOPE);
        assert_eq!(
            framed.get(START.len()..framed.len() - END.len()),
            Some(text.as_bytes())
        );
    }
    /// A tiny deterministic PRNG, so a property failure reproduces from its seed.
    fn lcg(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *state >> 33
    }

    /// Property: the events the decoder emits do not depend on how the same
    /// bytes are split into chunks. This is the guarantee the fragmented-paste
    /// design rests on, checked here over random adversarial byte streams
    /// (escapes, partial and nested paste markers, control bytes, UTF-8
    /// fragments) rather than one hand-written sequence. cargo-fuzz would need
    /// a standalone target, but `paste.rs` pulls in bevy_ecs and four fux
    /// modules, so a libfuzzer harness would compile all of fux; a property
    /// test is the isolated tool and runs on stable in every CI run.
    #[test]
    fn chunking_never_changes_what_the_decoder_emits() -> crate::testing::Outcome {
        // Bytes drawn to hit the parser's own alphabet often: paste markers,
        // escapes, and a few multibyte leaders.
        let alphabet: &[&[u8]] = &[
            b"\x1b[200~",
            b"\x1b[201~",
            b"\x1b",
            b"[",
            b"2",
            b"0",
            b"~",
            b"a",
            b"\x02",
            b"\r",
            b"\n",
            b"\x00",
            b"\xe7\x95\x8c",
            b"\xff",
            b"z",
        ];
        let mut seed = 0x9e3779b97f4a7c15u64;
        for _ in 0..2000 {
            let mut bytes = Vec::new();
            for _ in 0..(lcg(&mut seed) % 40) {
                let index = (lcg(&mut seed) as usize) % alphabet.len();
                if let Some(piece) = alphabet.get(index) {
                    bytes.extend_from_slice(piece);
                }
            }
            // The reference: everything at once, then a timeout to flush.
            let mut whole = Decoder::default();
            let mut expected = Vec::new();
            whole.bytes(&bytes, |event| expected.push(event));
            whole.timeout(|event| expected.push(event));
            // The same bytes split at random boundaries must emit the same.
            let mut split = Decoder::default();
            let mut got = Vec::new();
            let mut rest = bytes.as_slice();
            while !rest.is_empty() {
                let take = 1 + (lcg(&mut seed) as usize) % rest.len();
                let (head, tail) = rest.split_at(take);
                split.bytes(head, |event| got.push(event));
                rest = tail;
            }
            split.timeout(|event| got.push(event));
            assert_eq!(
                describe(&got),
                describe(&expected),
                "chunking changed the events for {bytes:?}"
            );
            // And the pending paste buffer is always bounded.
            assert!(
                split
                    .paste
                    .as_ref()
                    .is_none_or(|paste| paste.len() <= LIMIT + 1)
            );
        }
        Ok(())
    }

    /// Inputs compared by shape and payload, since `Input` is not `PartialEq`.
    fn describe(events: &[Input]) -> Vec<String> {
        events
            .iter()
            .map(|event| match event {
                Input::PasteBegin => "begin".to_owned(),
                Input::Paste { text } => format!("paste:{text:?}"),
                Input::Key { key, modifiers } => format!("key:{key:?}:{modifiers:?}"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn oversized_paste_is_bounded_and_drains_before_following_keys() -> crate::testing::Outcome {
        let mut decoder = Decoder::default();
        let mut events = Vec::new();
        decoder.bytes(START, |e| events.push(e));
        decoder.bytes(&vec![b'a'; LIMIT * 4], |e| events.push(e));
        assert!(decoder.paste.as_ref().need()?.len() <= LIMIT + 1);
        assert!(!decoder.deadline_needed());
        decoder.bytes(b"\x1b[201~z", |e| events.push(e));
        assert!(matches!(events.get(1), Some(Input::Paste { text }) if text.len() == LIMIT + 1));
        assert!(matches!(events.get(2), Some(Input::Key { key, .. }) if *key == Key::Char('z')));
        Ok(())
    }
}
