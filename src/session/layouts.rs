//! What a session holds, written as the fuxth program that makes it again
//! (`dump`): each pane made becomes a constant (`p5`) that later lines name.
//! Its layouts and launches, never its processes' state.
use super::Session;
use crate::command::AnyRef;
use crate::id::{PaneId, WsId};
use crate::layout::{Axis, Tree};
use crate::pane::{self, Launch, Runs};
use crate::workspace::{Fit, Tab, Workspace};

impl Session {
    /// What there is, or `only` that tab or workspace (a pane's: its tab),
    /// as a fuxth program.
    pub(crate) fn dump_layout(&self, only: Option<AnyRef>) -> String {
        // A script, as a file `chmod +x` makes runnable.
        let shebang = crate::script::SHEBANG;
        let mut out = format!("{shebang}\n# fux: running this makes it again\n");
        // A tab's or a pane's is its tab alone, not its workspace's part.
        let whole = match only {
            None | Some(AnyRef::Workspace(_)) => true,
            Some(AnyRef::Tab(_) | AnyRef::Pane(_)) => false,
        };
        for ws in self.parts.workspaces() {
            let tabs = ws
                .tabs()
                .iter()
                .filter(|t| self.takes_in(only, ws.id(), t.id()));
            for (i, tab) in tabs.enumerate() {
                // The first opens its workspace.
                self.dump_tab(ws, tab, whole, whole && i == 0, &mut out);
            }
        }
        out
    }

    /// The lines that make tab `tab` of `ws`, and its panes:
    /// with `whole`, as part of its workspace, which it `opens` if it is the
    /// first written.
    fn dump_tab(&self, ws: &Workspace, tab: &Tab, whole: bool, opens: bool, out: &mut String) {
        let (root, first) = (tab.root(), tab.root().first_pane());
        let program = self.program(first);
        // A dump run anywhere puts each tab in the workspace of its name,
        // made if there is none (`in`).
        let mut line = vec!["opts".to_owned()];
        match whole && !opens {
            true => line.extend([workspace(ws.id()), "in".to_owned()]),
            false => line.extend(string(ws.name()).chain(["in".to_owned()])),
        }
        // A name fux gave (`main`, `tab-N`, the program's) it gives again.
        if tab.given() {
            line.extend(string(tab.name()).chain(["name".to_owned()]));
        }
        // Its size, which a server with no client attached needs to make
        // it: one that fits clients is made at the size it has, then fits
        // clients again (`resize-auto`).
        let size = tab.fit().size();
        let (cols, rows) = (size.cols().to_string(), size.rows().to_string());
        line.extend([cols, rows, "size".to_owned()]);
        self.pane_words(first, &mut line, out);
        line.push(match program {
            Some(_) => "run".to_owned(),
            None => "new-tab".to_owned(),
        });
        let named = pane_name(first);
        line.extend(["let".to_owned(), named.clone()]);
        push(out, &line);
        if opens {
            push(out, &[&named, "workspace", "let", &workspace(ws.id())]);
        }
        if matches!(tab.fit(), Fit::Clients(_)) {
            push(out, &[&named, "tab", "resize-auto"]);
        }
        self.dump_tree(root, out);
        // Panes renamed keep their names.
        for pane in root.panes() {
            let Some(p) = self.parts.panes().get(&pane) else {
                continue;
            };
            let shell = || pane::basename(self.config.shell.program());
            let default = self
                .program(pane)
                .map_or_else(shell, |l| pane::named(&l.argv));
            if default != p.name() {
                push(out, &[&pane_name(pane), "str", p.name(), "rename"]);
            }
        }
    }

    /// The `split` lines that make `tree` from its first pane: each
    /// divides what its first child's first pane has with its second's,
    /// as its shares say, and then each child is made from those. So the
    /// layout made again is the one there is, share for share.
    fn dump_tree(&self, tree: &Tree, out: &mut String) {
        let Tree::Split(split) = tree else {
            return;
        };
        let (first, second) = split.children();
        let way = match split.axis() {
            Axis::Horizontal => "horizontal",
            Axis::Vertical => "vertical",
        };
        let (a, b) = split.shares().get();
        let pane = second.first_pane();
        let mut line = vec![pane_name(first.first_pane()), way.into()];
        line.extend([a.to_string(), b.to_string(), "shares".to_owned()]);
        self.pane_words(pane, &mut line, out);
        line.extend(["split".to_owned(), "let".to_owned(), pane_name(pane)]);
        push(out, &line);
        self.dump_tree(first, out);
        self.dump_tree(second, out);
    }

    /// The program `run` started in pane `id`, if it runs one: its words,
    /// where, and the variables it was given.
    fn program(&self, id: PaneId) -> Option<&Launch> {
        match &self.parts.panes().get(&id)?.runs {
            Runs::Program(launch) => Some(launch),
            Runs::Shell => None,
        }
    }

    /// The options a line gives pane `id`: where it starts, and a program's
    /// variables and words. A directory that is not text, which a fuxth
    /// string cannot hold, is left out, never written changed: a comment
    /// in `out` says so, and the line starts it where it runs.
    fn pane_words(&self, id: PaneId, line: &mut Vec<String>, out: &mut String) {
        let program = self.program(id);
        let shell = || self.cwd_for(super::Here::NOWHERE, Some(id));
        let cwd = program.map_or_else(shell, |launch| launch.cwd.clone());
        match cwd.to_str() {
            Some(cwd) => line.extend(string(cwd).chain(["cwd".to_owned()])),
            None => out.push_str(&format!(
                "# {id}: its directory {cwd:?} is not UTF-8 text, so it starts where this runs\n"
            )),
        }
        for (key, value) in program.iter().flat_map(|launch| &launch.env) {
            line.extend(string(key).chain(string(value)).chain(["env".to_owned()]));
        }
        let Some(launch) = program else {
            return;
        };
        for (i, word) in launch.argv.iter().enumerate() {
            let what = if i == 0 { "program" } else { "arg" };
            line.extend(string(word).chain([what.to_owned()]));
        }
    }
}

/// The `let` name a dump names pane `id` by.
fn pane_name(id: PaneId) -> String {
    format!("p{}", id.number())
}

/// The `let` name a dump names workspace `id` by.
fn workspace(id: WsId) -> String {
    format!("w{}", id.number())
}

/// `str TEXT`, as words.
fn string(text: &str) -> impl Iterator<Item = String> {
    ["str".to_owned(), text.to_owned()].into_iter()
}

/// Appends a line of words, each quoted as fuxth's tokenizer reads it
/// back.
fn push(out: &mut String, line: &[impl AsRef<str>]) {
    let words = line.iter().map(|w| crate::words::quote(w.as_ref()));
    out.push_str(&super::joined(words, " "));
    out.push('\n');
}
