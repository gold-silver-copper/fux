//! Words in a sentence, and quoting words for display.

pub use fuxth_core::quote;

/// `names` in a sentence: `a, b and c`, `last` (`and`, `or`) before the
/// last one.
pub fn listed(names: &[&str], last: &str) -> String {
    match names.split_last() {
        Some((end, [])) => (*end).to_owned(),
        Some((end, rest)) => format!("{} {last} {end}", rest.join(", ")),
        None => String::new(),
    }
}

/// `items`, each as it displays, with `between` between them.
pub fn joined<T: std::fmt::Display>(items: impl IntoIterator<Item = T>, between: &str) -> String {
    let items: Vec<String> = items.into_iter().map(|item| item.to_string()).collect();
    items.join(between)
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
pub enum Label<'a> {
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
pub struct Labelled<I, L>(pub I, pub L);
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

pub fn labelled(id: impl std::fmt::Display, label: &str) -> String {
    Labelled(&id.to_string(), label).to_string()
}

/// Inert single-line UI text; terminal controls have no display authority.
pub fn clean(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                c
            }
        })
        .collect()
}
