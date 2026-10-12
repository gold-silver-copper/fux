//! Shell integration's command marks (OSC 133, FinalTerm's semantic prompts):
//! `C` where a command's output starts, `D ; status` where it ends and how.
//! A host reads them to know when a command typed into a shell is done, its
//! status, and its output ([`Screen::commands`], [`Screen::last_output`]).

use super::Screen;
use crate::RowId;

/// A place in the retained rows: a row, which scrolling carries, and a
/// column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    /// The row.
    pub row: RowId,
    /// The column, from 0.
    pub col: u16,
}

/// What the shell's marks say of its commands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Commands {
    /// A command's output began (`C`) and has not ended (`D`).
    pub running: bool,
    /// How many commands have ended (`D`): a host waits for it to grow.
    pub finished: u64,
    /// The status the last command ended with, if the shell said it.
    pub last_status: Option<i32>,
    /// Where the last command's output began and ended.
    pub last_output: Option<(Mark, Mark)>,
    /// Where the running command's output began.
    output_start: Option<Mark>,
}

impl Screen {
    /// What the shell's marks say of its commands, on the primary screen,
    /// where a shell runs.
    pub fn commands(&self) -> Commands {
        self.commands
    }

    /// `C`, the output of the command just typed begins, or `D [; status]`,
    /// it ended: called with the mark's payload after `133;`.
    pub(crate) fn command_mark(&mut self, command: &[u8], status: Option<&[u8]>) {
        let Some(here) = self.cursor_mark() else {
            return;
        };
        match command {
            b"C" => {
                self.commands.running = true;
                self.commands.output_start = Some(here);
            }
            b"D" => {
                let status = status
                    .and_then(|s| std::str::from_utf8(s).ok())
                    .and_then(|s| s.parse().ok());
                // A D with no C before it ends no command: a shell sends one
                // with every prompt, the first included.
                if let Some(start) = self.commands.output_start.take() {
                    self.commands.last_output = Some((start, here));
                    self.commands.last_status = status;
                    self.commands.finished = self.commands.finished.wrapping_add(1);
                }
                self.commands.running = false;
            }
            _ => {}
        }
    }

    fn cursor_mark(&self) -> Option<Mark> {
        let grid = self.grid();
        let index = grid
            .history_len()
            .checked_add(usize::from(grid.cursor.row()))?;
        let row = grid.row_at(index)?.id();
        Some(Mark {
            row,
            col: grid.cursor.col(),
        })
    }
}
