//! What tabs a viewer shows: every pane in one tab, or the rules of a small
//! TOML file. Each `[[tab]]` has a `title`, exact `match` tests on labels and
//! an `axis`.
pub const MAX_LAYOUT: usize = 64 << 10;
/// The same, as a file's size.
pub const MAX_LAYOUT_BYTES: u64 = 64 << 10;
const MAX_TABS: usize = 32;
const MAX_WORKSPACES: usize = 32;

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

/// A named list of tabs; a viewer shows one at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub name: String,
    pub tabs: Vec<Tab>,
}

/// What is shown with no layout file.
pub fn everything() -> Vec<Workspace> {
    vec![Workspace {
        name: "default".to_owned(),
        tabs: vec![new_tab("all".to_owned())],
    }]
}

fn new_tab(title: String) -> Tab {
    Tab {
        title,
        tests: Vec::new(),
        axis: Axis::Auto,
    }
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

/// The workspaces `text` describes: `[[workspace]]` sections with
/// `[[workspace.tab]]`s, or plain `[[tab]]`s for one unnamed workspace. Any
/// fault is refused with its line.
pub fn parse(text: &str) -> Result<Vec<Workspace>, String> {
    if text.len() > MAX_LAYOUT {
        return Err(format!("{} bytes, at most {MAX_LAYOUT}", text.len()));
    }
    let mut spaces: Vec<Workspace> = Vec::new();
    let mut named = false;
    for (index, line) in text.lines().enumerate() {
        let number = index.saturating_add(1);
        let at = |why: String| format!("line {number}: {why}");
        let line = without_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        if line == "[[workspace]]" {
            if !named && !spaces.is_empty() {
                return Err(at("[[tab]] and [[workspace]] do not mix".to_owned()));
            }
            if spaces.len() >= MAX_WORKSPACES {
                return Err(at(format!("more than {MAX_WORKSPACES} workspaces")));
            }
            named = true;
            spaces.push(Workspace {
                name: format!("workspace {}", spaces.len().saturating_add(1)),
                tabs: Vec::new(),
            });
            continue;
        }
        if line == "[[tab]]" || line == "[[workspace.tab]]" {
            if named != (line == "[[workspace.tab]]") {
                let why = "use [[workspace.tab]] under [[workspace]], and [[tab]] without one";
                return Err(at(why.to_owned()));
            }
            if spaces.is_empty() {
                spaces.extend(everything().into_iter().map(|mut space| {
                    space.tabs.clear();
                    space
                }));
            }
            let space = spaces
                .last_mut()
                .ok_or_else(|| at("a tab outside a workspace".to_owned()))?;
            if space.tabs.len() >= MAX_TABS {
                return Err(at(format!("more than {MAX_TABS} tabs")));
            }
            let title = format!("tab {}", space.tabs.len().saturating_add(1));
            space.tabs.push(new_tab(title));
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| at("expected a [[section]] or key = value".to_owned()))?;
        let (key, value) = (key.trim(), value.trim());
        let space = spaces
            .last_mut()
            .ok_or_else(|| at("a setting before the first section".to_owned()))?;
        let Some(tab) = space.tabs.last_mut() else {
            if key != "name" {
                return Err(at(format!("no workspace setting {key:?}")));
            }
            space.name = whole_string(value).map_err(at)?;
            continue;
        };
        match key {
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
    if spaces.is_empty() {
        return Err("no [[tab]]".to_owned());
    }
    if let Some(empty) = spaces.iter().find(|space| space.tabs.is_empty()) {
        return Err(format!("workspace {:?} has no tab", empty.name));
    }
    Ok(spaces)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layout_is_read_and_a_bad_one_says_where() -> Result<(), String> {
        let spaces = parse(
            "# mine\n[[tab]]\ntitle = \"a # b\"\nmatch = [\"p=q\", \"name=x\"] # both\n\
             axis = \"rows\"\n[[tab]]\n",
        )?;
        assert_eq!(spaces.len(), 1);
        let tabs = spaces.first().map_or(&[][..], |space| &space.tabs);
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.first().map(|t| t.title.as_str()), Some("a # b"));
        assert_eq!(tabs.first().map(|t| t.axis), Some(Axis::Rows));
        assert_eq!(tabs.first().map(|t| t.tests.len()), Some(2));
        let refused = parse("[[tab]]\naxis = \"sideways\"\n").err();
        assert!(refused.is_some_and(|why| why.starts_with("line 2")));
        Ok(())
    }

    #[test]
    fn workspaces_hold_their_own_tabs() -> Result<(), String> {
        let spaces = parse(
            "[[workspace]]\nname = \"one\"\n[[workspace.tab]]\ntitle = \"a\"\n\
             [[workspace.tab]]\n[[workspace]]\n[[workspace.tab]]\n",
        )?;
        let shape: Vec<(&str, usize)> = (spaces.iter())
            .map(|space| (space.name.as_str(), space.tabs.len()))
            .collect();
        assert_eq!(shape, [("one", 2), ("workspace 2", 1)]);
        assert!(parse("[[tab]]\n[[workspace]]\n").is_err());
        assert!(parse("[[workspace]]\n").is_err());
        Ok(())
    }
}
