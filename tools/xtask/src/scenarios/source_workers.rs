//! Embedded source-check workers, executed only by the standalone fixture harness.
use anyhow::{Context, Result};
use std::{
    fs,
    io::Write,
    path::Path,
    time::{Duration, Instant},
};
pub fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("source-produce") => {
            fs::write("report.bin", (0..=255u8).collect::<Vec<_>>())?;
            fs::write("log.txt", "log")?;
        }
        Some("source-unsafe") => match args.get(1).context("unsafe fixture mode")?.as_str() {
            "symlink" => std::os::unix::fs::symlink("source", "report.bin")?,
            "hardlink" => fs::hard_link("source", "report.bin")?,
            "fifo" => nix::unistd::mkfifo(
                Path::new("report.bin"),
                nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
            )?,
            "oversized" => fs::write("report.bin", vec![b'x'; 65537])?,
            _ => anyhow::bail!("unknown unsafe fixture"),
        },
        Some("source-recovered") => {
            fs::write("report.bin", "recovered")?;
            fs::File::create("missing.bin")?;
        }
        Some("source-renamed") => {
            let old = std::env::current_dir()?;
            let name = old
                .file_name()
                .context("cwd name")?
                .to_str()
                .context("UTF-8 cwd")?;
            fs::rename(&old, old.with_file_name(format!("{name}-moved")))?;
            fs::create_dir(&old)?;
            fs::write(old.join("report.bin"), "replacement")?;
            fs::write("report.bin", "original directory")?;
        }
        Some("source-held" | "source-capacity") => {
            let capacity = args[0] == "source-capacity";
            fs::write(
                "report.bin",
                if capacity {
                    vec![255; 65536]
                } else {
                    b"older success".to_vec()
                },
            )?;
            let started = Path::new(args.get(1).context("started")?);
            let gate = Path::new(args.get(2).context("gate")?);
            fs::File::create(started)?;
            let end = Instant::now() + Duration::from_secs(15);
            while !gate.exists() && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(20));
            }
            if capacity {
                std::io::stdout().write_all(&[0; 4096])?;
                std::io::stderr().write_all(&[0; 4096])?;
            }
        }
        _ => anyhow::bail!("unknown source worker"),
    }
    Ok(())
}
