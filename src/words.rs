//! Splitting a line into words as a shell does, for the `command` word and
//! the `shell` setting; and quoting words back for display.

/// `names` in a sentence: `a, b and c`, `last` (`and`, `or`) before the
/// last one.
pub fn listed(names: &[&str], last: &str) -> String {
    match names.split_last() {
        Some((end, [])) => (*end).to_owned(),
        Some((end, rest)) => format!("{} {last} {end}", rest.join(", ")),
        None => String::new(),
    }
}

/// Splits `line` like a shell: whitespace separates words; `'…'` is literal;
/// `"…"` is literal except that a backslash escapes `"`, `\`, `$` and a
/// backquote; outside quotes a backslash escapes any character; and a `#` at
/// the start of a word begins a comment that runs to the end of the line.
/// A line that does not split says why.
pub fn split(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    // Whether a word has started, so that `''` is an empty word, not none.
    let mut started = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            if started {
                words.push(std::mem::take(&mut word));
                started = false;
            }
            continue;
        }
        if c == '#' && !started {
            break;
        }
        started = true;
        match c {
            '\'' => loop {
                match chars.next() {
                    Some('\'') => break,
                    Some(c) => word.push(c),
                    None => return Err(unterminated('\'')),
                }
            },
            '"' => loop {
                match chars.next() {
                    Some('"') => break,
                    Some('\\') => match chars.next() {
                        Some(c @ ('"' | '\\' | '$' | '`')) => word.push(c),
                        Some(c) => {
                            word.push('\\');
                            word.push(c);
                        }
                        None => return Err(unterminated('"')),
                    },
                    Some(c) => word.push(c),
                    None => return Err(unterminated('"')),
                }
            },
            '\\' => match chars.next() {
                Some(c) => word.push(c),
                None => return Err("a backslash at the end of the line escapes nothing".to_owned()),
            },
            c => word.push(c),
        }
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

/// Whether a word needs no quoting for `split` or a shell. A leading `=`
/// does: zsh expands `=ls` to the path of `ls` (its EQUALS option), though
/// `a=b` stays as it is.
fn bare(word: &str) -> bool {
    !word.is_empty()
        && !word.starts_with('=')
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c))
}

fn unterminated(quote: char) -> String {
    format!("unterminated {quote} quote")
}

/// Quotes one word so that `split`, a shell and fuxth read it back unchanged.
pub fn quote(word: &str) -> String {
    if bare(word) {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        split(line).unwrap_or_else(|e| vec![format!("ERROR {e}")])
    }

    #[test]
    fn errors_say_what_is_wrong() {
        for (line, message) in [
            ("'open", "unterminated ' quote"),
            ("a \"open", "unterminated \" quote"),
            (
                "end\\",
                "a backslash at the end of the line escapes nothing",
            ),
        ] {
            assert_eq!(split(line), Err(message.to_owned()), "{line}");
        }
    }

    #[test]
    fn splits_like_a_shell() {
        assert_eq!(words("  bind h  split -h "), ["bind", "h", "split", "-h"]);
        assert_eq!(
            words("set shell '/bin/zsh -l'"),
            ["set", "shell", "/bin/zsh -l"]
        );
        assert_eq!(words(r#"a "b \"c\" \\ \x" d"#), ["a", r#"b "c" \ \x"#, "d"]);
        assert_eq!(words(r"a\ b c\#d"), ["a b", "c#d"]);
        assert_eq!(words("a '' b"), ["a", "", "b"]);
        assert_eq!(words("x # a comment 'unterminated"), ["x"]);
        assert_eq!(words("a#b"), ["a#b"]);
        assert_eq!(words("# only"), Vec::<String>::new());
        assert_eq!(words("it'''s'"), ["its"]);
    }

    #[test]
    fn quoting_round_trips() {
        for word in [
            "plain",
            "",
            "two words",
            "it's",
            "a\"b",
            "back\\slash",
            "#x",
            "tab\tx",
        ] {
            let quoted = quote(word);
            assert_eq!(split(&quoted).ok(), Some(vec![word.to_owned()]), "{quoted}");
        }
        let argv: Vec<String> = ["grep", "a b", "it's"].map(String::from).to_vec();
        let joined: Vec<String> = argv.iter().map(|w| quote(w)).collect();
        assert_eq!(split(&joined.join(" ")).ok(), Some(argv));
    }
}
