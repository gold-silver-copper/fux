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
pub(crate) fn labelled(id: impl std::fmt::Display, label: &str) -> String {
    let id = id.to_string();
    if label == id {
        id
    } else {
        format!("{id} {label}")
    }
}

/// Whether a word needs no quoting for fuxth or a shell. A leading `=`
/// does: zsh expands `=ls` to the path of `ls` (its EQUALS option), though
/// `a=b` stays as it is.
pub(crate) fn bare(word: &str) -> bool {
    !word.is_empty()
        && !word.starts_with('=')
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c))
}

/// Quotes one word so that a shell and fuxth read it back unchanged.
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
