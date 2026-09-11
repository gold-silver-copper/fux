use clap::{Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum TitleMode {
    Never,
    #[default]
    Prefix,
    Replace,
}

/// Run a command in a pseudoterminal and publish its observed agent state as OSC 7877.
#[derive(Debug, Parser)]
#[command(name = "zor-wrap", version)]
pub struct Cli {
    /// Also write event lines to a unix socket or fifo (`-` selects fd 3).
    #[arg(long)]
    pub events: Option<PathBuf>,
    /// How to touch the child's OSC 0/2 window title.
    #[arg(long, value_enum, default_value_t = TitleMode::Prefix)]
    pub title: TitleMode,
    /// Never emit the state OSC; title updates only.
    #[arg(long)]
    pub no_osc: bool,
    /// Extra rule files or directories; later files win on the same agent id.
    #[arg(long)]
    pub rules: Vec<PathBuf>,
    /// Skip identification and force one rule set.
    #[arg(long)]
    pub agent: Option<String>,
    /// Dump matched rules and machine events to stderr.
    #[arg(long)]
    pub debug: bool,
    /// Command and arguments to wrap (default: `$SHELL -l`).
    #[arg(allow_hyphen_values = true)]
    pub command: Vec<String>,
}
