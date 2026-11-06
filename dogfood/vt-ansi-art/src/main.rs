//! An ANSI art viewer and converter.
//!
//! Reads a `.ans`/`.txt` art file (CP437 bytes with ANSI.SYS-era escapes, or
//! UTF-8 with 256-colour and truecolour escapes), plays it on a fux-vt
//! screen of the art's width, and writes what the screen shows back out as
//! UTF-8 with the fewest escapes that draw it: one SGR where the style
//! changes, none for trailing blanks, a reset at each line's end; or as
//! plain text.

use fux_vt::{Attributes, Blink, CellRef, Color, Options, Parser, Rgb, Size};
use std::io::{Read, Write};
use std::process::ExitCode;

const USAGE: &str = "\
usage: vt-ansi-art [OPTION]... [FILE]
Render ANSI art (FILE, or stdin) and write it as clean UTF-8 with minimal escapes.

  -w, --width N     columns to render at (default: the SAUCE record's, else 80)
      --cp437       read the input as CP437 (default unless it is valid UTF-8)
      --utf8        read the input as UTF-8
      --dos         DOS colours: bold makes colours 0-7 bright, blink makes
                    backgrounds 0-7 bright (iCE); default for CP437 input
      --no-dos      keep bold and blink as they are; default for UTF-8 input
      --vga         write colours 0-15 as the VGA palette's RGB, not indexes
      --256         write truecolour as the nearest of the 256 indexed colours
      --plain       write the text only, no escapes
  -h, --help        show this help";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Encoding {
    Cp437,
    Utf8,
}

#[derive(Clone, Debug, Default)]
struct Config {
    width: Option<u16>,
    encoding: Option<Encoding>,
    dos: Option<bool>,
    vga: bool,
    indexed: bool,
    plain: bool,
    file: Option<String>,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = match parse(&args) {
        Ok(Some(config)) => config,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(why) => {
            eprintln!("vt-ansi-art: {why}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let input = match read_input(config.file.as_deref()) {
        Ok(input) => input,
        Err(why) => {
            eprintln!("vt-ansi-art: {why}");
            return ExitCode::FAILURE;
        }
    };
    let out = match convert(&input, &config) {
        Ok(out) => out,
        Err(why) => {
            eprintln!("vt-ansi-art: {why}");
            return ExitCode::FAILURE;
        }
    };
    let mut stdout = std::io::stdout().lock();
    match stdout.write_all(&out).and_then(|()| stdout.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vt-ansi-art: writing: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The options asked for; `None` for `--help`.
fn parse(args: &[String]) -> Result<Option<Config>, String> {
    let mut config = Config::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "-w" | "--width" => {
                let n = args.next().ok_or("--width needs a number")?;
                let width = n.parse().ok().filter(|w| *w > 0);
                config.width = Some(width.ok_or_else(|| format!("bad width {n:?}"))?);
            }
            "--cp437" => config.encoding = Some(Encoding::Cp437),
            "--utf8" => config.encoding = Some(Encoding::Utf8),
            "--dos" => config.dos = Some(true),
            "--no-dos" => config.dos = Some(false),
            "--vga" => config.vga = true,
            "--256" => config.indexed = true,
            "--plain" => config.plain = true,
            "-" => config.file = None,
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            file => config.file = Some(file.to_owned()),
        }
    }
    Ok(Some(config))
}

fn read_input(file: Option<&str>) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    match file {
        Some(path) => {
            bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        }
        None => {
            std::io::stdin()
                .read_to_end(&mut bytes)
                .map_err(|e| format!("stdin: {e}"))?;
        }
    }
    Ok(bytes)
}

/// The art's bytes, without the SAUCE record and DOS end of file, and the
/// width the record gives, if any.
fn strip_sauce(input: &[u8]) -> (&[u8], Option<u16>) {
    let mut art = input;
    let mut width = None;
    if let Some(at) = input.len().checked_sub(128)
        && let Some(record) = input.get(at..)
        && record.starts_with(b"SAUCE00")
    {
        // DataType 1 (Character): TInfo1 is the width in characters.
        if let (Some(1), Some(&[lo, hi])) = (record.get(94), record.get(96..98)) {
            width = Some(u16::from_le_bytes([lo, hi])).filter(|w| *w > 0);
        }
        art = input.get(..at).unwrap_or(input);
    }
    // Ctrl-Z ends a DOS text file; a comment block and the record follow it.
    if let Some(end) = art.iter().position(|&b| b == 0x1a) {
        art = art.get(..end).unwrap_or(art);
    }
    (art, width)
}

/// CP437's characters 0x80 to 0xFF.
const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', //
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', //
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', //
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', //
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', //
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀', //
    'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', //
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

/// CP437's glyphs for 0x00 to 0x1F, shown where DOS printed the byte.
const CP437_LOW: [char; 32] = [
    ' ', '☺', '☻', '♥', '♦', '♣', '♠', '•', '◘', '○', '◙', '♂', '♀', '♪', '♫', '☼', //
    '►', '◄', '↕', '‼', '¶', '§', '▬', '↨', '↑', '↓', '→', '←', '∟', '↔', '▲', '▼',
];

/// CP437 bytes as UTF-8. ANSI.SYS acted on BS, HT, LF, CR and ESC and
/// printed the other controls' glyphs; NUL and BEL showed nothing. A
/// glyph is never left as a control the terminal would act on (SO would
/// switch character sets).
fn cp437_to_utf8(bytes: &[u8]) -> Vec<u8> {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            0x00 | 0x07 => {}
            0x08 | 0x09 | 0x0a | 0x0d | 0x1b => out.push(char::from(b)),
            0x7f => out.push('⌂'),
            0x20..0x7f => out.push(char::from(b)),
            0x80.. => out.extend(CP437_HIGH.get(usize::from(b - 0x80))),
            _ => out.extend(CP437_LOW.get(usize::from(b))),
        }
    }
    out.into_bytes()
}

/// About how many rows the art takes at `width`: its line feeds and its
/// text wrapped, escapes left out. Too many only costs blank rows, which
/// are trimmed; too few sends the top into history, which is read too.
fn estimate_rows(bytes: &[u8], width: u16) -> usize {
    #[derive(PartialEq)]
    enum State {
        Text,
        Escape,
        Csi,
    }
    let (mut feeds, mut text, mut state) = (0usize, 0usize, State::Text);
    for c in String::from_utf8_lossy(bytes).chars() {
        state = match (state, c) {
            (_, '\x1b') => State::Escape,
            (State::Escape, '[') => State::Csi,
            (State::Csi, '@'..='~') | (State::Escape, _) => State::Text,
            (State::Csi, _) => State::Csi,
            (State::Text, '\n') => {
                feeds = feeds.saturating_add(1);
                State::Text
            }
            (State::Text, c) => {
                text = text.saturating_add(usize::from(!c.is_control()));
                State::Text
            }
        };
    }
    feeds
        .saturating_add(text / usize::from(width))
        .saturating_add(2)
}

/// The art rendered and written out as `config` asks.
fn convert(input: &[u8], config: &Config) -> Result<Vec<u8>, String> {
    let (art, sauce_width) = strip_sauce(input);
    let width = config.width.or(sauce_width).unwrap_or(80);
    let encoding = config.encoding.unwrap_or(match std::str::from_utf8(art) {
        Ok(_) => Encoding::Utf8,
        Err(_) => Encoding::Cp437,
    });
    let dos = config.dos.unwrap_or(encoding == Encoding::Cp437);
    let text = match encoding {
        Encoding::Cp437 => cp437_to_utf8(art),
        Encoding::Utf8 => art.to_vec(),
    };
    // A screen as tall as the art, within the cells a screen may have.
    let most = (64usize << 20) / usize::from(width);
    let rows = estimate_rows(&text, width).clamp(25, most.min(usize::from(u16::MAX)));
    let rows = u16::try_from(rows).unwrap_or(u16::MAX);
    let size = Size::new(rows, width).map_err(|_| "the screen's size is zero".to_owned())?;
    let mut parser = Parser::with_options(size, 64 << 20, Options::default())
        .map_err(|e| format!("making a {rows}x{width} screen: {e}"))?;
    // A line feed returns the carriage too (LNM), as a terminal's line
    // discipline makes it for `cat`: art saved with Unix line ends, and this
    // program's own output, read as it shows.
    // What the art asks back is sent nowhere. A refusal of room is
    // counted, and the rest is still read.
    let _ = parser.process_all(b"\x1b[20h", &mut ());
    let _ = parser.process_all(&text, &mut ());
    if parser.refused() > 0 {
        eprintln!(
            "vt-ansi-art: the screen refused room {} times; the art may be cut",
            parser.refused()
        );
    }
    Ok(render(&parser, dos, config))
}

/// What the screen shows, rows with nothing in them trimmed from the end.
fn render(parser: &Parser, dos: bool, config: &Config) -> Vec<u8> {
    let rows: Vec<Vec<CellRef>> = parser
        .screen()
        .rows()
        .map(|row| {
            let mut cells: Vec<CellRef> = row.glyphs().map(|(_, c)| c).collect();
            while cells.last().is_some_and(|c| blank(c, dos)) {
                cells.pop();
            }
            cells
        })
        .collect();
    let used = rows
        .iter()
        .rposition(|r| !r.is_empty())
        .map_or(0, |i| i.saturating_add(1));
    let mut out = Vec::new();
    for cells in rows.iter().take(used) {
        let mut pen = Attributes::default();
        for cell in cells {
            if !config.plain {
                let style = paint(cell.attributes(), dos, config);
                style.write_sgr_change(pen, &mut out, false);
                pen = style;
            }
            out.extend_from_slice(cell.shown().as_bytes());
        }
        if pen != Attributes::default() {
            out.extend_from_slice(b"\x1b[0m");
        }
        out.push(b'\n');
    }
    out
}

/// Whether a cell shows nothing: no text, and no background or inverse
/// that would colour it.
fn blank(cell: &CellRef, dos: bool) -> bool {
    let a = cell.attributes();
    let bg = dos_colours(a, dos).background();
    cell.shown() == " " && bg == Color::Default && !a.inverse() && !a.underline() && !a.strikeout()
}

/// Bold as bright colours and blink as bright backgrounds, as a VGA text
/// screen showed them.
fn dos_colours(a: Attributes, dos: bool) -> Attributes {
    if !dos {
        return a;
    }
    let mut a = a;
    match a.foreground() {
        Color::Idx(n) if a.bold() && n < 8 => {
            a = a.with_foreground(Color::Idx(n + 8)).with_bold(false)
        }
        // The default colour was light grey, and bold made it white.
        Color::Default if a.bold() => a = a.with_foreground(Color::Idx(15)).with_bold(false),
        _ => {}
    }
    match a.background() {
        Color::Idx(n) if a.blink() != Blink::None && n < 8 => {
            a = a.with_background(Color::Idx(n + 8)).with_blink(Blink::None);
        }
        _ => {}
    }
    a
}

/// The attributes as they are to be written.
fn paint(a: Attributes, dos: bool, config: &Config) -> Attributes {
    let a = dos_colours(a, dos);
    let fix = |c: Color| match c {
        Color::Idx(n) if config.vga => VGA
            .get(usize::from(n))
            .map_or(c, |&rgb| Color::Rgb(Rgb::from(rgb))),
        Color::Rgb(rgb) if config.indexed => Color::Idx(nearest_256(rgb)),
        _ => c,
    };
    a.with_foreground(fix(a.foreground()))
        .with_background(fix(a.background()))
}

/// The VGA text palette.
const VGA: [[u8; 3]; 16] = [
    [0, 0, 0],
    [170, 0, 0],
    [0, 170, 0],
    [170, 85, 0],
    [0, 0, 170],
    [170, 0, 170],
    [0, 170, 170],
    [170, 170, 170],
    [85, 85, 85],
    [255, 85, 85],
    [85, 255, 85],
    [255, 255, 85],
    [85, 85, 255],
    [255, 85, 255],
    [85, 255, 255],
    [255, 255, 255],
];

/// The xterm 256-colour index nearest `rgb`: of the 6x6x6 cube and the
/// grey ramp, whichever is closer.
fn nearest_256(Rgb { r, g, b }: Rgb) -> u8 {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let step = |v: u8| -> u8 {
        let mut best = 0u8;
        for (i, s) in (0u8..).zip(STEPS) {
            if v.abs_diff(s) < v.abs_diff(STEPS.get(usize::from(best)).copied().unwrap_or(0)) {
                best = i;
            }
        }
        best
    };
    let distance = |x: [u8; 3]| -> u32 {
        [r, g, b]
            .iter()
            .zip(x)
            .map(|(&a, b)| u32::from(a.abs_diff(b)).pow(2))
            .sum()
    };
    let (ri, gi, bi) = (step(r), step(g), step(b));
    let level = |i: u8| STEPS.get(usize::from(i)).copied().unwrap_or(0);
    let cube = 16 + 36 * ri + 6 * gi + bi;
    let cube_distance = distance([level(ri), level(gi), level(bi)]);
    let mean = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    // Greys 232-255 are 8, 18, ..., 238.
    let grey = u8::try_from((mean.saturating_sub(3) / 10).min(23)).unwrap_or(23);
    let level = 8 + 10 * grey;
    if distance([level; 3]) < cube_distance {
        232 + grey
    } else {
        cube
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, reason = "test data")]
mod tests {
    use super::*;

    fn run(input: &[u8], config: &Config) -> String {
        String::from_utf8_lossy(&convert(input, config).unwrap_or_default()).into_owned()
    }

    #[test]
    fn dos_art_becomes_utf8_with_bright_colours_and_one_sgr_a_change() {
        // Clear, home, bold blue full blocks, cursor forward 2, a shade in
        // red on a blinking (iCE: bright) black, then SAUCE-less EOF.
        let art = b"\x1b[2J\x1b[H\x1b[1;34m\xdb\xdb\x1b[2C\x1b[0;5;31;40m\xb0\r\n\x01\x1a junk";
        let out = run(art, &Config::default());
        assert_eq!(
            out,
            "\x1b[94m██\x1b[0m  \x1b[31;100m░\x1b[0m\n\x1b[31;100m☺\x1b[0m\n"
        );
        assert_eq!(
            run(out.as_bytes(), &Config::default()),
            out,
            "its output renders as itself"
        );
        let plain = run(
            art,
            &Config {
                plain: true,
                ..Config::default()
            },
        );
        assert_eq!(plain, "██  ░\n☺\n");
    }

    #[test]
    fn eighty_columns_wrap_and_truecolour_can_be_indexed() {
        let mut art = vec![b'#'; 80];
        art.extend_from_slice(b"\r\nx\x1b[38;2;255;0;0my\x1b[m\r\n");
        let out = run(&art, &Config::default());
        assert_eq!(
            out,
            format!("{}\nx\x1b[38;2;255;0;0my\x1b[0m\n", "#".repeat(80))
        );
        let indexed = run(
            &art,
            &Config {
                indexed: true,
                ..Config::default()
            },
        );
        assert!(
            indexed.ends_with("x\x1b[38;5;196my\x1b[0m\n"),
            "{indexed:?}"
        );
        assert_eq!(
            nearest_256(Rgb {
                r: 128,
                g: 128,
                b: 128
            }),
            244
        );
    }

    #[test]
    fn a_sauce_record_gives_the_width() {
        let mut file = b"abcdefgh".to_vec();
        file.push(0x1a);
        let mut sauce = vec![0u8; 128];
        sauce[..7].copy_from_slice(b"SAUCE00");
        sauce[94] = 1;
        sauce[96..98].copy_from_slice(&4u16.to_le_bytes());
        file.extend_from_slice(&sauce);
        assert_eq!(run(&file, &Config::default()), "abcd\nefgh\n");
    }
}
