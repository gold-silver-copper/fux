//! The layout of a workspace: its `layout` file, read against the panes that
//! are really there, and the edits `fux3 layout` makes to it. One tab a line:
//!
//! ```text
//! * row(sh col(logs top):2) focus=logs
//!   vim zoom
//! ```
//!
//! `row(…)` puts its parts side by side, `col(…)` stacks them, `:N` is a
//! part's share, `*` marks the tab shown, `focus=` the pane that gets the
//! keys, `zoom` shows that pane alone.

use crate::holder::replace;
use crate::is_name;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// The parts of a split, each with its share.
type Parts = Vec<(Node, u32)>;

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Pane(String),
    /// Parts with their shares; `true` for a row (side by side).
    Split(bool, Vec<(Node, u32)>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub root: Node,
    pub focus: String,
    pub zoom: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub tabs: Vec<Tab>,
    pub current: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Node {
    pub fn panes<'a>(&'a self, into: &mut Vec<&'a str>) {
        match self {
            Node::Pane(name) => into.push(name),
            Node::Split(_, parts) => parts.iter().for_each(|(part, _)| part.panes(into)),
        }
    }

    /// Keeps only the panes `keep` accepts; a split left with one part becomes that part.
    fn filter(self, keep: &mut impl FnMut(&str) -> bool) -> Option<Node> {
        match self {
            Node::Pane(name) => keep(&name).then_some(Node::Pane(name)),
            Node::Split(row, parts) => {
                let mut parts: Vec<(Node, u32)> =
                    parts.into_iter().filter_map(|(part, share)| Some((part.filter(keep)?, share))).collect();
                if parts.len() > 1 { Some(Node::Split(row, parts)) } else { parts.pop().map(|(part, _)| part) }
            }
        }
    }

    fn print(&self, out: &mut String) {
        match self {
            Node::Pane(name) => out.push_str(name),
            Node::Split(row, parts) => {
                out.push_str(if *row { "row(" } else { "col(" });
                for (i, (part, share)) in parts.iter().enumerate() {
                    out.push_str(if i > 0 { " " } else { "" });
                    part.print(out);
                    if *share != 1 {
                        let _ = write!(out, ":{share}");
                    }
                }
                out.push(')');
            }
        }
    }

    /// Gives every pane its rectangle. Parts of a row have one column between
    /// them (the viewer draws a line there).
    fn place(&self, area: Rect, into: &mut Vec<(String, Rect)>) {
        let (row, parts) = match self {
            Node::Pane(name) => return into.push((name.clone(), area)),
            Node::Split(row, parts) => (*row, parts),
        };
        let gaps = if row { parts.len().saturating_sub(1) } else { 0 };
        let room = if row { area.w } else { area.h }.saturating_sub(gaps);
        let total: usize = parts.iter().map(|p| p.1.max(1) as usize).sum();
        let mut sizes: Vec<usize> = parts.iter().map(|p| room * p.1.max(1) as usize / total.max(1)).collect();
        let mut spare = room.saturating_sub(sizes.iter().sum());
        for size in &mut sizes {
            *size += usize::from(spare > 0);
            spare = spare.saturating_sub(1);
        }
        let mut at = if row { area.x } else { area.y };
        for ((part, _), size) in parts.iter().zip(sizes) {
            let rect = if row { Rect { x: at, w: size, ..area } } else { Rect { y: at, h: size, ..area } };
            part.place(rect, into);
            at += size + usize::from(row);
        }
    }

    /// Calls `edit` on the list that holds the pane and the pane's place in
    /// it. Says whether the pane was found in a list.
    fn with_parent(&mut self, name: &str, edit: &mut dyn FnMut(&mut bool, &mut Parts, usize)) -> bool {
        let Node::Split(row, parts) = self else { return false };
        if let Some(at) = parts.iter().position(|(part, _)| *part == Node::Pane(name.into())) {
            edit(row, parts, at);
            return true;
        }
        parts.iter_mut().any(|(part, _)| part.with_parent(name, edit))
    }

    /// The share of the part of the nearest row (or column) that holds the pane.
    fn share_mut(&mut self, name: &str, of_row: bool) -> Option<&mut u32> {
        let Node::Split(row, parts) = self else { return None };
        let row = *row;
        let at = parts.iter().position(|(part, _)| {
            let mut names = Vec::new();
            part.panes(&mut names);
            names.contains(&name)
        })?;
        if parts.get_mut(at)?.0.share_mut(name, of_row).is_some() {
            return parts.get_mut(at)?.0.share_mut(name, of_row);
        }
        if row != of_row {
            return None;
        }
        // Shares become parts of a hundred, so that a step of 5 is a small one.
        let total: u32 = parts.iter().map(|p| p.1.max(1)).sum();
        parts.iter_mut().for_each(|p| p.1 = (p.1.max(1) * 100 / total).max(1));
        parts.get_mut(at).map(|p| &mut p.1)
    }
}

impl Tab {
    pub fn panes(&self) -> Vec<&str> {
        let mut names = Vec::new();
        self.root.panes(&mut names);
        names
    }

    /// Where each pane of the tab is drawn in `area`.
    pub fn tiles(&self, area: Rect) -> Vec<(String, Rect)> {
        let mut tiles = Vec::new();
        if self.zoom {
            tiles.push((self.focus.clone(), area));
        } else {
            self.root.place(area, &mut tiles);
        }
        tiles
    }
}

fn tab_of(name: &str) -> Tab {
    Tab { root: Node::Pane(name.into()), focus: name.into(), zoom: false }
}

/// Parses a tree from words, `(` and `)`.
fn tree(words: &mut std::iter::Peekable<std::vec::IntoIter<String>>) -> Result<(Node, u32), String> {
    let word = words.next().ok_or("a pane's name is missing")?;
    let node = if (word == "row" || word == "col") && words.next_if(|w| w == "(").is_some() {
        let mut parts = Vec::new();
        while words.next_if(|w| w == ")").is_none() {
            parts.push(tree(words)?);
        }
        Node::Split(word == "row", parts)
    } else {
        let name = word.split(':').next().unwrap_or_default();
        if !is_name(name) {
            return Err(format!("'{word}' is not a pane's name"));
        }
        Node::Pane(name.into())
    };
    // A share follows a name as `name:2` and a split as `):2`.
    let share = match &node {
        Node::Pane(_) => word.split_once(':').map(|(_, share)| share.to_string()),
        Node::Split(..) => words.next_if(|w| w.starts_with(':')).map(|w| w.trim_start_matches(':').to_string()),
    };
    let share = share.map_or(Ok(1), |s| s.parse::<u32>().map_err(|_| format!("'{s}' is not a share")))?;
    Ok((node, share.clamp(1, 10000)))
}

pub fn parse(text: &str) -> Result<Layout, String> {
    let mut layout = Layout::default();
    for (number, line) in text.lines().enumerate().filter(|(_, line)| !line.trim().is_empty()) {
        let spaced = line.replace('(', " ( ").replace(')', " ) ");
        let mut words: Vec<String> = spaced.split_whitespace().map(String::from).collect();
        let fail = |why: String| format!("layout line {}: {why}", number + 1);
        if words.first().is_some_and(|w| w == "*") {
            layout.current = layout.tabs.len();
            words.remove(0);
        }
        let mut words = words.into_iter().peekable();
        let (root, _) = tree(&mut words).map_err(fail)?;
        let mut tab = Tab { root, focus: String::new(), zoom: false };
        for word in words {
            match word.strip_prefix("focus=") {
                Some(name) => tab.focus = name.into(),
                None if word == "zoom" => tab.zoom = true,
                None => return Err(fail(format!("'{word}' is not understood"))),
            }
        }
        layout.tabs.push(tab);
    }
    Ok(layout)
}

impl Layout {
    pub fn print(&self) -> String {
        let mut out = String::new();
        for (i, tab) in self.tabs.iter().enumerate() {
            out.push_str(if i == self.current { "* " } else { "  " });
            tab.root.print(&mut out);
            let _ = write!(out, " focus={}{}", tab.focus, if tab.zoom { " zoom" } else { "" });
            out.push('\n');
        }
        out
    }

    /// Makes the layout agree with the panes that exist: a pane that is gone
    /// leaves it, a pane it does not place gets a tab of its own, and every
    /// tab's focus is one of its panes.
    pub fn settle(&mut self, mut existing: Vec<String>) {
        let mut seen: Vec<String> = Vec::new();
        let tabs = std::mem::take(&mut self.tabs);
        let current = tabs.get(self.current).map(|tab| tab.focus.clone());
        for tab in tabs {
            let mut keep = |name: &str| {
                let fresh = existing.iter().any(|e| e == name) && !seen.iter().any(|s| s == name);
                seen.extend(fresh.then(|| name.to_string()));
                fresh
            };
            if let Some(root) = tab.root.filter(&mut keep) {
                self.tabs.push(Tab { root, ..tab });
            }
        }
        existing.retain(|name| !seen.contains(name));
        existing.sort_by(|a, b| (a.len(), a).cmp(&(b.len(), b)));
        self.tabs.extend(existing.iter().map(|name| tab_of(name)));
        for tab in &mut self.tabs {
            if !tab.panes().contains(&tab.focus.as_str()) {
                tab.focus = tab.panes().first().map(|name| name.to_string()).unwrap_or_default();
            }
        }
        // The tab shown stays the one that was shown, wherever it is now.
        let same = self.tabs.iter().position(|tab| Some(&tab.focus) == current.as_ref());
        self.current = same.unwrap_or(self.current).min(self.tabs.len().saturating_sub(1));
    }

    /// The workspace's layout as it stands: its file, settled against its
    /// folders. A file that cannot be read as a layout counts as empty, and
    /// the second answer says what is wrong with it.
    pub fn load(workspace: &Path) -> (Layout, Option<String>) {
        let text = fs::read_to_string(workspace.join("layout")).unwrap_or_default();
        let (mut layout, mut wrong) = match parse(&text) {
            Ok(layout) => (layout, None),
            Err(why) => (Layout::default(), Some(why)),
        };
        match fs::read_dir(workspace) {
            Ok(folders) => {
                let names = folders.flatten().filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()));
                let names = names.filter_map(|entry| entry.file_name().into_string().ok());
                layout.settle(names.filter(|name| is_name(name)).collect());
            }
            Err(e) => (layout, wrong) = (Layout::default(), Some(format!("{}: {e}", workspace.display()))),
        }
        (layout, wrong)
    }

    pub fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.current)
    }

    fn tab_mut(&mut self) -> Result<&mut Tab, String> {
        self.tabs.get_mut(self.current).ok_or_else(|| "there are no panes here".into())
    }

    /// Takes a pane out of the layout, wherever it is.
    fn remove(&mut self, name: &str) {
        let tabs = std::mem::take(&mut self.tabs);
        self.tabs =
            tabs.into_iter().filter_map(|t| Some(Tab { root: t.root.filter(&mut |n| n != name)?, ..t })).collect();
        for tab in &mut self.tabs {
            if tab.focus == name {
                tab.focus = tab.panes().first().map(|n| n.to_string()).unwrap_or_default();
            }
        }
    }

    /// One edit, as `fux3 layout WORDS…` gives it.
    pub fn edit(&mut self, words: &[&str]) -> Result<(), String> {
        let count = self.tabs.len();
        let step = |at: usize, word: &str, count: usize| match word {
            "next" => Ok((at + 1) % count.max(1)),
            "prev" => Ok((at + count.max(1) - 1) % count.max(1)),
            _ => Err(format!("'{word}' is not next or prev")),
        };
        match words {
            ["split", way @ ("row" | "col"), name] => {
                let row = *way == "row";
                let beside = self.tab().map(|tab| tab.focus.clone()).filter(|focus| focus != name);
                self.remove(name);
                let new = (Node::Pane(name.to_string()), 1);
                let at = self.tabs.iter().position(|tab| Some(&tab.focus) == beside.as_ref());
                let Some(tab) = at.and_then(|at| self.tabs.get_mut(at)) else {
                    self.tabs.push(tab_of(name));
                    self.current = self.tabs.len() - 1;
                    return Ok(());
                };
                let focus = std::mem::replace(&mut tab.focus, name.to_string());
                tab.zoom = false;
                let mut new = Some(new);
                let found = tab.root.with_parent(&focus, &mut |parent_row, parts, at| {
                    if *parent_row == row {
                        parts.extend(new.take());
                        parts.get_mut(at + 1..).into_iter().for_each(|rest| rest.rotate_right(1));
                    } else if let Some(part) = parts.get_mut(at) {
                        part.0 = Node::Split(row, vec![(part.0.clone(), 1), (Node::Pane(name.to_string()), 1)]);
                    }
                });
                if !found {
                    tab.root = Node::Split(row, vec![(tab.root.clone(), 1), (Node::Pane(name.to_string()), 1)]);
                }
                self.current = at.unwrap_or(0);
            }
            ["focus", way @ ("left" | "right" | "up" | "down")] => {
                let tab = self.tab_mut()?;
                let tiles = Tab { zoom: false, ..tab.clone() }.tiles(Rect { x: 0, y: 0, w: 10000, h: 10000 });
                let Some(&(_, from)) = tiles.iter().find(|tile| tile.0 == tab.focus) else { return Ok(()) };
                // The nearest pane on that side whose edge overlaps ours the most.
                let overlap = |a: usize, a_len: usize, b: usize, b_len: usize| {
                    (a + a_len).min(b + b_len).saturating_sub(a.max(b))
                };
                let best = tiles.iter().filter_map(|(name, to)| {
                    let (gap, shared) = match *way {
                        "left" => (from.x.checked_sub(to.x + to.w)?, overlap(from.y, from.h, to.y, to.h)),
                        "right" => (to.x.checked_sub(from.x + from.w)?, overlap(from.y, from.h, to.y, to.h)),
                        "up" => (from.y.checked_sub(to.y + to.h)?, overlap(from.x, from.w, to.x, to.w)),
                        _ => (to.y.checked_sub(from.y + from.h)?, overlap(from.x, from.w, to.x, to.w)),
                    };
                    (shared > 0).then_some(((gap, usize::MAX - shared), name))
                });
                if let Some((_, name)) = best.min() {
                    tab.focus = name.clone();
                }
            }
            ["focus", way @ ("next" | "prev")] => {
                let tab = self.tab_mut()?;
                let panes = tab.panes();
                let at = step(panes.iter().position(|p| *p == tab.focus).unwrap_or(0), way, panes.len())?;
                tab.focus = panes.get(at).map(|name| name.to_string()).unwrap_or_default();
            }
            ["focus", name] => {
                let at = self
                    .tabs
                    .iter()
                    .position(|tab| tab.panes().contains(name))
                    .ok_or(format!("no pane {name} here"))?;
                self.current = at;
                self.tab_mut()?.focus = name.to_string();
            }
            ["tab", way @ ("next" | "prev")] => self.current = step(self.current, way, count)?,
            ["tab", number] => {
                let number = number.parse::<usize>().ok().filter(|n| (1..=count).contains(n));
                self.current = number.ok_or(format!("tabs are numbered 1 to {count}"))? - 1;
            }
            ["break"] => {
                let name = self.tab_mut()?.focus.clone();
                self.remove(&name);
                self.current = (self.current + 1).min(self.tabs.len());
                self.tabs.insert(self.current, tab_of(&name));
            }
            ["swap", way @ ("next" | "prev")] => {
                let tab = self.tab_mut()?;
                let (focus, panes) = (tab.focus.clone(), tab.panes().iter().map(|p| p.to_string()).collect::<Vec<_>>());
                let at = step(panes.iter().position(|p| *p == focus).unwrap_or(0), way, panes.len())?;
                let other = panes.get(at).cloned().unwrap_or_default();
                // Three renames through a name no pane can have.
                for (from, to) in [(&focus, "/"), (&other, focus.as_str()), (&"/".to_string(), other.as_str())] {
                    tab.root.rename(from, to);
                }
            }
            ["size", way @ ("row" | "col"), change] => {
                let change = change.parse::<i64>().map_err(|_| format!("'{change}' is not a number like +5 or -5"))?;
                let tab = self.tab_mut()?;
                let focus = tab.focus.clone();
                let share = tab.root.share_mut(&focus, *way == "row").ok_or(format!("this pane is in no {way}"))?;
                *share = (i64::from(*share) + change).clamp(5, 10000) as u32;
            }
            ["zoom"] => {
                let tab = self.tab_mut()?;
                tab.zoom = !tab.zoom;
            }
            ["rotate"] => {
                let tab = self.tab_mut()?;
                let focus = tab.focus.clone();
                tab.root.with_parent(&focus, &mut |row, _, _| *row = !*row);
            }
            ["arrange", way @ ("row" | "col" | "grid" | "main")] => {
                let tab = self.tab_mut()?;
                let mut panes = tab.panes().into_iter().map(|p| (Node::Pane(p.to_string()), 1));
                tab.root = match *way {
                    "row" | "col" => Node::Split(*way == "row", panes.collect()),
                    "main" => {
                        let first = panes.next();
                        let rest = (Node::Split(false, panes.collect()), 1);
                        Node::Split(true, first.map(|first| (first.0, 2)).into_iter().chain([rest]).collect())
                    }
                    _ => {
                        let panes: Vec<(Node, u32)> = panes.collect();
                        let across = (1..).find(|n| n * n >= panes.len()).unwrap_or(1);
                        Node::Split(
                            false,
                            panes.chunks(across).map(|row| (Node::Split(true, row.to_vec()), 1)).collect(),
                        )
                    }
                }
                .filter(&mut |_| true)
                .ok_or("there are no panes here")?;
                tab.zoom = false;
            }
            _ => return Err(format!("'{}' is not a layout edit: see the README", words.join(" "))),
        }
        Ok(())
    }
}

impl Node {
    fn rename(&mut self, from: &str, to: &str) {
        match self {
            Node::Pane(name) if name == from => *name = to.to_string(),
            Node::Pane(_) => {}
            Node::Split(_, parts) => parts.iter_mut().for_each(|(part, _)| part.rename(from, to)),
        }
    }
}

/// `fux3 layout [WORDS…]`: edits the workspace's layout file, or with no
/// words only settles it against the panes there; says how it reads then.
/// The workspace's folder is locked meanwhile, so two edits never mix.
pub fn command(workspace: &Path, words: &[&str]) -> Result<String, String> {
    let lock = fs::File::open(workspace).map_err(|e| format!("{}: {e}", workspace.display()))?;
    lock.lock().map_err(|e| format!("{}: {e}", workspace.display()))?;
    let (mut layout, wrong) = Layout::load(workspace);
    if let Some(wrong) = wrong {
        return Err(wrong);
    }
    if !words.is_empty() {
        layout.edit(words)?;
    }
    let (file, text) = (workspace.join("layout"), layout.print());
    if fs::read_to_string(&file).unwrap_or_default() != text {
        replace(&file.to_string_lossy(), text.as_bytes()).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    Ok(text)
}
