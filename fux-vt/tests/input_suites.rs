//! Other projects' input suites, ported as data in `tests/data/keys/`:
//! each case is what that suite expects for a key (an encoder suite) or for
//! bytes a terminal sent (a decoder suite), and a verdict. `agree`: fux-vt
//! gives what the suite expects. `differs: REASON; fux: WHAT`: it gives
//! WHAT instead, for the reason named (an authority, or a choice the
//! README records), and still must, so a change shows as a failure here.
//! `na: REASON`: the case tests what fux-vt has no part in, and is not
//! run.
//!
//! The encoder cases name a mode (`legacy`, with `app` for DECCKM,
//! `keypad` for DECKPAM, `bkm` for DECBKM and `mok=N` for modifyOtherKeys
//! level N; or `kitty=FLAGS`) and a key: a key name as fux reads them
//! (`C-M-Up`), and for a terminal speaking the kitty protocol `code=N`,
//! `mods=BITS` (Shift 1, Alt 2, Ctrl 4, Super 8, Hyper 16, Meta 32, Caps
//! Lock 64, Num Lock 128), `shifted=N` and `base=N`. Bytes are written with
//! `\e` for ESC, `\xHH` for a byte, `\\` for a backslash.

use fux_vt::keys::decode::{Decoder, Input};
use fux_vt::keys::encode::{KeyMode, OtherKeys, OtherKeysLevel, Report, factored_mask, key_bytes};
use fux_vt::keys::{KeyPress, Keystroke, Kitty};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Bytes as the data writes them, back.
fn written(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        match b {
            0x1b => out.push_str("\\e"),
            b'\\' => out.push_str("\\\\"),
            0x21..=0x7e | b' ' => out.push(char::from(b)),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

fn mode(spec: &str) -> Result<KeyMode> {
    let words: Vec<&str> = spec.split(' ').collect();
    let has = |w: &str| words.contains(&w);
    let backarrow = has("bkm");
    if let Some(flags) = words.iter().find_map(|w| w.strip_prefix("kitty=")) {
        let flags: u8 = flags.parse()?;
        let report = match (flags & 8 != 0, flags & 16 != 0) {
            (true, true) => Report::AllKeysWithText,
            (true, false) => Report::AllKeys,
            (false, _) => Report::Disambiguated,
        };
        return Ok(KeyMode::Kitty {
            alternate: flags & 4 != 0,
            report,
            backarrow,
        });
    }
    let level = match words.iter().find_map(|w| w.strip_prefix("mok=")) {
        Some("1") => Some(OtherKeysLevel::Usual),
        Some("2") => Some(OtherKeysLevel::All),
        Some("3") => Some(OtherKeysLevel::Unmodified),
        Some(level) => return Err(format!("no modifyOtherKeys level {level}").into()),
        None => None,
    };
    // `factor=N`: XTMODKEYS's mask of modifiers to factor out.
    let mask = match words.iter().find_map(|w| w.strip_prefix("factor=")) {
        Some(mask) => mask.parse()?,
        None => 0,
    };
    let other_keys = level.map(|level| OtherKeys::new(level, factored_mask(mask)));
    Ok(KeyMode::Legacy {
        application: has("app"),
        keypad: has("keypad"),
        other_keys,
        backarrow,
    })
}

fn keystroke(spec: &str) -> Result<Keystroke> {
    let mut words = spec.split(' ');
    let press: KeyPress = words.next().ok_or("no key")?.parse()?;
    let mut kitty: Option<Kitty> = None;
    for word in words {
        let (name, value) = word.split_once('=').ok_or("a word of name=value")?;
        let value: u32 = value.parse()?;
        let k = kitty.get_or_insert_with(Kitty::default);
        match name {
            "code" => k.code = Some(value),
            "mods" => k.mods = u8::try_from(value)?,
            "shifted" => k.shifted = Some(value),
            "base" => k.base = Some(value),
            _ => return Err(format!("no key field {name}").into()),
        }
    }
    Ok(Keystroke { press, kitty })
}

/// Runs an encoder suite: every case not `na` gives what its verdict says.
fn encoder_suite(name: &str, text: &str) -> Result<(usize, usize, usize)> {
    let (mut agree, mut differ, mut na) = (0usize, 0usize, 0usize);
    let mut bad = Vec::new();
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let fields: Vec<&str> = line.split('\t').collect();
        let [source, mode_spec, key, expected, verdict] = fields.as_slice() else {
            return Err(format!("{name}: not five fields: {line}").into());
        };
        if verdict.starts_with("na: ") {
            na = na.saturating_add(1);
            continue;
        }
        let mut out = Vec::new();
        key_bytes(keystroke(key)?, mode(mode_spec)?, &mut out);
        // `none`: no bytes.
        let got = if out.is_empty() {
            "none".to_string()
        } else {
            written(&out)
        };
        let want = match verdict.strip_prefix("differs: ") {
            Some(rest) => {
                differ = differ.saturating_add(1);
                rest.rsplit_once("; fux: ")
                    .ok_or("a difference with no fux: bytes")?
                    .1
            }
            None if *verdict == "agree" => {
                agree = agree.saturating_add(1);
                expected
            }
            None => return Err(format!("{name}: verdict {verdict:?}").into()),
        };
        if got != *want {
            bad.push(format!(
                "{source}\t{mode_spec}\t{key}\t{expected}\t{verdict}\tgot {got}"
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{name}: {} cases:\n{}",
        bad.len(),
        bad.join("\n")
    );
    Ok((agree, differ, na))
}

/// tmux's `regress/input-keys.sh`.
#[test]
fn tmux_input_keys() -> Result {
    let counts = encoder_suite("tmux", include_str!("data/keys/tmux-input-keys.txt"))?;
    // Agree, differ as recorded, not applicable.
    assert_eq!(counts, (473, 4, 30));
    Ok(())
}

/// Bytes as the data writes them.
fn bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = text.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        rest = tail;
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match rest.split_first() {
            Some((b'e', tail)) => {
                out.push(0x1b);
                rest = tail;
            }
            Some((b'x', tail)) => {
                let hex = tail.get(..2).and_then(|h| std::str::from_utf8(h).ok());
                out.push(
                    hex.and_then(|h| u8::from_str_radix(h, 16).ok())
                        .unwrap_or(0),
                );
                rest = tail.get(2..).unwrap_or_default();
            }
            Some((b'\\', tail)) => {
                out.push(b'\\');
                rest = tail;
            }
            _ => out.push(b'\\'),
        }
    }
    out
}

/// Ghostty's `src/input/key_encode.zig`.
#[test]
fn ghostty_key_encode() -> Result {
    let counts = encoder_suite("Ghostty", include_str!("data/keys/ghostty-key-encode.txt"))?;
    // Agree, differ as recorded, not applicable.
    assert_eq!(counts, (48, 41, 43));
    Ok(())
}

/// kitty's `kitty_tests/keys.py`, re-authored.
#[test]
fn kitty_keys() -> Result {
    let counts = encoder_suite("kitty", include_str!("data/keys/kitty-keys.txt"))?;
    // Agree, differ as recorded, not applicable.
    assert_eq!(counts, (554, 12, 18));
    Ok(())
}

/// Real xterm 411's bytes for every key fux names, through XTest.
#[test]
fn xterm_keys() -> Result {
    let counts = encoder_suite("xterm", include_str!("data/keys/xterm-keys.txt"))?;
    // Agree, differ as recorded, not applicable.
    assert_eq!(counts, (2312, 0, 112));
    Ok(())
}

/// Real xterm 411's bytes with modifyOtherKeys's mask of modifiers to factor
/// out (`CSI > 4 ; level : mask m`), recorded through XTest as
/// `xterm_keys`'s are: a key whose only modifier is factored out is sent as
/// with modifyOtherKeys off (at level 2 Ctrl making a control of `@` to DEL
/// alone; at level 3 `CSI 27 ; 1 ; code ~`), and with Shift factored out a
/// key escaped with more modifiers names its unshifted code.
#[test]
fn xterm_keys_factored() -> Result {
    let text = include_str!("data/keys/xterm-keys-factored.txt");
    let counts = encoder_suite("xterm factored", text)?;
    // Agree, differ as recorded, not applicable.
    assert_eq!(counts, (420, 0, 28));
    Ok(())
}

/// What a terminal's bytes decode to, as the decoder suites write it: each
/// input space-separated, a key by its name (`C-a`, `M-Up`, a character as
/// itself, or `U+XXXX` for one past ASCII), others by their kind.
fn decoded(bytes: &[u8]) -> String {
    let mut decoder = Decoder::default();
    let mut out = Vec::new();
    decoder.bytes(bytes, &mut out);
    decoder.timeout(&mut out);
    let shown: Vec<String> = out
        .into_iter()
        .map(|input| match input {
            Input::Key(stroke) => {
                let name = stroke.press.to_string();
                let wide = name.chars().last().filter(|c| !c.is_ascii());
                match wide {
                    Some(c) => format!("{}U+{:04X}", name.trim_end_matches(c), u32::from(c)),
                    None => name,
                }
            }
            Input::Paste(text) => format!("paste:{text}"),
            Input::PasteTooLong => "paste-too-long".to_string(),
            Input::FocusIn => "focus-in".to_string(),
            Input::FocusOut => "focus-out".to_string(),
            Input::Mouse(event) => {
                let held = KeyPress::new(fux_vt::keys::Key::Char('x'), event.mods).to_string();
                let mods = held.trim_end_matches('x');
                format!("mouse:{mods}{:?}:{},{}", event.action, event.col, event.row)
            }
            Input::Reply(reply) => format!("reply:{reply:?}"),
        })
        .collect();
    if shown.is_empty() {
        "none".to_string()
    } else {
        shown.join(" ")
    }
}

/// Runs a decoder suite: source, bytes, what the suite expects, verdict.
fn decoder_suite(name: &str, text: &str) -> Result<(usize, usize, usize)> {
    let (mut agree, mut differ, mut na) = (0usize, 0usize, 0usize);
    let mut bad = Vec::new();
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let fields: Vec<&str> = line.split('\t').collect();
        let [source, input, expected, verdict] = fields.as_slice() else {
            return Err(format!("{name}: not four fields: {line}").into());
        };
        if verdict.starts_with("na: ") {
            na = na.saturating_add(1);
            continue;
        }
        // A key name, which libtermkey's `strpkey` reads too.
        let got = match input.strip_prefix("name:") {
            Some(name) => name
                .parse::<KeyPress>()
                .map_or("none".to_string(), |k| k.to_string()),
            None => decoded(&bytes(input)),
        };
        let want = match verdict.strip_prefix("differs: ") {
            Some(rest) => {
                differ = differ.saturating_add(1);
                rest.rsplit_once("; fux: ")
                    .ok_or("a difference with no fux: inputs")?
                    .1
            }
            None if *verdict == "agree" => {
                agree = agree.saturating_add(1);
                expected
            }
            None => return Err(format!("{name}: verdict {verdict:?}").into()),
        };
        if got != *want {
            bad.push(format!(
                "{source}\t{input}\t{expected}\t{verdict}\tgot {got}"
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{name}: {} cases:\n{}",
        bad.len(),
        bad.join("\n")
    );
    Ok((agree, differ, na))
}

/// libtermkey 0.22's decoding cases.
#[test]
fn libtermkey_decoding() -> Result {
    let counts = decoder_suite("libtermkey", include_str!("data/keys/libtermkey.txt"))?;
    // Agree, differ as recorded, not applicable.
    assert_eq!(counts, (37, 7, 15));
    Ok(())
}
