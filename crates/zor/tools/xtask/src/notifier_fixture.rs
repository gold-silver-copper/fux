//! Dashboard notification subprocess fixture: no desktop service or credentials.
use anyhow::{Context, Result};
use std::{fs, io::Write, time::Duration};
fn main() -> Result<()> {
    let executable = std::env::current_exe()?;
    let root = executable.parent().context("fixture parent")?;
    let record = serde_json::json!({"pid":std::process::id(),"args":std::env::args().skip(1).collect::<Vec<_>>()});
    writeln!(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("notices.jsonl"))?,
        "{record}"
    )?;
    println!("NOTIFIER_STDOUT_MUST_STAY_HIDDEN");
    eprintln!("NOTIFIER_STDERR_MUST_STAY_HIDDEN");
    match fs::read_to_string(root.join("notification-mode"))?.as_str() {
        "stall" => std::thread::sleep(Duration::from_secs(60)),
        "fail" => std::process::exit(7),
        _ => {}
    }
    Ok(())
}
