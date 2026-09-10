//! Disposable adapter CLI fixture, separate from the production zor executable.
use anyhow::{Context, Result};
use std::{
    fs::OpenOptions,
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let root = Path::new(args.get(2).context("missing fixture root")?);
    let commands = args.get(4..).context("missing fixture command")?;
    let command = commands.first().context("empty fixture command")?;
    let mut record = serde_json::to_vec(commands)?;
    record.push(b'\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("calls"))?
        .write_all(&record)?;
    if command == "heartbeat-adapter" && root.join("heartbeat-delay").exists() {
        std::thread::sleep(Duration::from_secs(3));
    }
    if command == "bind-report" && root.join("binding-delay").exists() {
        std::thread::sleep(Duration::from_secs(1));
    }
    if command == "bind-report" && commands.iter().any(|arg| arg == "user-13") {
        let deadline = Instant::now() + Duration::from_secs(5);
        while root.join("binding-hold").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    println!("{{}}");
    Ok(())
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("zor adapter fixture: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
