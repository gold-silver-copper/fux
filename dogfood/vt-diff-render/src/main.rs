//! vt-diff-render: replays a recorded terminal stream through fux-vt in
//! frames, renders each frame as the minimal repaint from the last, feeds
//! that repaint into a second fux-vt, and checks both show the same.
use std::io::{Read, Write};
use std::process::ExitCode;

use fux_vt::{Options, Parser, Size};
use vt_diff_render::{Renderer, first_difference};

const USAGE: &str = "usage: vt-diff-render [--size ROWSxCOLS] [--chunk BYTES] [--emit] [FILE]
  Feeds FILE (or stdin) to a terminal of SIZE (default 24x80) CHUNK bytes at
  a time (default 4096): after each chunk it renders the repaint from the last
  frame, feeds it to a second terminal and compares the two screens.
  --emit  write the repaint stream to stdout (cat it to watch the replay)
  Prints a summary to stderr; exits 1 if any frame differs.";

struct Args {
    size: Size,
    chunk: usize,
    emit: bool,
    file: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        size: Size::new(24, 80).map_err(|e| e.to_string())?,
        chunk: 4096,
        emit: false,
        file: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--size" => {
                let v = it.next().ok_or("--size needs ROWSxCOLS")?;
                let (r, c) = v.split_once('x').ok_or("--size is ROWSxCOLS")?;
                let r = r.parse().map_err(|_| format!("bad rows in {v}"))?;
                let c = c.parse().map_err(|_| format!("bad columns in {v}"))?;
                args.size = Size::new(r, c).map_err(|e| format!("--size {v}: {e}"))?;
            }
            "--chunk" => {
                let v = it.next().ok_or("--chunk needs BYTES")?;
                args.chunk = v.parse().ok().filter(|&n| n > 0).ok_or(format!("bad --chunk {v}"))?;
            }
            "--emit" => args.emit = true,
            "-h" | "--help" => return Err(USAGE.to_string()),
            _ if arg.starts_with('-') && arg != "-" => return Err(format!("unknown option {arg}\n{USAGE}")),
            _ => args.file = Some(arg),
        }
    }
    Ok(args)
}

fn run(args: Args) -> Result<bool, String> {
    let mut input = Vec::new();
    match args.file.as_deref() {
        None | Some("-") => std::io::stdin().read_to_end(&mut input),
        Some(path) => std::fs::File::open(path).and_then(|mut f| f.read_to_end(&mut input)),
    }
    .map_err(|e| format!("reading input: {e}"))?;
    let inner = |history| Parser::with_options(args.size, history, Options::default());
    let mut source = inner(1 << 20).map_err(|e| e.to_string())?;
    let mut outer = inner(0).map_err(|e| e.to_string())?;
    let mut renderer = Renderer::new();
    let mut stdout = std::io::stdout().lock();
    let (mut repaint, mut full, mut bad) = (Vec::new(), 0usize, 0u64);
    let (mut out_bytes, mut refused) = (0usize, 0u64);
    for (frame, chunk) in input.chunks(args.chunk).enumerate() {
        refused += source.process_all(chunk, &mut ());
        repaint.clear();
        renderer.render(source.screen(), &mut repaint).map_err(|e| e.to_string())?;
        out_bytes += repaint.len();
        // What a renderer that knows nothing would send, to compare with.
        let mut whole = Vec::new();
        Renderer::new().render(source.screen(), &mut whole).map_err(|e| e.to_string())?;
        full += whole.len();
        refused += outer.process_all(&repaint, &mut ());
        if let Some(diff) = first_difference(source.screen(), outer.screen()) {
            bad += 1;
            if bad <= 5 {
                eprintln!("frame {frame}: {diff}");
            }
        }
        if args.emit {
            stdout.write_all(&repaint).map_err(|e| format!("writing: {e}"))?;
        }
    }
    stdout.flush().map_err(|e| format!("writing: {e}"))?;
    let s = renderer.stats;
    eprintln!(
        "frames {} input {} B, repaint {} B, full repaints would be {} B ({:.1}%); \
         scrolls {} ({} rows), rows compared {}, skipped {}; mismatched frames {bad}; room refused {refused}",
        s.frames,
        input.len(),
        out_bytes,
        full,
        100.0 * out_bytes as f64 / full.max(1) as f64,
        s.scrolls,
        s.scrolled_rows,
        s.rows_compared,
        s.rows_skipped,
    );
    Ok(bad == 0)
}

fn main() -> ExitCode {
    match parse_args().and_then(run) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("vt-diff-render: {e}");
            ExitCode::from(2)
        }
    }
}
