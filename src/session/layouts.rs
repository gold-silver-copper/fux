//! The session's layouts and launch intent, streamed as a replayable program.
use super::Session;
use crate::command::AnyRef;
use crate::id::PaneId;
use crate::layout::{Axis, Tree};
use crate::pane::{Launch, Runs};
use crate::workspace::{Fit, Tab, Workspace};

type Directories<'a> = dyn FnMut(PaneId, &mut crate::wire::Budget) -> Result<Option<std::path::PathBuf>, super::Error>
    + 'a;

impl Session {
    pub(crate) fn dump_layout(&self, only: Option<AnyRef>) -> Result<String, super::Error> {
        self.dump_with(
            only,
            super::DUMP_BYTES.min(crate::wire::MAX_REPLY),
            &mut |id, budget| self.dir_with(id, budget),
        )
    }

    fn dump_with(
        &self,
        only: Option<AnyRef>,
        limit: usize,
        directories: &mut Directories<'_>,
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
            "{}\n# fux: running this makes it again\n# Environment patches apply to the server startup environment.\n",
            crate::script::SHEBANG
        ))?;
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
                self.dump_tab(ws, tab, whole, whole && i == 0, &mut out, directories)?;
            }
        }
        Ok(out.text)
    }

    fn dump_tab(
        &self,
        ws: &Workspace,
        tab: &Tab,
        whole: bool,
        opens: bool,
        out: &mut Source,
        directories: &mut Directories<'_>,
    ) -> Result<(), super::Error> {
        let (root, first) = (tab.root(), tab.root().first_pane());
        let explicit = self.program(first).is_some();
        let creates = (opens || !whole) && ws.name().is_none();
        self.dump_launch_line(
            first,
            out,
            directories,
            |out| {
                out.word("opts")?;
                if whole && !opens {
                    out.native(format_args!("w{}", ws.id().number()))?;
                    out.word("in")?;
                } else if let Some(name) = ws.name() {
                    out.string(name)?;
                    out.word("in")?;
                } else if explicit {
                    out.string("new-workspace")?;
                    out.word("in")?;
                }
                let size = tab.fit().size();
                out.native(format_args!("{}", size.cols()))?;
                out.native(format_args!("{}", size.rows()))?;
                out.word("size")
            },
            |out| {
                out.word(if creates && !explicit {
                    "new-workspace"
                } else if explicit {
                    "run"
                } else {
                    "new-tab"
                })?;
                out.word("let")?;
                out.pane(first)
            },
        )?;
        if let Some(name) = tab.name() {
            out.pane(first)?;
            out.word("tab")?;
            out.string(name)?;
            out.word("rename")?;
            out.end()?;
        }
        if opens {
            out.pane(first)?;
            out.word("workspace")?;
            out.word("let")?;
            out.native(format_args!("w{}", ws.id().number()))?;
            out.end()?;
        }
        if matches!(tab.fit(), Fit::Clients(_)) {
            out.pane(first)?;
            out.word("tab")?;
            out.word("resize-auto")?;
            out.end()?;
        }
        self.dump_tree(root, out, directories)?;
        let mut result = Ok(());
        root.for_each_pane(&mut |pane| {
            if result.is_ok()
                && let Some(name) = self.parts.panes().get(&pane).and_then(|p| p.name())
            {
                result = (|| {
                    out.pane(pane)?;
                    out.string(name)?;
                    out.word("rename")?;
                    out.end()
                })();
            }
        });
        result
    }

    fn dump_tree(
        &self,
        tree: &Tree,
        out: &mut Source,
        directories: &mut Directories<'_>,
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
        self.dump_launch_line(
            pane,
            out,
            directories,
            |out| {
                out.pane(first.first_pane())?;
                out.word(way)?;
                out.native(format_args!("{a}"))?;
                out.native(format_args!("{b}"))?;
                out.word("shares")
            },
            |out| {
                out.word("split")?;
                out.word("let")?;
                out.pane(pane)
            },
        )?;
        self.dump_tree(first, out, directories)?;
        self.dump_tree(second, out, directories)
    }

    fn program(&self, id: PaneId) -> Option<&Launch> {
        match &self.parts.panes().get(&id)?.runs {
            Runs::Explicit(launch) => Some(launch),
            Runs::Shell(_) => None,
        }
    }

    /// Observe once before writing the line, so a diagnostic precedes it.
    /// Launch intent is borrowed; an observed path stays charged until dropped.
    fn dump_launch_line(
        &self,
        id: PaneId,
        out: &mut Source,
        directories: &mut Directories<'_>,
        prefix: impl FnOnce(&mut Source) -> Result<(), super::Error>,
        suffix: impl FnOnce(&mut Source) -> Result<(), super::Error>,
    ) -> Result<(), super::Error> {
        let pane = self
            .parts
            .panes()
            .get(&id)
            .ok_or(super::Error::NoPane(id))?;
        let (launch, shell) = match &pane.runs {
            Runs::Shell(launch) => (launch, true),
            Runs::Explicit(launch) => (launch, false),
        };
        let observed = directories(id, &mut out.budget)?;
        let cwd = observed.as_deref().or(Some(launch.cwd.as_path()));
        let result = (|| {
            match cwd {
                Some(path) if path.to_str().is_none() => out.format(format_args!("# {id}: its directory {path:?} is not UTF-8 text, so it starts where this runs\n"))?,
                None => out.format(format_args!("# {id}: its directory is unknown, so it starts where this runs\n"))?,
                Some(_) => {}
            }
            for (key, value) in launch.env.as_slice() {
                if value.as_ref().is_some_and(|v| v.to_str().is_none()) {
                    out.format(format_args!(
                        "# {id}: environment {key} is not UTF-8 text; its saved value is omitted\n"
                    ))?;
                }
            }
            prefix(out)?;
            out.word("list")?;
            out.word("end")?;
            out.word("copy-environment")?;
            if let Some(cwd) = cwd.and_then(|p| p.to_str()) {
                out.string(cwd)?;
                out.word("cwd")?;
            }
            for (key, value) in launch.env.as_slice() {
                match value {
                    None => {
                        out.string(key)?;
                        out.word("unset-env")?;
                    }
                    Some(value) => {
                        if let Some(value) = value.to_str() {
                            out.string(key)?;
                            out.string(value)?;
                            out.word("env")?;
                        }
                    }
                }
            }
            if shell {
                out.word("list")?;
                for word in launch.argv.iter() {
                    out.string(word)?;
                }
                out.word("end")?;
                out.word("shell-program")?;
            } else {
                for (i, word) in launch.argv.iter().enumerate() {
                    out.string(word)?;
                    out.word(if i == 0 { "program" } else { "arg" })?;
                }
            }
            suffix(out)?;
            out.end()
        })();
        let capacity = observed.as_ref().map_or(0, std::path::PathBuf::capacity);
        drop(observed);
        out.budget.release(capacity)?;
        result
    }
}

/// Quoting, native tokens and diagnostics share one checked output buffer.
struct Source {
    text: String,
    limit: usize,
    budget: crate::wire::Budget,
    first: bool,
}
impl Source {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            limit,
            budget: crate::wire::Budget::new(),
            first: true,
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
    fn separator(&mut self) -> Result<(), super::Error> {
        if !self.first {
            self.push(" ")?;
        }
        self.first = false;
        Ok(())
    }
    fn word(&mut self, word: &str) -> Result<(), super::Error> {
        self.separator()?;
        self.format(format_args!("{}", crate::words::Quoted(word)))
    }
    /// Only native identifiers and numbers use this unquoted token writer.
    fn native(&mut self, args: std::fmt::Arguments<'_>) -> Result<(), super::Error> {
        self.separator()?;
        self.format(args)
    }
    fn pane(&mut self, id: PaneId) -> Result<(), super::Error> {
        self.native(format_args!("p{}", id.number()))
    }
    fn string(&mut self, text: &str) -> Result<(), super::Error> {
        self.word("str")?;
        self.word(text)
    }
    fn end(&mut self) -> Result<(), super::Error> {
        self.push("\n")?;
        self.first = true;
        Ok(())
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
        let result = session.dump_with(None, 8192, &mut |_, budget| {
            reads += 1;
            fuxix::Allocation::os_string(budget, directory.as_os_str().as_encoded_bytes())
                .map(|p| Some(std::path::PathBuf::from(p)))
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
