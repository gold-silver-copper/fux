use std::process::ExitCode;
use vt_cast_compress::{Cast, minimize, samples, verify};

const USAGE: &str = "usage: vt-cast-compress [--merge-ms N] [--lookahead N] IN.cast OUT.cast
       vt-cast-compress --check ORIGINAL.cast COMPRESSED.cast [--merge-ms N]

Writes OUT.cast with output events that change nothing visible dropped when
the next N events (--lookahead, default 16; 0 keeps every byte) show the
same screens without them, else moved into the next one that shows
something; trailing output that never shows is dropped, and, with
--merge-ms, frames closer together than N ms joined. Then replays both and
compares the screens: at every original event's time (no merge window), or
at every compressed frame (with one), and at the end. If a dropped byte
mattered past the look ahead, it compresses again keeping every byte. OUT
is written only if the screens match.";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("vt-cast-compress: {error}");
            ExitCode::FAILURE
        }
    }
}

fn read(path: &str) -> Result<Cast, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    Cast::parse(&text).map_err(|e| format!("{path}: {e}"))
}

fn run() -> Result<(), String> {
    let mut merge_ms: u64 = 0;
    let mut lookahead: usize = 16;
    let mut check = false;
    let mut paths = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--merge-ms" => {
                let n = args.next().ok_or("--merge-ms needs a number")?;
                merge_ms = n.parse().map_err(|_| format!("--merge-ms {n}: not a number"))?;
            }
            "--lookahead" => {
                let n = args.next().ok_or("--lookahead needs a number")?;
                lookahead = n.parse().map_err(|_| format!("--lookahead {n}: not a number"))?;
            }
            "--check" => check = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option {arg}\n{USAGE}")),
            _ => paths.push(arg),
        }
    }
    let [input, output] = paths.as_slice() else {
        return Err(USAGE.into());
    };
    let window = merge_ms.saturating_mul(1000);
    let original = read(input)?;
    if check {
        let compressed = read(output)?;
        let n = verify(&original, &compressed, &samples(&original, &compressed, window))?;
        println!("same screens at {n} sampled times");
        return Ok(());
    }
    let (small, stats, n, fell_back) = minimize(&original, window, lookahead)?;
    if fell_back {
        println!("a dropped byte mattered past the look ahead: kept every byte");
    }
    let text = small.write();
    let size_in = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
    std::fs::write(output, &text).map_err(|e| format!("{output}: {e}"))?;
    println!(
        "events {} -> {} ({} invisible, {} merged)\n\
         output bytes {} -> {} ({} dropped)\n\
         file bytes {} -> {}\n\
         same screens at {} sampled times; fux-vt refused room {} times",
        stats.events_in,
        stats.events_out,
        stats.invisible,
        stats.merged,
        stats.bytes_in,
        stats.bytes_out,
        stats.bytes_dropped,
        size_in,
        text.len(),
        n,
        stats.refused
    );
    Ok(())
}
