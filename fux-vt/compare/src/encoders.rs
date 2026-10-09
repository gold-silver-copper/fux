//! `encoders`: fux-vt's input encoders beside libghostty-vt's. Both
//! terminals are put in the same modes by the same bytes (mouse tracking and
//! its encoding, focus reporting, bracketed paste, cursor keys, the kitty
//! keyboard flags, modifyOtherKeys); then each key, mouse event, focus
//! change and paste is encoded by both, each reading the modes from its own
//! terminal (`Screen::encode_*`, ghostty's `set_options_from_terminal`), and
//! the bytes are compared. A difference is a failure unless it is one of
//! the recorded verdicts below, each with who decides it.
use fux_vt::keys::mouse::{MouseAction, MouseButton, MouseEvent};
use fux_vt::keys::{Direction, Key, KeyPress, Keystroke, Kitty, Modifiers};
use fux_vt::{Feature, Options, Parser};
use libghostty_vt::terminal::Mode;
use libghostty_vt::{Terminal, TerminalOptions, focus, key as gkey, mouse as gmouse, paste};

/// Differences decided in fux-vt's favour or recorded as a choice, and the
/// source that decides each.
const VERDICTS: &[(&str, &str)] = &[
    (
        "utf8-button",
        "UTF-8 mouse mode (1005), a button code of 96 or more: ctlseqs says \"Cb will be UTF-8 \
         encoded\", as fux-vt does; ghostty writes it as one raw byte",
    ),
    (
        "utf8-limit",
        "UTF-8 mouse mode (1005), a position past 2015: ctlseqs gives 2015 as its last, and fux-vt \
         sends nothing past it, as xterm does; ghostty encodes it in three bytes",
    ),
    (
        "legacy-extras",
        "no keyboard mode set: ghostty sends keys legacy bytes cannot carry as CSI u (Ctrl-I, \
         Ctrl-Shift-letters, Ctrl-punctuation) and modified Enter, Tab and Escape as CSI 27 ; m ; \
         k ~; xterm with modifyOtherKeys off sends the legacy bytes, as fux-vt does: a program \
         that asked for nothing gets nothing it did not ask for",
    ),
    (
        "f3",
        "F3 with modifiers, outside the kitty protocol: xterm's PC-style function keys send CSI 1 \
         ; m R, as fux-vt does; ghostty sends kitty's CSI 13 ; m ~",
    ),
    (
        "ctrl-backspace",
        "Ctrl-Backspace outside the kitty protocol: fux-vt sends DEL, as for Backspace; ghostty \
         sends BS (a choice no reference decides)",
    ),
    (
        "modify-other-keys",
        "modifyOtherKeys 2: xterm sends every modified key as CSI 27 ; m ; k ~, Ctrl-letters and \
         Alt-Escape included, but a printable key with Shift alone as its character, as fux-vt \
         (a port of xterm's input.c) does; ghostty keeps the legacy bytes of the first and \
         escapes the second",
    ),
    (
        "kitty-flags-without-disambiguate",
        "kitty flags without disambiguate (1) or all keys (8): the spec gives these keys no other \
         encoding, and fux-vt sends their legacy bytes; ghostty sends kitty's forms",
    ),
    (
        "kitty-press-type",
        "report event types (2): a press may leave out its event type (\"CSI key-code;modifier \
         # this is a press event\") and fux-vt does, as kitty does; ghostty writes ;1:1",
    ),
    (
        "paste-controls",
        "a paste holding NUL, BS, ENQ, EOT, ESC, DEL or the tty's special characters (^C ^\\ ^U \
         ^Z ^Q ^S ^W ^V ^R ^O): ghostty, as xterm, makes each a space; fux-vt passes the text as \
         pasted, removing only what would end a bracketed paste",
    ),
    (
        "paste-newline",
        "an unbracketed paste holding LF: ghostty, as xterm, sends CR; fux-vt passes the text as \
         pasted",
    ),
    (
        "paste-c1-end",
        "a bracketed paste holding U+009B 201 ~ (the C1 form of its end): fux-vt removes it, as \
         it removes ESC [ 201 ~; ghostty keeps it",
    ),
];

/// Columns and rows of both terminals: past 2015 columns, the last
/// position UTF-8 mouse mode carries.
const COLS: u16 = 2100;
const ROWS: u16 = 230;
/// The pixel size of a cell ghostty's mouse encoder is told.
const CELL: (u32, u32) = (10, 20);

/// The tally of one kind of input.
#[derive(Default)]
struct Tally {
    compared: u64,
    alike: u64,
    verdicts: std::collections::BTreeMap<&'static str, u64>,
    failures: Vec<String>,
}

impl Tally {
    fn judge(
        &mut self,
        what: impl FnOnce() -> String,
        ours: &[u8],
        theirs: &[u8],
        verdict: Option<&'static str>,
    ) {
        self.compared = self.compared.saturating_add(1);
        if ours == theirs {
            self.alike = self.alike.saturating_add(1);
            return;
        }
        match verdict {
            Some(name) => {
                let n = self.verdicts.entry(name).or_default();
                *n = n.saturating_add(1);
            }
            None => {
                if self.failures.len() < 40 || std::env::var_os("FUX_ENCODERS_ALL").is_some() {
                    self.failures.push(format!(
                        "{}\n    fux-vt  {}\n    ghostty {}",
                        what(),
                        shown(ours),
                        shown(theirs)
                    ));
                }
            }
        }
    }

    fn report(&self, name: &str) -> bool {
        let verdicts: Vec<String> = self
            .verdicts
            .iter()
            .map(|(v, n)| format!("{v} {n}"))
            .collect();
        println!(
            "{name:<6} {} compared, {} alike{}{}",
            self.compared,
            self.alike,
            if verdicts.is_empty() {
                String::new()
            } else {
                format!(", recorded verdicts: {}", verdicts.join(", "))
            },
            if self.failures.is_empty() {
                String::new()
            } else {
                format!(
                    ", {} DIFFER",
                    self.compared
                        .saturating_sub(self.alike)
                        .saturating_sub(self.verdicts.values().sum())
                )
            }
        );
        for failure in &self.failures {
            println!("  {failure}");
        }
        self.failures.is_empty()
    }
}

fn shown(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "(nothing)".into();
    }
    bytes.escape_ascii().to_string()
}

fn err(what: &'static str) -> impl Fn(libghostty_vt::Error) -> String {
    move |e| format!("ghostty {what}: {e:?}")
}

/// Both terminals, put in the modes `setup` sets.
struct Pair {
    fux: Parser,
    ghostty: Terminal<'static, 'static>,
}

impl Pair {
    fn new(setup: &[u8]) -> Result<Pair, String> {
        let options = Options::new().with(Feature::KittyKeyboard);
        let mut fux =
            Parser::with_options(ROWS, COLS, 0, options).map_err(|e| format!("fux-vt: {e:?}"))?;
        fux.process(setup).map_err(|e| format!("fux-vt: {e:?}"))?;
        let mut ghostty = Terminal::new(TerminalOptions {
            cols: COLS,
            rows: ROWS,
            max_scrollback: 0,
        })
        .map_err(err("new"))?;
        ghostty.vt_write(setup);
        Ok(Pair { fux, ghostty })
    }
}

/// The mode settings compared: every mouse mode in every encoding, with
/// focus reporting and bracketed paste on or off; cursor keys normal or
/// application with every kitty flag set and modifyOtherKeys 2.
fn mouse_setups() -> Vec<String> {
    let mut out = Vec::new();
    for mode in ["", "\x1b[?9h", "\x1b[?1000h", "\x1b[?1002h", "\x1b[?1003h"] {
        for encoding in ["", "\x1b[?1005h", "\x1b[?1006h"] {
            out.push(format!("{mode}{encoding}"));
        }
    }
    out
}

fn key_setups() -> Vec<String> {
    let mut out = Vec::new();
    for cursor in ["", "\x1b[?1h"] {
        out.push(cursor.to_owned());
        out.push(format!("{cursor}\x1b[>4;2m"));
        for flags in 1..32 {
            out.push(format!("{cursor}\x1b[>{flags}u"));
        }
    }
    out
}

pub fn run(seed: u64, cases: usize) -> Result<bool, String> {
    let mut rng = crate::rng::Rng::new(seed);
    let (mut keys, mut mice, mut focuses, mut pastes) = (
        Tally::default(),
        Tally::default(),
        Tally::default(),
        Tally::default(),
    );
    for setup in key_setups() {
        let pair = Pair::new(setup.as_bytes())?;
        compare_keys(&pair, &setup, &mut keys)?;
    }
    for setup in mouse_setups() {
        let pair = Pair::new(setup.as_bytes())?;
        compare_mouse(&pair, &setup, &mut mice)?;
    }
    for setup in [
        "",
        "\x1b[?1004h",
        "\x1b[?2004h",
        "\x1b[?1004h\x1b[?2004h",
        "\x1b[?1004h\x1b[?1004l",
    ] {
        let pair = Pair::new(setup.as_bytes())?;
        compare_focus(&pair, setup, &mut focuses)?;
        compare_pastes(&pair, setup, &mut pastes, &mut rng, cases)?;
    }
    for (name, why) in VERDICTS {
        println!("verdict {name}: {why}");
    }
    let mut ok = true;
    for (name, tally) in [
        ("keys", &keys),
        ("mouse", &mice),
        ("focus", &focuses),
        ("paste", &pastes),
    ] {
        ok &= tally.report(name);
    }
    Ok(ok)
}

/// Every modifier combination of Shift, Alt and Ctrl.
fn all_mods() -> impl Iterator<Item = Modifiers> {
    (0u8..8).map(|bits| Modifiers {
        shift: bits & 1 != 0,
        alt: bits & 2 != 0,
        ctrl: bits & 4 != 0,
    })
}

/// US layout: the unshifted key of a shifted character.
const SHIFTED: &[(char, char)] = &[
    ('!', '1'),
    ('@', '2'),
    ('#', '3'),
    ('$', '4'),
    ('%', '5'),
    ('^', '6'),
    ('&', '7'),
    ('*', '8'),
    ('(', '9'),
    (')', '0'),
    ('_', '-'),
    ('+', '='),
    ('{', '['),
    ('}', ']'),
    ('|', '\\'),
    (':', ';'),
    ('"', '\''),
    ('<', ','),
    ('>', '.'),
    ('?', '/'),
    ('~', '`'),
];

/// The physical key typing `c` unshifted on a US layout.
fn physical(c: char) -> Option<gkey::Key> {
    use gkey::Key as K;
    Some(match c.to_ascii_lowercase() {
        'a' => K::A,
        'b' => K::B,
        'c' => K::C,
        'd' => K::D,
        'e' => K::E,
        'f' => K::F,
        'g' => K::G,
        'h' => K::H,
        'i' => K::I,
        'j' => K::J,
        'k' => K::K,
        'l' => K::L,
        'm' => K::M,
        'n' => K::N,
        'o' => K::O,
        'p' => K::P,
        'q' => K::Q,
        'r' => K::R,
        's' => K::S,
        't' => K::T,
        'u' => K::U,
        'v' => K::V,
        'w' => K::W,
        'x' => K::X,
        'y' => K::Y,
        'z' => K::Z,
        '0' => K::Digit0,
        '1' => K::Digit1,
        '2' => K::Digit2,
        '3' => K::Digit3,
        '4' => K::Digit4,
        '5' => K::Digit5,
        '6' => K::Digit6,
        '7' => K::Digit7,
        '8' => K::Digit8,
        '9' => K::Digit9,
        '-' => K::Minus,
        '=' => K::Equal,
        '[' => K::BracketLeft,
        ']' => K::BracketRight,
        '\\' => K::Backslash,
        ';' => K::Semicolon,
        '\'' => K::Quote,
        ',' => K::Comma,
        '.' => K::Period,
        '/' => K::Slash,
        '`' => K::Backquote,
        ' ' => K::Space,
        _ => return None,
    })
}

/// The keys compared: the functional keys fux-vt knows, and the characters
/// a US layout types, each with every modifier.
fn keys() -> Vec<(Key, Modifiers)> {
    let mut named = vec![
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
    named.extend(Direction::ALL.map(Key::Arrow));
    named.extend((1..=12).map(Key::F));
    let mut chars: Vec<char> = ('a'..='z').chain('A'..='Z').chain('0'..='9').collect();
    chars.extend("-=[]\\;',./` ".chars());
    chars.extend(SHIFTED.iter().map(|&(c, _)| c));
    named.extend(chars.into_iter().map(Key::Char));
    named
        .into_iter()
        .flat_map(|key| all_mods().map(move |mods| (key, mods)))
        .collect()
}

/// The event ghostty's host makes of `key` with `mods` held, and the
/// keystroke a kitty-protocol terminal's report decodes to in fux-vt:
/// the text the key types (Shift applied), the unshifted key, Shift
/// consumed by a shifted character.
fn key_event(
    key: Key,
    mods: Modifiers,
) -> Result<Option<(gkey::Event<'static>, Keystroke)>, String> {
    use gkey::Key as K;
    let mut event = gkey::Event::new().map_err(err("key event"))?;
    event.set_action(gkey::Action::Press);
    let mut held = gkey::Mods::empty();
    let mut kitty = None;
    let mut press_mods = mods;
    let physical_key = match key {
        Key::Char(c) => {
            let (base, shifted) = if c.is_ascii_uppercase() {
                (c.to_ascii_lowercase(), true)
            } else if let Some(&(_, base)) = SHIFTED.iter().find(|&&(s, _)| s == c) {
                (base, true)
            } else {
                (c, false)
            };
            // Shift with a character that has a shifted form is that form;
            // the case is the character's own.
            if mods.shift && !shifted {
                return Ok(None);
            }
            let Some(physical) = physical(base) else {
                return Ok(None);
            };
            if shifted {
                held |= gkey::Mods::SHIFT;
                event.set_consumed_mods(gkey::Mods::SHIFT);
                press_mods.shift = true;
            }
            event.set_utf8(Some(c.to_string()));
            event.set_unshifted_codepoint(base);
            let bits =
                u8::from(press_mods.shift) | (u8::from(mods.alt) << 1) | (u8::from(mods.ctrl) << 2);
            kitty = Some(Kitty {
                code: Some(u32::from(base)),
                shifted: shifted.then_some(u32::from(c)),
                base: None,
                mods: bits,
            });
            physical
        }
        Key::Enter => K::Enter,
        Key::Tab => K::Tab,
        Key::Escape => K::Escape,
        Key::Backspace => K::Backspace,
        Key::Delete => K::Delete,
        Key::Insert => K::Insert,
        Key::Home => K::Home,
        Key::End => K::End,
        Key::PageUp => K::PageUp,
        Key::PageDown => K::PageDown,
        Key::Arrow(Direction::Up) => K::ArrowUp,
        Key::Arrow(Direction::Down) => K::ArrowDown,
        Key::Arrow(Direction::Left) => K::ArrowLeft,
        Key::Arrow(Direction::Right) => K::ArrowRight,
        Key::F(n) => match n {
            1 => K::F1,
            2 => K::F2,
            3 => K::F3,
            4 => K::F4,
            5 => K::F5,
            6 => K::F6,
            7 => K::F7,
            8 => K::F8,
            9 => K::F9,
            10 => K::F10,
            11 => K::F11,
            12 => K::F12,
            _ => return Ok(None),
        },
    };
    if mods.shift {
        held |= gkey::Mods::SHIFT;
    }
    if mods.alt {
        held |= gkey::Mods::ALT;
    }
    if mods.ctrl {
        held |= gkey::Mods::CTRL;
    }
    event.set_key(physical_key).set_mods(held);
    let stroke = Keystroke {
        press: KeyPress::new(key, press_mods),
        kitty,
    };
    Ok(Some((event, stroke)))
}

fn compare_keys(pair: &Pair, setup: &str, tally: &mut Tally) -> Result<(), String> {
    let mut encoder = gkey::Encoder::new().map_err(err("key encoder"))?;
    encoder
        .set_options_from_terminal(&pair.ghostty)
        .set_alt_esc_prefix(true);
    for (key, mods) in keys() {
        let Some((event, stroke)) = key_event(key, mods)? else {
            continue;
        };
        let mut ours = Vec::new();
        pair.fux.screen().encode_key(stroke, &mut ours);
        let mut theirs = Vec::new();
        encoder
            .encode_to_vec(&event, &mut theirs)
            .map_err(err("key encode"))?;
        let mut legacy = Vec::new();
        fux_vt::keys::encode::key_bytes(
            stroke,
            fux_vt::keys::encode::KeyMode::legacy(
                pair.fux.screen().mode(fux_vt::Mode::ApplicationCursor),
            ),
            &mut legacy,
        );
        let verdict = key_verdict(setup, key, mods, &ours, &theirs, &legacy);
        tally.judge(
            || format!("{} {:?} with {mods:?}", shown(setup.as_bytes()), key),
            &ours,
            &theirs,
            verdict,
        );
    }
    Ok(())
}

/// The recorded verdict a key's difference is, if it is one.
fn key_verdict(
    setup: &str,
    key: Key,
    mods: Modifiers,
    ours: &[u8],
    theirs: &[u8],
    legacy: &[u8],
) -> Option<&'static str> {
    let flags: u8 = setup
        .split("\x1b[>")
        .nth(1)
        .and_then(|rest| rest.strip_suffix('u'))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let other_keys = setup.contains("\x1b[>4;2m");
    let csi = |b: &[u8]| b.starts_with(b"\x1b[");
    let kitty_form = |b: &[u8]| csi(b) && b.ends_with(b"u") || b.starts_with(b"\x1b[27;");
    if key == Key::F(3) && ours.ends_with(b"R") && theirs.ends_with(b"~") && flags & 9 == 0 {
        return Some("f3");
    }
    if key == Key::Backspace && mods.ctrl && flags & 9 == 0 {
        return Some("ctrl-backspace");
    }
    if flags == 0 && !other_keys && kitty_form(theirs) && !kitty_form(ours) {
        return Some("legacy-extras");
    }
    if other_keys && ours.starts_with(b"\x1b[27;") != theirs.starts_with(b"\x1b[27;") {
        return Some("modify-other-keys");
    }
    if flags != 0 && flags & 9 == 0 && ours == legacy {
        return Some("kitty-flags-without-disambiguate");
    }
    if flags & 2 != 0 {
        let mut typeless = String::from_utf8_lossy(theirs)
            .replace(";1:1", "")
            .replace(":1", "");
        // With neither modifiers nor type, `CSI 1 X` is `CSI X`.
        if typeless.len() == 4
            && let Some(last) = typeless.strip_prefix("\x1b[1")
        {
            typeless = format!("\x1b[{last}");
        }
        if typeless.as_bytes() == ours {
            return Some("kitty-press-type");
        }
    }
    None
}

const BUTTONS: [MouseButton; 9] = [
    MouseButton::Left,
    MouseButton::Middle,
    MouseButton::Right,
    MouseButton::WheelUp,
    MouseButton::WheelDown,
    MouseButton::WheelLeft,
    MouseButton::WheelRight,
    MouseButton::Back,
    MouseButton::Forward,
];

fn ghostty_button(button: MouseButton) -> gmouse::Button {
    match button {
        MouseButton::Left => gmouse::Button::Left,
        MouseButton::Middle => gmouse::Button::Middle,
        MouseButton::Right => gmouse::Button::Right,
        MouseButton::WheelUp => gmouse::Button::Four,
        MouseButton::WheelDown => gmouse::Button::Five,
        MouseButton::WheelLeft => gmouse::Button::Six,
        MouseButton::WheelRight => gmouse::Button::Seven,
        MouseButton::Back => gmouse::Button::Eight,
        MouseButton::Forward => gmouse::Button::Nine,
    }
}

/// The mouse events compared: what a host makes, as ghostty's does (a
/// press and a release name their button; motion names the button held,
/// if any; the wheel is never released or held), at positions about each
/// encoding's limits, with every modifier.
fn mouse_events() -> Vec<MouseEvent> {
    let positions = [
        (0u16, 0u16),
        (4, 9),
        (93, 94),
        (94, 95),
        (95, 96),
        (221, 222),
        (222, 222),
        (222, 223),
        (223, 0),
        (229, 2014),
        (0, 2015),
        (5, 2099),
    ];
    let mut out = Vec::new();
    for (row, col) in positions {
        for mods in all_mods() {
            for button in BUTTONS {
                out.push(MouseEvent {
                    action: MouseAction::Press,
                    button: Some(button),
                    mods,
                    row,
                    col,
                });
                if !button.is_wheel() {
                    out.push(MouseEvent {
                        action: MouseAction::Release,
                        button: Some(button),
                        mods,
                        row,
                        col,
                    });
                    out.push(MouseEvent {
                        action: MouseAction::Motion,
                        button: Some(button),
                        mods,
                        row,
                        col,
                    });
                }
            }
            out.push(MouseEvent {
                action: MouseAction::Motion,
                button: None,
                mods,
                row,
                col,
            });
        }
    }
    out
}

fn compare_mouse(pair: &Pair, setup: &str, tally: &mut Tally) -> Result<(), String> {
    let utf8 = pair.fux.screen().mouse_protocol_encoding() == fux_vt::MouseProtocolEncoding::Utf8;
    for event in mouse_events() {
        let mut encoder = gmouse::Encoder::new().map_err(err("mouse encoder"))?;
        encoder
            .set_options_from_terminal(&pair.ghostty)
            .set_size(gmouse::EncoderSize {
                screen_width: u32::from(COLS).saturating_mul(CELL.0),
                screen_height: u32::from(ROWS).saturating_mul(CELL.1),
                cell_width: CELL.0,
                cell_height: CELL.1,
                padding_top: 0,
                padding_bottom: 0,
                padding_right: 0,
                padding_left: 0,
            })
            .set_any_button_pressed(event.button.is_some() && event.action == MouseAction::Motion)
            .set_track_last_cell(false);
        let mut g = gmouse::Event::new().map_err(err("mouse event"))?;
        let mut held = gkey::Mods::empty();
        for (on, bit) in [
            (event.mods.shift, gkey::Mods::SHIFT),
            (event.mods.alt, gkey::Mods::ALT),
            (event.mods.ctrl, gkey::Mods::CTRL),
        ] {
            if on {
                held |= bit;
            }
        }
        let px = |cell: u16, size: u32| {
            (u32::from(cell)
                .saturating_mul(size)
                .saturating_add(size / 2)) as f32
        };
        g.set_action(match event.action {
            MouseAction::Press => gmouse::Action::Press,
            MouseAction::Release => gmouse::Action::Release,
            MouseAction::Motion => gmouse::Action::Motion,
        })
        .set_button(event.button.map(ghostty_button))
        .set_mods(held)
        .set_position(gmouse::Position {
            x: px(event.col, CELL.0),
            y: px(event.row, CELL.1),
        });
        let mut ours = Vec::new();
        pair.fux.screen().encode_mouse(event, &mut ours);
        let mut theirs = Vec::new();
        encoder
            .encode_to_vec(&g, &mut theirs)
            .map_err(err("mouse encode"))?;
        // The button code ghostty wrote raw, read as ctlseqs has it.
        let verdict = if utf8 && ours.is_empty() && (event.col >= 2015 || event.row >= 2015) {
            Some("utf8-limit")
        } else {
            (utf8 && theirs.get(3).is_some_and(|&b| b >= 128) && ours.len() > theirs.len())
                .then_some("utf8-button")
        };
        tally.judge(
            || format!("{} {event:?}", shown(setup.as_bytes())),
            &ours,
            &theirs,
            verdict,
        );
    }
    Ok(())
}

fn compare_focus(pair: &Pair, setup: &str, tally: &mut Tally) -> Result<(), String> {
    let reporting = pair.ghostty.mode(Mode::FOCUS_EVENT).map_err(err("mode"))?;
    for (focused, event) in [(true, focus::Event::Gained), (false, focus::Event::Lost)] {
        let mut ours = Vec::new();
        pair.fux.screen().encode_focus(focused, &mut ours);
        let mut buf = [0u8; 16];
        let theirs = if reporting {
            let n = event.encode(&mut buf).map_err(err("focus encode"))?;
            buf.get(..n).unwrap_or_default().to_vec()
        } else {
            Vec::new()
        };
        tally.judge(
            || format!("{} focused {focused}", shown(setup.as_bytes())),
            &ours,
            &theirs,
            None,
        );
    }
    Ok(())
}

/// `framed` with every U+009B 201 ~ inside its frame removed, as often as
/// removing one makes another: what fux-vt does with them.
fn without_c1_ends(framed: &[u8]) -> Vec<u8> {
    const END: &[u8] = b"\xc2\x9b201~";
    let mut out: Vec<u8> = Vec::with_capacity(framed.len());
    for &b in framed {
        out.push(b);
        if b == b'~' && out.ends_with(END) && out.len() >= END.len().saturating_add(6) {
            out.truncate(out.len().saturating_sub(END.len()));
        }
    }
    out
}

/// The bytes ghostty's paste encoder makes a space of.
const STRIPPED: &[u8] = &[
    0x00, 0x08, 0x05, 0x04, 0x1b, 0x7f, 0x03, 0x1c, 0x15, 0x1a, 0x11, 0x13, 0x17, 0x16, 0x12, 0x0f,
];

const PASTE_PIECES: &[&str] = &[
    "a",
    "hello world",
    "é",
    "界",
    "🙂",
    "\n",
    "\r\n",
    "\t",
    "\x1b",
    "[",
    "2",
    "0",
    "1",
    "~",
    "\x1b[201~",
    "\x1b[200~",
    "\u{9b}",
    "\x03",
    "\x7f",
    "\x00",
    "\x02",
    "\x15",
    "\x1a",
];

fn compare_pastes(
    pair: &Pair,
    setup: &str,
    tally: &mut Tally,
    rng: &mut crate::rng::Rng,
    cases: usize,
) -> Result<(), String> {
    let bracketed = pair
        .ghostty
        .mode(Mode::BRACKETED_PASTE)
        .map_err(err("mode"))?;
    let fixed = [
        "",
        "plain text",
        "line one\nline two",
        "a\x1b[201~b",
        "\x1b[20\x1b[201~1~",
        "a\u{9b}201~b",
        "\u{9b}20\u{9b}201~1~x",
    ];
    let random = (0..cases).map(|_| {
        let mut text = String::new();
        for _ in 0..rng.below(12) {
            if let Some(piece) = PASTE_PIECES.get(rng.below(PASTE_PIECES.len())) {
                text.push_str(piece);
            }
        }
        text
    });
    let texts: Vec<String> = fixed
        .iter()
        .map(|t| (*t).to_owned())
        .chain(random)
        .collect();
    for text in texts {
        let mut ours = Vec::new();
        pair.fux.screen().encode_paste(&text, &mut ours);
        let mut data = text.clone().into_bytes();
        let mut buf = vec![0u8; data.len().saturating_add(16)];
        let n = paste::encode(&mut data, bracketed, &mut buf).map_err(err("paste encode"))?;
        let theirs = buf.get(..n).unwrap_or_default().to_vec();
        // ghostty's paste as fux-vt would make it of the text ghostty sees.
        let mut seen: Vec<u8> = text
            .bytes()
            .map(|b| if STRIPPED.contains(&b) { b' ' } else { b })
            .collect();
        let controls = seen.as_slice() != text.as_bytes();
        let newline = !bracketed && seen.contains(&b'\n');
        if newline {
            for b in &mut seen {
                if *b == b'\n' {
                    *b = b'\r';
                }
            }
        }
        let modelled = {
            let mut out = Vec::new();
            fux_vt::keys::encode::paste(&String::from_utf8_lossy(&seen), bracketed, &mut out);
            out
        };
        let verdict = if modelled == theirs {
            if controls {
                Some("paste-controls")
            } else if newline {
                Some("paste-newline")
            } else {
                None
            }
        } else if bracketed && without_c1_ends(&theirs) == modelled {
            Some("paste-c1-end")
        } else {
            None
        };
        tally.judge(
            || format!("{} {text:?}", shown(setup.as_bytes())),
            &ours,
            &theirs,
            verdict,
        );
    }
    Ok(())
}
