//! A real raw-terminal application used only by the native viewer harness.
use anyhow::Result;
use nix::sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

pub(super) fn worker(directory: &Path) -> Result<()> {
    let input = std::io::stdin();
    let saved = tcgetattr(&input)?;
    let mut raw = saved.clone();
    cfmakeraw(&mut raw);
    tcsetattr(&input, SetArg::TCSANOW, &raw)?;
    let result = (|| -> Result<()> {
        let mut log = File::create(directory.join(format!("{}.input", std::process::id())))?;
        let mut output = std::io::stdout().lock();
        output.write_all(b"\x1b[?1003h\x1b[?1006h")?;
        for row in 1..=80 {
            write!(output, "PRIMARY{row:03}\r\n")?;
        }
        output.write_all(b"MOUSE_APP_READY")?;
        output.flush()?;
        let mut input = input.lock();
        let mut bytes = [0; 1024];
        loop {
            let count = input.read(&mut bytes)?;
            if count == 0 {
                break;
            }
            log.write_all(&bytes[..count])?;
            log.flush()?;
            for byte in &bytes[..count] {
                match byte {
                    2 => output.write_all(b"\x1b[?1049h\x1b[2J\x1b[HALTERNATE_READY")?,
                    3 => output.write_all(b"\x1b[?1049l\x1b[HPRIMARY_RETURNED")?,
                    _ => {}
                }
            }
            output.flush()?;
        }
        output.write_all(b"\x1b[?1003l\x1b[?1006l\x1b[?1049l")?;
        output.flush()?;
        Ok(())
    })();
    tcsetattr(&input, SetArg::TCSANOW, &saved)?;
    result
}
