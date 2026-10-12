//! Layout files: what a session holds, written as the fux commands that
//! make it (`fux dump-layout`), and made again from them, all or nothing
//! (`fux layout`). A server saves its whole session as one as it changes,
//! and a server started after a crash or a reboot makes it again with the
//! ids it had (`Session::restore`).
//!
//! A layout file is fux command lines, as a config file is: `new-workspace`,
//! `new-tab`, `run` and `split --run` make what they made, `rename`,
//! `resize` and `resize-pane` change it. `--id ID` says which id a line's
//! pane, tab or workspace has in the file: later lines name it by that id,
//! and fux puts the id it makes in its place.
use super::{Caller, Error, Here, MAIN, Origin, RUN_WORKSPACE, Reach, Session, basename};
use crate::command::{self, AnyRef, Command, Kind, WsRef};
use crate::id::{PaneId, TabId, WsId};
use crate::json::Json;
use crate::layout::{Axis, Split, Tree};
use crate::pane::{Launch, Runs};
use crate::words;
use crate::workspace::{Fit, Tab, Workspace};
use std::collections::{BTreeSet, HashMap};

/// The largest layout file read.
pub const MAX_LAYOUT: u64 = 1 << 20;

/// What there is of each kind: before and after a line, to tell what it
/// made.
#[derive(Default)]
struct Made {
    workspaces: BTreeSet<WsId>,
    tabs: BTreeSet<TabId>,
    panes: BTreeSet<PaneId>,
}

impl Session {
    fn made(&self) -> Made {
        let mut made = Made::default();
        for ws in &self.workspaces {
            made.workspaces.insert(ws.id);
            for tab in ws.tabs() {
                made.tabs.insert(tab.id);
            }
        }
        made.panes.extend(self.panes.keys().copied());
        made
    }

    /// What there is, or `only` that tab or workspace (a pane's: its
    /// tab), as a layout file. A whole workspace begins with the line that
    /// makes it, a tab alone with one that makes it where the file is
    /// applied.
    pub(super) fn dump_layout(&self, only: Option<AnyRef<WsId>>) -> String {
        let only = match only {
            Some(AnyRef::Pane(pane)) => self.locate(pane).map(|(_, tab)| AnyRef::Tab(tab)),
            other => other,
        };
        let mut out = String::from("# fux layout: `fux layout FILE` makes it again\n");
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
                self.dump_tab(ws, (index, tab), root, whole, &mut out);
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
        whole: bool,
        out: &mut String,
    ) {
        let Some(first) = root.first_pane() else {
            return;
        };
        let opens = whole && index == 0;
        let program = self.program(first);
        let mut line: Vec<String> = Vec::new();
        let default_name = match program {
            Some(launch) => launch.argv.first().map(|p| basename(p)).unwrap_or_default(),
            None if index == 0 => MAIN.to_owned(),
            None => format!("tab-{}", index.saturating_add(1)),
        };
        match (program, opens) {
            (Some(_), true) => line.extend(["run".into(), "-t".into(), ws.name.clone()]),
            (Some(_), false) => line.push("run".into()),
            (None, true) => line.extend(["new-workspace".into(), "-n".into(), ws.name.clone()]),
            (None, false) => line.push("new-tab".into()),
        }
        if whole && !opens {
            line.extend(["-t".into(), ws.id.to_string()]);
        }
        let named = tab.name != default_name;
        if named && !(program.is_none() && opens) {
            line.extend(["-n".into(), tab.name.clone()]);
        }
        if let Fit::Fixed(size) = tab.fit() {
            line.extend(["--size".into(), format!("{}x{}", size.cols(), size.rows())]);
        }
        if opens {
            line.extend(["--id".into(), ws.id.to_string()]);
        }
        line.extend(["--id".into(), tab.id.to_string()]);
        self.pane_words(first, &mut line);
        push(out, &line);
        if named && program.is_none() && opens {
            push(
                out,
                &[
                    "rename".into(),
                    "-t".into(),
                    tab.id.to_string(),
                    tab.name.clone(),
                ],
            );
        }
        if let Tree::Split(axis, split) = root {
            self.dump_split(*axis, split, out);
        }
        // Each pane's size, for the shares of its splits; the last takes
        // what is left.
        let panes = root.panes();
        if let Some((_, sized)) = panes.split_last() {
            for pane in sized {
                let Some(size) = self.panes.get(pane).map(|p| p.size()) else {
                    continue;
                };
                let (cols, rows) = (size.cols().to_string(), size.rows().to_string());
                let line = [
                    "resize-pane",
                    "-t",
                    &pane.to_string(),
                    "-x",
                    &cols,
                    "-y",
                    &rows,
                ];
                push(out, &line.map(str::to_owned));
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
                push(
                    out,
                    &[
                        "rename".into(),
                        "-t".into(),
                        pane.to_string(),
                        p.name.clone(),
                    ],
                );
            }
        }
    }

    /// The `split` lines that make `split`'s children beside its first,
    /// along `axis`, and then each child's own.
    fn dump_split(&self, axis: Axis, split: &Split, out: &mut String) {
        let firsts: Vec<PaneId> = split.children().filter_map(Tree::first_pane).collect();
        let flag = match axis {
            Axis::Horizontal => "-h",
            Axis::Vertical => "-v",
        };
        for (before, pane) in firsts.iter().zip(firsts.iter().skip(1)) {
            let mut line = vec!["split".into(), flag.into(), "-t".into(), before.to_string()];
            if self.program(*pane).is_some() {
                line.push("--run".into());
            }
            self.pane_words(*pane, &mut line);
            push(out, &line);
        }
        for child in split.children() {
            if let Tree::Split((), inner) = child {
                self.dump_split(axis.cross(), inner, out);
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

    /// What a line says of pane `id`: where it starts, its id, and a
    /// program's variables and words, last.
    fn pane_words(&self, id: PaneId, line: &mut Vec<String>) {
        match self.program(id) {
            Some(launch) => {
                line.extend(["--cwd".into(), launch.cwd.to_string_lossy().into_owned()]);
                for (key, value) in &launch.env {
                    line.extend(["--env".into(), format!("{key}={value}")]);
                }
                line.extend(["--id".into(), id.to_string(), "--".into()]);
                line.extend(launch.argv.iter().cloned());
            }
            None => {
                let cwd = self.cwd_for(Here::Cli(None, None, None), Some(id));
                line.extend(["--cwd".into(), cwd.to_string_lossy().into_owned()]);
                line.extend(["--id".into(), id.to_string()]);
            }
        }
    }

    /// `fux layout`: what `text` says, made all or nothing; its new tabs go
    /// to `workspace` (`agent`) unless they say where. With `again` (a
    /// restore), each id is the one the file gives, and a program waits
    /// for `respawn`. The ids each line made, a line each.
    pub(super) fn apply_layout(
        &mut self,
        text: &str,
        workspace: Option<&WsRef>,
        origin: &Origin,
        (again, json): (bool, bool),
    ) -> Result<String, Error> {
        // Every line is read before any runs: one that does not read makes
        // nothing.
        let mut lines = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let at = index.saturating_add(1);
            let argv = words::split(line).map_err(|e| Error::LayoutLine(at, e.to_string()))?;
            if argv.is_empty() {
                continue;
            }
            match command::parse(&argv) {
                Ok(
                    Command::NewWorkspace { .. }
                    | Command::NewTab { .. }
                    | Command::Run { then: None, .. }
                    | Command::Split { .. }
                    | Command::Rename { .. }
                    | Command::Resize { .. }
                    | Command::ResizePane { .. },
                ) => lines.push((at, argv)),
                Ok(_) => {
                    let only = "a layout is made by new-workspace, new-tab, run, split, \
                                rename, resize and resize-pane alone";
                    return Err(Error::LayoutLine(at, only.to_owned()));
                }
                // A tab's name is the server's to read: as the line runs.
                Err(command::Usage::Not(Kind::Pane | Kind::Tab, _)) => lines.push((at, argv)),
                Err(usage) => return Err(Error::LayoutLine(at, usage.to_string())),
            }
        }
        let place = match workspace {
            Some(WsRef::Id(id)) => id.to_string(),
            Some(WsRef::Name(name)) => name.clone(),
            None => RUN_WORKSPACE.to_owned(),
        };
        let scope = match origin {
            Origin::Cli(Caller { scope, .. }) => scope.clone(),
            Origin::Client(_) | Origin::Nowhere => None,
        };
        let mut ids: HashMap<String, String> = HashMap::new();
        let mut made: Vec<AnyRef<WsId>> = Vec::new();
        let mut said = Vec::new();
        for (at, argv) in lines {
            let result =
                self.layout_line(&argv, &place, (origin, scope.as_deref()), again, &mut ids);
            match result {
                Ok(line_made) => {
                    if !line_made.is_empty() {
                        said.push(line_made.clone());
                    }
                    made.extend(line_made);
                }
                Err(error) => {
                    self.unmake(&made);
                    return Err(Error::LayoutLine(at, error.to_string()));
                }
            }
        }
        if json {
            let lines = said.iter().map(|ids| {
                Json::Array(
                    ids.iter()
                        .map(|id| Json::str(super::describe(*id)))
                        .collect(),
                )
            });
            return Ok(format!("{}\n", Json::Array(lines.collect()).render()));
        }
        let lines = said.iter().map(|ids| {
            let ids: Vec<String> = ids.iter().map(|id| super::describe(*id)).collect();
            format!("{}\n", ids.join(" "))
        });
        Ok(lines.collect())
    }

    /// Runs one line of a layout file: the file's ids in it as what they
    /// were made, a new tab with no place put at `place`, checked against a
    /// grant's `scope`. What it made.
    fn layout_line(
        &mut self,
        argv: &[String],
        place: &str,
        (origin, scope): (&Origin, Option<&str>),
        again: bool,
        ids: &mut HashMap<String, String>,
    ) -> Result<Vec<AnyRef<WsId>>, Error> {
        let mut words: Vec<String> = Vec::new();
        let mut asked: Vec<String> = Vec::new();
        let mut rest = argv.iter();
        while let Some(word) = rest.next() {
            if word == "--" {
                words.push(word.clone());
                words.extend(rest.by_ref().cloned());
                break;
            }
            if word == "--id" {
                let Some(id) = rest.next() else {
                    words.push(word.clone());
                    continue;
                };
                asked.push(id.clone());
                // The file's own ids, kept only by a restore.
                if again {
                    words.extend([word.clone(), id.clone()]);
                }
                continue;
            }
            words.push(ids.get(word).cloned().unwrap_or_else(|| word.clone()));
        }
        let mut command =
            command::parse(&self.named(&words).map_err(Error::NotNamed)?).map_err(Error::Usage)?;
        let placed = matches!(
            command,
            Command::NewTab { target: None, .. }
                | Command::Run {
                    workspace: None,
                    ..
                }
        );
        if placed {
            let mut rest = std::mem::take(&mut words).into_iter();
            words.extend(rest.next());
            words.extend(["-t".to_owned(), place.to_owned()]);
            words.extend(rest);
            command = command::parse(&words).map_err(Error::Usage)?;
        }
        if let Some(scope) = scope {
            let reach = Reach {
                session: self,
                ws: self.resolve_ws(&WsRef::Name(scope.to_owned())).ok(),
                scope,
                here: self.here(origin)?,
            };
            reach.command(&command)?;
        }
        let before = self.made();
        let executed = self.execute(&command, origin);
        self.ids.unwant();
        // A size that cannot be had where the file is made (a smaller
        // tab) is left as it comes.
        match executed {
            Err(_) if matches!(command, Command::ResizePane { .. }) => {}
            other => {
                other?;
            }
        }
        let after = self.made();
        let new: Vec<AnyRef<WsId>> = (after.workspaces.difference(&before.workspaces))
            .map(|w| AnyRef::Workspace(*w))
            .chain(after.tabs.difference(&before.tabs).map(|t| AnyRef::Tab(*t)))
            .chain(
                after
                    .panes
                    .difference(&before.panes)
                    .map(|p| AnyRef::Pane(*p)),
            )
            .collect();
        // Each id the line asked for is the one of its kind it made.
        for id in asked {
            let kind = match id.chars().next() {
                Some('+') => Kind::Workspace,
                Some('@') => Kind::Tab,
                Some('%') => Kind::Pane,
                _ => continue,
            };
            if let Some(made) = new.iter().find(|m| m.kind() == kind) {
                ids.insert(id, super::describe(*made));
            }
        }
        if again {
            for pane in new.iter().filter_map(|m| match m {
                AnyRef::Pane(p) => Some(*p),
                AnyRef::Tab(_) | AnyRef::Workspace(_) => None,
            }) {
                self.tell_paused(pane);
            }
        }
        Ok(new)
    }

    /// A program a restore brought back says, on its screen, how to start
    /// it.
    fn tell_paused(&mut self, pane: PaneId) {
        let Some(launch) = self.program(pane) else {
            return;
        };
        let words = words::join(&launch.argv);
        let text = format!(
            "fux brought this pane back after its server stopped; its program waits:\r\n  \
             {words}\r\n`fux respawn -t {pane}` starts it.\r\n"
        );
        self.output(pane, text.as_bytes());
    }

    /// Closes what a layout made, last first, after one of its lines
    /// failed.
    fn unmake(&mut self, made: &[AnyRef<WsId>]) {
        for item in made.iter().rev() {
            match *item {
                AnyRef::Pane(pane) => self.close_pane(pane, None),
                AnyRef::Tab(tab) => self.remove_tab(tab),
                AnyRef::Workspace(ws) => self.remove_workspace(ws),
            }
        }
        self.after_close();
    }

    /// Makes again what a server held, from the layout file it saved: the
    /// same ids, programs waiting for `respawn`, shells fresh in their
    /// directories. How many panes it brought back; none, and nothing
    /// made, if any line failed.
    pub fn restore(&mut self, text: &str) -> Result<usize, Error> {
        self.restoring = true;
        let restored = self.apply_layout(text, None, &Origin::Nowhere, (true, false));
        self.restoring = false;
        self.touch();
        self.settle();
        restored.map(|_| self.panes.len())
    }

    /// The layout file a server saves: everything, as it is now.
    pub fn saved_layout(&self) -> String {
        self.dump_layout(None)
    }
}

/// Appends a command line, its words quoted as `words::split` reads them.
fn push(out: &mut String, line: &[String]) {
    out.push_str(&words::join(line));
    out.push('\n');
}
