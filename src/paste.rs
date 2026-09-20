//! A bounded bracketed-paste envelope around Termina's native input decoder.
//! The start event lets the server retain ownership even if a modal is cancelled
//! by another control request before the terminal sends the end marker.
use crate::protocol::Input;
use crate::{
    actions::Target,
    interaction::{Mode, Overlay},
    model::Viewer,
};
use bevy_ecs::prelude::*;

#[derive(Component, Default)]
pub struct Ownership {
    pub serial: u64,
    pending: Option<Owner>,
}
enum Owner {
    Text(u64),
    Pane(Target),
    Discard,
}

pub fn input(world: &mut World, id: Entity, input: &Input) -> bool {
    if matches!(input, Input::PasteBegin) {
        let Some(v) = world.get::<Viewer>(id) else {
            return true;
        };
        let owner = if let Some(overlay) = world.get::<Overlay>(id) {
            if matches!(overlay.mode, Mode::Text { .. }) {
                Owner::Text(overlay.serial)
            } else {
                Owner::Discard
            }
        } else if v.prefix
            || v.prompt.is_some()
            || world.get::<crate::selection::Selection>(id).is_some()
        {
            Owner::Discard
        } else {
            Owner::Pane(Target::viewer(v))
        };
        world.get_mut::<Ownership>(id).unwrap().pending = Some(owner);
        let mut v = world.get_mut::<Viewer>(id).unwrap();
        v.notice = "pasting...".into();
        v.notice_error = false;
        return true;
    }
    let Input::Paste { text } = input else {
        return false;
    };
    let owner = world
        .get_mut::<Ownership>(id)
        .and_then(|mut state| state.pending.take());
    if let Some(mut v) = world.get_mut::<Viewer>(id)
        && v.notice == "pasting..."
    {
        v.notice.clear();
    }
    let valid = match owner {
        None => true,
        Some(Owner::Discard) => return true,
        Some(Owner::Text(serial)) => world.get::<Overlay>(id).is_some_and(|o| o.serial == serial),
        Some(Owner::Pane(target)) => {
            world
                .get::<Viewer>(id)
                .is_some_and(|v| Target::viewer(v) == target && !v.prefix && v.prompt.is_none())
                && world.get::<Overlay>(id).is_none()
                && world.get::<crate::selection::Selection>(id).is_none()
        }
    };
    if !valid || text.len() > LIMIT {
        if let Some(mut v) = world.get_mut::<Viewer>(id) {
            v.notice = if valid {
                "paste exceeds 64 KiB; discarded"
            } else {
                "paste owner changed; discarded"
            }
            .into();
            v.notice_error = true;
        }
        return true;
    }
    false
}

const START: &[u8] = b"\x1b[200~";
const END: &[u8] = b"\x1b[201~";
pub const LIMIT: usize = 64 * 1024;

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
    #[test]
    fn every_fragment_boundary_preserves_paste_ownership_and_embedded_escape() {
        let bytes = b"\x02r\x1b[200~one\x1btwo\x1b[201~\x1b";
        for chunk in 1..=bytes.len() {
            let mut decoder = Decoder::default();
            let mut events = Vec::new();
            for bytes in bytes.chunks(chunk) {
                decoder.bytes(bytes, |event| events.push(event));
            }
            decoder.timeout(|event| events.push(event));
            assert!(matches!(events[2], Input::PasteBegin));
            assert!(matches!(&events[3], Input::Paste { text } if text == "one\x1btwo"));
            assert!(matches!(&events[4], Input::Key { key, .. } if key == "escape"));
            assert_eq!(events.len(), 5);
        }
    }
    #[test]
    fn oversized_paste_is_bounded_and_drains_before_following_keys() {
        let mut decoder = Decoder::default();
        let mut events = Vec::new();
        decoder.bytes(START, |e| events.push(e));
        decoder.bytes(&vec![b'a'; LIMIT * 4], |e| events.push(e));
        assert!(decoder.paste.as_ref().unwrap().len() <= LIMIT + 1);
        assert!(!decoder.deadline_needed());
        decoder.bytes(b"\x1b[201~z", |e| events.push(e));
        assert!(matches!(&events[1], Input::Paste { text } if text.len() == LIMIT + 1));
        assert!(matches!(&events[2], Input::Key { key, .. } if key == "z"));
    }
}
