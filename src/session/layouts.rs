//! What a session holds, written as the fuxth program that makes it again
//! (`dump`): each pane made becomes a constant (`p5`) that later lines name.
//! A server saves its whole session so, the ids asked for too, and a server
//! started after a crash or a reboot runs it (through a client, as fuxth
//! always runs) to bring the session back with the ids it had.
use super::{MAIN, Session, basename};
use crate::command::AnyRef;
use crate::id::{PaneId, WsId};
use crate::layout::{Axis, Split, Tree};
use crate::pane::{Launch, Runs};
use crate::workspace::{Fit, Tab, Workspace};

impl Session {
    /// What there is, or `only` that tab or workspace (a pane's: its tab),
    /// as a fuxth program; with `ids`, the ids to ask for, as a saved
    /// session keeps them.
    pub(super) fn dump_layout(&self, only: Option<AnyRef<WsId>>, ids: bool) -> String {
        let only = match only {
            Some(AnyRef::Pane(pane)) => self.locate(pane).map(|(_, tab)| AnyRef::Tab(tab)),
            other => other,
        };
        let mut out = String::from("# fux: running this makes it again\n");
        for ws in &self.workspaces {
            if matches!(only, Some(AnyRef::Workspace(w)) if w != ws.id) {
                continue;
            }
            for (index, tab) in ws.tabs().iter().enumerate() {
                if matches!(only, Some(AnyRef::Tab(t)) if t != tab.id) {
                    continue;
                }
                let Some(root) = tab.root() else {
                    continue;
                };
                let whole = !matches!(only, Some(AnyRef::Tab(_)));
                self.dump_tab(ws, (index, tab), root, (whole, ids), &mut out);
            }
        }
        out
    }

    /// The lines that make tab `tab`, the `index`th of `ws`, and its panes:
    /// with `whole`, as part of its workspace.
    fn dump_tab(
        &self,
        ws: &Workspace,
        (index, tab): (usize, &Tab),
        root: &Tree,
        (whole, ids): (bool, bool),
        out: &mut String,
    ) {
        let Some(first) = root.first_pane() else {
            return;
        };
        let opens = whole && index == 0;
        let program = self.program(first);
        let default_name = match program {
            Some(launch) => launch.argv.first().map(|p| basename(p)).unwrap_or_default(),
            None if index == 0 => MAIN.to_owned(),
            None => format!("tab-{}", index.saturating_add(1)),
        };
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
        let named = tab.name != default_name;
        if named && !makes {
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
        line.extend(["constant".to_owned(), constant(first)]);
        push(out, &line);
        if opens {
            push(
                out,
                &[
                    constant(first),
                    "workspace".into(),
                    "constant".into(),
                    workspace(ws.id),
                ],
            );
        }
        if named && makes {
            let mut rename = vec![constant(first), "tab".to_owned()];
            rename.extend(string(&tab.name).chain(["rename".to_owned()]));
            push(out, &rename);
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
                let name = constant(*pane);
                push(
                    out,
                    &[name.clone(), size.cols().to_string(), "resize-x".into()],
                );
                push(out, &[name, size.rows().to_string(), "resize-y".into()]);
            }
        }
        // Panes renamed keep their names.
        for pane in root.panes() {
            let Some(p) = self.panes.get(&pane) else {
                continue;
            };
            let default = match self.program(pane) {
                Some(launch) => launch.argv.first().map(|a| basename(a)),
                None => self.config.shell.first().map(|s| basename(s)),
            };
            if default.as_deref() != Some(p.name.as_str()) {
                let mut rename = vec![constant(pane)];
                rename.extend(string(&p.name).chain(["rename".to_owned()]));
                push(out, &rename);
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
            let mut line = vec![constant(*before), "opts".into(), way.into()];
            self.pane_words(*pane, ids, &mut line);
            line.extend(["split".to_owned(), "constant".to_owned(), constant(*pane)]);
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
        match self.program(id) {
            Some(launch) => {
                line.extend(string(&launch.cwd.to_string_lossy()).chain(["cwd".to_owned()]));
                for (key, value) in &launch.env {
                    line.extend(string(key).chain(string(value)).chain(["env".to_owned()]));
                }
                if ids {
                    line.extend(asked(&id.to_string()));
                }
                let mut words = launch.argv.iter();
                if let Some(program) = words.next() {
                    line.extend(string(program).chain(["program".to_owned()]));
                }
                for arg in words {
                    line.extend(string(arg).chain(["arg".to_owned()]));
                }
            }
            None => {
                let cwd = self.cwd_for(super::Here::Cli(None, None, None), Some(id));
                line.extend(string(&cwd.to_string_lossy()).chain(["cwd".to_owned()]));
                if ids {
                    line.extend(asked(&id.to_string()));
                }
            }
        }
    }

    /// The program a server saves: everything, as it is now, its ids kept.
    pub fn saved_layout(&self) -> String {
        self.dump_layout(None, true)
    }
}

/// The constant a dump names pane `id` by.
fn constant(id: PaneId) -> String {
    format!("p{}", id.number())
}

/// The constant a dump names workspace `id` by.
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
fn push(out: &mut String, line: &[String]) {
    let words: Vec<String> = line.iter().map(|w| crate::screens::quote(w)).collect();
    out.push_str(&words.join(" "));
    out.push('\n');
}
