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

use crate::{is_name, replace};
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
    fn panes(&self) -> Vec<&str> {
        match self {
            Node::Pane(name) => vec![name],
            Node::Split(_, parts) => parts.iter().flat_map(|(part, _)| part.panes()).collect(),
        }
    }

    fn panes_mut<'a>(&'a mut self, into: &mut Vec<&'a mut String>) {
        match self {
            Node::Pane(name) => into.push(name),
            Node::Split(_, parts) => parts.iter_mut().for_each(|(part, _)| part.panes_mut(into)),
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
        let focused = parts.iter().skip(shown).find(|(part, _)| part.panes().contains(&focus));
        let parts = parts.iter().take(shown - usize::from(focused.is_some())).chain(focused);
        let total: u64 = parts.clone().map(|p| p.1.max(1) as u64).sum();
        // A part ends at its share of the room, counted from the start, leaving a cell
        // for itself and for each part after it; the last ends where the room does.
        let (start, mut used, mut weight) = (if row { area.x } else { area.y }, 0, 0);
        for (i, (part, share)) in parts.enumerate() {
            weight += (*share).max(1) as u64;
            let end =
                ((room as u64 * weight).div_ceil(total) as usize).min(room.saturating_sub(shown - 1 - i)).max(used + 1);
            let (at, size) = (start + used + i, end - used);
            let rect = if row { Rect { x: at, w: size, ..area } } else { Rect { y: at, h: size, ..area } };
            part.place(rect, focus, into);
            used = end;
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
        let Some(at) = parts.iter().position(|(part, _)| part.panes().contains(&name)) else { return false };
        if parts.get_mut(at).is_some_and(|(part, _)| part.change_share(name, of_row, change)) {
            return true;
        }
        if *row != of_row {
            return false;
        }
        // Shares become parts of a hundred, so that a step of 5 is a small one.
        let total: u32 = parts.iter().map(|p| p.1.max(1)).sum();
        // (A part under a hundredth would be made one by that, and grow: asked to shrink, it is left.)
        if change < 0 && parts.get(at).is_some_and(|p| p.1.max(1) * 100 < total) {
            return true;
        }
        parts.iter_mut().for_each(|p| p.1 = (p.1.max(1) * 100 / total).max(1));
        if let Some((_, share)) = parts.get_mut(at) {
            *share = i64::from(*share).saturating_add(change).clamp(5.min(i64::from(*share)), 10000) as u32;
        }
        true
    }
}

impl Tab {
    pub fn panes(&self) -> Vec<&str> {
        self.root.panes()
    }

    /// Where each pane of the tab is drawn in `area`.
    pub fn tiles(&self, area: Rect) -> Vec<(String, Rect)> {
        let mut tiles = Vec::new();
        if area.w > 0 && area.h > 0 {
            self.root.place(area, &self.focus, &mut tiles);
        }
        tiles
    }

    /// Puts a pane beside (in a row) or below (in a column) one of the tab's panes, and gives
    /// it the keys.
    pub fn beside(&mut self, anchor: &str, name: &str, row: bool) {
        self.focus = name.to_string();
        let found = self.root.with_parent(anchor, &mut |parent_row, parts, at| {
            if *parent_row == row {
                // The new part is as large as the one it is put beside (parts that were
                // given shares since would leave one of share 1 a sliver).
                let share = parts.get(at).map_or(1, |part| part.1);
                parts.insert(at + 1, (Node::Pane(name.to_string()), share));
            } else if let Some(part) = parts.get_mut(at) {
                part.0 = Node::Split(row, vec![(part.0.clone(), 1), (Node::Pane(name.to_string()), 1)]);
            }
        });
        if !found {
            self.root = Node::Split(row, vec![(self.root.clone(), 1), (Node::Pane(name.to_string()), 1)]);
        }
    }
}

fn tab_of(name: &str) -> Tab {
    Tab { root: Node::Pane(name.into()), focus: name.into() }
}

/// Parses a tree from words, `(` and `)`.
fn tree(words: &mut std::iter::Peekable<std::str::SplitWhitespace<'_>>, depth: usize) -> Result<(Node, u32), String> {
    if depth > DEPTH_MAX {
        return Err(format!("rows and columns are nested more than {DEPTH_MAX} deep"));
    }
    let word = words.next().ok_or("a pane's name is missing")?;
    let (node, share) = if matches!(word, "row" | "col") && words.next_if_eq(&"(").is_some() {
        let mut parts = Vec::new();
        while words.next_if_eq(&")").is_none() {
            parts.push(tree(words, depth + 1)?);
        }
        let share = words.next_if(|w| w.starts_with(':')).map_or("1", |w| w.trim_start_matches(':'));
        (Node::Split(word == "row", parts), share)
    } else {
        let (name, share) = word.split_once(':').unwrap_or((word, "1"));
        if !is_name(name) {
            return Err(format!("'{word}' is not a pane's name"));
        }
        (Node::Pane(name.into()), share)
    };
    let share = share.parse::<u32>().ok().filter(|n| (1..=10000).contains(n));
    Ok((node, share.ok_or("a share is a number from 1 to 10000")?))
}

pub fn parse(text: &str) -> Result<Layout, String> {
    if text.len() as u64 > FILE_MAX {
        return Err(format!("the layout file is over {} KiB", FILE_MAX >> 10));
    }
    let mut layout = Layout::default();
    for (number, line) in text.lines().enumerate().filter(|(_, line)| !line.trim().is_empty()) {
        let spaced = line.replace('(', " ( ").replace(')', " ) ");
        let mut words = spaced.split_whitespace().peekable();
        let fail = |why: String| format!("layout line {}: {why}", number + 1);
        if words.next_if_eq(&"*").is_some() {
            layout.current = layout.tabs.len();
        } else if words.next_if_eq(&"-").is_some() {
            layout.before = Some(layout.tabs.len());
        }
        let (root, _) = tree(&mut words, 0).map_err(fail)?;
        let mut focus = "";
        for word in words {
            focus = word.strip_prefix("focus=").ok_or_else(|| fail(format!("'{word}' is not understood")))?;
        }
        layout.tabs.push(Tab { root, focus: focus.into() });
    }
    Ok(layout)
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
    pub fn settle(&mut self, existing: Vec<String>) {
        let mut unplaced: std::collections::HashSet<_> = existing.into_iter().collect();
        self.retain_panes(|name| unplaced.remove(name));
        let mut unplaced: Vec<_> = unplaced.into_iter().collect();
        unplaced.sort_by(|a, b| (a.len(), a).cmp(&(b.len(), b)));
        self.tabs.extend(unplaced.iter().map(|name| tab_of(name)));
        // (The place of a shown tab that was the last and is gone is the first new tab's, or the last tab's.)
        self.current = self.current.min(self.tabs.len().saturating_sub(1));
    }

    /// The workspace's layout as it stands: its file's text, the layout
    /// settled against the folders, and what is wrong with the file if it
    /// cannot be read as a layout (it then counts as empty, and is left alone).
    pub fn load(workspace: &Path) -> (String, Layout, Option<String>) {
        let (bytes, read) = crate::read(crate::sys::open(workspace.join("layout"), false), FILE_MAX + 1);
        let read = read.or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(0) } else { Err(e) });
        let text = read
            .map_err(|e| format!("the layout file cannot be read: {e}"))
            .and_then(|_| String::from_utf8(bytes).map_err(|_| "the layout file is not text".to_string()));
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

    /// The tab a pane is in.
    fn tab_with(&self, name: &str) -> Result<usize, String> {
        self.tabs.iter().position(|tab| tab.panes().contains(&name)).ok_or(format!("no pane {name} here"))
    }

    fn tab_mut(&mut self) -> Result<&mut Tab, String> {
        self.tabs.get_mut(self.current).ok_or_else(|| "there are no panes here".into())
    }

    /// Prune the trees, dropping empty tabs and giving a surviving pane the focus.
    /// It alone turns places in the old list of tabs into places in the new: the tab shown stays
    /// the one that was shown, wherever it is now; if it is gone, the tab shown before it is
    /// shown; if that is gone too, the tab now in its place. Says where the two are, if still there.
    fn retain_panes(&mut self, mut keep: impl FnMut(&str) -> bool) -> [Option<usize>; 2] {
        let mut kept = Vec::new();
        self.tabs = std::mem::take(&mut self.tabs)
            .into_iter()
            .enumerate()
            .filter_map(|(at, mut tab)| {
                tab.root = tab.root.filter(&mut keep)?;
                if !tab.panes().contains(&tab.focus.as_str()) {
                    tab.focus = tab.panes().first()?.to_string();
                }
                kept.push(at);
                Some(tab)
            })
            .collect();
        let [same, back] = [Some(self.current), self.before].map(|old| kept.iter().position(|at| Some(*at) == old));
        self.current = same.or(back).unwrap_or_else(|| kept.partition_point(|at| *at < self.current));
        self.before = back.filter(|back| *back != self.current);
        [same, back]
    }

    /// One edit, as `fux layout WORDS…` gives it. An edit that ends on another
    /// tab marks the one it left as the tab shown before.
    pub fn edit(&mut self, words: &[&str]) -> Result<(), String> {
        // Where the tab shown and the tab shown before it are, as the edit moves tabs.
        let (count, mut marks) = (self.tabs.len(), [Some(self.current), self.before]);
        let step = |at: usize, count: usize, previous: bool| {
            let count = count.max(1);
            (at + if previous { count - 1 } else { 1 }) % count
        };
        match words {
            ["split", way @ ("row" | "col"), name, tail @ ..] => {
                self.tab_with(name)?;
                let row = *way == "row";
                let beside = match tail {
                    [] => self.tab_mut()?.focus.clone(),
                    ["beside", pane] => pane.to_string(),
                    _ => return Err("split takes row|col PANE [beside ANCHOR]".into()),
                };
                // (The anchor is another pane that is there, named or not: a pane is not put beside itself.)
                if beside == *name || self.tab_with(&beside).is_err() {
                    return Err(format!("cannot split pane {name} beside {beside}"));
                }
                marks = self.retain_panes(|pane| pane != *name);
                self.current = self.tab_with(&beside)?;
                self.tab_mut()?.beside(&beside, name, row);
            }
            ["focus", way @ ("left" | "right" | "up" | "down")] => {
                let tab = self.tab_mut()?;
                // (In the room the viewer told the key's script of, where the tiles are as it draws them.)
                let told = |v| std::env::var(v).ok().and_then(|n| n.parse::<u16>().ok()).map_or(10000, usize::from);
                let tiles = tab.tiles(Rect { x: 0, y: 0, w: told("COLUMNS"), h: told("LINES") });
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
            [edit @ ("focus" | "swap"), way @ ("next" | "prev")] => {
                let tab = self.tab_mut()?;
                let mut panes = Vec::new();
                tab.root.panes_mut(&mut panes);
                let from = panes.iter().position(|name| name.as_str() == tab.focus).unwrap_or(0);
                let to = step(from, panes.len(), *way == "prev");
                if *edit == "focus" {
                    tab.focus = panes.get(to).map(|name| name.to_string()).unwrap_or_default();
                } else if let Ok([a, b]) = panes.get_disjoint_mut([from, to]) {
                    std::mem::swap(*a, *b);
                }
            }
            ["pane", name] => {
                self.current = self.tab_with(name)?;
                self.tab_mut()?.focus = name.to_string();
            }
            ["tab", way @ ("next" | "prev")] => self.current = step(self.current, count, *way == "prev"),
            ["tab", "-"] => self.current = self.before.ok_or("no tab was shown before this one")?,
            ["break"] if self.tab().is_some_and(|tab| tab.panes().len() < 2) => {}
            ["break"] => {
                let name = self.tab_mut()?.focus.clone();
                marks = self.retain_panes(|pane| pane != name);
                self.current = (self.current + 1).min(self.tabs.len());
                self.tabs.insert(self.current, tab_of(&name));
            }
            ["size", way @ ("row" | "col"), change] => {
                let change = change.parse::<i64>().map_err(|_| format!("'{change}' is not a number like +5 or -5"))?;
                let tab = self.tab_mut()?;
                if !tab.root.change_share(&tab.focus, *way == "row", change) {
                    return Err(format!("this pane is in no {way}"));
                }
            }
            ["rotate"] => {
                let tab = self.tab_mut()?;
                tab.root.with_parent(&tab.focus, &mut |row, _, _| *row = !*row);
            }
            ["arrange", way @ ("row" | "col" | "grid" | "main")] => {
                let tab = self.tab_mut()?;
                let panes = tab.panes();
                let parts = |names: &[&str]| names.iter().map(|&name| (Node::Pane(name.into()), 1)).collect();
                tab.root = match *way {
                    "row" | "col" => Node::Split(*way == "row", parts(&panes)),
                    "main" => {
                        let (&first, rest) = panes.split_first().ok_or("there are no panes here")?;
                        Node::Split(true, vec![(Node::Pane(first.into()), 2), (Node::Split(false, parts(rest)), 1)])
                    }
                    _ => {
                        let across = panes.len().saturating_sub(1).isqrt() + 1;
                        Node::Split(false, panes.chunks(across).map(|row| (Node::Split(true, parts(row)), 1)).collect())
                    }
                }
                .filter(&mut |_| true)
                .ok_or("there are no panes here")?;
            }
            _ => return Err(format!("'{}' is not a layout edit: see the README", words.join(" "))),
        }
        self.before = marks.into_iter().flatten().find(|at| *at != self.current);
        Ok(())
    }
}

/// `fux layout [WORDS…]`: edits the workspace's layout file, or with no
/// words only settles it against the panes there; says how it reads then.
/// The workspace's folder is locked meanwhile, so two edits never mix.
pub fn command(workspace: &Path, words: &[&str]) -> Result<Layout, String> {
    let lock = crate::sys::open(workspace, false).map_err(|e| format!("{}: {e}", workspace.display()))?;
    lock.lock().map_err(|e| format!("{}: {e}", workspace.display()))?;
    let (old, mut layout, wrong) = Layout::load(workspace);
    wrong.map_or(Ok(()), Err)?;
    if !words.is_empty() {
        layout.edit(words)?;
    }
    let (file, text) = (workspace.join("layout"), layout.print());
    if old != text {
        // What is written must be what the next reading takes: an edit that nests too deep is refused here.
        parse(&text)?;
        replace(&file, text.as_bytes()).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    Ok(layout)
}
