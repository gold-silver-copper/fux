//! Keys: the bytes a terminal sends, cut into keys with names, and the
//! default key scripts. A key's name is the name of the file that is run for
//! it: `C-b` (a directory: a menu), `v`, `up`, `S-h`, `M-left`, `f5`.

/// The scripts written to the keys directory when it does not exist yet.
macro_rules! scripts {
    ($($name:literal)*) => { &[$(($name, include_str!(concat!("../keys/", $name)))),*] };
}
pub const DEFAULT: &[(&str, &str)] = scripts!(
    "C-b/c" "C-b/v" "C-b/s" "C-b/x" "C-b/h" "C-b/j" "C-b/k" "C-b/l" "C-b/left" "C-b/down" "C-b/up" "C-b/right"
    "C-b/o" "C-b/n" "C-b/p" "C-b/1" "C-b/2" "C-b/3" "C-b/4" "C-b/5" "C-b/6" "C-b/7" "C-b/8" "C-b/9" "C-b/z"
    "C-b/!" "C-b/{" "C-b/}" "C-b/r" "C-b/S-h" "C-b/S-j" "C-b/S-k" "C-b/S-l" "C-b/a/r" "C-b/a/c" "C-b/a/g"
    "C-b/a/m" "C-b/d" "C-b/[" "C-b/(" "C-b/)" "C-b/w" "C-b/C-b"
);

/// The key at the start of `bytes`: how many bytes it is and its name. The
/// name is empty for a sequence that is no key we know (it is passed on as
/// it is). `None` when more bytes are needed to tell; with `whole`, no more
/// are coming, and what is there is taken as it stands.
pub fn next(bytes: &[u8], whole: bool) -> Option<(usize, String)> {
    let first = *bytes.first()?;
    if first == 0x1b {
        return match bytes.get(1) {
            None => whole.then(|| (1, "esc".into())),
            Some(b'[') => {
                let Some(end) = bytes.iter().skip(2).position(|b| (0x40..=0x7e).contains(b)) else {
                    return (whole || bytes.len() > 64).then(|| (bytes.len(), String::new()));
                };
                let parameters = String::from_utf8_lossy(bytes.get(2..end + 2)?).into_owned();
                Some((end + 3, sequence(&parameters, *bytes.get(end + 2)?)))
            }
            Some(b'O') => match bytes.get(2) {
                None => whole.then(|| (2, "M-S-o".into())),
                Some(last) => Some((3, sequence("", if (b'P'..=b'S').contains(last) { last + 32 } else { *last }))),
            },
            Some(_) => {
                let (length, name) = next(bytes.get(1..)?, true)?;
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
    let mut modifier = numbers.next().and_then(|n| n.parse::<u8>().ok()).unwrap_or(1).saturating_sub(1);
    let key = match (last, first) {
        (b'A', _) => "up",
        (b'B', _) => "down",
        (b'C', _) => "right",
        (b'D', _) => "left",
        (b'H', _) | (b'~', "1" | "7") => "home",
        (b'F', _) | (b'~', "4" | "8") => "end",
        (b'Z', _) => {
            modifier |= 1;
            "tab"
        }
        (b'~', "2") => "insert",
        (b'~', "3") => "delete",
        (b'~', "5") => "pgup",
        (b'~', "6") => "pgdn",
        (b'~', "200") => "paste-start",
        (b'~', "201") => "paste-end",
        (b'p'..=b's', "") => return format!("f{}", last - b'o'),
        (b'~', number) => {
            let f = match number.parse::<u8>() {
                Ok(n @ 11..=15) => n - 10,
                Ok(n @ 17..=21) => n - 11,
                Ok(n @ 23..=24) => n - 12,
                _ => return String::new(),
            };
            return format!("f{f}");
        }
        // The terminal's answer to "what are you": not a key, but ours to read.
        (b'c', answer) if answer.starts_with('?') => return format!("terminal:{parameters}"),
        _ => return String::new(),
    };
    let held = [(4, "C-"), (2, "M-"), (1, "S-")].iter().filter(|(bit, _)| modifier & bit != 0).map(|(_, name)| *name);
    held.chain([key]).collect()
}
