//! The pane-file codec: turning a terminal's screens, modes, links, marks and
//! host colours into the text formats fux writes to a pane folder, and reading
//! them back. No filesystem I/O happens here, so the same formats travel over
//! disk files, memory, or a network stream.

use crate::width::Joiner;
use crate::{
    BACKGROUND, Cell, Color, FOREGROUND, LINKS_LIST_MAX, MARKS, Mark, MarkKind, Own, PAD, Picture, SIZE_MAX, Screen,
    Style, Term, colour, continues, text_cells, with_next,
};
use std::fmt::Write as _;

/// A screen as its files tell it: its rows as plain text (a line each, blanks at the end cut) and with
/// their looks; `lines`, how many lines are before its first row (the pane's text's, on the main screen;
/// 0 on the alternate one, which is in no text), then the rows that go on; its links; its pictures.
pub struct Files<'a> {
    pub screen: String,
    pub styled: String,
    pub lines: String,
    pub links: String,
    pub pictures: &'a [Picture],
}

/// What a screen's files say: the main one's, or (`alt`) the alternate one's while a program has it.
pub fn render(term: &Term, alt: bool) -> Option<Files<'_>> {
    let (grid, pictures) = match (alt, term.screen.alt) {
        (false, true) => (&term.other, &term.other_pictures),
        (true, false) => return None,
        _ => (&term.screen.grid, &term.screen.pictures),
    };
    let top = if alt { 0 } else { term.tail.lines };
    let (screen, lines) = (text(grid), format!("{top}\n{}\n", goes_on(term, grid, !alt)));
    Some(Files { screen, styled: looks(grid, &term.own), lines, links: links_of(term, grid, top), pictures })
}

/// The screen a `styled` text says, `cols` wide and of as many rows as it has lines (a reader's
/// side of `render`): cells and looks, and none of what `parse_modes` and `set_link` say.
pub fn read_screen(styled: &str, cols: usize) -> Screen {
    let mut term = Term::sized(cols, styled.lines().count(), false);
    for (row, line) in styled.lines().enumerate().take(SIZE_MAX) {
        // The rows are text and colours, as the holder writes them: anything else in a
        // file of someone's own making is shown as `?`.
        let mut text = String::with_capacity(line.len());
        let mut rest = line;
        while let Some(at) = rest.find(|c: char| c.is_control()) {
            let (before, after) = rest.split_at(at);
            let colour = after.strip_prefix("\x1b[").and_then(|sgr| {
                let end = sgr.find(|c: char| !(c.is_ascii_digit() || c == ';' || c == ':'))?;
                sgr.get(end..)?.starts_with('m').then_some(2 + end + 1)
            });
            text.push_str(before);
            text.push_str(colour.and_then(|length| after.get(..length)).unwrap_or("?"));
            let length = colour.or(after.chars().next().map(char::len_utf8)).unwrap_or(1);
            rest = after.get(length..).unwrap_or_default();
        }
        text.push_str(rest);
        term.feed(format!("\x1b[{};1H\x1b[m{text}", row + 1).as_bytes());
        term.feed(format!("\x1b[{};{}H\x1b[m\x1b[K", row + 1, term.screen.cols).as_bytes());
    }
    if cols > 0 {
        for row in &mut term.screen.grid {
            row.pop();
        }
        term.screen.cols = term.screen.cols.saturating_sub(1);
    }
    term.screen
}

/// Says of a cell which link it is part of (for a reader that has the links from a file).
pub fn set_link(screen: &mut Screen, row: usize, col: usize, link: u16) {
    if let Some(cell) = screen.grid.get_mut(row).and_then(|cells| cells.get_mut(col)) {
        cell.link = link;
    }
}

/// How input is to be sent to the program, as words: what the `modes` file says.
pub fn modes(term: &mut Term) -> String {
    format_modes(&mut term.screen)
}

/// Formats a screen's input modes as space-separated words.
pub fn format_modes(screen: &mut Screen) -> String {
    let mouse = (screen.mouse != 0).then(|| format!("mouse{}", screen.mouse));
    // And the keyboard flags a program asked for (the kitty protocol), as a number.
    let keys = (screen.keys() != 0).then(|| format!("keys{}", screen.keys()));
    let on = screen.switches().into_iter().filter(|s| *s.2 && !s.1.is_empty()).map(|s| s.1.to_string());
    on.chain(mouse).chain(keys).collect::<Vec<String>>().join(" ")
}

/// Takes the modes those words say (a reader's side of `format_modes`).
pub fn parse_modes(screen: &mut Screen, words: &str) {
    let has = |mode: &str| words.split_whitespace().any(|m| m == mode);
    screen.switches().into_iter().filter(|s| !s.1.is_empty()).for_each(|(_, name, on)| *on = has(name));
    screen.mouse = [9, 1000, 1002, 1003].into_iter().find(|m| has(&format!("mouse{m}"))).unwrap_or(0);
    // (Whichever screen is shown has the keyboard flags.)
    let keys = words.split_whitespace().find_map(|m| m.strip_prefix("keys")?.parse().ok()).unwrap_or(0);
    screen.keys = [(keys, Vec::new()), (keys, Vec::new())];
}

/// The shell's marks formatted for the `marks` file: `LINE AT KIND [CODE]`.
pub fn marks(term: &Term) -> String {
    let mut out = String::new();
    for Mark { line, byte, kind, exit } in term.marks() {
        let name = match kind {
            MarkKind::Prompt => "prompt",
            MarkKind::Command => "command",
            MarkKind::Output => "output",
            MarkKind::End => "end",
        };
        let _ = write!(out, "{line} {byte} {name}");
        let _ = writeln!(out, "{}", exit.map_or(String::new(), |code| format!(" {code}")));
    }
    out
}

/// Parses a session `colours` file (`foreground #rrggbb`, `background #rrggbb`, `cursor #rrggbb`,
/// or `0..255 #rrggbb`; last line for each name wins, and an invalid colour clears it) into `told`.
pub fn parse_colours(text: &str, told: &mut crate::Told) {
    told.fg = None;
    told.bg = None;
    told.cursor = None;
    told.palette.clear();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let Some(name) = words.next() else { continue };
        let rgb = words.next().filter(|value| value.len() == 7).and_then(colour);
        match name {
            "foreground" => told.fg = rgb,
            "background" => told.bg = rgb,
            "cursor" => told.cursor = rgb,
            index => {
                if let Ok(slot) = index.parse::<u8>()
                    && slot.to_string() == index
                {
                    match rgb {
                        Some(rgb) => _ = told.palette.insert(slot, rgb),
                        None => _ = told.palette.remove(&slot),
                    }
                }
            }
        }
    }
}

/// A wrapped row keeps its trailing spaces, except for a wide character's padding cell.
pub(crate) fn row_text(row: &[Cell], goes_on: bool, out: &mut String) {
    let blank = |cell: &Cell| cell.ch == '\0' || (cell.ch == ' ' && cell.more.first() == Some(&'\0'));
    let used = if goes_on {
        text_cells(row).len()
    } else {
        row.iter().rposition(|cell| !blank(cell)).map_or(0, |last| last + 1)
    };
    out.extend(row.iter().take(used).flat_map(Cell::chars));
}

fn text(grid: &[Vec<Cell>]) -> String {
    let mut out = String::new();
    for (row, goes_on) in with_next(grid) {
        row_text(row, goes_on, &mut out);
        out.push('\n');
    }
    out
}

/// A screen with colours: text and SGR codes only, each row on a line
/// that starts and ends in the default style. A colour the program set for a palette number
/// or for the two it draws with by default is told as what it set (direct colour), wherever
/// a cell has that number or that default: so with a background of its own every cell of
/// the screen has it, the blank ones too, and every row is as long as the screen is wide.
fn looks(grid: &[Vec<Cell>], own: &Own) -> String {
    let mut out = String::new();
    for (row, goes_on) in with_next(grid) {
        row_styled(row, goes_on, true, own, &mut Joiner::default(), &mut out);
        out.push('\n');
    }
    out
}

/// A row with its looks, from the default style to the default style, and no line's end.
/// `own` are the colours the program set: a cell that has such a palette number or default
/// is told in that colour. `joiner` is what came before the row in its line, and then what
/// the row leaves.
/// `screen`: the cell a wide character left empty at the row's end is told too, if it has a
/// look (it is no text: U+200B before it says so, and a history has none of it).
pub(crate) fn row_styled(row: &[Cell], goes_on: bool, screen: bool, own: &Own, joiner: &mut Joiner, out: &mut String) {
    let plain = Style::default();
    let own = |color: Color, default: Option<u16>| {
        let slot = match color {
            Color::Default => default,
            Color::Indexed(n) => Some(u16::from(n)),
            Color::Rgb(..) => None,
        };
        slot.and_then(|slot| own.get(&slot)).map_or(color, |[r, g, b]| Color::Rgb(*r, *g, *b))
    };
    // (A blank at a row's end is not told for a foreground of its own alone: it shows none.)
    let blank = Cell { style: Style { fg: own(Color::Default, Some(FOREGROUND)), ..plain }, ..Cell::blank(plain) };
    // (The marks are no looks, and a link is none here: the file has no links. An underline
    // with no colour of its own has the text's.)
    let look = |cell: &Cell| {
        let Style { fg, bg, ul, flags } = cell.style;
        let (fg, bg, ul) = (own(fg, Some(FOREGROUND)), own(bg, Some(BACKGROUND)), own(ul, None));
        Cell { style: Style { fg, bg, ul, flags: flags & !MARKS }, link: 0, ..*cell }
    };
    // (A row that goes on in the next keeps the blanks at its end, as in `plain`.)
    let gap = row.last().filter(|cell| goes_on && cell.style.flags & PAD != 0);
    let used = match goes_on {
        true => row.len() - usize::from(gap.is_some()),
        false => row.iter().rposition(|c| look(c) != blank).map_or(0, |last| last + 1),
    };
    let gap = gap.filter(|cell| screen && look(cell) != blank);
    let mut style = plain;
    for cell in row.iter().take(used).chain(gap).filter(|cell| cell.ch != '\0') {
        if look(cell).style != style {
            style = look(cell).style;
            style.sgr(out);
        }
        if gap.is_some_and(|gap| std::ptr::eq(gap, cell)) {
            out.push('\u{200b}');
        }
        // Two characters that would read back as one are kept apart by U+200B, which
        // takes no room and is no part of either.
        if joiner.joins(cell.ch) {
            out.push('\u{200b}');
            *joiner = Joiner::default();
            joiner.joins(cell.ch);
        }
        cell.more.iter().take_while(|&&c| c != '\0').for_each(|&c| _ = joiner.joins(c));
        out.extend(cell.chars());
    }
    if style != plain {
        out.push_str("\x1b[m");
    }
}

/// The links on a screen, a line each: `LINE COL LEN ADDRESS`. LINE is the line's number, the
/// first row's being `top`; COL counts cells from the start of that line's first row on the
/// screen, a whole row for each row before; LEN is how many cells, in reading order.
fn links_of(term: &Term, grid: &[Vec<Cell>], top: usize) -> String {
    // (A list that would be longer than a reader reads ends before the run that makes it so.)
    let (mut out, mut full) = (String::new(), false);
    for (line, rows) in (top..).zip(grid.chunk_by(|_, next| continues(next))) {
        // A run is a line's cells of one link, one after the other: no link after the line's
        // last cell ends the run that reaches it.
        let (mut start, mut open) = (0, 0);
        for (at, link) in rows.iter().flatten().map(|cell| cell.link).chain([0]).enumerate() {
            if link != open && open != 0 {
                let address = term.link(open);
                let record = format!("{line} {start} {} {}\n", at - start, address.unwrap_or_default());
                full |= out.len() + record.len() > LINKS_LIST_MAX;
                if address.is_some() && !full {
                    out.push_str(&record);
                }
            }
            (start, open) = if link == open { (start, open) } else { (at, link) };
        }
    }
    out
}

/// The rows of a screen that go on from the row above (the text wrapped into them), as numbers
/// from 0: what its text cannot say. The main screen's first row may go on from history's last line.
fn goes_on(term: &Term, grid: &[Vec<Cell>], main: bool) -> String {
    let open = term.tail.open.is_some() && main;
    let rows = grid.iter().enumerate().filter(|(y, row)| continues(row) && (*y > 0 || open));
    rows.map(|(y, _)| y.to_string()).collect::<Vec<String>>().join(" ")
}
