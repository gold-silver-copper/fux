//! Owned streaming fixtures use bounded temporary files, never inherited output pipes.
use super::process::{Guard, wait};
use anyhow::{Result, ensure};
use std::{
    fs::File,
    io::Read,
    process::{Command, Stdio},
    time::Duration,
};

pub struct Logged {
    pub child: Guard,
    stdout: tempfile::NamedTempFile,
    stderr: tempfile::NamedTempFile,
    reader: File,
    pending: Vec<u8>,
}
impl Logged {
    pub fn spawn(mut command: Command) -> Result<Self> {
        let stdout = tempfile::NamedTempFile::new()?;
        let stderr = tempfile::NamedTempFile::new()?;
        // A fresh open gives the reader an independent offset from the child writer.
        let reader = File::open(stdout.path())?;
        let child = Guard(
            command
                .stdin(Stdio::null())
                .stdout(Stdio::from(stdout.reopen()?))
                .stderr(Stdio::from(stderr.reopen()?))
                .spawn()?,
        );
        Ok(Self {
            child,
            stdout,
            stderr,
            reader,
            pending: Vec::new(),
        })
    }
    pub fn lines(&mut self) -> Result<Vec<Vec<u8>>> {
        ensure!(
            self.stdout.as_file().metadata()?.len() <= 4 * 1024 * 1024
                && self.stderr.as_file().metadata()?.len() <= 4 * 1024 * 1024,
            "streaming fixture log bound"
        );
        (&mut self.reader)
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut self.pending)?;
        ensure!(
            self.pending.len() <= 4 * 1024 * 1024,
            "streaming fixture read bound"
        );
        let mut lines = Vec::new();
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            ensure!(end < 1024 * 1024, "streaming fixture line bound");
            let mut line: Vec<_> = self.pending.drain(..=end).collect();
            line.pop();
            lines.push(line);
        }
        ensure!(
            self.pending.len() < 1024 * 1024,
            "incomplete fixture line bound"
        );
        Ok(lines)
    }
    pub fn kill(&mut self) -> Result<()> {
        if self.child.0.try_wait()?.is_none() {
            self.child.0.kill()?;
        }
        wait(&mut self.child.0, Duration::from_secs(5))?;
        Ok(())
    }
}
