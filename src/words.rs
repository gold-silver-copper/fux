//! Splitting a line into words, the one grammar shared by the config file,
//! bindings and the command prompt; and quoting words back for display and for a
//! shell.

/// Splits `line` like a shell: whitespace separates words; `'…'` is literal;
/// `"…"` is literal except that a backslash escapes `"`, `\`, `$` and a
/// backquote; outside quotes a backslash escapes any character; and a `#` at
/// the start of a word begins a comment that runs to the end of the line.
pub fn split(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    // Whether a word has started, so that `''` is an empty word, not none.
    let mut started = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            '#' if !started => break,
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("unterminated ' quote".into()),
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return Err("unterminated \" quote".into()),
                        },
                        Some(c) => word.push(c),
                        None => return Err("unterminated \" quote".into()),
                    }
                }
            }
            '\\' => {
                started = true;
                match chars.next() {
                    Some(c) => word.push(c),
                    None => return Err("a backslash at the end of the line escapes nothing".into()),
                }
            }
            c => {
                started = true;
                word.push(c);
            }
        }
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

/// Whether a word needs no quoting for `split` or a shell.
fn bare(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c))
}

/// Quotes one word so that `split` reads it back unchanged.
pub fn quote(word: &str) -> String {
    if bare(word) {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// Words joined for display, each quoted as `split` would need.
pub fn join(words: &[String]) -> String {
    words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ")
}

/// A command line for a shell to read as typed input, from an argv. Each
/// argument is bare if it has only safe characters and otherwise in single
/// quotes, with each `'` written `'\''`: literal in sh, dash, bash and zsh.
/// fish also treats `\` inside single quotes as an escape, so for fish each
/// `\` is doubled there. A control character would act as a key in the
/// shell's line editor, so an argument with one is refused.
pub fn shell_line(argv: &[String], fish: bool) -> Result<String, String> {
    let mut words = Vec::new();
    for arg in argv {
        if let Some(c) = arg.chars().find(|c| c.is_control()) {
            return Err(format!(
                "the command argument {arg:?} contains the control character {:?}; \
                 it would act as a key in the shell",
                c
            ));
        }
        if bare(arg) {
            words.push(arg.clone());
        } else {
            let inner = if fish {
                arg.replace('\\', "\\\\")
            } else {
                arg.clone()
            };
            words.push(format!("'{}'", inner.replace('\'', "'\\''")));
        }
    }
    Ok(words.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        split(line).unwrap_or_else(|e| vec![format!("ERROR {e}")])
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
        assert!(split("'open").is_err());
        assert!(split("\"open").is_err());
        assert!(split("end\\").is_err());
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
        assert_eq!(split(&join(&argv)).ok(), Some(argv));
    }

    #[test]
    fn shell_lines_quote_for_posix_shells_and_fish() {
        let argv = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            shell_line(&argv(&["grep", "-r", "a b", "it's", "x/y.z"]), false).ok(),
            Some(r"grep -r 'a b' 'it'\''s' x/y.z".to_owned())
        );
        assert_eq!(
            shell_line(&argv(&["echo", r"a\b"]), false).ok(),
            Some(r"echo 'a\b'".to_owned())
        );
        assert_eq!(
            shell_line(&argv(&["echo", r"a\b"]), true).ok(),
            Some(r"echo 'a\\b'".to_owned())
        );
        for bad in ["a\nb", "tab\there", "esc\x1b"] {
            let error = shell_line(&argv(&["echo", bad]), false)
                .err()
                .unwrap_or_default();
            assert!(error.contains("control character"), "{bad:?}: {error}");
        }
    }
}
