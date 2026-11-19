//! What tabs a viewer shows: every pane in one tab, or the rules of a small
//! TOML file. Each `[[tab]]` has a `title`, exact `match` tests on labels and
//! an `axis`.
pub const MAX_LAYOUT: usize = 64 << 10;
const MAX_TABS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Auto,
    Rows,
    Columns,
}

/// One tab: the panes that pass every test, split along `axis`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub title: String,
    pub tests: Vec<(String, String)>,
    pub axis: Axis,
}

/// The tab shown with no layout file.
pub fn everything() -> Vec<Tab> {
    vec![Tab {
        title: "all".to_owned(),
        tests: Vec::new(),
        axis: Axis::Auto,
    }]
}

/// Reads one `"…"` from the start of `text`: the string and what follows.
fn string(text: &str) -> Result<(String, &str), String> {
    let mut chars = text
        .strip_prefix('"')
        .ok_or("a string must start with \"")?
        .char_indices();
    let body = text.get(1..).unwrap_or_default();
    let mut out = String::new();
    while let Some((at, c)) = chars.next() {
        match c {
            '"' => {
                let end = at.saturating_add(1);
                return Ok((out, body.get(end..).unwrap_or_default()));
            }
            '\\' => match chars.next() {
                Some((_, '"')) => out.push('"'),
                Some((_, '\\')) => out.push('\\'),
                other => return Err(format!("the escape {other:?} is not supported")),
            },
            c => out.push(c),
        }
    }
    Err("a string is not closed".to_owned())
}

fn tests(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut rest = text
        .strip_prefix('[')
        .ok_or("match is a list of \"key=value\" strings")?
        .trim_start();
    let mut out = Vec::new();
    while !rest.starts_with(']') {
        let (item, after) = string(rest)?;
        out.push(fux2::dir::parse_label(&item).map_err(|f| f.message)?);
        rest = after.trim_start();
        if let Some(after) = rest.strip_prefix(',') {
            rest = after.trim_start();
        } else if !rest.starts_with(']') {
            return Err("expected , or ] in a list".to_owned());
        }
    }
    match rest.strip_prefix(']').map(str::trim) {
        Some("") => Ok(out),
        _ => Err("text after a list".to_owned()),
    }
}

fn whole_string(text: &str) -> Result<String, String> {
    let (value, rest) = string(text)?;
    match rest.trim().is_empty() {
        true => Ok(value),
        false => Err("text after a string".to_owned()),
    }
}

/// A `#` outside a string ends the line.
fn without_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    for (at, c) in line.char_indices() {
        match c {
            '#' if !quoted => return line.get(..at).unwrap_or(line),
            '"' if !escaped => quoted = !quoted,
            _ => {}
        }
        escaped = c == '\\' && !escaped;
    }
    line
}

/// The tabs `text` describes. Any fault is refused with its line.
pub fn parse(text: &str) -> Result<Vec<Tab>, String> {
    if text.len() > MAX_LAYOUT {
        return Err(format!("{} bytes, at most {MAX_LAYOUT}", text.len()));
    }
    let mut tabs: Vec<Tab> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let number = index.saturating_add(1);
        let at = |why: String| format!("line {number}: {why}");
        let line = without_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        if line == "[[tab]]" {
            if tabs.len() >= MAX_TABS {
                return Err(at(format!("more than {MAX_TABS} tabs")));
            }
            tabs.push(Tab {
                title: format!("tab {}", tabs.len().saturating_add(1)),
                tests: Vec::new(),
                axis: Axis::Auto,
            });
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| at("expected [[tab]] or key = value".to_owned()))?;
        let value = value.trim();
        let tab = tabs
            .last_mut()
            .ok_or_else(|| at("a setting before the first [[tab]]".to_owned()))?;
        match key.trim() {
            "title" => tab.title = whole_string(value).map_err(at)?,
            "match" => tab.tests = tests(value).map_err(at)?,
            "axis" => {
                tab.axis = match whole_string(value).map_err(at)?.as_str() {
                    "auto" => Axis::Auto,
                    "rows" => Axis::Rows,
                    "columns" => Axis::Columns,
                    other => return Err(at(format!("no axis {other:?}"))),
                }
            }
            other => return Err(at(format!("no setting {other:?}"))),
        }
    }
    match tabs.is_empty() {
        true => Err("no [[tab]]".to_owned()),
        false => Ok(tabs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layout_is_read_and_a_bad_one_says_where() -> Result<(), String> {
        let tabs = parse(
            "# mine\n[[tab]]\ntitle = \"a # b\"\nmatch = [\"p=q\", \"name=x\"] # both\n\
             axis = \"rows\"\n[[tab]]\n",
        )?;
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.first().map(|t| t.title.as_str()), Some("a # b"));
        assert_eq!(tabs.first().map(|t| t.axis), Some(Axis::Rows));
        assert_eq!(tabs.first().map(|t| t.tests.len()), Some(2));
        let refused = parse("[[tab]]\naxis = \"sideways\"\n")
            .err()
            .unwrap_or_default();
        assert!(refused.starts_with("line 2"), "{refused}");
        Ok(())
    }
}
