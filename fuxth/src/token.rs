//! Tokens: the words of a program, each with where it came from. A file is
//! split as a shell splits a line, quotes and all; command-line arguments
//! arrive split already, and stay one token each.
use crate::error::{Code, Error};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

/// Where a token came from: a line of a file, or an argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// A line of a source, from 1.
    Line { source: Arc<str>, line: u32 },
    /// A command-line argument, from 1.
    Argument(u32),
}

impl std::fmt::Display for Place {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Line { source, line } => write!(f, "{source}:{line}"),
            Self::Argument(n) => write!(f, "argument {n}"),
        }
    }
}

/// One word of a program, as written, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub text: Rc<str>,
    pub place: Place,
}

/// Command-line arguments, each one token, never split again.
pub fn arguments<S: AsRef<str>>(args: &[S]) -> Vec<Token> {
    (args.iter().zip(1u32..))
        .map(|(arg, n)| Token {
            text: Rc::from(arg.as_ref()),
            place: Place::Argument(n),
        })
        .collect()
}

/// The tokens of `text`, a file named `source`, split as a shell splits a
/// line: whitespace between tokens; `'…'` as it is; `"…"` with `\"` and
/// `\\` its only escapes; a backslash elsewhere taking the next character
/// as it is; parts side by side one token (`a'b c'`); `''` an empty token;
/// `#` at a token's start a comment to the end of its line. A quote may
/// span lines; one left open is an error at the line it opened on.
pub fn source(source: &str, text: &str) -> Result<Vec<Token>, Error> {
    Ok(spans(source, text)?
        .into_iter()
        .map(|(token, _)| token)
        .collect())
}

/// `source`'s tokens, each with the bytes of `text` it is written in:
/// between two tokens is only whitespace and comments.
pub(crate) fn spans(source: &str, text: &str) -> Result<Vec<(Token, Range<usize>)>, Error> {
    spans_limited(source, text, usize::MAX)
}

fn spans_limited(
    source: &str,
    text: &str,
    limit: usize,
) -> Result<Vec<(Token, Range<usize>)>, Error> {
    let source: Arc<str> = Arc::from(source);
    let place = |line: u32| Place::Line {
        source: Arc::clone(&source),
        line,
    };
    let mut tokens = Vec::new();
    let mut push = |word: Option<(String, u32, usize)>, to| {
        if let Some((text, at, from)) = word {
            let token = Token {
                text: Rc::from(text),
                place: place(at),
            };
            if tokens.len() >= limit {
                return Err(Error::new(
                    Code::TooLong,
                    "source token limit exceeded",
                    Some(&token),
                ));
            }
            tokens.push((token, from..to));
        }
        Ok(())
    };
    let mut chars = text.char_indices().peekable();
    let mut line = 1u32;
    // The token being read, the line it began on and where; `None` between
    // tokens.
    let mut word: Option<(String, u32, usize)> = None;
    let next_line = |line: u32| line.saturating_add(1);
    while let Some((at, c)) = chars.next() {
        match c {
            '\n' | ' ' | '\t' | '\r' => {
                push(word.take(), at)?;
                if c == '\n' {
                    line = next_line(line);
                }
            }
            '#' if word.is_none() => while chars.next_if(|(_, c)| *c != '\n').is_some() {},
            quote @ ('\'' | '"') => {
                let (text, ..) = word.get_or_insert_with(|| (String::new(), line, at));
                let opened = line;
                loop {
                    match chars.next() {
                        Some((_, c)) if c == quote => break,
                        Some((_, '\\'))
                            if quote == '"' && matches!(chars.peek(), Some((_, '"' | '\\'))) =>
                        {
                            text.extend(chars.next().map(|(_, c)| c));
                        }
                        Some((_, c)) => {
                            if c == '\n' {
                                line = next_line(line);
                            }
                            text.push(c);
                        }
                        None => return Err(open_quote(place(opened), quote)),
                    }
                }
            }
            // A backslash and newline join lines, as in a shell: neither is
            // part of a word, nor starts one.
            '\\' if matches!(chars.peek(), Some((_, '\n'))) => {
                chars.next();
                line = next_line(line);
            }
            '\\' => {
                let (text, ..) = word.get_or_insert_with(|| (String::new(), line, at));
                text.push(chars.next().map_or('\\', |(_, c)| c));
            }
            c => {
                let (text, ..) = word.get_or_insert_with(|| (String::new(), line, at));
                text.push(c);
            }
        }
    }
    push(word, text.len())?;
    Ok(tokens)
}

fn open_quote(place: Place, quote: char) -> Error {
    Error {
        code: Code::OpenQuote,
        message: format!("a {quote} opened here is never closed"),
        place: Some(place),
        word: None,
    }
}

/// Tokenizes while retaining at most `limit` tokens.
pub fn source_limited(source: &str, text: &str, limit: usize) -> Result<Vec<Token>, Error> {
    Ok(spans_limited(source, text, limit)?
        .into_iter()
        .map(|(token, _)| token)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(text: &str) -> Result<Vec<String>, Error> {
        Ok(source("t", text)?
            .into_iter()
            .map(|t| t.text.to_string())
            .collect())
    }

    #[test]
    fn a_file_splits_as_a_shell_line_does() -> Result<(), Error> {
        assert_eq!(
            texts("str 'hello world' \"say \\\"hi\\\"\" a'b c'd x\\ y ''")?,
            ["str", "hello world", "say \"hi\"", "ab cd", "x y", ""]
        );
        assert_eq!(texts("dup # a comment\nx#y '#z'")?, ["dup", "x#y", "#z"]);
        assert_eq!(texts("Dup DUP")?, ["Dup", "DUP"], "nothing is lowercased");
        Ok(())
    }

    #[test]
    fn tokens_know_their_lines_and_a_quote_left_open_says_where() -> Result<(), Error> {
        let tokens = source("f", "a\n'b\nc' d\n")?;
        let lines: Vec<String> = tokens.iter().map(|t| t.place.to_string()).collect();
        assert_eq!(lines, ["f:1", "f:2", "f:3"]);
        let open = source("f", "a\n'b\nc d");
        assert!(matches!(
            open,
            Err(Error {
                code: Code::OpenQuote,
                place: Some(Place::Line { line: 2, .. }),
                ..
            })
        ));
        Ok(())
    }

    /// In double quotes, a backslash escapes only `"` and itself; and a
    /// newline inside them counts toward the next token's line.
    #[test]
    fn double_quotes_escape_little_and_count_lines() -> Result<(), Error> {
        assert_eq!(texts(r#""a\"b" "c\\d" "e\x""#)?, [r#"a"b"#, r"c\d", r"e\x"]);
        let tokens = source("t", "\"one\ntwo\" three")?;
        let lines: Vec<String> = tokens.iter().map(|t| t.place.to_string()).collect();
        assert_eq!(lines, ["t:1", "t:2"]);
        Ok(())
    }

    #[test]
    fn the_limit_counts_emitted_tokens() -> Result<(), Error> {
        for text in [
            "1 2",
            "1 2 # comment\n",
            "1 2 \\\n # comment",
            "'one two' ''",
        ] {
            assert_eq!(source_limited("t", text, 2)?.len(), 2);
        }
        for text in ["1\n2", "1\n2 ", "1\n'2 3'"] {
            let error = source_limited("t", text, 1).err();
            assert!(matches!(
                error,
                Some(Error {
                    code: Code::TooLong,
                    place: Some(Place::Line { line: 2, .. }),
                    ..
                })
            ));
        }
        assert!(source_limited("t", "''", 0).is_err());
        Ok(())
    }

    #[test]
    fn arguments_stay_whole() {
        let tokens = arguments(&["str", "hello world", "# not a comment"]);
        let texts: Vec<&str> = tokens.iter().map(|t| &*t.text).collect();
        assert_eq!(texts, ["str", "hello world", "# not a comment"]);
        assert_eq!(
            tokens.last().map(|t| t.place.to_string()),
            Some("argument 3".into())
        );
    }
}
