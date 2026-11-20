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
    ".shell" "C-b/C-b" "C-b/a/.about" "C-b/a/k" "C-b/a/n" "C-b/b" "C-b/c" "C-b/d" "C-b/g/.about" "C-b/g/c" "C-b/g/g"
    "C-b/g/m" "C-b/g/r" "C-b/g/t" "C-b/h" "C-b/j" "C-b/k" "C-b/l" "C-b/m/.about" "C-b/m/.repeat" "C-b/m/h" "C-b/m/j"
    "C-b/m/k" "C-b/m/l" "C-b/n" "C-b/o" "C-b/r/.about" "C-b/r/.repeat" "C-b/r/h" "C-b/r/j" "C-b/r/k" "C-b/r/l"
    "C-b/s" "C-b/t/.about" "C-b/t/h" "C-b/t/l" "C-b/t/n" "C-b/v" "C-b/w/.about" "C-b/w/h" "C-b/w/l" "C-b/w/n"
    "C-b/x/.about" "C-b/x/y"
);

/// The key at the start of `bytes`: how many bytes it is and its name. The
/// name is empty for a sequence that is no key we know (it is passed on as
/// it is). `None` when more bytes are needed to tell; with `whole`, no more
/// are coming, and what is there is taken as it stands. With `answers`, the
/// terminal was asked something it answers in a string, and such a string is read as one.
pub fn next(bytes: &[u8], whole: bool, answers: bool) -> Option<(usize, String)> {
    let first = *bytes.first()?;
    if first == 0x1b {
        return match bytes.get(1) {
            None => whole.then(|| (1, "esc".into())),
            Some(b'[') => {
                // A sequence not yet whole waits for its end, however long that takes to come.
                let Some(end) = bytes.iter().skip(2).position(|b| (0x40..=0x7e).contains(b)) else {
                    // Alt-[ alone, with nothing after it for a moment, is that key; more than that waits.
                    let alone = (whole && bytes.len() == 2).then(|| (2, "M-[".to_string()));
                    return alone.or((bytes.len() > 64).then(|| (bytes.len(), String::new())));
                };
                let parameters = String::from_utf8_lossy(bytes.get(2..end + 2)?).into_owned();
                Some((end + 3, sequence(&parameters, *bytes.get(end + 2)?)))
            }
            // The terminal's answer to a question asked in a string (what a colour is): not a
            // key, but ours to read. It ends with BEL or ESC `\\`.
            Some(b']') if answers => {
                let bel = bytes.iter().position(|b| *b == 7).map(|at| (at, at + 1));
                let st = bytes.windows(2).skip(1).position(|pair| pair == b"\x1b\\").map(|at| (at + 1, at + 3));
                let end = bel.into_iter().chain(st).min();
                let Some((end, length)) = end else {
                    let alone = (whole && bytes.len() == 2).then(|| (2, "M-]".to_string()));
                    return alone.or((bytes.len() > 256).then(|| (bytes.len(), String::new())));
                };
                Some((length, format!("answer:{}", String::from_utf8_lossy(bytes.get(2..end)?))))
            }
            // The short forms of the arrows, Home, End and F1 to F4. Others (the keypad's) are not keys of ours.
            Some(b'O') => match bytes.get(2) {
                None => whole.then(|| (2, "M-S-o".into())),
                Some(last @ (b'A'..=b'D' | b'H' | b'F' | b'P'..=b'S')) => Some((3, sequence("", *last))),
                Some(_) => Some((3, String::new())),
            },
            Some(_) => {
                let (length, name) = next(bytes.get(1..)?, true, false)?;
                Some((length + 1, if name.is_empty() { name } else { format!("M-{name}") }))
            }
        };
    }
    let length = match first {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    };
    let Some(text) = bytes.get(..length) else { return whole.then(|| (bytes.len(), String::new())) };
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
        b'A'..=b'Z' => format!("S-{}", char::from(first + 0x20)),
        _ => String::from_utf8(text.to_vec()).unwrap_or_default(),
    };
    Some((length, name))
}

/// The name of an escape sequence with these parameters and last byte.
fn sequence(parameters: &str, last: u8) -> String {
    let mut numbers = parameters.split(';');
    let first = numbers.next().unwrap_or_default();
    // (In the kitty protocol's forms a part may go on after a colon: the key's other faces, the
    // kind of event. The key and what is held come first.)
    let held = numbers.next().and_then(|n| n.split(':').next()?.parse::<u16>().ok()).unwrap_or(1).saturating_sub(1);
    // Caps Lock and Num Lock (64, 128) are no part of a key's name.
    let mut modifier = (held & 7) as u8;
    let f = |n: u8| format!("f{n}");
    let key = match (last, first.parse::<u8>().ok()) {
        // A mouse report (always asked for in this form): not a key either.
        (b'M' | b'm', _) if first.starts_with('<') => {
            return format!("mouse:{}{}", parameters.trim_start_matches('<'), char::from(last));
        }
        // The terminal's answer to "what are you": not a key, but ours to read.
        (b'c', _) if first.starts_with('?') => return format!("terminal:{parameters}"),
        // Its answer to "which keyboard flags are in force" (the kitty protocol).
        (b'u', _) if first.starts_with('?') => return format!("keyboard:{}", first.trim_start_matches('?')),
        // A key in that protocol's form: its number is the character it gives with nothing held.
        (b'u', _) => match first.split(':').next().and_then(|n| n.parse().ok()).and_then(char::from_u32) {
            Some('\r') => "enter".into(),
            Some('\t') => "tab".into(),
            Some('\u{1b}') => "esc".into(),
            Some('\u{7f}') => "backspace".into(),
            Some(' ') => "space".into(),
            Some('/') => "slash".into(),
            Some('.') => "dot".into(),
            // (The keys with no character, the keypad's and the modifiers themselves, are numbered from U+E000.)
            Some(c) if !c.is_control() && !('\u{e000}'..='\u{f8ff}').contains(&c) => c.to_string(),
            _ => return String::new(),
        },
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
        (b'~', Some(200)) => return "paste-start".into(),
        (b'~', Some(201)) => return "paste-end".into(),
        _ => return String::new(),
    };
    // With Super, Hyper or Meta held it is no key of ours.
    if held & 0x38 != 0 {
        return String::new();
    }
    let held = [(4, "C-"), (2, "M-"), (1, "S-")].iter().filter(|(bit, _)| modifier & bit != 0).map(|(_, name)| *name);
    held.chain([key.as_str()]).collect()
}
