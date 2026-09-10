//! Scripted headless provider: record literal argv once, then exit without model work.
use anyhow::{Context, Result};
use std::{fs::OpenOptions, io::Write};
fn run() -> Result<()> {
    let executable = std::env::current_exe()?;
    let name = executable
        .file_name()
        .context("fixture executable name")?
        .to_str()
        .context("fixture name UTF-8")?;
    let output = executable
        .parent()
        .context("fixture directory")?
        .join(format!("{name}.jsonl"));
    let mut bytes = serde_json::to_vec(
        &serde_json::json!({"argv":std::env::args().skip(1).collect::<Vec<_>>(),"pid":std::process::id()}),
    )?;
    bytes.push(b'\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(output)?
        .write_all(&bytes)?;
    Ok(())
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("argv fixture: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
