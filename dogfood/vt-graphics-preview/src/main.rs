use std::io::{Read, Write};
use vt_graphics_preview::{
    Res, Setup, blocks, demo, encode_sixel, map, parse_ppm, preview, resixel,
};

const USAGE: &str = "usage:
  vt-graphics-preview encode FILE.ppm            sixel for a PPM (P3 or P6) on stdout
  vt-graphics-preview preview [OPTIONS] FILE|-   feed a terminal's output and report its pictures
  vt-graphics-preview demo [OPTIONS]             a test-bed stream on stdout, for preview
options: --size ROWSxCOLS (24x80)  --cell WxH pixels (10x20)  --history BYTES (65536)
         --map (the screen, # for picture cells)  --blocks (pictures as half blocks)
         --sixel (pictures again as sixel, through fux-vt's encoder)";

fn pair(s: &str, what: &str) -> Res<(u16, u16)> {
    let (a, b) = s
        .split_once('x')
        .ok_or(format!("{what}: want AxB, got {s:?}"))?;
    let n = |v: &str| v.parse::<u16>().map_err(|e| format!("{what}: {v:?}: {e}"));
    Ok((n(a)?, n(b)?))
}

fn read_input(path: &str) -> Res<Vec<u8>> {
    let mut data = Vec::new();
    if path == "-" {
        std::io::stdin()
            .read_to_end(&mut data)
            .map_err(|e| format!("stdin: {e}"))?;
    } else {
        data = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(data)
}

fn run() -> Res<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().ok_or(USAGE)?;
    let (mut setup, mut file) = (
        Setup {
            rows: 24,
            cols: 80,
            cell: (10, 20),
            history_bytes: 65536,
        },
        None,
    );
    let (mut show_map, mut show_blocks, mut show_sixel) = (false, false, false);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value\n{USAGE}"));
        match arg.as_str() {
            "--size" => (setup.rows, setup.cols) = pair(&value()?, "--size")?,
            "--cell" => setup.cell = pair(&value()?, "--cell")?,
            "--history" => {
                setup.history_bytes = value()?.parse().map_err(|e| format!("--history: {e}"))?
            }
            "--map" => show_map = true,
            "--blocks" => show_blocks = true,
            "--sixel" => show_sixel = true,
            "-h" | "--help" => return Err(USAGE.to_owned()),
            _ if file.is_none() && (arg == "-" || !arg.starts_with('-')) => file = Some(arg),
            _ => return Err(format!("unknown argument {arg:?}\n{USAGE}")),
        }
    }
    let mut stdout = std::io::stdout().lock();
    let out = match command.as_str() {
        "encode" => encode_sixel(&parse_ppm(&read_input(
            &file.ok_or("encode: no PPM file")?,
        )?)?),
        "demo" => demo(setup.rows, setup.cols, setup.cell),
        "preview" => {
            let report = preview(
                &read_input(&file.ok_or("preview: no input (FILE or -)")?)?,
                setup,
            )?;
            let mut out = report.text.into_bytes();
            if show_map {
                out.extend(map(&report.parser).into_bytes());
            }
            if show_blocks {
                out.extend(blocks(&report.parser).into_bytes());
            }
            if show_sixel {
                out.extend(resixel(&report.parser));
            }
            out
        }
        _ => return Err(USAGE.to_owned()),
    };
    stdout
        .write_all(&out)
        .and_then(|()| stdout.flush())
        .map_err(|e| format!("stdout: {e}"))
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vt-graphics-preview: {e}");
            std::process::ExitCode::from(2)
        }
    }
}
