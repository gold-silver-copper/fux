//! The layout of a workspace: its `layout` file, read against the panes that
//! are really there, and the edits `fux layout` makes to it. One tab a line:
//!
//! ```text
//! * row(sh col(logs top):2) focus=logs
//! - vim focus=vim
//! ```
//!
//! `row(…)` puts its parts side by side, `col(…)` stacks them, `:N` is a
//! part's share, `*` marks the tab shown and `-` the one shown before it,
//! `focus=` is the pane that gets the keys.

use crate::holder::replace;
use crate::is_name;
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
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
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub tabs: Vec<Tab>,
    pub current: usize,
    /// The tab shown before this one, if it is still there: where to go when the shown tab goes.
    pub before: Option<usize>,
}

/// The deepest rows and columns may nest, and the longest a layout file may be.
const DEPTH_MAX: usize = 32;
// Room for 1,000 maximum-length pane names and their focus names, even
// allowing a full tree of weighted splits and a line header for every pane.
const FILE_MAX: u64 = 160 << 10;
/// The most panes of a workspace a layout places; more folders than that are not shown.
const PANES_MAX: usize = 1000;

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

    fn contains(&self, name: &str) -> bool {
        match self {
            Node::Pane(pane) => pane == name,
            Node::Split(_, parts) => parts.iter().any(|(part, _)| part.contains(name)),
        }
    }

    /// Gives every pane its rectangle. Between two parts there is one column
    /// or one row that is neither's (the viewer draws a line there).
    fn place(&self, area: Rect, focus: &str, into: &mut Vec<(String, Rect)>) {
        let (row, parts) = match self {
            Node::Pane(name) => return into.push((name.clone(), area)),
            Node::Split(row, parts) => (*row, parts),
        };
        // Each part shown has at least one cell, and a line between it and the
        // next. If focus would be hidden, give it the last available place.
        let length = if row { area.w } else { area.h };
        let shown = parts.len().min(length.div_ceil(2));
        let room = length.saturating_sub(shown.saturating_sub(1));
        let focused = parts.iter().skip(shown).find(|(part, _)| part.contains(focus));
        let parts = parts.iter().take(shown - usize::from(focused.is_some())).chain(focused);
        let total: usize = parts.clone().map(|p| p.1.max(1) as usize).sum();
        let mut sizes: Vec<usize> = parts.clone().map(|p| (room * p.1.max(1) as usize / total.max(1)).max(1)).collect();
        while sizes.iter().sum::<usize>() > room {
            match sizes.iter_mut().max() {
                Some(largest) if *largest > 1 => *largest -= 1,
                _ => break,
            }
        }
        let mut spare = room.saturating_sub(sizes.iter().sum());
        for size in &mut sizes {
            *size += usize::from(spare > 0);
            spare = spare.saturating_sub(1);
        }
        let mut at = if row { area.x } else { area.y };
        for ((part, _), size) in parts.zip(sizes) {
            let rect = if row { Rect { x: at, w: size, ..area } } else { Rect { y: at, h: size, ..area } };
            part.place(rect, focus, into);
            at += size + 1;
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

    /// Changes the share of the part in the nearest row (or column) that holds the pane.
    fn change_share(&mut self, name: &str, of_row: bool, change: i64) -> bool {
        let Node::Split(row, parts) = self else { return false };
        let Some(at) = parts.iter().position(|(part, _)| part.contains(name)) else { return false };
        if parts.get_mut(at).is_some_and(|(part, _)| part.change_share(name, of_row, change)) {
            return true;
        }
        if *row != of_row {
            return false;
        }
        // Shares become parts of a hundred, so that a step of 5 is a small one.
        let total: u32 = parts.iter().map(|p| p.1.max(1)).sum();
        parts.iter_mut().for_each(|p| p.1 = (p.1.max(1) * 100 / total).max(1));
        if let Some((_, share)) = parts.get_mut(at) {
            *share = i64::from(*share).saturating_add(change).clamp(5, 10000) as u32;
        }
        true
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
        if area.w > 0 && area.h > 0 {
            self.root.place(area, &self.focus, &mut tiles);
        }
        tiles
    }
}

fn tab_of(name: &str) -> Tab {
    Tab { root: Node::Pane(name.into()), focus: name.into() }
}

/// Parses a tree from words, `(` and `)`.
fn tree(words: &mut std::iter::Peekable<std::vec::IntoIter<String>>, depth: usize) -> Result<(Node, u32), String> {
    if depth > DEPTH_MAX {
        return Err(format!("rows and columns are nested more than {DEPTH_MAX} deep"));
    }
    let word = words.next().ok_or("a pane's name is missing")?;
    let node = if (word == "row" || word == "col") && words.next_if(|w| w == "(").is_some() {
        let mut parts = Vec::new();
        while words.next_if(|w| w == ")").is_none() {
            parts.push(tree(words, depth + 1)?);
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
    let share = share.map_or(Some(1), |s| s.parse::<u32>().ok().filter(|n| (1..=10000).contains(n)));
    Ok((node, share.ok_or("a share is a number from 1 to 10000")?))
}

pub fn parse(text: &str) -> Result<Layout, String> {
    let mut layout = Layout::default();
    for (number, line) in text.lines().enumerate().filter(|(_, line)| !line.trim().is_empty()) {
        let spaced = line.replace('(', " ( ").replace(')', " ) ");
        let mut words: Vec<String> = spaced.split_whitespace().map(String::from).collect();
        let fail = |why: String| format!("layout line {}: {why}", number + 1);
        match words.first().map(String::as_str) {
            Some("*") => layout.current = layout.tabs.len(),
            Some("-") => layout.before = Some(layout.tabs.len()),
            _ => words.insert(0, String::new()),
        }
        words.remove(0);
        let mut words = words.into_iter().peekable();
        let (root, _) = tree(&mut words, 0).map_err(fail)?;
        let mut tab = Tab { root, focus: String::new() };
        for word in words {
            match word.strip_prefix("focus=") {
                Some(name) => tab.focus = name.into(),
                None => return Err(fail(format!("'{word}' is not understood"))),
            }
        }
        layout.tabs.push(tab);
    }
    Ok(layout)
}

fn check_file_size(bytes: usize) -> Result<(), String> {
    (bytes as u64 <= FILE_MAX).then_some(()).ok_or_else(|| format!("the layout file is over {} KiB", FILE_MAX >> 10))
}

impl Layout {
    pub fn print(&self) -> String {
        let mut out = String::new();
        for (i, tab) in self.tabs.iter().enumerate() {
            out.push_str(if i == self.current {
                "* "
            } else if Some(i) == self.before {
                "- "
            } else {
                "  "
            });
            tab.root.print(&mut out);
            let _ = write!(out, " focus={}", tab.focus);
            out.push('\n');
        }
        out
    }

    /// Makes the layout agree with the panes that exist: a pane that is gone
    /// leaves it, a pane it does not place gets a tab of its own, and every
    /// tab's focus is one of its panes.
    pub fn settle(&mut self, mut existing: Vec<String>) {
        let mut seen: Vec<String> = Vec::new();
        let (shown, marked) = (self.names(Some(self.current)), self.names(self.before));
        let tabs = std::mem::take(&mut self.tabs);
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
        // The tab shown stays the one that was shown, wherever it is now. If
        // it is gone, the tab shown before it is shown; if that is gone too,
        // the tab now in its place.
        let find = |tabs: &[Tab], names: &[String]| {
            tabs.iter().position(|tab| tab.panes().iter().any(|p| names.iter().any(|n| n == p)))
        };
        let (same, back) = (find(&self.tabs, &shown), find(&self.tabs, &marked));
        self.current = same.or(back).unwrap_or(self.current).min(self.tabs.len().saturating_sub(1));
        self.before = back.filter(|back| *back != self.current);
    }

    /// The names of the panes of a tab, to find it again after tabs came and went.
    fn names(&self, tab: Option<usize>) -> Vec<String> {
        let tab = tab.and_then(|at| self.tabs.get(at));
        tab.map(|tab| tab.panes().iter().map(|name| name.to_string()).collect()).unwrap_or_default()
    }

    /// One edit, as `fux layout WORDS…` gives it. An edit that ends on another
    /// tab marks the one it left as the tab shown before.
    pub fn edit(&mut self, words: &[&str]) -> Result<(), String> {
        let (shown, marked) = (self.names(Some(self.current)), self.names(self.before));
        self.apply(words)?;
        let other = |names: &[String]| {
            let has = |tab: &Tab| tab.panes().iter().any(|p| names.iter().any(|n| n == p));
            self.tabs.iter().enumerate().position(|(at, tab)| at != self.current && has(tab))
        };
        self.before = other(&shown).or_else(|| other(&marked));
        Ok(())
    }

    /// The workspace's layout as it stands: its file's text, the layout
    /// settled against the folders, and what is wrong with the file if it
    /// cannot be read as a layout (it then counts as empty, and is left alone).
    pub fn load(workspace: &Path) -> (String, Layout, Option<String>) {
        let mut bytes = Vec::new();
        let read = match fs::File::open(workspace.join("layout")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            file => file.and_then(|file| file.take(FILE_MAX + 1).read_to_end(&mut bytes)),
        };
        let text = read.map_err(|e| format!("the layout file cannot be read: {e}")).and_then(|n| {
            check_file_size(n)?;
            String::from_utf8(bytes).map_err(|_| "the layout file is not text".to_string())
        });
        let (mut layout, mut wrong) = match text.as_ref().map_err(String::clone).and_then(|text| parse(text)) {
            Ok(layout) => (layout, None),
            Err(why) => (Layout::default(), Some(why)),
        };
        match fs::read_dir(workspace) {
            Ok(folders) => {
                let names = folders.flatten().filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()));
                let names = names.filter_map(|entry| entry.file_name().into_string().ok());
                let names: Vec<String> = names.filter(|name| is_name(name)).take(PANES_MAX + 1).collect();
                if names.len() > PANES_MAX {
                    (layout, wrong) =
                        (Layout::default(), Some(format!("more than {PANES_MAX} panes in one workspace")));
                }
                layout.settle(names.into_iter().take(PANES_MAX).collect());
            }
            Err(e) => (layout, wrong) = (Layout::default(), Some(format!("{}: {e}", workspace.display()))),
        }
        (text.unwrap_or_default(), layout, wrong)
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

    fn apply(&mut self, words: &[&str]) -> Result<(), String> {
        let count = self.tabs.len();
        let step = |at: usize, word: &str, count: usize| match word {
            "next" => Ok((at + 1) % count.max(1)),
            "prev" => Ok((at + count.max(1) - 1) % count.max(1)),
            _ => Err(format!("'{word}' is not next or prev")),
        };
        match words {
            ["split", way @ ("row" | "col"), name, tail @ ..] => {
                if !self.tabs.iter().any(|tab| tab.panes().contains(name)) {
                    return Err(format!("no pane {name} here"));
                }
                let row = *way == "row";
                let beside = match tail {
                    [] => self.tab().map(|tab| tab.focus.clone()).filter(|focus| focus != name),
                    ["beside", pane] if pane != name && self.tabs.iter().any(|tab| tab.panes().contains(pane)) => {
                        Some(pane.to_string())
                    }
                    ["beside", pane] => return Err(format!("cannot split pane {name} beside {pane}")),
                    _ => return Err("split takes row|col PANE [beside ANCHOR]".into()),
                };
                self.remove(name);
                let new = (Node::Pane(name.to_string()), 1);
                let at =
                    self.tabs.iter().position(|tab| beside.as_deref().is_some_and(|pane| tab.panes().contains(&pane)));
                let Some((tab, beside)) = at.and_then(|at| self.tabs.get_mut(at)).zip(beside) else {
                    self.tabs.push(tab_of(name));
                    self.current = self.tabs.len() - 1;
                    return Ok(());
                };
                tab.focus = name.to_string();
                let mut new = Some(new);
                let found = tab.root.with_parent(&beside, &mut |parent_row, parts, at| {
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
                let tiles = tab.tiles(Rect { x: 0, y: 0, w: 10000, h: 10000 });
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
            // `pane NAME` is `focus NAME` for a pane whose name is a direction's word.
            ["focus" | "pane", name] => {
                let at = self
                    .tabs
                    .iter()
                    .position(|tab| tab.panes().contains(name))
                    .ok_or(format!("no pane {name} here"))?;
                self.current = at;
                self.tab_mut()?.focus = name.to_string();
            }
            ["tab", way @ ("next" | "prev")] => self.current = step(self.current, way, count)?,
            ["tab", "-"] => self.current = self.before.ok_or("no tab was shown before this one")?,
            ["tab", number] => {
                let number = number.parse::<usize>().ok().filter(|n| (1..=count).contains(n));
                self.current = number.ok_or(format!("tabs are numbered 1 to {count}"))? - 1;
            }
            ["break"] if self.tab().is_some_and(|tab| tab.panes().len() < 2) => {}
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
                if !tab.root.change_share(&focus, *way == "row", change) {
                    return Err(format!("this pane is in no {way}"));
                }
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

/// `fux layout [WORDS…]`: edits the workspace's layout file, or with no
/// words only settles it against the panes there; says how it reads then.
/// The workspace's folder is locked meanwhile, so two edits never mix.
pub fn command(workspace: &Path, words: &[&str]) -> Result<String, String> {
    let lock = fs::File::open(workspace).map_err(|e| format!("{}: {e}", workspace.display()))?;
    lock.lock().map_err(|e| format!("{}: {e}", workspace.display()))?;
    let (old, mut layout, wrong) = Layout::load(workspace);
    if let Some(wrong) = wrong {
        return Err(wrong);
    }
    if !words.is_empty() {
        layout.edit(words)?;
    }
    let (file, text) = (workspace.join("layout"), layout.print());
    if old != text {
        // What is written must be what the next reading takes: an edit that nests too deep is refused here.
        check_file_size(text.len())?;
        parse(&text)?;
        replace(&file, text.as_bytes()).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    Ok(text)
}
