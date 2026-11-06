//! vt-html-shot: feeds a program's raw output through fux-vt at a given
//! size and writes an HTML page of the final screen, with its colours,
//! bold, italic, underlines, strikeout, reverse video and OSC 8 links.
//!
//!     some-program | vt-html-shot --rows 24 --cols 80 > shot.html
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, reason = "tests fail by panicking"))]

use fux_vt::{Attributes, Color, Feature, Hyperlink, Mode, Options, Parser, Rgb, Screen, Size, UnderlineStyle};
use std::fmt::Write as _;
use std::io::{Read, Write};

const USAGE: &str = "usage: vt-html-shot [--rows N] [--cols N] [--light] [--cursor] [--raw] [--title TEXT] [FILE]
Reads a program's raw terminal output (FILE, or stdin), runs it through a
terminal of N rows and columns (default 24 by 80), and writes an HTML page
of the final screen to stdout. A line feed starts a new line, as a terminal's
driver makes it for output written to it; --raw takes it as the terminal
does, down a row only (for output recorded from a terminal, by script(1)).";

/// Input larger than this is refused rather than read into memory.
const MAX_INPUT: u64 = 256 << 20;

struct Settings {
    rows: u16,
    cols: u16,
    light: bool,
    cursor: bool,
    raw: bool,
    title: String,
    file: Option<String>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Settings, String> {
    let mut s = Settings { rows: 24, cols: 80, light: false, cursor: false, raw: false, title: "terminal".into(), file: None };
    let number = |v: Option<String>, name: &str| -> Result<u16, String> {
        let v = v.ok_or(format!("{name} needs a number"))?;
        match v.parse::<u16>() {
            Ok(n) if n > 0 => Ok(n),
            _ => Err(format!("{name}: {v:?} is not a number from 1 to 65535")),
        }
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--rows" => s.rows = number(args.next(), "--rows")?,
            "--cols" => s.cols = number(args.next(), "--cols")?,
            "--light" => s.light = true,
            "--cursor" => s.cursor = true,
            "--raw" => s.raw = true,
            "--title" => s.title = args.next().ok_or("--title needs text")?,
            "-h" | "--help" => return Err(String::new()),
            a if a.starts_with('-') && a != "-" => return Err(format!("unknown option {a}")),
            _ if s.file.is_some() => return Err("only one FILE".into()),
            _ => s.file = Some(arg),
        }
    }
    Ok(s)
}

/// Runs `input` through a terminal of `rows` by `cols`, returning its parser.
/// With `newline`, a line feed also returns the carriage (LNM), as a
/// terminal driver's `onlcr` would have made a pipe's output.
fn run(input: &[u8], rows: u16, cols: u16, newline: bool) -> Result<Parser, String> {
    let size = Size::new(rows, cols).map_err(|e| e.to_string())?;
    if !fux_vt::fits(size) {
        return Err(format!("{rows}x{cols} is more cells than a screen may have"));
    }
    let options = Options::new().with(Feature::Hyperlinks).with(Feature::Palette);
    let mut parser = Parser::with_options(size, 0, options).map_err(|e| e.to_string())?;
    // Replies and events go nowhere: nothing goes back to a program.
    if newline {
        parser.process_all(b"\x1b[20h", &mut ()).map_err(|e| e.to_string())?;
    }
    // `Err` means some output was refused room, but every byte was read:
    // keep the screen.
    let _ = parser.process_all(input, &mut ());
    Ok(parser)
}

/// xterm's default colour for palette entry `i`.
fn xterm_colour(i: u8) -> Rgb {
    const BASE: [[u8; 3]; 16] = [
        [0, 0, 0], [205, 0, 0], [0, 205, 0], [205, 205, 0],
        [0, 0, 238], [205, 0, 205], [0, 205, 205], [229, 229, 229],
        [127, 127, 127], [255, 0, 0], [0, 255, 0], [255, 255, 0],
        [92, 92, 255], [255, 0, 255], [0, 255, 255], [255, 255, 255],
    ];
    match i {
        0..16 => BASE.get(usize::from(i)).copied().unwrap_or_default().into(),
        16..232 => {
            let level = |n: u8| if n == 0 { 0 } else { 55 + 40 * n };
            let n = i - 16;
            [level(n / 36), level(n / 6 % 6), level(n % 6)].into()
        }
        _ => {
            let g = 8 + 10 * (i - 232);
            [g, g, g].into()
        }
    }
}

/// The colours a page is drawn in: the defaults and the palette.
struct Colours<'a> {
    screen: &'a Screen,
    fg: Rgb,
    bg: Rgb,
}

impl Colours<'_> {
    fn new(screen: &Screen, light: bool) -> Colours<'_> {
        let (mut fg, mut bg) = if light {
            (Rgb { r: 0, g: 0, b: 0 }, Rgb { r: 255, g: 255, b: 255 })
        } else {
            (xterm_colour(7), xterm_colour(0))
        };
        fg = screen.dynamic_color(10).unwrap_or(fg);
        bg = screen.dynamic_color(11).unwrap_or(bg);
        // DECSCNM swaps the screen's default colours.
        if screen.mode(Mode::ReverseVideo) {
            std::mem::swap(&mut fg, &mut bg);
        }
        Colours { screen, fg, bg }
    }
    fn resolve(&self, c: Color, default: Rgb) -> Rgb {
        match c {
            Color::Default => default,
            Color::Idx(i) => self.screen.palette_color(i).unwrap_or_else(|| xterm_colour(i)),
            Color::Rgb(rgb) => rgb,
        }
    }
    /// A cell's CSS: only what differs from the page's defaults.
    fn style(&self, a: Attributes) -> String {
        let mut fg = self.resolve(a.foreground(), self.fg);
        let mut bg = self.resolve(a.background(), self.bg);
        if a.inverse() {
            std::mem::swap(&mut fg, &mut bg);
        }
        if a.dim() {
            let mix = |f: u8, b: u8| ((u16::from(f) + u16::from(b)) / 2) as u8;
            fg = Rgb { r: mix(fg.r, bg.r), g: mix(fg.g, bg.g), b: mix(fg.b, bg.b) };
        }
        let mut css = String::new();
        if fg != self.fg {
            let _ = write!(css, "color:{};", hex(fg));
        }
        if bg != self.bg {
            let _ = write!(css, "background:{};", hex(bg));
        }
        if a.bold() {
            css.push_str("font-weight:bold;");
        }
        if a.italic() {
            css.push_str("font-style:italic;");
        }
        let line = match a.underline_style() {
            UnderlineStyle::None => None,
            UnderlineStyle::Single => Some("solid"),
            UnderlineStyle::Double => Some("double"),
            UnderlineStyle::Curly => Some("wavy"),
            UnderlineStyle::Dotted => Some("dotted"),
            UnderlineStyle::Dashed => Some("dashed"),
        };
        match (line, a.strikeout()) {
            (None, false) => {}
            (None, true) => css.push_str("text-decoration:line-through;"),
            (Some(style), strike) => {
                let strike = if strike { " line-through" } else { "" };
                let _ = write!(css, "text-decoration:underline{strike} {style};");
                if a.underline_color() != Color::Default {
                    let _ = write!(css, "text-decoration-color:{};", hex(self.resolve(a.underline_color(), fg)));
                }
            }
        }
        css
    }
}

fn hex(Rgb { r, g, b }: Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn escape(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

/// A link's URI if a page may link to it: a program chose it, so no
/// `javascript:` or `data:` that would run in the reader's browser.
fn safe_uri(link: &Hyperlink) -> Option<&str> {
    let uri = link.uri();
    let scheme = uri.split_once(':')?.0.to_ascii_lowercase();
    ["http", "https", "ftp", "file", "mailto"].contains(&scheme.as_str()).then_some(uri)
}

/// Cells drawn alike: their CSS, link (key and URI) and whether the cursor
/// is on them.
#[derive(Default, PartialEq)]
struct Run {
    css: String,
    link: Option<(u64, String)>,
    cursor: bool,
}

impl Run {
    /// Writes `text`, escaped already, in this run's look, and empties it.
    fn write(&self, html: &mut String, text: &mut String) {
        if text.is_empty() {
            return;
        }
        if let Some((_, uri)) = &self.link {
            html.push_str("<a href=\"");
            escape(html, uri);
            html.push_str("\">");
        }
        let class = if self.cursor { " class=\"cursor\"" } else { "" };
        match (self.css.as_str(), class) {
            ("", "") => html.push_str(text),
            ("", class) => { let _ = write!(html, "<span{class}>{text}</span>"); }
            (css, class) => { let _ = write!(html, "<span{class} style=\"{css}\">{text}</span>"); }
        }
        if self.link.is_some() {
            html.push_str("</a>");
        }
        text.clear();
    }
}

/// The page for the final screen of `parser`.
fn page(parser: &Parser, s: &Settings) -> String {
    let screen = parser.screen();
    let colours = Colours::new(screen, s.light);
    let window = screen.window();
    let cursor = (s.cursor && screen.mode(Mode::ShowCursor)).then(|| screen.cursor_position());
    let mut html = String::new();
    let _ = write!(
        html,
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>"
    );
    escape(&mut html, &s.title);
    let _ = write!(
        html,
        "</title>\n<style>body{{margin:0;background:{bg}}}pre.term{{margin:0;padding:8px;color:{fg};background:{bg};\
font:14px/1.2 ui-monospace,Menlo,Consolas,monospace;display:inline-block}}\
pre.term a{{color:inherit}}pre.term .cursor{{outline:1px solid {fg}}}</style></head>\n<body><pre class=\"term\">",
        fg = hex(colours.fg),
        bg = hex(colours.bg)
    );
    for (r, row) in window.iter() {
        // Runs of cells with one style, link and cursor become one span.
        let mut run = Run::default();
        let mut text = String::new();
        for (col, cell) in row.glyphs() {
            let link = row.link(usize::from(col)).and_then(|l| Some((l.key(), safe_uri(l)?.to_owned())));
            let next = Run {
                css: colours.style(cell.attributes()),
                link,
                cursor: cursor == Some((r, col)),
            };
            if next != run {
                run.write(&mut html, &mut text);
                run = next;
            }
            if cell.attributes().hidden() {
                text.push_str(if cell.is_wide() { "  " } else { " " });
            } else {
                escape(&mut text, cell.shown());
            }
        }
        run.write(&mut html, &mut text);
        html.push('\n');
    }
    html.push_str("</pre></body></html>\n");
    html
}

fn main() -> std::process::ExitCode {
    match parse_args(std::env::args().skip(1)).and_then(|s| shoot(&s)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) if e.is_empty() => {
            println!("{USAGE}");
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("vt-html-shot: {e}\n{USAGE}");
            std::process::ExitCode::from(2)
        }
    }
}

fn shoot(s: &Settings) -> Result<(), String> {
    let mut input = Vec::new();
    let read = match s.file.as_deref() {
        None | Some("-") => std::io::stdin().take(MAX_INPUT + 1).read_to_end(&mut input),
        Some(path) => std::fs::File::open(path)
            .and_then(|f| f.take(MAX_INPUT + 1).read_to_end(&mut input))
            .map_err(|e| std::io::Error::new(e.kind(), format!("{path}: {e}"))),
    };
    read.map_err(|e| e.to_string())?;
    if input.len() as u64 > MAX_INPUT {
        return Err(format!("input is over {MAX_INPUT} bytes"));
    }
    let parser = run(&input, s.rows, s.cols, !s.raw)?;
    let html = page(&parser, s);
    let mut out = std::io::stdout().lock();
    out.write_all(html.as_bytes()).and_then(|()| out.flush()).map_err(|e| format!("writing the page: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(input: &[u8], rows: u16, cols: u16) -> String {
        let s = Settings { rows, cols, light: false, cursor: false, raw: false, title: "t".into(), file: None };
        page(&run(input, rows, cols, false).unwrap(), &s)
    }

    #[test]
    fn colours_renditions_and_links_reach_the_page() {
        let html = shot(
            b"\x1b[1;31mred\x1b[0m \x1b[3;4:3;38;2;1;2;3mx\x1b[0m \x1b[7mrev\x1b[0m \
              \x1b]8;;https://example.com/?a=1&b=2\x1b\\link\x1b]8;;\x1b\\ \
              \x1b]8;;javascript:alert(1)\x1b\\bad\x1b]8;;\x1b\\ <&>",
            2,
            40,
        );
        assert!(html.contains("<span style=\"color:#cd0000;font-weight:bold;\">red</span>"), "{html}");
        assert!(html.contains("color:#010203;font-style:italic;text-decoration:underline wavy;\">x<"), "{html}");
        // Reverse: default fg and bg swapped.
        assert!(html.contains("<span style=\"color:#000000;background:#e5e5e5;\">rev</span>"), "{html}");
        assert!(html.contains("<a href=\"https://example.com/?a=1&amp;b=2\">link</a>"), "{html}");
        assert!(!html.contains("javascript"), "{html}");
        assert!(html.contains(" bad &lt;&amp;&gt;"), "{html}");
    }

    #[test]
    fn the_final_screen_is_drawn_after_scrolling_reverse_video_and_wide_glyphs() {
        // Three lines on a two-row screen: only the last two remain. DECSCNM
        // swaps the page's colours; a wide glyph is written once.
        let html = shot("one\r\ntwo\r\n界x\x1b[?5h".as_bytes(), 2, 6);
        let body = html.split("<pre class=\"term\">").nth(1).unwrap();
        assert_eq!(body, "two   \n界x   \n</pre></body></html>\n");
        assert!(html.contains("color:#000000;background:#e5e5e5"), "{html}");
    }
}
