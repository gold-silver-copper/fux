//! Scripted terminal fixture. Never connects to a provider or reads credentials.
use anyhow::{Result, ensure};
use nix::sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr};
use std::io::{Read, Write};

fn screen(text: &str) -> Result<()> {
    print!("\x1b[2J\x1b[H{}", text.replace('\n', "\r\n"));
    std::io::stdout().flush()?;
    Ok(())
}
fn enter() -> Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let mut byte = [0];
        std::io::stdin().read_exact(&mut byte)?;
        if byte[0] == b'\r' {
            return Ok(line);
        }
        line.push(byte[0]);
    }
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--version") {
        println!("zor scripted provider capture fixture; no provider calls");
        return Ok(());
    }
    let mut terminal = tcgetattr(std::io::stdin())?;
    cfmakeraw(&mut terminal);
    tcsetattr(std::io::stdin(), SetArg::TCSANOW, &terminal)?;
    if args.iter().any(|s| s == "--bare") {
        for (text, expected) in [
            ("Choose the text style that looks best", ""),
            ("Detected a custom API key in your environment", "\x1b[A"),
            ("Security notes:", ""),
            ("Yes, I trust this folder", "\x1b[B"),
        ] {
            screen(text)?;
            ensure!(enter()? == expected.as_bytes(), "unexpected fixture input");
        }
        screen("esc to interrupt")?;
        std::thread::sleep(std::time::Duration::from_millis(1000));
        screen("⏺ FUX_CLAUDE_OBSERVATION_OK\n? for shortcuts")?;
        ensure!(enter()? == b"/exit", "unexpected exit input");
    } else {
        screen("Do you trust the contents of this directory?\n1. Yes, continue")?;
        ensure!(enter()?.is_empty(), "unexpected trust input");
        if args.iter().any(|s| s == "on-request") {
            screen(
                "Would you like to run the following command?\nPress enter to confirm or esc to cancel\nReason: May I run the harmless approval fixture?\n$ /usr/bin/true\n3. No, and tell Codex what to do differently (esc)",
            )?;
            anyhow::bail!("approval must remain unanswered: {:?}", enter()?);
        }
        screen("• Working (fixture)")?;
        std::thread::sleep(std::time::Duration::from_millis(400));
        screen("• FUX_OBSERVATION_OK")?;
        ensure!(enter()? == b"/quit", "unexpected exit input");
    }
    Ok(())
}
