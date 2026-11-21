//! Keys: the bytes a terminal sends, cut into keys with names, and the
//! default key scripts. A key's name is the name of the file that is run for
//! it: `C-b` (a directory: a menu), `v`, `up`, `S-h`, `M-left`, `f5`. In a
//! menu's directory, `.about` names it and `.repeat` keeps it open; a line
//! `# group: NAME` after a script's caption puts it under that heading.

/// The scripts written to the keys directory when it does not exist yet.
macro_rules! scripts {
    ($($name:literal)*) => { &[$(($name, include_str!(concat!("../keys/", $name)))),*] };
}
pub const DEFAULT: &[(&str, &str)] = scripts!(
    ".new-shell" ".shell" ".empty" ".pick" ".text" ".edit" ".search" ".restore" "C-b/f/s" "C-b/S-c" "C-b/M-c" "C-b/p" "C-b/f/.about" "C-b/f/p" "C-b/f/t" "C-b/f/w" "C-b/C-b" "C-b/a/.about" "C-b/a/k" "C-b/a/n" "C-b/b" "C-b/c" "C-b/d" "C-b/g/.about" "C-b/g/c" "C-b/g/g"
    "C-b/g/m" "C-b/g/r" "C-b/g/t" "C-b/h" "C-b/j" "C-b/k" "C-b/l" "C-b/m/.about" "C-b/m/.repeat" "C-b/m/h"
    "C-b/m/l" "C-b/n" "C-b/o" "C-b/r/.about" "C-b/r/.repeat" "C-b/r/h" "C-b/r/j" "C-b/r/k" "C-b/r/l"
    "C-b/s" "C-b/t/.about" "C-b/t/n" "C-b/v" "C-b/w/.about" "C-b/w/h" "C-b/w/l" "C-b/w/n"
    "C-b/x/.about" "C-b/x/y"
);

/// The key at the start of `bytes`: how many bytes it is, its name, and what more it says. The
/// name is empty for a sequence that is no key we know (it is passed on as it is). `None` when more bytes are
/// needed to tell; with `whole`, no more are coming, and what is there is taken as it stands, but for a character
/// not all of whose bytes have come: it waits for the next, which is its own or shows that it is none. With
/// `answers`, the terminal was asked something it answers in a string, and such a string is read as one.
pub fn next(bytes: &[u8], whole: bool, answers: bool) -> Option<(usize, String, Option<Event>)> {
    let first = *bytes.first()?;
    if first == 0x1b {
        return match bytes.get(1) {
            None => whole.then(|| (1, "esc".into(), None)),
            Some(b'[') => {
                // A sequence not yet whole waits for its end, however long that takes to come.
                let Some(end) = bytes.iter().skip(2).position(|b| (0x40..=0x7e).contains(b)) else {
                    // Alt-[ alone, with nothing after it for a moment, is that key; more than that waits, until it
                    // is too long to be a key (the kitty protocol's text of a key can be many numbers long).
                    let alone = (whole && bytes.len() == 2).then(|| (2, "M-[".to_string(), None));
                    return alone.or((bytes.len() > 1024).then(|| (bytes.len(), String::new(), None)));
                };
                let parameters = String::from_utf8_lossy(bytes.get(2..end + 2)?).into_owned();
                let (name, event) = sequence(&parameters, *bytes.get(end + 2)?);
                Some((end + 3, name, event))
            }
            // The terminal's answer to a question asked in a string (what a colour is): not a
            // key, but ours to read. It ends with BEL or ESC `\\`.
            Some(b']') if answers => {
                let bel = bytes.iter().position(|b| *b == 7).map(|at| (at, at + 1));
                let st = bytes.windows(2).skip(1).position(|pair| pair == b"\x1b\\").map(|at| (at + 1, at + 3));
                let end = bel.into_iter().chain(st).min();
                let Some((end, length)) = end else {
                    let alone = (whole && bytes.len() == 2).then(|| (2, "M-]".to_string(), None));
                    return alone.or((bytes.len() > 256).then(|| (bytes.len(), String::new(), None)));
                };
                Some((length, format!("answer:{}", String::from_utf8_lossy(bytes.get(2..end)?)), None))
            }
            // The short forms of the arrows, Home, End and F1 to F4. Others (the keypad's) are not keys of ours.
            Some(b'O') => match bytes.get(2) {
                None => whole.then(|| (2, "M-S-o".into(), None)),
                Some(last @ (b'A'..=b'D' | b'H' | b'F' | b'P'..=b'S')) => Some((3, sequence("", *last).0, None)),
                Some(_) => Some((3, String::new(), None)),
            },
            // (Of three ESC the first is Escape by itself: a run of them is never one key of a long name.)
            Some(0x1b) if bytes.get(2) == Some(&0x1b) => Some((1, "esc".into(), None)),
            // (With Alt a key is in the old form, of which no more than its name is told.)
            Some(_) => {
                let (length, name, _) = next(bytes.get(1..)?, whole, false)?;
                // (Held keys are named in one order, Ctrl, Alt, Shift, whatever form a key came in.)
                let named = match name.strip_prefix("C-") {
                    _ if name.is_empty() => name,
                    Some(rest) => format!("C-M-{rest}"),
                    None => format!("M-{name}"),
                };
                Some((length + 1, named, None))
            }
        };
    }
    let text = std::str::from_utf8(bytes.get(..(first.leading_ones() as usize).clamp(1, 4)).unwrap_or(bytes));
    let length = text.map_or_else(|none| none.error_len(), |text| Some(text.len()))?;
    let character = text.ok().and_then(|text| text.chars().next());
    let small = |c: char| c.to_lowercase().next().unwrap_or(c);
    let name = match first {
        0x0d => "enter".into(),
        0x09 => "tab".into(),
        0x7f => "backspace".into(),
        0x00 => "C-space".into(),
        b' ' => "space".into(),
        b'/' => "slash".into(),
        b'.' => "dot".into(),
        0x01..=0x1a => format!("C-{}", char::from(first + 0x60)),
        0x1c..=0x1f => format!("C-{}", char::from(first + 0x40)),
        // (A capital is Shift and the small letter, of any alphabet, as in the kitty protocol's form.)
        _ => character.map(|c| format!("{}{}", if c.is_uppercase() { "S-" } else { "" }, small(c))).unwrap_or_default(),
    };
    // A character by itself is a press of its key.
    Some((length, name, character.filter(|c| !c.is_control()).map(|c| ((u32::from(small(c)), b'u'), None, 1, false))))
}

/// A key as the kitty protocol tells of it: its number and its sequence's last byte.
pub type Key = (u32, u8);

/// What a key's sequence says beyond its name, in the kitty protocol's forms: which key it is
/// (its number and the sequence's last byte), the same for the sign it gives with Shift if that
/// is told, whether it is pressed (1), held and repeating (2) or let go (3), and whether it is a
/// modifier key by itself (Shift, Ctrl and their like, and the locks).
/// A character by itself is a press of its key (a terminal may send a letter's press so and its
/// release as a sequence: a sign is then known by its own number first and by its key's after).
/// None for other bytes: a key in the old form, of which no more is told.
pub type Event = (Key, Option<Key>, u8, bool);

/// The name of an escape sequence with these parameters and last byte, and what more it says.
fn sequence(parameters: &str, last: u8) -> (String, Option<Event>) {
    let mut numbers = parameters.split(';');
    let first = numbers.next().unwrap_or_default();
    // (In the kitty protocol's forms a part may go on after a colon: the key's other faces, the
    // kind of event. The key and what is held come first.)
    let mut faces = first.split(':').map(|n| n.parse::<u32>().ok());
    let (code, sign, standard) = (faces.next().flatten(), faces.next().flatten(), faces.next().flatten());
    let mut second = numbers.next().unwrap_or_default().split(':');
    let held = second.next().and_then(|n| n.parse::<u16>().ok()).unwrap_or(1).saturating_sub(1);
    let kind = second.next().and_then(|n| n.parse::<u8>().ok()).unwrap_or(1);
    let alone = last == b'u' && matches!(code, Some(57358..=57360 | 57441..=57454));
    let event = ((code.unwrap_or(1), last), sign.map(|sign| (sign, last)), kind, alone);
    // (Number 0 is text with no key: nothing that is pressed, held or let go.)
    let event = (code != Some(0) && matches!(last, b'u' | b'~' | b'A'..=b'H' | b'P'..=b'S')).then_some(event);
    // Caps Lock and Num Lock (64, 128) are no part of a key's name.
    let mut modifier = (held & 7) as u8;
    let f = |n: u8| format!("f{n}");
    let key = match (last, first.parse::<u8>().ok()) {
        // A mouse report (always asked for in this form): not a key either.
        (b'M' | b'm', _) if first.starts_with('<') => {
            return (format!("mouse:{}{}", parameters.trim_start_matches('<'), char::from(last)), None);
        }
        // The terminal's answer to "what are you": not a key, but ours to read.
        (b'c', _) if first.starts_with('?') => return (format!("terminal:{parameters}"), None),
        // The terminal's window gained or lost the keys (it was asked to say: mode 1004).
        (b'I' | b'O', _) if parameters.is_empty() => return (format!("focus:{}", char::from(last)), None),
        // Its answer to "is this mode set": `?MODE;STATE$y`.
        (b'y', _) if first.starts_with('?') && parameters.ends_with('$') => {
            return (format!("mode:{}", parameters.trim_start_matches('?').trim_end_matches('$')), None);
        }
        // Its answer to "which keyboard flags are in force" (the kitty protocol).
        (b'u', _) if first.starts_with('?') => return (format!("keyboard:{}", first.trim_start_matches('?')), None),
        // A key in that protocol's form: its number is the character it gives with nothing
        // held; after colons may come the character it gives with Shift, and the key of the
        // standard layout in its place.
        (b'u', _) => {
            let [plain, shifted, standard] = [code, sign, standard].map(|n| n.and_then(char::from_u32));
            // A key is named as it is in the old form: a sign that needs Shift by the sign and not by Shift
            // (`M-#`), a letter of another alphabet by the letter of ours it shares a key with (Ctrl with it
            // is the same Ctrl-b) and by itself where that key is no letter, a capital as Shift and the small letter.
            let sign = shifted.filter(|c| modifier & 1 != 0 && !c.is_alphabetic());
            modifier &= if sign.is_some() { !1 } else { !0 };
            let ours = |c: &char| c.is_alphabetic() && plain.is_some_and(|c| !c.is_ascii() && c.is_alphabetic());
            let key = sign.or(standard.filter(ours)).or(plain);
            modifier |= u8::from(key.is_some_and(|c| c.is_uppercase()));
            match key.map(|c| c.to_lowercase().next().unwrap_or(c)) {
                Some('\r') => "enter".into(),
                Some('\t') => "tab".into(),
                Some('\u{1b}') => "esc".into(),
                Some('\u{7f}') => "backspace".into(),
                Some(' ') => "space".into(),
                Some('/') => "slash".into(),
                Some('.') => "dot".into(),
                // The keypad's keys have numbers of their own (57399 to 57426): they are named as
                // the keys they double, as they are in the old form.
                Some(c @ '\u{e037}'..='\u{e052}') => {
                    let named = "0 1 2 3 4 5 6 7 8 9 dot slash * - + enter = , left right up down pgup pgdn home end insert delete";
                    named.split(' ').nth((u32::from(c) - 0xe037) as usize).unwrap_or_default().to_string()
                }
                // (The other keys with no character, the modifiers themselves among them, are numbered from U+E000.)
                Some(c) if !c.is_control() && !('\u{e000}'..='\u{f8ff}').contains(&c) => c.to_string(),
                _ => return (String::new(), event),
            }
        }
        (b'A', _) => "up".into(),
        (b'B', _) => "down".into(),
        (b'C', _) => "right".into(),
        (b'D', _) => "left".into(),
        (b'H', _) | (b'~', Some(1 | 7)) => "home".into(),
        (b'F', _) | (b'~', Some(4 | 8)) => "end".into(),
        (b'Z', _) => {
            modifier |= 1;
            "tab".into()
        }
        (b'P'..=b'S', _) => f(last - b'O'),
        (b'~', Some(2)) => "insert".into(),
        (b'~', Some(3)) => "delete".into(),
        (b'~', Some(5)) => "pgup".into(),
        (b'~', Some(6)) => "pgdn".into(),
        (b'~', Some(n @ 11..=15)) => f(n - 10),
        (b'~', Some(n @ 17..=21)) => f(n - 11),
        (b'~', Some(n @ 23..=24)) => f(n - 12),
        (b'~', Some(200)) => return ("paste-start".into(), None),
        (b'~', Some(201)) => return ("paste-end".into(), None),
        _ => return (String::new(), event),
    };
    // With Super, Hyper or Meta held it is no key of ours.
    if held & 0x38 != 0 {
        return (String::new(), event);
    }
    let held = [(4, "C-"), (2, "M-"), (1, "S-")].iter().filter(|(bit, _)| modifier & bit != 0).map(|(_, name)| *name);
    (held.chain([key.as_str()]).collect(), event)
}
