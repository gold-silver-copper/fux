//! Byte encodings for keys, pastes and focus changes delivered to a
//! program, in the modes it asked for (application cursor keys, the kitty
//! keyboard protocol, modifyOtherKeys, bracketed paste, focus reporting):
//! [`crate::Screen::encode_key`], [`crate::Screen::encode_paste`] and
//! [`crate::Screen::encode_focus`] read them from the program's screen,
//! [`key_bytes`] and [`paste`] take them given. Mouse reports are
//! [`crate::keys::mouse`]'s.
use crate::keys::{Direction, Key, KeyPress, Keystroke, Modifiers};
use std::io::Write;

/// How a pane's program asked for its keys: one of the protocols fux
/// speaks, with only the options that protocol reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyMode {
    /// xterm's bytes, which every key has.
    Legacy {
        /// Application cursor keys (DECCKM): the arrows, Home and End as SS3.
        application: bool,
        /// xterm's modifyOtherKeys, if set: the keys it applies to are
        /// `CSI 27 ; mods ; code ~`, the others keep their legacy bytes.
        other_keys: Option<OtherKeys>,
    },
    /// The kitty keyboard protocol (`references/modern/kitty_keyboard_protocol.html`),
    /// which leaves application cursor keys and modifyOtherKeys unread.
    Kitty {
        /// Report alternate keys (4): a text key's shifted and base-layout keys.
        alternate: bool,
        /// Which keys are escapes.
        report: Report,
    },
}

/// xterm's modifyOtherKeys levels (`CSI > 4 ; level m`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OtherKeys {
    /// Level 1: "the usual shift- and control-modifiers work as expected".
    Usual,
    /// Level 2, and 3, which xterm extends to unmodified keys: "all of the
    /// modifiers apply".
    All,
}

/// Which keys the kitty protocol escapes, as its flags ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Report {
    /// Disambiguate (1): keys that type text, with no modifier but Shift,
    /// send their text, and Enter, Tab and Backspace their legacy byte.
    Disambiguated,
    /// Report all keys (8): every key is an escape.
    AllKeys,
    /// Report all keys with their text (8 and 16): a key that types text
    /// carries it as `CSI code ; mods ; text u`.
    AllKeysWithText,
}

impl crate::Screen {
    /// How the program on this screen asked for its keys: the kitty
    /// protocol, if its flags disambiguate or report all keys; else legacy
    /// keys, in the cursor mode and modifyOtherKeys level it set. The other
    /// flags alone change nothing.
    pub fn key_mode(&self) -> KeyMode {
        let flags = self.kitty_keyboard_flags();
        let report = match (flags & ALL_KEYS != 0, flags & TEXT != 0) {
            (true, true) => Report::AllKeysWithText,
            (true, false) => Report::AllKeys,
            (false, _) if flags & DISAMBIGUATE != 0 => Report::Disambiguated,
            (false, _) => {
                return KeyMode::Legacy {
                    application: self.mode(crate::Mode::ApplicationCursor),
                    other_keys: self.modify_other_keys().map(|level| match level {
                        1 => OtherKeys::Usual,
                        _ => OtherKeys::All,
                    }),
                };
            }
        };
        KeyMode::Kitty {
            alternate: flags & ALTERNATE_KEYS != 0,
            report,
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
        KeyMode::Legacy {
            application,
            other_keys: None,
        }
    }
}

/// The kitty protocol's progressive enhancements, as a screen holds them.
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
/// the kitty keyboard protocol's; or xterm's modifyOtherKeys, if it is set
/// and applies to the key, else legacy xterm bytes, which every key has.
pub fn key_bytes(stroke: Keystroke, mode: KeyMode, out: &mut Vec<u8>) {
    match mode {
        KeyMode::Kitty { alternate, report } => kitty(stroke, alternate, report, out),
        KeyMode::Legacy {
            application,
            other_keys: level,
        } => {
            if !level.is_some_and(|level| other_keys(stroke, level, out)) {
                legacy(stroke.press, application, out);
            }
        }
    }
}

/// The kitty keyboard protocol's bytes for a key, its keys escaped as
/// `report` says (`references/modern/kitty_keyboard_protocol.html`):
/// - a key that types text, with no modifier but Shift, sends its text,
///   unless all keys are reported; then `CSI code ; mods u`, the code the
///   unshifted key's, with the text as its third parameter if asked;
/// - any other text key is `CSI code ; mods u`, with its shifted and
///   base-layout keys if `alternate` and known;
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
fn kitty(stroke: Keystroke, alternate: bool, report: Report, out: &mut Vec<u8>) {
    let KeyPress { key, mods } = stroke.press;
    let exact = stroke.kitty;
    let all = report != Report::Disambiguated;
    let capital = matches!(key, Key::Char(c) if c.is_ascii_uppercase()) && exact.is_none();
    let bits = exact.map_or_else(|| xterm_bits(mods) | u8::from(capital), |k| k.mods);
    let held = bits & !LOCKS;
    let modifier = u16::from(bits).saturating_add(1);
    // `;mods`, left out when there are none; empty if a parameter follows,
    // as the spec's examples have it (`CSI 0 ; ; 229 u`).
    let mods_param = |out: &mut Vec<u8>, more: bool| {
        if bits != 0 {
            let _ = write!(out, ";{modifier}");
        } else if more {
            out.push(b';');
        }
    };
    let csi_u = |out: &mut Vec<u8>, code: u32| {
        let _ = write!(out, "\x1b[{code}");
        mods_param(out, false);
        out.push(b'u');
    };
    // A keypad key the terminal named is sent as it named it.
    if let Some(code @ 57399..=57427) = exact.and_then(|k| k.code) {
        return csi_u(out, code);
    }
    match Form::from(key) {
        Form::Text(c) => {
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
            if alternate {
                let shifted = shifted.filter(|_| bits & 1 != 0);
                let base = exact.and_then(|k| k.base);
                match (shifted, base) {
                    (Some(s), Some(b)) => drop(write!(out, ":{s}:{b}")),
                    (Some(s), None) => drop(write!(out, ":{s}")),
                    (None, Some(b)) => drop(write!(out, "::{b}")),
                    (None, None) => {}
                }
            }
            let text = (report == Report::AllKeysWithText && types_text).then_some(u32::from(c));
            mods_param(out, text.is_some());
            if let Some(text) = text {
                let _ = write!(out, ";{text}");
            }
            out.push(b'u');
        }
        // Enter, Tab and Backspace: the legacy byte, which is also the code.
        Form::Control(byte) if byte != 0x1b && held == 0 && !all => out.push(byte),
        Form::Control(byte) => csi_u(out, u32::from(byte)),
        // F3 is `CSI 13 ~`: the protocol leaves `CSI R` to cursor position reports.
        Form::Numbered(_, 'R') => numbered(out, 13, modifier, '~'),
        Form::Numbered(number, final_byte) => numbered(out, number, modifier, final_byte),
        // F13 to F35 are numbered from 57376; fux names none past F12.
        Form::Unnumbered(n) => csi_u(
            out,
            57376_u32.saturating_add(u32::from(n.saturating_sub(13))),
        ),
    }
}

/// A key as both protocols start from xterm's bytes for it.
#[derive(Clone, Copy)]
enum Form {
    /// A key that types a character.
    Text(char),
    /// Enter, Tab, Escape and Backspace: their C0 control or DEL.
    Control(u8),
    /// `CSI number ; mods final` ([`numbered`]): the arrows, Home, End and
    /// F1 to F4 numbered 1, the others with xterm's numbers and gaps, as
    /// `decode.rs` (`csi`) reads them back.
    Numbered(u8, char),
    /// A function key past F12, which xterm has no number for.
    Unnumbered(u8),
}

impl From<Key> for Form {
    fn from(key: Key) -> Form {
        let f_codes = [15, 17, 18, 19, 20, 21, 23, 24];
        match key {
            Key::Char(c) => Form::Text(c),
            Key::Enter => Form::Control(0x0d),
            Key::Tab => Form::Control(0x09),
            Key::Escape => Form::Control(0x1b),
            Key::Backspace => Form::Control(0x7f),
            Key::Arrow(Direction::Up) => Form::Numbered(1, 'A'),
            Key::Arrow(Direction::Down) => Form::Numbered(1, 'B'),
            Key::Arrow(Direction::Right) => Form::Numbered(1, 'C'),
            Key::Arrow(Direction::Left) => Form::Numbered(1, 'D'),
            Key::Home => Form::Numbered(1, 'H'),
            Key::End => Form::Numbered(1, 'F'),
            Key::F(1) => Form::Numbered(1, 'P'),
            Key::F(2) => Form::Numbered(1, 'Q'),
            Key::F(3) => Form::Numbered(1, 'R'),
            Key::F(4) => Form::Numbered(1, 'S'),
            Key::Insert => Form::Numbered(2, '~'),
            Key::Delete => Form::Numbered(3, '~'),
            Key::PageUp => Form::Numbered(5, '~'),
            Key::PageDown => Form::Numbered(6, '~'),
            Key::F(n) => match usize::from(n).checked_sub(5).and_then(|i| f_codes.get(i)) {
                Some(&number) => Form::Numbered(number, '~'),
                None => Form::Unnumbered(n),
            },
        }
    }
}

/// `CSI number ; modifier final`, the modifier left out when it is 1, none
/// held, and then the number too if it is 1 (`CSI A`).
fn numbered(out: &mut Vec<u8>, number: u8, modifier: u16, final_byte: char) {
    let _ = match (number, modifier) {
        (_, 2..) => write!(out, "\x1b[{number};{modifier}{final_byte}"),
        (1, _) => write!(out, "\x1b[{final_byte}"),
        (_, _) => write!(out, "\x1b[{number}{final_byte}"),
    };
}

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
fn other_keys(stroke: Keystroke, level: OtherKeys, out: &mut Vec<u8>) -> bool {
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
    let sent = match level {
        OtherKeys::Usual => allowed(sym, state),
        OtherKeys::All => state,
    };
    if sent.is_empty() {
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
/// which at level 1 becomes `ISO_Left_Tab`, a function key, `CSI Z`. At
/// level 2, Shift alone on a character other than Space types its text.
fn modified(sym: Sym, state: Modifiers, level: OtherKeys) -> bool {
    if state.is_empty() {
        return false;
    }
    let OtherKeys::Usual = level else {
        return match sym {
            Sym::BackSpace => state != Modifiers::CTRL,
            Sym::Delete | Sym::Escape | Sym::Return | Sym::Tab => true,
            Sym::Char(c) => state != Modifiers::SHIFT || c == ' ',
        };
    };
    if sym == Sym::Tab && state == Modifiers::SHIFT {
        return false;
    }
    // A character's modifiers are filtered first, a predefined key's not.
    let st = match sym {
        Sym::Char(_) => allowed(sym, state),
        Sym::Return | Sym::Tab | Sym::Escape | Sym::BackSpace | Sym::Delete => state,
    };
    let control = sym.control_input() || sym.control_alias(state.ctrl);
    match sym {
        _ if st.is_empty() => false,
        Sym::BackSpace | Sym::Delete => false,
        Sym::Return | Sym::Tab => true,
        Sym::Char(_) | Sym::Escape => !control || st != Modifiers::CTRL && st != Modifiers::SHIFT,
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
    let modifier = u16::from(xterm_bits(mods)).saturating_add(1);
    let start = out.len();
    match Form::from(key) {
        // SS3, for F1 to F4 and in application mode the other keys numbered
        // 1, unmodified.
        Form::Numbered(1, final_byte)
            if modifier == 1 && (application || matches!(key, Key::F(_))) =>
        {
            let _ = write!(out, "\x1bO{final_byte}");
        }
        Form::Numbered(number, final_byte) => numbered(out, number, modifier, final_byte),
        Form::Control(0x09) if shift => out.extend_from_slice(b"\x1b[Z"),
        Form::Control(byte) => out.push(byte),
        Form::Unnumbered(_) => out.push(27),
        Form::Text(c) if ctrl && c.is_ascii() => out.push(control_byte(c).unwrap_or(c as u8)),
        Form::Text(c) => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
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

    /// The bytes for a named key, or a stroke, in `mode`, as text, appended
    /// after others, which stay as they were.
    fn named(name: &str, mode: KeyMode) -> String {
        let press = name.parse::<KeyPress>().unwrap_or(KeyPress::char('?'));
        stroke(press.into(), mode)
    }
    fn stroke(stroke: Keystroke, mode: KeyMode) -> String {
        let mut out = b"before".to_vec();
        key_bytes(stroke, mode, &mut out);
        assert!(out.starts_with(b"before"), "{stroke:?}");
        String::from_utf8_lossy(out.get(6..).unwrap_or_default()).into_owned()
    }
    const LEGACY: KeyMode = KeyMode::Legacy {
        application: false,
        other_keys: None,
    };
    const fn kitty_mode(alternate: bool, report: Report) -> KeyMode {
        KeyMode::Kitty { alternate, report }
    }
    const DISAMBIGUATED: KeyMode = kitty_mode(false, Report::Disambiguated);
    const ALTERNATE: KeyMode = kitty_mode(true, Report::Disambiguated);
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
            assert_eq!(named(name, DISAMBIGUATED), bytes, "{name}");
        }
    }

    /// What a kitty-protocol terminal said beyond the press is given back:
    /// Shift with Ctrl, which the press folds away (ctrl+shift+i is
    /// `CSI 105 ; 6 u`, the spec's "Legacy text keys" table), Super and the
    /// locks, a keypad key's own number; with alternate keys (4), the
    /// shifted and base-layout keys ("Key codes").
    #[test]
    fn a_reported_key_is_given_back_as_reported() {
        let ctrl_shift_i = reported("C-i", 105, Some(73), None, 5);
        assert_eq!(stroke(ctrl_shift_i, DISAMBIGUATED), "\x1b[105;6u");
        assert_eq!(stroke(ctrl_shift_i, ALTERNATE), "\x1b[105:73;6u");
        let super_a = reported("a", 97, None, None, 8);
        assert_eq!(stroke(super_a, DISAMBIGUATED), "\x1b[97;9u");
        // Ctrl and С on a Cyrillic layout, whose base-layout key is c.
        let cyrillic = reported("C-c", 1089, None, Some(99), 4);
        assert_eq!(stroke(cyrillic, DISAMBIGUATED), "\x1b[1089;5u");
        assert_eq!(stroke(cyrillic, ALTERNATE), "\x1b[1089::99;5u");
        let keypad_enter = reported("Enter", 57414, None, None, 0);
        assert_eq!(stroke(keypad_enter, DISAMBIGUATED), "\x1b[57414u");
        assert_eq!(stroke(keypad_enter, LEGACY), "\r");
        let caps_ctrl_a = reported("C-a", 97, None, None, 4 | 64);
        assert_eq!(stroke(caps_ctrl_a, DISAMBIGUATED), "\x1b[97;69u");
        // A legacy pane gets the press's bytes alone, what was reported
        // beside them unused.
        assert_eq!(stroke(ctrl_shift_i, LEGACY), "\t");
        assert_eq!(stroke(cyrillic, LEGACY), "\x03");
        // Alternate keys from a legacy press: a capital's shifted key.
        assert_eq!(named("M-A", ALTERNATE), "\x1b[97:65;4u");
    }

    /// Report all keys (8): text keys too are `CSI code ; mods u`, with the
    /// text as a third parameter if asked (16; the spec's "Text as code
    /// points": shift+a is `CSI 97 ; 2 ; 65 u`); Enter, Tab and Backspace
    /// are escapes.
    #[test]
    fn reporting_all_keys_escapes_text_keys() {
        let (all, text) = (Report::AllKeys, Report::AllKeysWithText);
        for (alternate, report, name, bytes) in [
            (false, all, "a", "\x1b[97u"),
            (false, all, "A", "\x1b[97;2u"),
            (true, all, "A", "\x1b[97:65;2u"),
            (false, text, "a", "\x1b[97;;97u"),
            (false, text, "A", "\x1b[97;2;65u"),
            (false, text, "C-a", "\x1b[97;5u"),
            (false, all, "Enter", "\x1b[13u"),
            (false, all, "Tab", "\x1b[9u"),
            (false, all, "BSpace", "\x1b[127u"),
            (false, all, "Escape", "\x1b[27u"),
            (false, all, "Up", "\x1b[A"),
        ] {
            let mode = kitty_mode(alternate, report);
            assert_eq!(named(name, mode), bytes, "{mode:?} {name}");
        }
    }

    const fn other_keys_level(level: OtherKeys) -> KeyMode {
        KeyMode::Legacy {
            application: false,
            other_keys: Some(level),
        }
    }
    const USUAL: KeyMode = other_keys_level(OtherKeys::Usual);
    const ALL_MODIFIERS: KeyMode = other_keys_level(OtherKeys::All);

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
            assert_eq!(named(name, USUAL), named(name, LEGACY), "{name}");
        }
        assert_eq!(named("M-x", USUAL), "\x1bx");
        assert_eq!(named("C-M-x", USUAL), "\x1b\x18");
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
            assert_eq!(named(name, USUAL), bytes, "{name}");
        }
        // Ctrl-Shift-a, which only a kitty-protocol terminal tells from
        // Ctrl-a, is a control too.
        let ctrl_shift_a = reported("C-a", 97, Some(65), None, 5);
        assert_eq!(stroke(ctrl_shift_a, USUAL), "\x01");
    }

    /// modifyOtherKeys level 2, as xterm's input.c sends it: every modifier
    /// applies (`ModifyOtherKeys`, case 2), and the Alt key is filtered no
    /// more; but Ctrl alone on Backspace, which xterm's backarrow toggle
    /// makes BS, and Shift alone on a character that types text, which is
    /// its text: xterm's for `!`, fux's departure for a letter. Shift-Tab is
    /// `CSI 27 ; 2 ; 9 ~`, as ctlseqs says. Other keys keep their legacy
    /// bytes.
    #[test]
    fn modify_other_keys_level_2_follows_xterm() {
        for (name, bytes) in [
            ("M-x", "\x1b[27;3;120~"),
            ("M-X", "\x1b[27;4;88~"),
            ("C-M-x", "\x1b[27;7;120~"),
            ("M-1", "\x1b[27;3;49~"),
            ("C-1", "\x1b[27;5;49~"),
            ("C-a", "\x1b[27;5;97~"),
            ("C-i", "\x1b[27;5;105~"),
            ("C-Space", "\x1b[27;5;32~"),
            ("BTab", "\x1b[27;2;9~"),
            ("M-Tab", "\x1b[27;3;9~"),
            ("S-Enter", "\x1b[27;2;13~"),
            ("C-Enter", "\x1b[27;5;13~"),
            ("M-Enter", "\x1b[27;3;13~"),
            ("M-Escape", "\x1b[27;3;27~"),
            ("S-Escape", "\x1b[27;2;27~"),
            ("M-BSpace", "\x1b[27;3;127~"),
            ("S-BSpace", "\x1b[27;2;127~"),
            ("C-S-BSpace", "\x1b[27;6;8~"),
            ("C-M-BSpace", "\x1b[27;7;8~"),
            ("C-BSpace", "\x7f"),
            ("a", "a"),
            ("A", "A"),
            ("!", "!"),
            ("Enter", "\r"),
            ("Escape", "\x1b"),
            ("C-Up", "\x1b[1;5A"),
            ("F5", "\x1b[15~"),
        ] {
            assert_eq!(named(name, ALL_MODIFIERS), bytes, "{name}");
        }
        let ctrl_shift_a = reported("C-a", 97, Some(65), None, 5);
        assert_eq!(stroke(ctrl_shift_a, ALL_MODIFIERS), "\x1b[27;6;65~");
        // Shift-Space, which only a kitty-protocol terminal tells from
        // Space, is xterm's one character modified by Shift alone.
        let shift_space = reported("Space", 32, None, None, 1);
        assert_eq!(stroke(shift_space, ALL_MODIFIERS), "\x1b[27;2;32~");
        assert_eq!(stroke(shift_space, USUAL), " ");
    }

    /// A screen's kitty flags pick the protocol: disambiguate or all keys
    /// make it kitty's, whatever the cursor mode and modifyOtherKeys; the
    /// other flags alone leave legacy keys. Level 3 is taken as 2.
    #[test]
    fn a_screen_s_flags_pick_its_key_protocol() -> Result<(), crate::Error> {
        for (setup, mode) in [
            ("", LEGACY),
            ("\x1b[?1h\x1b[>2u", KeyMode::legacy(true)),
            ("\x1b[>4;1m\x1b[>16u", USUAL),
            ("\x1b[>4;3m", ALL_MODIFIERS),
            ("\x1b[?1h\x1b[>4;2m\x1b[>5u", ALTERNATE),
            ("\x1b[>8u", kitty_mode(false, Report::AllKeys)),
            ("\x1b[>25u", kitty_mode(false, Report::AllKeysWithText)),
        ] {
            let options = crate::Feature::KittyKeyboard.into();
            let mut parser = crate::Parser::with_options(crate::Size::of(4, 10), 0, options)?;
            parser.process(setup.as_bytes())?;
            assert_eq!(parser.screen().key_mode(), mode, "{setup:?}");
        }
        Ok(())
    }

    fn pasted(text: &str, bracketed: bool) -> Vec<u8> {
        let mut out = Vec::new();
        paste(text, bracketed, &mut out);
        out
    }

    /// Legacy keys: SS3 for F1 to F4, and in application mode for the
    /// cursor keys, unless modified; Alt an ESC before a key's bytes, a lone
    /// ESC's too (xterm's Alt-Escape is ESC ESC), but not before a sequence,
    /// which carries it.
    #[test]
    fn legacy_keys_follow_xterm() {
        let application = KeyMode::legacy(true);
        for (name, mode, bytes) in [
            ("F1", LEGACY, "\x1bOP"),
            ("C-M-S-F1", LEGACY, "\x1b[1;8P"),
            ("C-S-F1", LEGACY, "\x1b[1;6P"),
            ("C-M-S-F4", LEGACY, "\x1b[1;8S"),
            ("F5", LEGACY, "\x1b[15~"),
            ("C-M-S-F5", LEGACY, "\x1b[15;8~"),
            ("M-F12", LEGACY, "\x1b[24;3~"),
            ("C-M-S-Insert", LEGACY, "\x1b[2;8~"),
            ("Home", LEGACY, "\x1b[H"),
            ("C-M-S-Home", LEGACY, "\x1b[1;8H"),
            ("Left", application, "\x1bOD"),
            ("C-Left", application, "\x1b[1;5D"),
            ("M-Up", application, "\x1b[1;3A"),
            ("C-c", LEGACY, "\x03"),
            ("M-a", LEGACY, "\x1ba"),
            ("M-Escape", LEGACY, "\x1b\x1b"),
            ("C-M-[", LEGACY, "\x1b\x1b"),
            ("C-M-3", LEGACY, "\x1b\x1b"),
            ("M-Delete", LEGACY, "\x1b[3;3~"),
        ] {
            assert_eq!(named(name, mode), bytes, "{name}");
        }
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
            let ctrl = KeyPress::new(Key::Char(c), Modifiers::CTRL);
            assert_eq!(stroke(ctrl.into(), LEGACY), char::from(byte).to_string());
        }
    }

    /// Every key, under all eight modifier sets and in every key mode, has a
    /// non-empty encoding, and a plain character is its own UTF-8 but where
    /// all keys are escapes.
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
                for mode in [
                    LEGACY,
                    KeyMode::legacy(true),
                    USUAL,
                    ALL_MODIFIERS,
                    DISAMBIGUATED,
                    ALTERNATE,
                    kitty_mode(true, Report::AllKeysWithText),
                ] {
                    let mut bytes = Vec::new();
                    key_bytes(KeyPress { key, mods }.into(), mode, &mut bytes);
                    assert!(!bytes.is_empty(), "{key:?} {mods:?} {mode:?}");
                    if let Key::Char(c) = key
                        && mods.is_empty()
                        && !matches!(mode, KeyMode::Kitty { report, .. } if report != Report::Disambiguated)
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
