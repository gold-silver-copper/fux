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
