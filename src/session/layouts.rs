//! What a session holds, written as the fuxth program that makes it again
//! (`dump`): each pane made becomes a constant (`p5`) that later lines name.
//! A server saves its whole session so, the ids asked for too, and a server
//! started after a crash or a reboot runs it (through a client, as fuxth
//! always runs) to bring the session back with the ids it had.
use super::Session;
use crate::command::AnyRef;
use crate::id::{PaneId, WsId};
use crate::layout::{Axis, Split, Tree};
use crate::pane::{self, Launch, Runs};
use crate::workspace::{Fit, Tab, Workspace};

impl Session {
    /// What there is, or `only` that tab or workspace (a pane's: its tab),
    /// as a fuxth program; with `ids`, the ids to ask for, as a saved
    /// session keeps them.
    pub(crate) fn dump_layout(&self, only: Option<AnyRef>, ids: bool) -> String {
        let mut out = String::from("# fux: running this makes it again\n");
        // A tab's or a pane's is its tab alone, not its workspace's part.
        let whole = match only {
            None | Some(AnyRef::Workspace(_)) => true,
            Some(AnyRef::Tab(_) | AnyRef::Pane(_)) => false,
        };
        for ws in &self.workspaces {
            // The first tab written opens its workspace: one before it may
            // be empty (its panes moved out), and is not written.
            let mut opens = whole;
            for tab in ws.tabs() {
                if !self.takes_in(only, ws.id, tab.id) {
                    continue;
                }
                if tab.root().is_none() {
                    continue;
                }
                self.dump_tab(ws, tab, whole, opens, ids, &mut out);
                opens = false;
            }
        }
        out
    }

    /// The lines that make tab `tab` of `ws`, and its panes:
    /// with `whole`, as part of its workspace, which it `opens` if it is the
    /// first written.
    fn dump_tab(
        &self,
        ws: &Workspace,
        tab: &Tab,
        whole: bool,
        opens: bool,
        ids: bool,
        out: &mut String,
    ) {
        let Some((root, first)) = tab.root().and_then(|r| Some((r, r.first_pane()?))) else {
            return;
        };
        let program = self.program(first);
        // A saved session makes each workspace, with the ids it had, on a
        // server that has none; a dump run anywhere puts each tab in the
        // workspace of its name, made if there is none (`in`).
        let makes = opens && ids && program.is_none();
        let mut line = vec!["opts".to_owned()];
        match (makes, whole && !opens) {
            (true, _) => line.extend(string(&ws.name).chain(["name".to_owned()])),
            (false, true) => line.extend([workspace(ws.id), "in".to_owned()]),
            (false, false) => line.extend(string(&ws.name).chain(["in".to_owned()])),
        }
        // A name fux gave (`main`, `tab-N`, the program's) it gives again.
        if tab.given && !makes {
            line.extend(string(&tab.name).chain(["name".to_owned()]));
        }
        if let Fit::Fixed(size) = tab.fit() {
            line.extend([
                size.cols().to_string(),
                size.rows().to_string(),
                "size".to_owned(),
            ]);
        }
        if ids {
            if opens {
                line.extend(asked(&ws.id.to_string()));
            }
            line.extend(asked(&tab.id.to_string()));
        }
        self.pane_words(first, ids, &mut line);
        line.push(match (program.is_some(), makes) {
            (true, _) => "run".to_owned(),
            (false, true) => "new-workspace".to_owned(),
            (false, false) => "new-tab".to_owned(),
        });
        line.extend(["let".to_owned(), pane_name(first)]);
        push(out, &line);
        if opens {
            push(
                out,
                &[&pane_name(first), "workspace", "let", &workspace(ws.id)],
            );
        }
        if tab.given && makes {
            push(out, &[&pane_name(first), "tab", "str", &tab.name, "rename"]);
        }
        if let Tree::Split(axis, split) = root {
            self.dump_split(*axis, split, ids, out);
        }
        // Each pane's size, for the shares of its splits, the last taking
        // what is left: in a tab of a fixed size, the only one whose panes'
        // sizes stay as they are wherever it is made.
        let panes = root.panes();
        let fixed = matches!(tab.fit(), Fit::Fixed(_));
        if let (Some((_, sized)), true) = (panes.split_last(), fixed) {
            for pane in sized {
                let Some(size) = self.panes.get(pane).map(|p| p.size()) else {
                    continue;
                };
                let name = pane_name(*pane);
                push(out, &[&name, &size.cols().to_string(), "resize-x"]);
                push(out, &[&name, &size.rows().to_string(), "resize-y"]);
            }
        }
        // Panes renamed keep their names.
        for pane in root.panes() {
            let Some(p) = self.panes.get(&pane) else {
                continue;
            };
            let shell = || pane::basename(self.config.shell.program());
            let default = self
                .program(pane)
                .map_or_else(shell, |l| pane::named(&l.argv));
            if default != p.name {
                push(out, &[&pane_name(pane), "str", &p.name, "rename"]);
            }
        }
    }

    /// The `split` lines that make `split`'s children beside its first,
    /// along `axis`, and then each child's own.
    fn dump_split(&self, axis: Axis, split: &Split, ids: bool, out: &mut String) {
        let firsts: Vec<PaneId> = split.children().filter_map(Tree::first_pane).collect();
        let way = match axis {
            Axis::Horizontal => "horizontal",
            Axis::Vertical => "vertical",
        };
        for (before, pane) in firsts.iter().zip(firsts.iter().skip(1)) {
            let mut line = vec![pane_name(*before), way.into()];
            self.pane_words(*pane, ids, &mut line);
            line.extend(["split".to_owned(), "let".to_owned(), pane_name(*pane)]);
            push(out, &line);
        }
        for child in split.children() {
            if let Tree::Split((), inner) = child {
                self.dump_split(axis.cross(), inner, ids, out);
            }
        }
    }

    /// The program `run` started in pane `id`, if it runs one: its words,
    /// where, and the variables it was given.
    fn program(&self, id: PaneId) -> Option<&Launch> {
        match &self.panes.get(&id)?.runs {
            Runs::Program(launch) => Some(launch),
            Runs::Shell => None,
        }
    }

    /// The options a line gives pane `id`: where it starts, and a program's
    /// variables and words; with `ids`, the id it asks for.
    fn pane_words(&self, id: PaneId, ids: bool, line: &mut Vec<String>) {
        let program = self.program(id);
        let shell = || self.cwd_for(super::Here::NOWHERE, Some(id));
        let cwd = program.map_or_else(shell, |launch| launch.cwd.clone());
        line.extend(string(&cwd.to_string_lossy()).chain(["cwd".to_owned()]));
        for (key, value) in program.iter().flat_map(|launch| &launch.env) {
            line.extend(string(key).chain(string(value)).chain(["env".to_owned()]));
        }
        if ids {
            line.extend(asked(&id.to_string()));
        }
        let Some(launch) = program else {
            return;
        };
        let mut words = launch.argv.iter();
        if let Some(program) = words.next() {
            line.extend(string(program).chain(["program".to_owned()]));
        }
        for arg in words {
            line.extend(string(arg).chain(["arg".to_owned()]));
        }
        // Made, and left to run: a replay waits for no program.
        line.extend(["0".to_owned(), "timeout".to_owned()]);
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

/// The words that ask for `id`.
fn asked(id: &str) -> impl Iterator<Item = String> {
    string(id).chain(["id".to_owned()])
}

/// Appends a line of words, each quoted as fuxth's tokenizer reads it
/// back.
fn push(out: &mut String, line: &[impl AsRef<str>]) {
    let words: Vec<String> = line
        .iter()
        .map(|w| crate::words::quote(w.as_ref()))
        .collect();
    out.push_str(&words.join(" "));
    out.push('\n');
}
