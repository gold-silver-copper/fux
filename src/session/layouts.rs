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
        self.dump_with(only, crate::wire::MAX_REPLY, &mut |id| self.dir_of(id))
    }

    fn dump_with(
        &self,
        only: Option<AnyRef>,
        limit: usize,
        directories: &mut impl FnMut(PaneId) -> Option<std::path::PathBuf>,
    ) -> Result<String, super::Error> {
        let empty = crate::protocol::Reply {
            run: Some(self.run),
            ..crate::protocol::Body::Text(String::new()).into()
        };
        let header = crate::wire::size(&empty).map_err(|_| crate::wire::Budget::exceeded())?;
        let mut out = Source::new(
            limit
                .checked_sub(header)
                .ok_or_else(crate::wire::Budget::exceeded)?,
        );
        out.format(format_args!(
            "{}\n# fux: running this makes it again\n",
            crate::script::SHEBANG
        ))?;
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
                self.dump_tab(ws, tab, whole, whole && i == 0, &mut out, directories)?;
            }
        }
        Ok(out.text)
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
        out: &mut Source,
        directories: &mut impl FnMut(PaneId) -> Option<std::path::PathBuf>,
    ) -> Result<(), super::Error> {
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
        self.pane_words(first, &mut line, out, directories)?;
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
        push(out, &line)?;
        if let Some(name) = tab.name() {
            let mut line = vec![named.clone(), "tab".to_owned()];
            line.extend(string(name).chain(["rename".to_owned()]));
            push(out, &line)?;
        }
        if opens {
            push(out, &[&named, "workspace", "let", &workspace(ws.id())])?;
        }
        if matches!(tab.fit(), Fit::Clients(_)) {
            push(out, &[&named, "tab", "resize-auto"])?;
        }
        self.dump_tree(root, out, directories)?;
        // Panes renamed keep their names.
        for pane in root.panes() {
            let Some(p) = self.parts.panes().get(&pane) else {
                continue;
            };
            if let Some(name) = p.name() {
                let mut line = vec![pane_name(pane)];
                line.extend(string(name).chain(["rename".to_owned()]));
                push(out, &line)?;
            }
        }
        Ok(())
    }

    /// The `split` lines that make `tree` from its first pane: each
    /// divides what its first child's first pane has with its second's,
    /// as its shares say, and then each child is made from those. So the
    /// layout made again is the one there is, share for share.
    fn dump_tree(
        &self,
        tree: &Tree,
        out: &mut Source,
        directories: &mut impl FnMut(PaneId) -> Option<std::path::PathBuf>,
    ) -> Result<(), super::Error> {
        let Tree::Split(split) = tree else {
            return Ok(());
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
        self.pane_words(pane, &mut line, out, directories)?;
        line.extend(["split".to_owned(), "let".to_owned(), pane_name(pane)]);
        push(out, &line)?;
        self.dump_tree(first, out, directories)?;
        self.dump_tree(second, out, directories)?;
        Ok(())
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
    fn pane_words(
        &self,
        id: PaneId,
        line: &mut Vec<String>,
        out: &mut Source,
        directories: &mut impl FnMut(PaneId) -> Option<std::path::PathBuf>,
    ) -> Result<(), super::Error> {
        let program = self.program(id);
        let cwd = program.map(|launch| launch.cwd.clone());
        let cwd = cwd.or_else(|| directories(id));
        match cwd.as_deref().map(|cwd| (cwd, cwd.to_str())) {
            Some((_, Some(cwd))) => line.extend(string(cwd).chain(["cwd".to_owned()])),
            Some((cwd, None)) => out.format(format_args!(
                "# {id}: its directory {cwd:?} is not UTF-8 text, so it starts where this runs\n"
            ))?,
            None => out.format(format_args!(
                "# {id}: its directory is unknown, so it starts where this runs\n"
            ))?,
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
        Ok(())
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
fn push(out: &mut Source, line: &[impl AsRef<str>]) -> Result<(), super::Error> {
    for (i, word) in line.iter().enumerate() {
        if i > 0 {
            out.push(" ")?;
        }
        out.format(format_args!("{}", crate::words::Quoted(word.as_ref())))?;
    }
    out.push("\n")
}

/// Quoted source and diagnostic formatting share one checked output sink.
struct Source {
    text: String,
    limit: usize,
    budget: crate::wire::Budget,
}
impl Source {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            limit,
            budget: crate::wire::Budget::new(),
        }
    }
    fn push(&mut self, text: &str) -> Result<(), super::Error> {
        if self
            .text
            .len()
            .checked_add(text.len())
            .is_none_or(|n| n > self.limit)
        {
            return Err(crate::wire::Budget::exceeded());
        }
        self.budget.text(&mut self.text, text)
    }
    fn format(&mut self, args: std::fmt::Arguments<'_>) -> Result<(), super::Error> {
        std::fmt::write(self, args).map_err(|_| crate::wire::Budget::exceeded())
    }
}
impl std::fmt::Write for Source {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.push(text).map_err(|_| std::fmt::Error)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dump_admits_observed_shell_directory_expansion() -> Result<(), String> {
        let session = crate::session::testing::started(crate::config::Config::default())?;
        let component: String = std::iter::repeat_n('\'', 200).collect();
        let directory = std::path::PathBuf::from(format!(
            "/{}",
            std::iter::repeat_n(component, 17)
                .collect::<Vec<_>>()
                .join("/")
        ));
        let mut reads = 0;
        let result = session.dump_with(None, 8192, &mut |_| {
            reads += 1;
            Some(directory.clone())
        });
        assert!(
            result.is_err(),
            "dump built past its allowance: {} bytes",
            result.as_ref().map_or(0, String::len)
        );
        assert_eq!(reads, 1, "directory must be observed once");
        Ok(())
    }
}
