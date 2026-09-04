use std::io::{self, BufRead, Write};

const MAX_LINE_BYTES: usize = 4096;
const MAX_COMMANDS: usize = 256;

fn main() -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "ready 1")?;
    stdout.flush()?;
    let stdin = io::stdin().lock();
    for (index, line) in stdin.lines().enumerate() {
        if index >= MAX_COMMANDS {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "command limit exceeded"));
        }
        let line = line?;
        if line.len() > MAX_LINE_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line limit exceeded"));
        }
        let mut fields = line.splitn(2, ' ');
        match (fields.next(), fields.next()) {
            (Some("write-hex"), Some(value)) => {
                let bytes = decode_hex(value)?;
                stdout.write_all(&bytes)?;
                stdout.flush()?;
            }
            (Some("size"), None) => {
                writeln!(stdout, "size {} {}", env_u16("LINES")?, env_u16("COLUMNS")?)?;
                stdout.flush()?;
            }
            (Some("exit"), Some(value)) => {
                let code = value.parse::<i32>().map_err(invalid)?;
                std::process::exit(code.clamp(0, 255));
            }
            (Some("quit"), None) => return Ok(()),
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown command")),
        }
    }
    Ok(())
}

fn decode_hex(value: &str) -> io::Result<Vec<u8>> {
    if value.len() > MAX_LINE_BYTES || value.len() % 2 != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid hex length"));
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).map_err(invalid)?;
            u8::from_str_radix(text, 16).map_err(invalid)
        })
        .collect()
}

fn env_u16(name: &str) -> io::Result<u16> {
    std::env::var(name)
        .map_err(invalid)?
        .parse::<u16>()
        .map_err(invalid)
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
