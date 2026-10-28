//! The session's layouts and launch intent, streamed as a replayable program.
use super::Session;
use crate::command::AnyRef;
use crate::id::PaneId;
use crate::layout::{Axis, Tree};
use crate::pane::{Launch, Runs};
use crate::workspace::{Fit, Tab, Workspace};

impl Session {
    pub(crate) fn dump_layout(&self, only: Option<AnyRef>) -> Result<String, super::Error> {
        self.dump_with(only, super::DUMP_BYTES.min(crate::wire::MAX_REPLY))
    }

    fn dump_with(&self, only: Option<AnyRef>, limit: usize) -> Result<String, super::Error> {
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
                self.dump_tab(ws, tab, whole, whole && i == 0, &mut out)?;
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
    ) -> Result<(), super::Error> {
        let (root, first) = (tab.root(), tab.root().first_pane());
        let explicit = self.program(first).is_some();
        let creates = (opens || !whole) && ws.name().is_none();
        self.dump_launch_line(
            first,
            out,
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
        self.dump_tree(root, out)?;
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

    fn dump_tree(&self, tree: &Tree, out: &mut Source) -> Result<(), super::Error> {
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
            |out| {
                out.pane(first.first_pane())?;
                out.string(way)?;
                out.word("axis")?;
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
        self.dump_tree(first, out)?;
        self.dump_tree(second, out)
    }

    fn program(&self, id: PaneId) -> Option<&Launch> {
        match &self.parts.panes().get(&id)?.runs {
            Runs::Explicit(launch) => Some(launch),
            Runs::Shell(_) => None,
        }
    }

    /// Restore shells in their observed cwd and programs in their saved launch cwd.
    fn dump_launch_line(
        &self,
        id: PaneId,
        out: &mut Source,
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
        let observed = if shell {
            self.dir_with(id, &mut out.budget)?.ok().flatten()
        } else {
            None
        };
        let result = (|| {
            if shell && observed.is_none() {
                out.format(format_args!(
                    "# {id}: its current directory is unknown, so it starts in its launch directory\n"
                ))?;
            }
            let cwd = observed.as_deref().unwrap_or(&launch.cwd);
            if cwd.to_str().is_none() {
                out.format(format_args!(
                    "# {id}: its directory {cwd:?} is not UTF-8 text, so it starts where this runs\n"
                ))?;
            }
            for (key, value) in launch.env.as_slice() {
                if value.as_ref().is_some_and(|v| v.to_str().is_none()) {
                    out.format(format_args!(
                        "# {id}: environment {key} is not UTF-8 text; its saved value is omitted\n"
                    ))?;
                }
            }
            prefix(out)?;
            out.native(format_args!("{}", pane.history_bytes()))?;
            out.word("history-bytes")?;
            out.word(if pane.options().reflow {
                "true"
            } else {
                "false"
            })?;
            out.word("reflow")?;
            out.native(format_args!(
                "{}",
                pane.options().alternate_scroll_lines.get()
            ))?;
            out.word("alternate-scroll-lines")?;
            out.native(format_args!("{}", pane.options().picture_bytes.get()))?;
            out.word("picture-bytes")?;
            out.word(if pane.options().program_clipboard {
                "true"
            } else {
                "false"
            })?;
            out.word("program-clipboard")?;
            out.word("list")?;
            out.word("end")?;
            out.word("copy-environment")?;
            if let Some(cwd) = cwd.to_str() {
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
    use crate::command::Program;

    #[test]
    fn dump_admits_saved_launch_directory_expansion() -> Result<(), String> {
        let component: String = std::iter::repeat_n('\'', 200).collect();
        let directory = std::path::PathBuf::from(format!(
            "/{}",
            std::iter::repeat_n(component, 17)
                .collect::<Vec<_>>()
                .join("/")
        ));
        let mut session = super::Session::new(
            "/nonexistent/fux.sock".into(),
            false,
            crate::command::Run::now().ok_or("clock")?,
        );
        let initial = crate::command::InitialWorkspace {
            options: crate::script::default_state()
                .map_err(|e| e.to_string())?
                .preferences
                .pane_options(),
            name: None,
            start: crate::command::Start::Shell(Program::new("/bin/sh".into(), Vec::new())?),
            cwd: directory,
            env: crate::command::EnvPatch::default(),
        };
        session
            .start(
                fux_vt::Size::new(24, 80).map_err(|e| e.to_string())?,
                &initial,
            )
            .map_err(|e| e.to_string())?;
        let result = session.dump_with(None, 8192);
        assert!(
            result.is_err(),
            "dump built past its allowance: {} bytes",
            result.as_ref().map_or(0, String::len)
        );
        Ok(())
    }
}
