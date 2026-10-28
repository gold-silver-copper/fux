//! Words in a sentence, and quoting words for display.

/// `names` in a sentence: `a, b and c`, `last` (`and`, `or`) before the
/// last one.
pub fn listed(names: &[&str], last: &str) -> String {
    match names.split_last() {
        Some((end, [])) => (*end).to_owned(),
        Some((end, rest)) => format!("{} {last} {end}", rest.join(", ")),
        None => String::new(),
    }
}

/// Whether `write` produces exactly `text`, without allocating.
pub(crate) fn writes_exactly(
    text: &str,
    write: impl FnOnce(&mut dyn std::fmt::Write) -> std::fmt::Result,
) -> bool {
    struct Rest<'a>(&'a str);
    impl std::fmt::Write for Rest<'_> {
        fn write_str(&mut self, part: &str) -> std::fmt::Result {
            self.0 = self.0.strip_prefix(part).ok_or(std::fmt::Error)?;
            Ok(())
        }
    }
    let mut rest = Rest(text);
    write(&mut rest).is_ok() && rest.0.is_empty()
}

/// A display label borrowed from state, or its native identity fallback.
#[derive(Clone, Copy)]
pub(crate) enum Label<'a> {
    Text(&'a str),
    Id(crate::command::AnyRef),
}
impl std::fmt::Display for Label<'_> {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(text) => out.write_str(text),
            Self::Id(id) => id.fmt(out),
        }
    }
}
impl PartialEq for Label<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Text(a), Self::Text(b)) => a == b,
            (Self::Id(a), Self::Id(b)) => a == b,
            (Self::Text(text), Self::Id(id)) | (Self::Id(id), Self::Text(text)) => {
                writes_exactly(text, |out| std::fmt::write(out, format_args!("{id}")))
            }
        }
    }
}
impl<'a> From<Label<'a>> for std::borrow::Cow<'a, str> {
    fn from(label: Label<'a>) -> Self {
        match label {
            Label::Text(text) => text.into(),
            Label::Id(id) => id.to_string().into(),
        }
    }
}

/// An id and its display label, without repeating an identical label.
pub(crate) struct Labelled<I, L>(pub I, pub L);
impl<I: std::fmt::Display + PartialEq<L>, L: std::fmt::Display> std::fmt::Display
    for Labelled<I, L>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)?;
        if self.0 != self.1 {
            write!(f, " {}", self.1)?;
        }
        Ok(())
    }
}

pub(crate) fn labelled(id: impl std::fmt::Display, label: &str) -> String {
    Labelled(&id.to_string(), label).to_string()
}

/// Whether a word needs no quoting for fuxth or a shell. A leading `=`
/// does: zsh expands `=ls` to the path of `ls` (its EQUALS option), though
/// `a=b` stays as it is.
fn bare(word: &str) -> bool {
    !word.is_empty()
        && !word.starts_with('=')
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c))
}

/// One quoting rule, streamed into either a String or a bounded sink.
pub(crate) struct Quoted<'a>(pub &'a str);
impl std::fmt::Display for Quoted<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if bare(self.0) {
            return f.write_str(self.0);
        }
        f.write_str("'")?;
        for (i, part) in self.0.split('\'').enumerate() {
            if i > 0 {
                f.write_str("'\\''")?;
            }
            f.write_str(part)?;
        }
        f.write_str("'")
    }
}

/// Quotes one word so that a shell and fuxth read it back unchanged.
pub fn quote(word: &str) -> String {
    Quoted(word).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quoted word is read back by fuxth as it was.
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
            let read = fuxth::token::source("t", &quoted).map_err(|e| e.to_string());
            let read: Result<Vec<String>, _> =
                read.map(|t| t.iter().map(|t| t.text.to_string()).collect());
            assert_eq!(read, Ok(vec![word.to_owned()]), "{quoted}");
        }
    }
}
