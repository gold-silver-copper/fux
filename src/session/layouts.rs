//! What a session holds, written as the fuxth program that makes it again
//! (`dump`): each pane made becomes a constant (`p5`) that later lines name.
//! Its layouts and launches, never its processes' state.
use super::Session;
use crate::command::AnyRef;
use crate::id::{PaneId, WsId};
use crate::layout::{Axis, Tree};
use crate::pane::{Launch, Runs};
use crate::workspace::{Fit, Tab, Workspace};

impl Session {
    /// What there is, or `only` that tab or workspace (a pane's: its tab),
    /// as a fuxth program.
    pub(crate) fn dump_layout(&self, only: Option<AnyRef>) -> Result<String, super::Error> {
        // Quote expansion is at most four bytes per input byte. Charge
        // launch words and per-pane syntax before building any source.
        let mut left = crate::wire::MAX_REPLY / 2;
        let mut charge = |bytes: usize| -> Result<(), super::Error> {
            left = left
                .checked_sub(bytes)
                .ok_or_else(crate::wire::Budget::exceeded)?;
            Ok(())
        };
        let expanded = |text: &str| {
            text.len()
                .checked_mul(4)
                .and_then(|n| n.checked_add(64))
                .ok_or_else(crate::wire::Budget::exceeded)
        };
        for ws in self.parts.workspaces() {
            for tab in ws
                .tabs()
                .iter()
                .filter(|t| self.takes_in(only, ws.id(), t.id()))
            {
                charge(expanded(ws.name().unwrap_or(""))?)?;
                charge(expanded(tab.name().unwrap_or(""))?)?;
                let mut result: Result<(), super::Error> = Ok(());
                tab.root().for_each_pane(&mut |id| {
                    if result.is_err() {
                        return;
                    }
                    result = (|| {
                        charge(2048)?;
                        if let Some(pane) = self.parts.panes().get(&id) {
                            charge(expanded(pane.name().unwrap_or(""))?)?;
                        }
                        if let Some(launch) = self.program(id) {
                            charge(
                                launch
                                    .cwd
                                    .as_os_str()
                                    .len()
                                    .checked_mul(4)
                                    .ok_or_else(crate::wire::Budget::exceeded)?,
                            )?;
                            for word in launch
                                .argv
                                .iter()
                                .chain(launch.env.iter().flat_map(|(k, v)| [k, v]))
                            {
                                charge(expanded(word)?)?;
                            }
                        }
                        Ok(())
                    })();
                });
                result?;
            }
        }
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
        Ok(out)
    }

    /// The lines that make tab `tab` of `ws`, and its panes:
    /// with `whole`, as part of its workspace, which it `opens` if it is the
    /// first written.
    fn dump_tab(&self, ws: &Workspace, tab: &Tab, whole: bool, opens: bool, out: &mut String) {
        let (root, first) = (tab.root(), tab.root().first_pane());
        let program = self.program(first);
        // An unnamed workspace is recreated as a new workspace, never
        // found by its display label. Later tabs use its new handle.
        let creates = (opens || !whole) && ws.name().is_none();
        let mut line = vec!["opts".to_owned()];
        if whole && !opens {
            line.extend([workspace(ws.id()), "in".to_owned()]);
        } else if let Some(name) = ws.name() {
            line.extend(string(name).chain(["in".to_owned()]));
        } else if program.is_some() {
            line.extend(string("new-workspace").chain(["in".to_owned()]));
        }
        // Its size, which a server with no client attached needs to make
        // it: one that fits clients is made at the size it has, then fits
        // clients again (`resize-auto`).
        let size = tab.fit().size();
        let (cols, rows) = (size.cols().to_string(), size.rows().to_string());
        line.extend([cols, rows, "size".to_owned()]);
        self.pane_words(first, &mut line, out);
        line.push(
            if creates && program.is_none() {
                "new-workspace"
            } else if program.is_some() {
                "run"
            } else {
                "new-tab"
            }
            .to_owned(),
        );
        let named = pane_name(first);
        line.extend(["let".to_owned(), named.clone()]);
        push(out, &line);
        if let Some(name) = tab.name() {
            let mut line = vec![named.clone(), "tab".to_owned()];
            line.extend(string(name).chain(["rename".to_owned()]));
            push(out, &line);
        }
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
            if let Some(name) = p.name() {
                let mut line = vec![pane_name(pane)];
                line.extend(string(name).chain(["rename".to_owned()]));
                push(out, &line);
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
            Runs::Explicit(launch) => Some(launch),
            Runs::DefaultShell => None,
        }
    }

    /// The options a line gives pane `id`: where it starts, and a program's
    /// variables and words. A directory that is not text, which a fuxth
    /// string cannot hold, or a shell's that cannot be read, is left out,
    /// never written changed or guessed: a comment in `out` says so, and
    /// the line starts it where it runs.
    fn pane_words(&self, id: PaneId, line: &mut Vec<String>, out: &mut String) {
        let program = self.program(id);
        let cwd = program.map(|launch| launch.cwd.clone());
        let cwd = cwd.or_else(|| self.dir_of(id));
        match cwd.as_deref().map(|cwd| (cwd, cwd.to_str())) {
            Some((_, Some(cwd))) => line.extend(string(cwd).chain(["cwd".to_owned()])),
            Some((cwd, None)) => out.push_str(&format!(
                "# {id}: its directory {cwd:?} is not UTF-8 text, so it starts where this runs\n"
            )),
            None => out.push_str(&format!(
                "# {id}: its directory is unknown, so it starts where this runs\n"
            )),
        }
        for (key, value) in program.iter().flat_map(|launch| &launch.env) {
            line.extend(string(key).chain(string(value)).chain(["env".to_owned()]));
        }
        if let Some(launch) = program {
            for (i, word) in launch.argv.iter().enumerate() {
                let what = if i == 0 { "program" } else { "arg" };
                line.extend(string(word).chain([what.to_owned()]));
            }
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
