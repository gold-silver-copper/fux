//! A word written so that fuxth's reader and a shell read it back as it
//! is: the inverse of the reader's quoting.

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

/// A word, displayed quoted: as it is when it needs no quotes, else in
/// single quotes, each of its own written `'\''`.
pub struct Quoted<'a>(pub &'a str);

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
