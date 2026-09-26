//! The error notices fux can show, taken from its source: every string
//! literal in it, with each `{…}` a wildcard. A notice no template matches
//! is a finding to judge; the list is never kept by hand.

/// fux's sources, read when the walk is built.
const SOURCES: &[&str] = &[
    include_str!("../../src/bytes.rs"),
    include_str!("../../src/client.rs"),
    include_str!("../../src/command.rs"),
    include_str!("../../src/config.rs"),
    include_str!("../../src/copy.rs"),
    include_str!("../../src/decode.rs"),
    include_str!("../../src/encode.rs"),
    include_str!("../../src/input.rs"),
    include_str!("../../src/json.rs"),
    include_str!("../../src/keys.rs"),
    include_str!("../../src/layout.rs"),
    include_str!("../../src/lib.rs"),
    include_str!("../../src/overlay.rs"),
    include_str!("../../src/pane.rs"),
    include_str!("../../src/process.rs"),
    include_str!("../../src/protocol.rs"),
    include_str!("../../src/render.rs"),
    include_str!("../../src/server.rs"),
    include_str!("../../src/session.rs"),
    include_str!("../../src/socket.rs"),
    include_str!("../../src/view.rs"),
    include_str!("../../src/words.rs"),
];

/// A template with less fixed text than this matches too much to mean
/// anything ("{}: {e}").
const LEAST_FIXED: usize = 6;

/// The string literals of `source`, their escapes resolved.
fn literals(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = source.chars().peekable();
    let mut in_comment = false;
    let mut previous = ' ';
    while let Some(c) = chars.next() {
        if in_comment {
            if c == '\n' {
                in_comment = false;
            }
            previous = c;
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            in_comment = true;
            continue;
        }
        // A char literal such as '"' is not a string.
        if c == '\'' && chars.peek() == Some(&'"') {
            let _ = chars.next();
            let _ = chars.next();
            continue;
        }
        if c != '"' || previous == '\\' {
            previous = c;
            continue;
        }
        let mut text = String::new();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') => text.push('\n'),
                    Some('t') => text.push('\t'),
                    Some('\n') => {
                        while chars.peek().is_some_and(|c| c.is_whitespace()) {
                            let _ = chars.next();
                        }
                    }
                    Some(other) => text.push(other),
                    None => break,
                },
                other => text.push(other),
            }
        }
        out.push(text);
        previous = '"';
    }
    out
}

/// A literal as a template: `{…}` becomes `*`, `{{` and `}}` their brace.
fn template(literal: &str) -> Option<String> {
    let mut out = String::new();
    let mut fixed = 0usize;
    let mut chars = literal.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                let _ = chars.next();
                out.push('{');
                fixed = fixed.saturating_add(1);
            }
            '{' => {
                for c in chars.by_ref() {
                    if c == '}' {
                        break;
                    }
                }
                if !out.ends_with('*') {
                    out.push('*');
                }
            }
            '}' if chars.peek() == Some(&'}') => {
                let _ = chars.next();
                out.push('}');
                fixed = fixed.saturating_add(1);
            }
            other => {
                out.push(other);
                fixed = fixed.saturating_add(1);
            }
        }
    }
    (fixed >= LEAST_FIXED).then_some(out)
}

/// Whether `text` matches `pattern`, where `*` is any run of characters;
/// with `prefix`, whether it matches the start of something the pattern
/// matches.
fn matches(pattern: &[char], text: &[char], prefix: bool) -> bool {
    // reach[j]: the first i characters of the pattern can match text[..j].
    let mut reach = vec![false; text.len().saturating_add(1)];
    if let Some(first) = reach.get_mut(0) {
        *first = true;
    }
    for p in pattern {
        let mut next = vec![false; reach.len()];
        if *p == '*' {
            let mut any = false;
            for (j, slot) in next.iter_mut().enumerate() {
                any = any || reach.get(j).copied().unwrap_or(false);
                *slot = any;
            }
        } else {
            for (j, c) in text.iter().enumerate() {
                if c == p
                    && reach.get(j).copied().unwrap_or(false)
                    && let Some(slot) = next.get_mut(j.saturating_add(1))
                {
                    *slot = true;
                }
            }
        }
        if prefix && reach.last().copied().unwrap_or(false) {
            return true;
        }
        reach = next;
    }
    reach.last().copied().unwrap_or(false)
}

pub struct Notices {
    templates: Vec<Vec<char>>,
}

impl Notices {
    pub fn from_source() -> Notices {
        let mut templates: Vec<Vec<char>> = SOURCES
            .iter()
            .flat_map(|s| literals(s))
            .filter_map(|l| template(&l))
            .map(|t| t.chars().collect())
            .collect();
        templates.sort();
        templates.dedup();
        Notices { templates }
    }

    /// Whether fux's source can produce `notice`. A notice cut short by the
    /// bar ends in `…`, and only its beginning is matched.
    pub fn known(&self, notice: &str) -> bool {
        let (text, prefix) = match notice.strip_suffix('…') {
            Some(start) => (start, true),
            None => (notice, false),
        };
        let text: Vec<char> = text.chars().collect();
        // A few characters before the cut, or anything would match.
        if prefix && text.len() < 3 {
            return false;
        }
        self.templates
            .iter()
            .any(|t| matches(t, &text, prefix) || (!prefix && contains_whole(t, &text)))
    }
}

/// A notice may be a template with a prefix of its own ("config: …"), or a
/// longer message around one: any template that matches a run of it whole,
/// from a word boundary.
fn contains_whole(pattern: &[char], text: &[char]) -> bool {
    text.iter()
        .enumerate()
        .filter(|(i, _)| *i == 0 || text.get(i.saturating_sub(1)) == Some(&' '))
        .any(|(i, _)| matches(pattern, text.get(i..).unwrap_or_default(), false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_come_from_the_source_and_match_what_fux_says() {
        let notices = Notices::from_source();
        assert!(notices.templates.len() > 100, "{}", notices.templates.len());
        for known in [
            "no pane %99",
            "only one pane",
            "nothing selected: v, s or x starts a selection",
            "the pane's program is not reading its input; nothing more is queued until it does",
            "C-b t q is not bound",
            "config: fux.conf:2: bind: \"C-Left\" is not a letter: keys after the prefix are a–z, without modifiers",
            "the pane's program is not reading its in…",
        ] {
            assert!(notices.known(known), "{known}");
        }
        for unknown in ["the server exploded", "", "…"] {
            assert!(!notices.known(unknown), "{unknown}");
        }
    }
}
