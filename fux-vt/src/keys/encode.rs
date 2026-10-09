//! Byte encodings for keys, pastes and focus changes delivered to a
//! program, in the modes it asked for (application cursor keys, the kitty
//! keyboard protocol, modifyOtherKeys, bracketed paste, focus reporting):
//! [`crate::Screen::encode_key`], [`crate::Screen::encode_paste`] and
//! [`crate::Screen::encode_focus`] read them from the program's screen,
//! [`key_bytes`] and [`paste`] take them given. Mouse reports are
//! [`crate::keys::mouse`]'s.
use crate::keys::{Direction, Key, KeyPress, Keystroke, Modifiers};
use std::io::Write;

/// How a pane's program asked for its keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyMode {
    /// Application cursor keys (DECCKM).
    pub application: bool,
    /// The kitty keyboard protocol's flags in force on the pane's screen.
    pub kitty: u8,
    /// xterm's modifyOtherKeys level, if set.
    pub other_keys: Option<u8>,
}

impl crate::Screen {
    /// How the program on this screen asked for its keys: application cursor keys, the kitty
    /// keyboard protocol's flags, modifyOtherKeys.
    pub fn key_mode(&self) -> KeyMode {
        KeyMode {
            application: self.mode(crate::Mode::ApplicationCursor),
            kitty: self.kitty_keyboard_flags(),
            other_keys: self.modify_other_keys(),
        }
    }

    /// Appends `stroke`'s bytes to `out` as the program on this screen asked for its keys
    /// ([`key_bytes`] in [`Screen::key_mode`](crate::Screen::key_mode)).
    pub fn encode_key(&self, stroke: Keystroke, out: &mut Vec<u8>) {
        key_bytes(stroke, self.key_mode(), out);
    }
}

impl KeyMode {
    /// Legacy keys, in normal or application cursor mode.
    pub fn legacy(application: bool) -> KeyMode {
        KeyMode {
            application,
            ..KeyMode::default()
        }
    }
}

/// The kitty protocol's progressive enhancements
/// (`references/modern/kitty_keyboard_protocol.html`).
const DISAMBIGUATE: u8 = 1;
const ALTERNATE_KEYS: u8 = 4;
const ALL_KEYS: u8 = 8;
const TEXT: u8 = 16;
/// The kitty protocol's lock modifiers, Caps Lock and Num Lock.
const LOCKS: u8 = 64 | 128;

/// What a bracketed paste begins with (`CSI 200 ~`).
pub const PASTE_START: &[u8] = b"\x1b[200~";
/// What a bracketed paste ends with (`CSI 201 ~`).
pub const PASTE_END: &[u8] = b"\x1b[201~";

/// Appends a paste as the pane should receive it to `out`: framed if it
/// asked for bracketed paste, with every end marker inside the text removed
/// (`CSI 201 ~`, ESC `[` or the C1 CSI, U+009B), as often as removing one
/// makes another, so the text cannot end the paste early. Unbracketed, the
/// text goes as it is: the program asked for no frame to break.
pub fn paste(text: &str, bracketed: bool, out: &mut Vec<u8>) {
    if !bracketed {
        return out.extend_from_slice(text.as_bytes());
    }
    out.extend_from_slice(PASTE_START);
    // A marker can only be made by removing one, so text with none goes as
    // it is.
    if !text.contains("201~") {
        out.extend_from_slice(text.as_bytes());
        return out.extend_from_slice(PASTE_END);
    }
    let start = out.len();
    for c in text.chars() {
        let mut utf8 = [0; 4];
        out.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
        // A marker can only end where a character was just added; what is
        // left after removing one is checked again by the characters after.
        if c == '~' {
            let text = out.get(start..).unwrap_or_default();
            for marker in [PASTE_END, C1_PASTE_END] {
                if text.ends_with(marker) {
                    out.truncate(out.len().saturating_sub(marker.len()));
                    break;
                }
            }
        }
    }
    out.extend_from_slice(PASTE_END);
}

/// `CSI 201 ~` with the C1 CSI, U+009B, in UTF-8.
const C1_PASTE_END: &[u8] = b"\xc2\x9b201~";

impl crate::Screen {
    /// Appends a paste to `out` as the program on this screen asked for it:
    /// framed by `CSI 200 ~` and `CSI 201 ~` if it set bracketed paste
    /// (`CSI ? 2004 h`), never ended early from inside ([`paste`]).
    pub fn encode_paste(&self, text: &str, out: &mut Vec<u8>) {
        paste(text, self.mode(crate::Mode::BracketedPaste), out);
    }

    /// Appends a focus change to `out`, `CSI I` for gained and `CSI O` for
    /// lost, if the program on this screen asked for them (`CSI ? 1004 h`),
    /// and returns whether it did.
    pub fn encode_focus(&self, focused: bool, out: &mut Vec<u8>) -> bool {
        if !self.mode(crate::Mode::FocusReporting) {
            return false;
        }
        out.extend_from_slice(if focused { b"\x1b[I" } else { b"\x1b[O" });
        true
    }
}

/// Appends a key's bytes, as the pane asked for them in `mode`, to `out`:
/// the kitty keyboard protocol's, if its flags disambiguate or report all
/// keys; else xterm's modifyOtherKeys, if it is set and applies to the key;
/// else legacy xterm bytes, which every key has.
pub fn key_bytes(stroke: Keystroke, mode: KeyMode, out: &mut Vec<u8>) {
    if mode.kitty & (DISAMBIGUATE | ALL_KEYS) != 0 {
        return kitty(stroke, mode.kitty, out);
    }
    if let Some(level @ 1..) = mode.other_keys
        && other_keys(stroke, level, out)
    {
        return;
    }
    legacy(stroke.press, mode.application, out);
}

/// The kitty keyboard protocol's bytes for a key, with `flags`, which
/// disambiguate (1) or report all keys (8)
/// (`references/modern/kitty_keyboard_protocol.html`):
/// - a key that types text, with no modifier but Shift, sends its text,
///   unless all keys are reported; then `CSI code ; mods u`, the code the
///   unshifted key's, with the text as its third parameter if asked (16);
/// - any other text key is `CSI code ; mods u`, with its shifted and
///   base-layout keys if asked (alternate keys, 4) and known;
/// - Enter, Tab and Backspace send their legacy byte unless modified (or all
///   keys are reported), Escape is `CSI 27 u`;
/// - the functional keys are `CSI 1 ; mods X` or `CSI X` (arrows, Home, End,
///   F1, F2, F4), or `CSI n ; mods ~` (Insert, Delete, the page keys, F3 as
///   13, F5 to F12), never SS3; a keypad key is its own `CSI n u`.
///
/// The modifiers are the terminal's, Super, Hyper, Meta and the locks
/// among them, if it spoke the protocol; else the press's, a capital letter
/// counting as shifted. Only presses are sent: fux reports no repeats or
/// releases (2), which the spec lets a terminal leave out.
fn kitty(stroke: Keystroke, flags: u8, out: &mut Vec<u8>) {
    let KeyPress { key, mods } = stroke.press;
    let exact = stroke.kitty;
    let all = flags & ALL_KEYS != 0;
    let capital = matches!(key, Key::Char(c) if c.is_ascii_uppercase()) && exact.is_none();
    let bits = exact.map_or_else(|| xterm_bits(mods) | u8::from(capital), |k| k.mods);
    let held = bits & !LOCKS;
    // `;mods`, left out when there are none; empty if a parameter follows,
    // as the spec's examples have it (`CSI 0 ; ; 229 u`).
    let modifier = |out: &mut Vec<u8>, more: bool| {
        if bits != 0 {
            let _ = write!(out, ";{}", u16::from(bits).saturating_add(1));
        } else if more {
            out.push(b';');
        }
    };
    let csi_u = |out: &mut Vec<u8>, code: u32| {
        let _ = write!(out, "\x1b[{code}");
        modifier(out, false);
        out.push(b'u');
    };
    // A keypad key the terminal named is sent as it named it.
    if let Some(code @ 57399..=57427) = exact.and_then(|k| k.code) {
        return csi_u(out, code);
    }
    // Enter, Tab and Backspace: the legacy byte, which is also the code.
    let legacy_or = |out: &mut Vec<u8>, byte: u8| {
        if held == 0 && !all {
            out.push(byte);
        } else {
            csi_u(out, u32::from(byte));
        }
    };
    let cursor = |out: &mut Vec<u8>, final_byte: char| {
        if bits == 0 {
            let _ = write!(out, "\x1b[{final_byte}");
        } else {
            let _ = write!(out, "\x1b[1");
            modifier(out, false);
            let _ = write!(out, "{final_byte}");
        }
    };
    let tilde = |out: &mut Vec<u8>, code: u8| {
        let _ = write!(out, "\x1b[{code}");
        modifier(out, false);
        out.push(b'~');
    };
    match key {
        Key::Char(c) => {
            let types_text = held & !1 == 0;
            if types_text && !all {
                return out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
            }
            let unshifted = if capital { c.to_ascii_lowercase() } else { c };
            let code = exact.and_then(|k| k.code).unwrap_or(u32::from(unshifted));
            let shifted = exact.and_then(|k| k.shifted).or_else(|| {
                let shifted = if unshifted.is_ascii_lowercase() {
                    unshifted.to_ascii_uppercase()
                } else {
                    c
                };
                (u32::from(shifted) != code).then_some(u32::from(shifted))
            });
            let _ = write!(out, "\x1b[{code}");
            if flags & ALTERNATE_KEYS != 0 {
                let shifted = shifted.filter(|_| bits & 1 != 0);
                let base = exact.and_then(|k| k.base);
                match (shifted, base) {
                    (Some(s), Some(b)) => drop(write!(out, ":{s}:{b}")),
                    (Some(s), None) => drop(write!(out, ":{s}")),
                    (None, Some(b)) => drop(write!(out, "::{b}")),
                    (None, None) => {}
                }
            }
            let text = (all && flags & TEXT != 0 && types_text).then_some(u32::from(c));
            modifier(out, text.is_some());
            if let Some(text) = text {
                let _ = write!(out, ";{text}");
            }
            out.push(b'u');
        }
        Key::Enter => legacy_or(out, 0x0d),
        Key::Tab => legacy_or(out, 0x09),
        Key::Backspace => legacy_or(out, 0x7f),
        Key::Escape => csi_u(out, 27),
        Key::Arrow(Direction::Up) => cursor(out, 'A'),
        Key::Arrow(Direction::Down) => cursor(out, 'B'),
        Key::Arrow(Direction::Right) => cursor(out, 'C'),
        Key::Arrow(Direction::Left) => cursor(out, 'D'),
        Key::Home => cursor(out, 'H'),
        Key::End => cursor(out, 'F'),
        Key::F(1) => cursor(out, 'P'),
        Key::F(2) => cursor(out, 'Q'),
        Key::F(3) => tilde(out, 13),
        Key::F(4) => cursor(out, 'S'),
        Key::Insert => tilde(out, 2),
        Key::Delete => tilde(out, 3),
        Key::PageUp => tilde(out, 5),
        Key::PageDown => tilde(out, 6),
        Key::F(n) => match usize::from(n).checked_sub(5).and_then(|i| F_CODES.get(i)) {
            Some(code) => tilde(out, *code),
            // F13 to F35 are numbered from 57376; fux names none past F12.
            None => csi_u(
                out,
                57376_u32.saturating_add(u32::from(n.saturating_sub(13))),
            ),
        },
    }
}

/// F5 to F12's numbers in `CSI n ~`, with xterm's gaps; `decode.rs`
/// (`csi`) reads them back.
const F_CODES: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];

/// Shift, Alt and Ctrl as xterm's and kitty's modifier bits.
fn xterm_bits(mods: Modifiers) -> u8 {
    u8::from(mods.shift) | u8::from(mods.alt) << 1 | u8::from(mods.ctrl) << 2
}

/// xterm's modifyOtherKeys bytes for a key, `CSI 27 ; mods ; code ~`
/// (formatOtherKeys 0, xterm's default), if the key is an ordinary one (a
/// character, Enter, Tab, Escape, Backspace) and xterm sends it so at
/// `level`; else nothing, and false, for the legacy bytes.
///
/// ctlseqs ("Alt and Meta Keys") gives the outline: at level 1 "the usual
/// shift- and control-modifiers work as expected", at level 2 "all of the
/// modifiers apply". The details are xterm's `input.c` (patch 412), which
/// this follows: `ModifyOtherKeys` decides whether a key is sent so
/// (`modified`), and `allowedCharModifiers` with `filterAltMeta` which
/// modifiers it carries (`allowed`). xterm is taken as X keyboards set it
/// up: the Alt key is xterm's Meta modifier (`mod1` holds `Alt_L` and
/// `Meta_L`; ctlseqs: "Common keyboard configurations assign the Meta
/// modifier to an 'Alt' key"), metaSendsEscape is off, the backarrow key
/// sends DEL, as fux's Backspace does, and Shift-Tab is Tab with Shift.
/// So, at level 1:
/// - Alt alone leaves a key its legacy bytes, Escape and all ("A bare
///   meta-modifier is independent of modifyOtherKeys"): emacs, which sets
///   level 1, reads `M-x` as `ESC x`. ctlseqs's alt-Tab example is of an
///   Alt key that is not Meta;
/// - so do Ctrl, Shift, or both, on a key that Ctrl makes a control (a
///   letter, Space, `2`, `/`, Escape), Shift on a key that types text, and
///   Shift-Tab; Ctrl-Alt on a letter, and Alt with Ctrl on Enter and Tab,
///   are legacy too, which emacs reads as `ESC C-x`; Alt with Shift drops
///   Alt on Enter and Tab;
/// - any other modifiers apply: Ctrl-1, Ctrl-Alt-Space, Shift-Enter,
///   Ctrl-Tab; Backspace's never.
///
/// At level 2 every modifier applies, but Ctrl alone on Backspace and Shift
/// alone on a character other than Space, which types its text. xterm sends
/// Shift-1 as `!` too, but Shift-a as `CSI 27 ; 2 ; 65 ~`: fux's one
/// departure, as it cannot tell Shift from Caps Lock on a letter, which the
/// client's terminal sends as text either way. Level 3, which xterm extends
/// to unmodified keys, is taken as 2.
///
/// The code is the key's character, Shift applied (Ctrl-Shift-a is 65), as
/// xterm sends the keysym; Backspace's is 127, or 8 with Ctrl, which the
/// backarrow toggle leaves BS.
fn other_keys(stroke: Keystroke, level: u8, out: &mut Vec<u8>) -> bool {
    let KeyPress { key, mods } = stroke.press;
    let kitty_shift = stroke.kitty.is_some_and(|k| k.mods & 1 != 0);
    let shift = mods.shift || kitty_shift || matches!(key, Key::Char(c) if c.is_ascii_uppercase());
    let state = Modifiers { shift, ..mods };
    let (sym, code) = match key {
        Key::Enter => (Sym::Return, 13),
        Key::Tab => (Sym::Tab, 9),
        Key::Escape => (Sym::Escape, 27),
        Key::Backspace if mods.ctrl => (Sym::BackSpace, 8),
        Key::Backspace => (Sym::Delete, 127),
        Key::Char(c) => {
            let c = if shift { c.to_ascii_uppercase() } else { c };
            (Sym::Char(c), u32::from(c))
        }
        Key::Delete
        | Key::Insert
        | Key::Arrow(_)
        | Key::Home
        | Key::End
        | Key::PageUp
        | Key::PageDown
        | Key::F(_) => return false,
    };
    if !modified(sym, state, level) {
        return false;
    }
    let sent = if level > 1 {
        state
    } else {
        allowed(sym, state)
    };
    let text = matches!(sym, Sym::Char(c) if c != ' ') && sent == Modifiers::SHIFT;
    if sent.is_empty() || level > 1 && text {
        return false;
    }
    let mods = u16::from(xterm_bits(sent)).saturating_add(1);
    let _ = write!(out, "\x1b[27;{mods};{code}~");
    true
}

/// An ordinary key as xterm's `input.c` sees it: by its keysym. Backspace
/// is `BackSpace` with Ctrl and `Delete` without, as xterm's backarrow
/// toggle (`IsBackarrowToggle`) makes it when the key sends DEL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sym {
    Char(char),
    Return,
    Tab,
    Escape,
    BackSpace,
    Delete,
}

impl Sym {
    /// `IsControlInput`: a character that Ctrl makes a control of, `@` to
    /// DEL.
    fn control_input(self) -> bool {
        matches!(self, Sym::Char('\u{40}'..='\u{7f}'))
    }

    /// `IsControlOutput`: a control character itself.
    fn control_output(self) -> bool {
        matches!(self, Sym::Char('\0'..='\u{1f}' | '\u{7f}'..='\u{9f}'))
    }

    /// `IsControlAlias`: the key's one byte, as X looks it up with the
    /// modifiers held, is a control. With Ctrl, `@` to `~`, Space, `2` to
    /// `8` and `/` are (Xlib's control mapping); Enter, Tab, Escape and
    /// Backspace always are.
    fn control_alias(self, ctrl: bool) -> bool {
        match self {
            Sym::Char(c) => {
                ctrl && matches!(c, '@'..='~' | ' ' | '2'..='8' | '/')
                    || matches!(c, '\0'..='\u{1f}' | '\u{7f}')
            }
            Sym::Return | Sym::Tab | Sym::Escape | Sym::BackSpace | Sym::Delete => true,
        }
    }
}

/// Whether xterm sends `sym` with `state` as a modifyOtherKeys sequence at
/// `level`: `input.c`'s `ModifyOtherKeys`, and its `Input`'s Shift-Tab,
/// which at level 1 becomes `ISO_Left_Tab`, a function key, `CSI Z`.
fn modified(sym: Sym, state: Modifiers, level: u8) -> bool {
    if state.is_empty() || level == 1 && sym == Sym::Tab && state == Modifiers::SHIFT {
        return false;
    }
    // A character's modifiers are filtered first, a predefined key's not.
    let st = match sym {
        Sym::Char(_) if level == 1 => allowed(sym, state),
        Sym::Char(_) | Sym::Return | Sym::Tab | Sym::Escape | Sym::BackSpace | Sym::Delete => state,
    };
    let without_ctrl = Modifiers { ctrl: false, ..st };
    if st.is_empty() {
        return false;
    }
    if level == 1 {
        return match sym {
            Sym::BackSpace | Sym::Delete => false,
            Sym::Return | Sym::Tab => true,
            Sym::Char(_) | Sym::Escape if sym.control_input() => {
                st != Modifiers::CTRL && st != Modifiers::SHIFT
            }
            Sym::Char(_) | Sym::Escape if sym.control_alias(state.ctrl) => {
                st != Modifiers::SHIFT && !without_ctrl.is_empty()
            }
            Sym::Char(_) | Sym::Escape => true,
        };
    }
    match sym {
        Sym::BackSpace => !without_ctrl.is_empty(),
        Sym::Delete | Sym::Escape | Sym::Return | Sym::Tab => true,
        Sym::Char(c) => {
            sym.control_input()
                || st == Modifiers::SHIFT && c == ' '
                || !Modifiers { shift: false, ..st }.is_empty()
        }
    }
}

/// The modifiers xterm sends with `sym` at level 1: `input.c`'s
/// `allowedCharModifiers`, and its `filterAltMeta` for the Alt key as Meta.
fn allowed(sym: Sym, state: Modifiers) -> Modifiers {
    let mut m = state;
    let text = matches!(sym, Sym::Char(_)) && !sym.control_output();
    if sym.control_input() && !m.shift && !m.alt {
        // Ctrl makes these a control already, which it is left to.
    } else if matches!(sym, Sym::Return | Sym::Tab) {
        // Enter and Tab keep Ctrl and Shift.
    } else if sym.control_alias(state.ctrl) {
        // Ctrl and Shift on a control are its own.
        if !m.alt {
            m = Modifiers::NONE;
        }
    } else if text && !m.ctrl {
        // "Printable keys are already associated with the shift-key".
        m.shift = false;
    }
    if m.alt {
        // "A bare meta-modifier is independent of modifyOtherKeys."
        if !m.ctrl && !m.shift {
            m.alt = false;
        }
        // "special cases of control+meta which are used by some
        // applications, e.g., emacs", and Enter and Tab.
        let control = (sym.control_input() || sym.control_output()) && m.ctrl;
        if control || matches!(sym, Sym::Return | Sym::Tab) {
            m.alt = false;
            m.ctrl = false;
        }
    }
    m
}

/// Legacy xterm bytes for a key: what xterm sends with its default
/// resources, for a pane that asked for neither the kitty protocol nor
/// modifyOtherKeys. Every key has an encoding.
fn legacy(press: KeyPress, application: bool, out: &mut Vec<u8>) {
    let KeyPress { key, mods } = press;
    let Modifiers { ctrl, alt, shift } = mods;
    // xterm's modifier parameter: 1 plus a bit for each, so at most 8.
    let modifier = xterm_bits(mods).saturating_add(1);
    let csi = |out: &mut Vec<u8>, code: u8, final_byte: char| {
        let _ = if modifier > 1 {
            write!(out, "\x1b[{code};{modifier}{final_byte}")
        } else {
            write!(out, "\x1b[{code}{final_byte}")
        };
    };
    // Cursor-style keys share one shape: `ESC [ final`, `ESC O final` in
    // application mode or for F1..F4, and `ESC [ 1 ; mod final` when
    // modified. Alt adds nothing to them.
    let cursor = |out: &mut Vec<u8>, final_byte: char, function: bool| {
        if modifier > 1 {
            csi(out, 1, final_byte);
        } else {
            let prefix = if application || function { 'O' } else { '[' };
            let _ = write!(out, "\x1b{prefix}{final_byte}");
        }
    };
    let start = out.len();
    match key {
        Key::Arrow(Direction::Up) => return cursor(out, 'A', false),
        Key::Arrow(Direction::Down) => return cursor(out, 'B', false),
        Key::Arrow(Direction::Right) => return cursor(out, 'C', false),
        Key::Arrow(Direction::Left) => return cursor(out, 'D', false),
        Key::Home => return cursor(out, 'H', false),
        Key::End => return cursor(out, 'F', false),
        Key::F(1) => return cursor(out, 'P', true),
        Key::F(2) => return cursor(out, 'Q', true),
        Key::F(3) => return cursor(out, 'R', true),
        Key::F(4) => return cursor(out, 'S', true),
        Key::Enter => out.push(13),
        Key::Tab if shift => out.extend_from_slice(b"\x1b[Z"),
        Key::Tab => out.push(9),
        Key::Escape => out.push(27),
        Key::Backspace => out.push(127),
        Key::Insert => csi(out, 2, '~'),
        Key::Delete => csi(out, 3, '~'),
        Key::PageUp => csi(out, 5, '~'),
        Key::PageDown => csi(out, 6, '~'),
        Key::F(n) => match usize::from(n).checked_sub(5).and_then(|i| F_CODES.get(i)) {
            Some(code) => csi(out, *code, '~'),
            None => out.push(27),
        },
        Key::Char(c) if ctrl && c.is_ascii() => out.push(control_byte(c).unwrap_or(c as u8)),
        Key::Char(c) => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
    }
    // Alt is an Escape before the key, unless the key is a sequence, which
    // carries Alt in its parameter; a lone ESC (Escape, Ctrl-[) takes one
    // too, as xterm sends Alt-Escape: ESC ESC.
    if alt && !(out.get(start) == Some(&27) && out.len() > start.saturating_add(1)) {
        out.push(27);
        // The key's bytes and the Escape after them: at least one to turn.
        if let Some(key) = out.get_mut(start..) {
            key.rotate_right(1);
        }
    }
}

/// xterm's control-key byte, Xlib's control mapping: `@` to `~` masked to
/// a C0 control, Space and `2` NUL, `3` to `7` ESC to US, `/` US, and `8`
/// and `?` DEL (`?` as kitty's table and xterm's own translations have it).
/// Ctrl leaves any other character untouched, as it does in both: `Ctrl-;`
/// is `;`, never the ESC masking would make of it.
fn control_byte(c: char) -> Option<u8> {
    let b = u8::try_from(c).ok()?;
    Some(match b {
        // Masking drops the case bit too: `a` and `A` are both 0x01.
        b'@'..=b'~' => b & 0x1f,
        b' ' | b'2' => 0,
        b'3'..=b'7' => b.wrapping_sub(b'3').wrapping_add(0x1b),
        b'/' => 0x1f,
        b'8' | b'?' => 0x7f,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Modifiers = Modifiers {
        ctrl: true,
        alt: true,
        shift: true,
    };
    fn press(key: Key, mods: Modifiers) -> KeyPress {
        KeyPress { key, mods }
    }
    /// A key's bytes, appended after others, which stay as they were.
    fn encoded(press: KeyPress, application: bool) -> Vec<u8> {
        let mut out = b"before".to_vec();
        key_bytes(press.into(), KeyMode::legacy(application), &mut out);
        assert!(out.starts_with(b"before"), "{press:?}");
        out.get(6..).map(<[u8]>::to_vec).unwrap_or_default()
    }
    /// The bytes for a named key, or a stroke, in `mode`, as text.
    fn named(name: &str, mode: KeyMode) -> String {
        let press = name.parse::<KeyPress>().unwrap_or(KeyPress::char('?'));
        stroke(press.into(), mode)
    }
    fn stroke(stroke: Keystroke, mode: KeyMode) -> String {
        let mut out = Vec::new();
        key_bytes(stroke, mode, &mut out);
        String::from_utf8_lossy(&out).into_owned()
    }
    fn kitty_mode(flags: u8) -> KeyMode {
        KeyMode {
            kitty: flags,
            ..KeyMode::default()
        }
    }
    /// A key as a kitty-protocol terminal reports it.
    fn reported(
        name: &str,
        code: u32,
        shifted: Option<u32>,
        base: Option<u32>,
        mods: u8,
    ) -> Keystroke {
        Keystroke {
            press: name.parse().unwrap_or(KeyPress::char('?')),
            kitty: Some(crate::keys::Kitty {
                code: Some(code),
                shifted,
                base,
                mods,
            }),
        }
    }

    /// With disambiguate (1), as the kitty spec's "Disambiguate escape
    /// codes" and "Functional key definitions" say: text keys send their
    /// text; Escape and keys with Ctrl or Alt are `CSI code ; mods u`;
    /// Enter, Tab and Backspace stay legacy unless modified; the functional
    /// keys take the `CSI 1 ; mods X` and `CSI n ; mods ~` forms, never SS3,
    /// even in application cursor mode; F3 is `CSI 13 ~`.
    #[test]
    fn disambiguated_keys_follow_the_kitty_spec() {
        let mode = kitty_mode(1);
        for (name, bytes) in [
            ("a", "a"),
            ("A", "A"),
            ("界", "界"),
            ("Escape", "\x1b[27u"),
            ("M-Escape", "\x1b[27;3u"),
            ("Enter", "\r"),
            ("S-Enter", "\x1b[13;2u"),
            ("C-Enter", "\x1b[13;5u"),
            ("Tab", "\t"),
            ("BTab", "\x1b[9;2u"),
            ("BSpace", "\x7f"),
            ("C-BSpace", "\x1b[127;5u"),
            ("M-BSpace", "\x1b[127;3u"),
            ("C-a", "\x1b[97;5u"),
            ("C-i", "\x1b[105;5u"),
            ("C-m", "\x1b[109;5u"),
            ("C-[", "\x1b[91;5u"),
            ("M-[", "\x1b[91;3u"),
            ("M-a", "\x1b[97;3u"),
            ("C-M-a", "\x1b[97;7u"),
            ("M-A", "\x1b[97;4u"),
            ("C-Space", "\x1b[32;5u"),
            ("M-!", "\x1b[33;3u"),
            ("Up", "\x1b[A"),
            ("C-Up", "\x1b[1;5A"),
            ("S-Left", "\x1b[1;2D"),
            ("Home", "\x1b[H"),
            ("End", "\x1b[F"),
            ("F1", "\x1b[P"),
            ("F2", "\x1b[Q"),
            ("F3", "\x1b[13~"),
            ("S-F3", "\x1b[13;2~"),
            ("F4", "\x1b[S"),
            ("F5", "\x1b[15~"),
            ("C-F12", "\x1b[24;5~"),
            ("Insert", "\x1b[2~"),
            ("C-Delete", "\x1b[3;5~"),
            ("PageUp", "\x1b[5~"),
            ("PageDown", "\x1b[6~"),
        ] {
            assert_eq!(named(name, mode), bytes, "{name}");
        }
        let application = KeyMode {
            application: true,
            ..mode
        };
        assert_eq!(named("Up", application), "\x1b[A");
        // Flags without disambiguate or all keys change nothing.
        assert_eq!(named("C-i", kitty_mode(4)), "\t");
        assert_eq!(named("Escape", kitty_mode(2)), "\x1b");
    }

    /// What a kitty-protocol terminal said beyond the press is given back:
    /// Shift with Ctrl, which the press folds away (ctrl+shift+i is
    /// `CSI 105 ; 6 u`, the spec's "Legacy text keys" table), Super and the
    /// locks, a keypad key's own number; with alternate keys (4), the
    /// shifted and base-layout keys ("Key codes").
    #[test]
    fn a_reported_key_is_given_back_as_reported() {
        let ctrl_shift_i = reported("C-i", 105, Some(73), None, 5);
        assert_eq!(stroke(ctrl_shift_i, kitty_mode(1)), "\x1b[105;6u");
        assert_eq!(stroke(ctrl_shift_i, kitty_mode(5)), "\x1b[105:73;6u");
        let super_a = reported("a", 97, None, None, 8);
        assert_eq!(stroke(super_a, kitty_mode(1)), "\x1b[97;9u");
        // Ctrl and С on a Cyrillic layout, whose base-layout key is c.
        let cyrillic = reported("C-c", 1089, None, Some(99), 4);
        assert_eq!(stroke(cyrillic, kitty_mode(1)), "\x1b[1089;5u");
        assert_eq!(stroke(cyrillic, kitty_mode(5)), "\x1b[1089::99;5u");
        let keypad_enter = reported("Enter", 57414, None, None, 0);
        assert_eq!(stroke(keypad_enter, kitty_mode(1)), "\x1b[57414u");
        assert_eq!(stroke(keypad_enter, KeyMode::default()), "\r");
        let caps_ctrl_a = reported("C-a", 97, None, None, 4 | 64);
        assert_eq!(stroke(caps_ctrl_a, kitty_mode(1)), "\x1b[97;69u");
        // A legacy pane gets the press's bytes alone, what was reported
        // beside them unused.
        assert_eq!(stroke(ctrl_shift_i, KeyMode::default()), "\t");
        assert_eq!(stroke(cyrillic, KeyMode::default()), "\x03");
        // Alternate keys from a legacy press: a capital's shifted key.
        assert_eq!(named("M-A", kitty_mode(5)), "\x1b[97:65;4u");
    }

    /// Report all keys (8): text keys too are `CSI code ; mods u`, with the
    /// text as a third parameter if asked (16; the spec's "Text as code
    /// points": shift+a is `CSI 97 ; 2 ; 65 u`); Enter, Tab and Backspace
    /// are escapes.
    #[test]
    fn reporting_all_keys_escapes_text_keys() {
        for (flags, name, bytes) in [
            (9, "a", "\x1b[97u"),
            (9, "A", "\x1b[97;2u"),
            (13, "A", "\x1b[97:65;2u"),
            (25, "a", "\x1b[97;;97u"),
            (25, "A", "\x1b[97;2;65u"),
            (25, "C-a", "\x1b[97;5u"),
            (9, "Enter", "\x1b[13u"),
            (9, "Tab", "\x1b[9u"),
            (9, "BSpace", "\x1b[127u"),
            (8, "Escape", "\x1b[27u"),
            (8, "Up", "\x1b[A"),
        ] {
            assert_eq!(named(name, kitty_mode(flags)), bytes, "{flags} {name}");
        }
    }

    fn other_keys_level(n: u8) -> KeyMode {
        KeyMode {
            other_keys: Some(n),
            ..KeyMode::default()
        }
    }

    /// modifyOtherKeys level 1, as xterm's input.c (patch 412) sends it with
    /// the Alt key as its Meta modifier. Alone, Alt leaves a key its legacy
    /// bytes (`filterAltMeta`: "A bare meta-modifier is independent of
    /// modifyOtherKeys"), as do Ctrl-Alt on a letter and Alt on Enter and
    /// Tab with Ctrl ("special cases of control+meta ... e.g., emacs"), Ctrl
    /// and Shift on a key Ctrl makes a control (`allowedCharModifiers`,
    /// `IsControlAlias`), Shift on a key that types text, Shift-Tab
    /// (`ISO_Left_Tab`), and Backspace (`ModifyOtherKeys`, `mokUser`).
    #[test]
    fn modify_other_keys_level_1_follows_xterm() {
        for name in [
            "M-x",
            "M-v",
            "M-X",
            "M-1",
            "M-<",
            "M-Space",
            "M-Escape",
            "M-Tab",
            "M-Enter",
            "M-BSpace",
            "C-M-x",
            "C-M-a",
            "C-M-Enter",
            "C-M-Tab",
            "C-a",
            "C-Space",
            "C-2",
            "C-/",
            "C-Escape",
            "S-Escape",
            "C-S-Escape",
            "BTab",
            "a",
            "A",
            "!",
            "Enter",
            "Tab",
            "Escape",
            "BSpace",
            "C-BSpace",
            "S-BSpace",
            "C-Up",
            "F5",
        ] {
            let legacy = String::from_utf8_lossy(&encoded(
                name.parse().unwrap_or(KeyPress::char('?')),
                false,
            ))
            .into_owned();
            assert_eq!(named(name, other_keys_level(1)), legacy, "{name}");
        }
        assert_eq!(named("M-x", other_keys_level(1)), "\x1bx");
        assert_eq!(named("C-M-x", other_keys_level(1)), "\x1b\x18");
        for (name, bytes) in [
            ("C-1", "\x1b[27;5;49~"),
            ("C-M-1", "\x1b[27;7;49~"),
            ("C-M-Space", "\x1b[27;7;32~"),
            ("C-M-/", "\x1b[27;7;47~"),
            ("C-M-%", "\x1b[27;7;37~"),
            ("C-M-Escape", "\x1b[27;7;27~"),
            ("M-S-Escape", "\x1b[27;4;27~"),
            ("S-Enter", "\x1b[27;2;13~"),
            ("C-Enter", "\x1b[27;5;13~"),
            ("C-S-Enter", "\x1b[27;6;13~"),
            ("M-S-Enter", "\x1b[27;2;13~"),
            ("C-Tab", "\x1b[27;5;9~"),
            ("C-BTab", "\x1b[27;6;9~"),
            ("M-BTab", "\x1b[27;2;9~"),
        ] {
            assert_eq!(named(name, other_keys_level(1)), bytes, "{name}");
        }
        // Ctrl-Shift-a, which only a kitty-protocol terminal tells from
        // Ctrl-a, is a control too.
        let ctrl_shift_a = reported("C-a", 97, Some(65), None, 5);
        assert_eq!(stroke(ctrl_shift_a, other_keys_level(1)), "\x01");
    }

    /// modifyOtherKeys level 2, as xterm's input.c sends it: every modifier
    /// applies (`ModifyOtherKeys`, case 2), and the Alt key is filtered no
    /// more; but Ctrl alone on Backspace, which xterm's backarrow toggle
    /// makes BS, and Shift alone on a character that types text, which is
    /// its text: xterm's for `!`, fux's departure for a letter. Shift-Tab is
    /// `CSI 27 ; 2 ; 9 ~`, as ctlseqs says. Other keys keep their legacy
    /// bytes, level 3 is taken as 2, and the kitty flags, when set, win.
    #[test]
    fn modify_other_keys_level_2_follows_xterm() {
        for (n, name, bytes) in [
            (2, "M-x", "\x1b[27;3;120~"),
            (2, "M-X", "\x1b[27;4;88~"),
            (2, "C-M-x", "\x1b[27;7;120~"),
            (2, "M-1", "\x1b[27;3;49~"),
            (2, "C-1", "\x1b[27;5;49~"),
            (2, "C-a", "\x1b[27;5;97~"),
            (2, "C-i", "\x1b[27;5;105~"),
            (2, "C-Space", "\x1b[27;5;32~"),
            (2, "BTab", "\x1b[27;2;9~"),
            (2, "M-Tab", "\x1b[27;3;9~"),
            (2, "S-Enter", "\x1b[27;2;13~"),
            (2, "C-Enter", "\x1b[27;5;13~"),
            (2, "M-Enter", "\x1b[27;3;13~"),
            (2, "M-Escape", "\x1b[27;3;27~"),
            (2, "S-Escape", "\x1b[27;2;27~"),
            (2, "M-BSpace", "\x1b[27;3;127~"),
            (2, "S-BSpace", "\x1b[27;2;127~"),
            (2, "C-S-BSpace", "\x1b[27;6;8~"),
            (2, "C-M-BSpace", "\x1b[27;7;8~"),
            (2, "C-BSpace", "\x7f"),
            (2, "a", "a"),
            (2, "A", "A"),
            (2, "!", "!"),
            (2, "Enter", "\r"),
            (2, "Escape", "\x1b"),
            (2, "C-Up", "\x1b[1;5A"),
            (2, "F5", "\x1b[15~"),
            (3, "C-a", "\x1b[27;5;97~"),
            (3, "M-x", "\x1b[27;3;120~"),
        ] {
            assert_eq!(named(name, other_keys_level(n)), bytes, "level {n} {name}");
        }
        let ctrl_shift_a = reported("C-a", 97, Some(65), None, 5);
        assert_eq!(stroke(ctrl_shift_a, other_keys_level(2)), "\x1b[27;6;65~");
        // Shift-Space, which only a kitty-protocol terminal tells from
        // Space, is xterm's one character modified by Shift alone.
        let shift_space = reported("Space", 32, None, None, 1);
        assert_eq!(stroke(shift_space, other_keys_level(2)), "\x1b[27;2;32~");
        assert_eq!(stroke(shift_space, other_keys_level(1)), " ");
        let both = KeyMode {
            kitty: 1,
            other_keys: Some(2),
            ..KeyMode::default()
        };
        assert_eq!(named("C-a", both), "\x1b[97;5u");
    }

    fn pasted(text: &str, bracketed: bool) -> Vec<u8> {
        let mut out = Vec::new();
        paste(text, bracketed, &mut out);
        out
    }

    #[test]
    fn modified_keys_preserve_xterm_protocol_semantics() {
        for (key, plain, modified) in [
            (Key::F(1), "\x1bOP", "\x1b[1;8P"),
            (Key::F(4), "\x1bOS", "\x1b[1;8S"),
            (Key::F(5), "\x1b[15~", "\x1b[15;8~"),
            (Key::Insert, "\x1b[2~", "\x1b[2;8~"),
            (Key::Home, "\x1b[H", "\x1b[1;8H"),
        ] {
            assert_eq!(
                encoded(press(key, Modifiers::NONE), false),
                plain.as_bytes()
            );
            assert_eq!(encoded(press(key, ALL), false), modified.as_bytes());
        }
        let left = Key::Arrow(Direction::Left);
        assert_eq!(encoded(press(left, Modifiers::NONE), true), b"\x1bOD");
        let (ctrl, alt) = (Modifiers::CTRL, Modifiers::ALT);
        assert_eq!(encoded(press(left, ctrl), true), b"\x1b[1;5D");
        let ctrl_shift = Modifiers {
            shift: true,
            ..ctrl
        };
        assert_eq!(encoded(press(Key::F(1), ctrl_shift), false), b"\x1b[1;6P");
        assert_eq!(encoded(press(Key::F(12), alt), false), b"\x1b[24;3~");
        assert_eq!(encoded(press(Key::Char('c'), ctrl), false), vec![3]);
        assert_eq!(encoded(press(Key::Char('x'), alt), false), b"\x1bx");
    }

    #[test]
    fn control_bytes_follow_xterm_for_every_c0_control() {
        for (c, byte) in [
            (' ', 0x00),
            ('2', 0x00),
            ('a', 0x01),
            ('Z', 0x1a),
            ('3', 0x1b),
            ('[', 0x1b),
            ('4', 0x1c),
            ('\\', 0x1c),
            ('5', 0x1d),
            (']', 0x1d),
            ('6', 0x1e),
            ('^', 0x1e),
            ('7', 0x1f),
            ('_', 0x1f),
            ('8', 0x7f),
            ('?', 0x7f),
            ('0', b'0'),
            ('9', b'9'),
            ('/', 0x1f),
            ('`', 0x00),
            ('~', 0x1e),
            // What Xlib does not map, Ctrl leaves alone: masking made
            // `;` ESC and `-` CR.
            (';', b';'),
            ('-', b'-'),
            (',', b','),
            ('.', b'.'),
            ('\'', b'\''),
            ('=', b'='),
            ('1', b'1'),
            ('!', b'!'),
        ] {
            assert_eq!(
                encoded(press(Key::Char(c), Modifiers::CTRL), false),
                vec![byte],
                "{c:?}"
            );
        }
    }

    /// Alt is an ESC before a key's bytes, a lone ESC's too (xterm's
    /// Alt-Escape is ESC ESC), but not before a sequence, which carries it.
    #[test]
    fn alt_prefixes_a_lone_escape_but_not_a_sequence() {
        let alt = Modifiers::ALT;
        let ctrl_alt = Modifiers { ctrl: true, ..alt };
        assert_eq!(encoded(press(Key::Escape, alt), false), b"\x1b\x1b");
        assert_eq!(encoded(press(Key::Char('['), ctrl_alt), false), b"\x1b\x1b");
        assert_eq!(encoded(press(Key::Char('3'), ctrl_alt), false), b"\x1b\x1b");
        assert_eq!(encoded(press(Key::Char('a'), alt), false), b"\x1ba");
        assert_eq!(encoded(press(Key::Delete, alt), false), b"\x1b[3;3~");
        assert_eq!(
            encoded(press(Key::Arrow(Direction::Up), alt), true),
            b"\x1b[1;3A"
        );
    }

    /// Every key, under all eight modifier sets and both cursor modes, has a
    /// non-empty encoding, and a plain character is its own UTF-8; in every
    /// pane mode too, but where all keys are escapes.
    #[test]
    fn key_bytes_is_total_and_never_empty() {
        let mut keys = vec![
            Key::Enter,
            Key::Tab,
            Key::Escape,
            Key::Backspace,
            Key::Delete,
            Key::Insert,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
        ];
        keys.extend(Direction::ALL.map(Key::Arrow));
        keys.extend((1..=12).map(Key::F));
        for code in [
            0x20u32, 0x61, 0x5a, 0x30, 0x7e, 0xe9, 0x203c, 0x1f600, 0x300,
        ] {
            if let Some(c) = char::from_u32(code) {
                keys.push(Key::Char(c));
            }
        }
        for &key in &keys {
            for bits in 0u8..8 {
                let mods = Modifiers {
                    ctrl: bits & 1 != 0,
                    alt: bits & 2 != 0,
                    shift: bits & 4 != 0,
                };
                for application in [false, true] {
                    let bytes = encoded(press(key, mods), application);
                    assert!(!bytes.is_empty(), "{key:?} {mods:?}");
                    if let Key::Char(c) = key
                        && mods.is_empty()
                    {
                        let mut buffer = [0u8; 4];
                        assert_eq!(bytes, c.encode_utf8(&mut buffer).as_bytes());
                    }
                }
                for (kitty, other_keys) in
                    [(1, None), (5, None), (31, None), (0, Some(1)), (0, Some(2))]
                {
                    let mode = KeyMode {
                        application: false,
                        kitty,
                        other_keys,
                    };
                    let mut bytes = Vec::new();
                    key_bytes(press(key, mods).into(), mode, &mut bytes);
                    assert!(!bytes.is_empty(), "{key:?} {mods:?} {mode:?}");
                    if let Key::Char(c) = key
                        && mods.is_empty()
                        && kitty & ALL_KEYS == 0
                    {
                        let mut buffer = [0u8; 4];
                        assert_eq!(bytes, c.encode_utf8(&mut buffer).as_bytes());
                    }
                }
            }
        }
    }

    #[test]
    fn a_bracketed_paste_cannot_end_itself_early() {
        assert_eq!(pasted("a\x1b[201~b", false), b"a\x1b[201~b");
        assert_eq!(pasted("a\x1b[201~b", true), b"\x1b[200~ab\x1b[201~");
        // A marker that removing another makes is removed too, however
        // deep: a single pass left `\x1b[201~` here.
        assert_eq!(pasted("\x1b[20\x1b[201~1~x", true), b"\x1b[200~x\x1b[201~");
        let mut nested = String::from("x");
        for _ in 0..5 {
            nested = nested.replacen('x', "\x1b[20x1~", 1);
        }
        let nested = nested.replacen('x', "\x1b[201~", 1);
        assert_eq!(pasted(&nested, true), b"\x1b[200~\x1b[201~");
        // The C1 CSI, U+009B, which a terminal may read as ESC [.
        assert_eq!(pasted("a\u{9b}201~b", true), b"\x1b[200~ab\x1b[201~");
        assert_eq!(pasted("\u{9b}20\x1b[201~1~", true), b"\x1b[200~\x1b[201~");
        // Text that only looks alike stays.
        for text in [
            "\x1b[200~",
            "\x1b[2011~",
            "201~",
            "\x1b[201",
            "~~\x1b[20~1~",
            "é~界",
        ] {
            let mut expected = PASTE_START.to_vec();
            expected.extend_from_slice(text.as_bytes());
            expected.extend_from_slice(PASTE_END);
            assert_eq!(pasted(text, true), expected, "{text:?}");
        }
    }

    /// Whatever the text, a bracketed paste holds no end marker before its
    /// own, and keeps all of it that is no marker.
    #[test]
    fn no_text_ends_a_bracketed_paste() {
        let pieces = ["\x1b", "[", "2", "0", "1", "~", "\u{9b}", "x", "\x1b[201~"];
        // Every text of up to six pieces.
        let mut texts = vec![String::new()];
        for _ in 0..6 {
            let longer: Vec<String> = texts
                .iter()
                .flat_map(|t| pieces.iter().map(move |p| format!("{t}{p}")))
                .collect();
            texts.extend(longer);
            texts.dedup();
        }
        for text in texts {
            let out = pasted(&text, true);
            let inner = out
                .strip_prefix(PASTE_START)
                .and_then(|o| o.strip_suffix(PASTE_END))
                .unwrap_or_default();
            let holds = |marker: &[u8]| {
                (0..inner.len()).any(|i| inner.get(i..).is_some_and(|t| t.starts_with(marker)))
            };
            assert!(
                !holds(PASTE_END) && !holds(C1_PASTE_END),
                "{text:?} gave {out:?}"
            );
            let kept = text.matches('x').count();
            assert_eq!(
                inner.iter().filter(|&&b| b == b'x').count(),
                kept,
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_screen_encodes_pastes_and_focus_as_its_program_asked() -> Result<(), crate::Error> {
        let mut parser = crate::Parser::new(crate::Size::of(4, 10), 0)?;
        let (mut out, screen) = (Vec::new(), parser.screen());
        screen.encode_paste("a\x1b[201~b", &mut out);
        assert_eq!(out, b"a\x1b[201~b");
        out.clear();
        assert!(!screen.encode_focus(true, &mut out) && !screen.encode_focus(false, &mut out));
        assert_eq!(out, b"");
        parser.process(b"\x1b[?2004h\x1b[?1004h")?;
        let screen = parser.screen();
        screen.encode_paste("a\x1b[201~b", &mut out);
        assert_eq!(out, b"\x1b[200~ab\x1b[201~");
        out.clear();
        assert!(screen.encode_focus(true, &mut out) && screen.encode_focus(false, &mut out));
        assert_eq!(out, b"\x1b[I\x1b[O");
        Ok(())
    }
}
