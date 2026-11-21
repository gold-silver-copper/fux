//! Properties of the emulator. None of them
//! fixes what a screen looks like beyond what its program asked for.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use crate::width::Joiner;
use crate::{Cell, Out, PICTURES_MAX, Screen, Term, continues, with_next};

impl Term {
    /// What waits to be taken, for the tests to look at.
    pub fn peek(&self) -> &Out {
        &self.out
    }

    /// For tests: the main screen's text as lines (rows that go on joined), all of it and the part
    /// before the cursor; and whether every wide character on both screens has both its cells.
    pub fn lines(&self) -> (String, String, bool) {
        let (main, row) = (self.main(), if self.screen.alt { self.saved[0].row } else { self.screen.cursor.row });
        let (mut all, mut before, mut open) = (Out::default(), String::new(), self.tail);
        for (y, (cells, goes_on)) in with_next(main).enumerate() {
            if y == row {
                before.clone_from(&all.history);
                if open.open.is_some() && !continues(cells) {
                    before.push('\n');
                }
                before.extend(cells.iter().take(self.screen.cursor.place().1).flat_map(Cell::chars));
            }
            open.joiner = Joiner::default();
            Term::row_history(cells, goes_on, &self.own, &mut open, &mut all);
        }
        let whole = [&self.screen.grid, &self.other].iter().all(|grid| {
            grid.len() == self.screen.rows
                && grid.iter().all(|cells| {
                    cells.len() == self.screen.cols
                        && (0..=cells.len()).all(|x| {
                            let half = cells.get(x).is_some_and(|cell| cell.ch == '\0');
                            let lead = x.checked_sub(1).and_then(|x| cells.get(x));
                            half == lead.is_some_and(|cell| cell.ch != '\0' && cell.wide())
                        })
                })
        });
        (all.history, before, whole)
    }
}

fn term(cols: usize, rows: usize, bytes: &str) -> Term {
    let mut term = Term::new(cols, rows);
    term.feed(bytes.as_bytes());
    term
}

#[test]
fn every_line_printed_is_on_the_screen_or_in_history_in_order() {
    let lines: Vec<String> = (0..100).map(|n| format!("line {n}")).collect();
    let term = term(20, 7, &lines.join("\r\n"));
    let all = term.peek().history.clone() + &term.plain();
    assert_eq!(all.lines().filter(|l| !l.is_empty()).collect::<Vec<_>>(), lines);
}

#[test]
fn styled_history_has_historys_lines_with_their_looks() {
    // A line with colours, one that wraps (its rows are joined, each from and to the default
    // style), wide characters, a link (text only: history is text), blanks with a colour to the
    // row's end, and a line with none of it: what a plain row costs is its text once more.
    let link = "\x1b]8;;http://x.example/\x1b\\link\x1b]8;;\x1b\\";
    let text = format!(
        "\x1b[1;31mred\x1b[m plain\r\n\x1b[32mgreen wraps\x1b[m\r\n日本 {link}\r\n\x1b[44mbar\x1b[K\x1b[m\r\nbare\r\n"
    );
    let mut term = term(8, 2, &format!("{text}1\r\n2"));
    assert_eq!(term.peek().history, "red plain\ngreen wraps\n日本 link\nbar\nbare\n");
    let styled = "\x1b[0;1;31mred\x1b[0m plain\n\x1b[0;32mgreen wr\x1b[m\x1b[0;32maps\x1b[m\n日本 link\n\x1b[0;44mbar     \x1b[m\nbare\n";
    assert_eq!(term.peek().styled, styled);
    // Rows a resize sends there go to both, and are taken back from both: from what was not
    // yet taken, and by the count of what was.
    let colours = "\x1b[35mab\x1b[36mcdefgh\x1b[m";
    let mut wide = crate::tests::term(8, 2, colours);
    wide.resize(2, 2);
    assert!(same_lines(&wide) && wide.peek().history == "abcd");
    let sent = wide.take(0);
    assert_eq!(sent.styled, "\x1b[0;35mab\x1b[m\x1b[0;36mcd\x1b[m");
    wide.resize(8, 2);
    assert!(wide.peek().history.is_empty() && wide.peek().styled.is_empty());
    assert_eq!((wide.peek().unhistory, wide.peek().unstyled), (4, sent.styled.len()));
    wide.resize(4, 1);
    wide.resize(8, 2);
    assert!(wide.peek().styled.is_empty() && wide.peek().unstyled > 0);
    // A line whose rest was cleared away ends in both; a full reset leaves both as they are.
    let mut cleared = crate::tests::term(4, 2, "\x1b[31mx\r\naaaabb\r\n\x1b[H\x1b[2J$ ls\r\nfile\r\n");
    assert!(same_lines(&cleared) && cleared.peek().history == "x\naaaa\n$ ls\n", "{:?}", cleared.peek().history);
    let kept = cleared.peek().styled.clone();
    cleared.feed(b"\x1bc");
    assert!(cleared.peek().styled == kept && kept.matches('\n').count() == 3 && kept.contains("\x1b[0;31maaaa"));
    term.feed(b"\x1bc\x1b[33mafter\r\n\r\n\r\n");
    assert!(same_lines(&term) && term.peek().styled.ends_with("\x1b[0;33mafter\x1b[m\n\n"));
    // Where a line's rows are joined, two characters that were two cells stay two: U+200B is
    // between them as it is inside a row, also before a row that is copied as it is. A blank
    // that is only part of a link is no look, and is cut at a line's end as in `history`.
    let flag = crate::tests::term(2, 2, "a\u{1f1ef}X\x1b[2;1H\u{1f1f5}\r\n\r\n");
    assert_eq!(flag.peek().styled, "a\u{1f1ef}\u{200b}\u{1f1f5}\n");
    let prepend = crate::tests::term(2, 2, "abX\x1b[1;2H\u{600}\x1b[2;2H\r\n\r\n");
    assert_eq!(prepend.peek().styled, "a\u{600}\u{200b}X\n");
    let linked = crate::tests::term(4, 1, "x\x1b]8;;https://example.test\x07 \x1b]8;;\x07\r\n");
    assert_eq!(linked.peek().styled, "x\n");
}

#[test]
fn history_waiting_to_be_written_is_bounded_in_both_texts_and_a_row_refused_ends_its_line_in_both() {
    // One line of 65,536 rows of one cell, each 82 bytes with its looks: past 4 MiB of those
    // the rows are refused, the line is ended in both texts, and the next line is its own.
    let looks = "\x1b[1;2;3;4:3;5;7;8;9;38;2;255;255;255;48;2;255;255;255;58;2;255;255;255m";
    let mut term = term(1, 1, &format!("{looks}x\x1b[65535b\r\n"));
    let first = term.take(0);
    assert!(first.styled.len() < (4 << 20) + 100 && first.history.ends_with("x\n") && first.refused.len() == 1);
    assert_eq!(first.history.matches('\n').count(), 1);
    term.feed(b"\x1b[mY\r\n");
    assert_eq!((term.peek().history.as_str(), term.peek().styled.as_str()), ("Y\n", "Y\n"));
    // A resize is held to the same: 499 rows of 500 cells, each cell with colours of its own.
    let cells = "\x1b[38;2;254;255;255ma\x1b[38;2;255;255;255ma".repeat(125_000);
    let mut term = crate::tests::term(500, 500, &format!("\x1b[48;2;255;255;255m{cells}"));
    term.resize(500, 1);
    assert!(same_lines(&term) && term.peek().refused.len() == 1);
    assert!(term.peek().styled.len() < (4 << 20) + (64 << 10), "{}", term.peek().styled.len());
    // (The rows kept to come back do: what was given of them is taken back, and no more.)
    let sent = term.take(0);
    term.resize(500, 500);
    assert!(term.peek().unstyled <= sent.styled.len() && term.plain().starts_with("aaa"));
    // And what waits of them is counted once: rows of 2.2 MiB that a resize sent there are taken
    // back and given again by the next, with nothing taken between, and none is refused.
    let mut term = crate::tests::term(200, 300, &format!("\x1b[48;2;255;255;255m{}", &cells[..cells.len() * 6 / 25]));
    term.resize(200, 2);
    assert!(term.peek().styled.len() > 2 << 20);
    term.resize(200, 1);
    assert!(term.peek().refused.is_empty() && same_lines(&term), "{:?}", term.peek().refused);
}

#[test]
fn the_styled_screen_read_back_is_the_same_screen() {
    let mut first = term(30, 6, "plain \x1b[1;31mbold red\x1b[0m \x1b[48;5;200m256\x1b[38;2;1;2;3mrgb\x1b[m\r\n");
    first.feed(
        "wide 日本語 e\u{301} \x1b[4:3mcurly\x1b[m \x1b[7mrev\x1b[27m\x1b[44m\x1b[K\r\n\x1b(0lqk\x1b(B".as_bytes(),
    );
    let second = Term { screen: Screen::read(&first.styled(), 30), ..Term::new(30, 6) };
    assert_eq!(first.styled(), second.styled());
    assert_eq!(first.plain(), second.plain());
    assert!(first.plain().contains("wide 日本語 e\u{301} curly rev") && first.plain().contains("┌─┐"));
    // (A control character of more than one byte is one `?` like any other, and the row goes on after it.)
    let odd = Term { screen: Screen::read("A\u{9b}B\n", 8), ..Term::new(8, 1) };
    assert_eq!(odd.plain(), "A?B\n");
}

#[test]
fn what_a_program_writes_never_puts_a_control_code_of_its_own_in_a_file() {
    // Bytes from a fixed generator, rich in escapes, string starts and broken UTF-8.
    let mut seed = 7u32;
    let mut bytes = Vec::new();
    for _ in 0..200_000 {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        let pick = (seed >> 16) as u8;
        bytes.push(
            *b"\x1b\x1b[]P\\;:?0123456789mHJKhlqrABCD\x07\x08\r\n\x9b\x90\xe2\x82\xf0\xff"
                .get(usize::from(pick) % 60)
                .unwrap_or(&pick),
        );
    }
    let mut term = Term::new(40, 10);
    term.feed(&bytes);
    let mut rest = term.styled();
    // Take out the colour codes the emulator wrote itself: nothing else may be a control.
    while let Some(start) = rest.find("\x1b[") {
        let end = rest.get(start..).and_then(|code| code.find('m')).expect("a colour code ends in m");
        assert!(
            rest.get(start + 2..start + end)
                .is_some_and(|inside| inside.bytes().all(|b| b.is_ascii_digit() || b == b';'))
        );
        rest.replace_range(start..=start + end, "");
    }
    for text in [rest, term.plain(), term.peek().history.clone()] {
        assert!(text.chars().all(|c| c == '\n' || !c.is_control()), "a control character reached a file");
    }
    assert!(term.screen.pictures.len() <= PICTURES_MAX && term.peek().reply.len() <= 4096);
}

#[test]
fn a_colour_a_program_sets_is_its_panes_until_it_gives_it_back() {
    let mut pane = Term::new(4, 2);
    pane.told.colours = "foreground #d0d0d0\nbackground #1c1c1c\n1 #cc0000\n".into();
    let answers = |pane: &mut Term, asks: &str| {
        pane.feed(asks.as_bytes());
        String::from_utf8(pane.take(0).reply).unwrap().replace("\x1b]", " ").replace('\x07', "")
    };
    // Text in palette colour 1 on colour 2, an underline in colour 3, and text in the defaults.
    pane.feed(b"\x1b[31;42;4;58:5:3ma\x1b[mb\r\n\x1b[7mc");
    let before = pane.styled();
    assert_eq!(before, "\x1b[0;4;31;42m\x1b[58:5:3ma\x1b[0mb\n\x1b[0;7mc\x1b[m\n");
    // The program sets colours in every form there is, several in one string, asking in between:
    // each is told as it was set (its first eight bits; `#f80` is f0 88 00 as X reads it, and
    // `rgb:f/8/0` is ff 88 00), and what it did not set is told as before.
    let told = answers(&mut pane, "\x1b]4;1;#ff8000;2;rgb:1/22/333;1;?;2;?;3;?;3;rgb:ffff/0000/8000;3;?\x07");
    assert_eq!(told, " 4;1;rgb:ffff/8080/0000 4;2;rgb:1111/2222/3333 4;3;rgb:ffff/0000/8080");
    let told = answers(&mut pane, "\x1b]10;#f80;rgb:f/8/0;#123456789abc\x07\x1b]10;?;?;?;?\x07\x1b]12;?\x07");
    assert_eq!(told, " 10;rgb:f0f0/8080/0000 11;rgb:ffff/8888/0000 12;rgb:1212/5656/9a9a 12;rgb:1212/5656/9a9a");
    assert_eq!(pane.cursor_colour(), Some([0x12, 0x56, 0x9a]));
    // The cells are as they were written, and are shown in the colours set, text that was there
    // before among it: direct colour for a palette number that was set and for the two defaults,
    // so that every cell of the screen has the background, the blank ones too.
    let (fg, bg) = ("38;2;240;128;0", "48;2;255;136;0");
    let first = format!("\x1b[0;4;38;2;255;128;0;48;2;17;34;51m\x1b[58:2::255:0:128ma\x1b[0;{fg};{bg}mb  \x1b[m\n");
    assert_eq!(pane.styled(), format!("{first}\x1b[0;7;{fg};{bg}mc\x1b[0;{fg};{bg}m   \x1b[m\n"));
    assert_eq!(pane.plain(), "ab\nc\n");
    // The alternate screen has the same colours; what the program asks about its looks is what it wrote.
    pane.feed(b"\x1b[?1049h\x1b[H\x1b[0;31mz");
    assert!(pane.styled().starts_with("\x1b[0;38;2;255;128;0;48;2;255;136;0mz"), "{:?}", pane.styled());
    assert_eq!(answers(&mut pane, "\x1bP$qm\x1b\\\x1b[?1049l\x1b[m"), "\x1bP1$r0;31m\x1b\\");
    // What sets nothing is said in the log, once for each kind until the log is written: what
    // is no colour (a name, `rgbi:`, a part too long, missing or one too many, no value at all),
    // and a number that is no palette number. Each of these alone is said, and none sets a
    // colour; a question about a number that is none is passed over without a word.
    let (no_colour, no_number) = (
        "a colour was not set: it was not `#` with hex digits or `rgb:R/G/B`",
        "a colour was not set: its number was not 0 to 255",
    );
    let slashes = format!("rgb:{}", "/".repeat(1 << 20));
    for bad in ["red", "rgbi:1/0/0", "rgb:12345/0/0", "rgb:0/0", "rgb:1/2/3/4", "#12345", "#+1+2+3", "", &slashes] {
        pane.feed(format!("\x1b]4;3;{bad}\x07\x1b]11;{bad}\x07").as_bytes());
        assert_eq!(pane.take(0).refused, [no_colour], "{bad}");
    }
    for bad in ["256", "x", "-1", ""] {
        pane.feed(format!("\x1b]4;{bad};#000000;{bad};?\x07").as_bytes());
        assert_eq!(pane.take(0).refused, [no_number], "{bad}");
    }
    assert_eq!(answers(&mut pane, "\x1b]4;1;red;1;?;3;?\x07\x1b]10;?;?\x07").split(' ').count(), 5);
    // One string may set more colours than a counter of 16 bits counts, and ask more than is
    // answered: 16 KiB of answers wait at most (said in the log), and what the string sets after
    // them is set all the same.
    pane.feed(format!("\x1b]4;{}7;#070707\x07", "0;#000;".repeat(1 << 16)).as_bytes());
    // (Here 646 answers of 25 bytes and 8 of 26; a ninth would have filled the 16 KiB to the byte.)
    pane.feed(format!("\x1b]4;10;#000;{}{}8;#080808\x07", "0;?;".repeat(646), "10;?;".repeat(9)).as_bytes());
    let (out, dropped) = (pane.take(0), "an answer to the program was dropped: 16 KiB of answers were waiting");
    assert_eq!((out.reply.len(), out.refused), (646 * 25 + 8 * 26, vec![dropped]));
    let told = answers(&mut pane, "\x1b]4;7;?;8;?\x07\x1b]104;0;7;8;10\x07");
    assert_eq!(told, " 4;7;rgb:0707/0707/0707 4;8;rgb:0808/0808/0808");
    // The cell a wide character left empty at a row's end has the background as every other: the
    // screen with colours tells it, after U+200B since it is no text, and no other file does.
    let mut gap = Term::new(4, 2);
    gap.feed("\x1b]11;#123456\x07ABC\u{754c}".as_bytes());
    let bg = "\x1b[0;48;2;18;52;86m";
    assert_eq!(gap.styled(), format!("{bg}ABC\u{200b} \x1b[m\n{bg}\u{754c}  \x1b[m\n"));
    assert_eq!((gap.plain().as_str(), gap.wraps().as_str()), ("ABC\n\u{754c}\n", "1"));
    gap.feed(b"\r\n\r\n");
    let styled = format!("{bg}ABC\x1b[m{bg}\u{754c}  \x1b[m\n");
    assert_eq!((gap.peek().history.as_str(), gap.peek().styled.as_str()), ("ABC\u{754c}\n", styled.as_str()));
    // Given back one by one (104 with numbers, 110, 111, 112), a colour is the terminal's again.
    let asks = "\x1b]4;1;?;2;?;3;?\x07\x1b]10;?;?;?\x07";
    let told = answers(&mut pane, &format!("\x1b]104;1;x;300;3\x07\x1b]111\x07\x1b]112\x07{asks}"));
    assert_eq!(told, " 4;1;rgb:cccc/0000/0000 4;2;rgb:1111/2222/3333 10;rgb:f0f0/8080/0000 11;rgb:1c1c/1c1c/1c1c");
    assert_eq!(pane.cursor_colour(), None);
    assert_eq!(
        pane.styled().lines().next(),
        Some("\x1b[0;4;31;48;2;17;34;51m\x1b[58:5:3ma\x1b[0;38;2;240;128;0mb\x1b[m")
    );
    // All 256 can be set, and no more is kept than those; 104 alone gives back the palette
    // and 110 the foreground: the screen is published as it was at first.
    let all: String = (0..300).map(|n| format!("\x1b]4;{n};#0000{:02x}\x07", n % 256)).collect();
    assert_eq!(
        answers(&mut pane, &(all + "\x1b]4;255;?;44;?\x07")),
        " 4;255;rgb:0000/0000/ffff 4;44;rgb:0000/0000/2c2c"
    );
    assert_eq!(
        answers(&mut pane, &format!("\x1b]104\x07\x1b]110\x07{asks}")),
        " 4;1;rgb:cccc/0000/0000 10;rgb:d0d0/d0d0/d0d0 11;rgb:1c1c/1c1c/1c1c"
    );
    assert_eq!(pane.styled(), before);
    // A full reset gives back all of them.
    let told = answers(&mut pane, &format!("\x1b]4;1;#000001\x07\x1b]11;#000002\x07\x1b]12;#000003\x07\x1bc{asks}"));
    assert_eq!(told, " 4;1;rgb:cccc/0000/0000 10;rgb:d0d0/d0d0/d0d0 11;rgb:1c1c/1c1c/1c1c");
    assert_eq!((pane.styled(), pane.cursor_colour()), ("\n\n".to_string(), None));
    // A row that leaves the screen is in the history with colours as it was on the screen then,
    // and stays so: one that left before a colour was set, two after (a row of plain text among
    // them), one as wide as the screen under a background of the pane's own, and a blank one and
    // one of text after all were given back.
    pane.feed(b"\x1b[31ma\x1b[m\r\nb\r\n\x1b]4;1;#010203\x07\x1b]10;#040506\x07\x1b[31mc\x1b[m\r\nd\r\n");
    pane.feed(b"\x1b]11;#070809\x07\r\n\x1b]104\x07\x1b]110\x07\x1b]111\x07e\r\n\r\n");
    let (red, own, fg) = ("\x1b[0;31ma\x1b[m\n", "\x1b[0;38;2;1;2;3mc\x1b[m\n", "38;2;4;5;6");
    let styled = format!("{red}\x1b[0;{fg}mb\x1b[m\n{own}\x1b[0;{fg};48;2;7;8;9md   \x1b[m\n\ne\n");
    assert_eq!((pane.peek().history.as_str(), pane.peek().styled.as_str()), ("a\nb\nc\nd\n\ne\n", styled.as_str()));
    // Only a row a resize sent there, which the next resize takes back, is told again: as it is then.
    let mut pane = term(4, 2, "\x1b[31ma\x1b[m\r\nb");
    pane.resize(4, 1);
    assert!(pane.peek().styled == red);
    pane.feed(b"\x1b]4;1;#010203\x07");
    pane.resize(3, 1);
    assert!(pane.peek().styled == own.replace('c', "a"), "{:?}", pane.peek().styled);
}

#[test]
fn a_program_says_where_it_is_what_to_notice_and_how_it_draws() {
    // The directory it is in (OSC 7), with the host it names; a path's odd bytes come as %XX.
    let mut term = term(20, 2, "\x1b]7;file://box/tmp/a%20b\x07");
    assert_eq!(term.take(0).cwd, Some(("box".to_string(), "/tmp/a b".to_string())));
    term.feed(b"\x1b]7;file:///home\x1b\\\x1b]7;nonsense\x07\x1b]7;file:///a%00b\x07");
    assert_eq!(term.take(0).cwd, Some((String::new(), "/home".to_string())));
    // A path that is no path (a control character, bytes that are no text, too long) is not
    // told: only the machine named is.
    for path in ["/a%00b".to_string(), "/a\u{fffd}".to_string(), "/".repeat(5000)] {
        term.feed(format!("\x1b]7;file://far{path}\x07").as_bytes());
        assert_eq!(term.take(0).cwd, Some(("far".to_string(), String::new())));
    }
    // What it wants noticed: the bell (no words), and words in three forms, the last of them kept.
    term.feed(b"\x07");
    assert_eq!(term.take(0).notice.as_deref(), Some(""));
    // (Not a progress bar or another numbered command after `9;`, nor a question in kitty's form.)
    term.feed(b"\x1b]9;4;1;50\x07\x1b]9;9;/tmp\x07\x1b]99;i=1:p=?;\x07\x1b]99;i=1:e=1;aGk=\x07");
    assert_eq!(term.peek().notice, None);
    term.feed(b"\x1b]9;done\x07\x1b]99;i=1;hello\x1b\\\x1b]777;notify;make;all\x01 built\x07");
    assert_eq!((term.take(0).notice.as_deref(), term.plain().as_str()), (Some("make: all built"), "\n\n"));
    // That it wants to be told when its pane gains the keys is a word among the modes; that it
    // is in the middle of drawing is the holder's to know. Both are told when asked.
    term.feed(b"\x1b[?1004h\x1b[?2026h\x1b[?1004$p\x1b[?2026$p");
    assert_eq!((term.words().as_str(), term.screen.sync), ("focus", true));
    assert_eq!(term.take(0).reply, b"\x1b[?1004;1$y\x1b[?2026;1$y");
    term.feed(b"\x1b[?2026l\x1b[?2026h\x1b[?2026l\x1bc");
    assert!(!term.screen.sync && term.words().is_empty());
    // An underline has a kind and a colour of its own, and both read back from `styled`.
    let mut lined = crate::tests::term(
        12,
        1,
        "\x1b[4:3m\x1b[58:2::1:2:3ma\x1b[59;21mb\x1b[4:4;58;5;9mc\x1b[4:5md\x1b[24me\x1b[4mf\x1b[4:0mg",
    );
    let styled = lined.styled();
    // (In a code of their own, after the plain underline: a terminal that does not know the
    // form with colons passes that code over and has the rest.)
    let start =
        "\x1b[0;4m\x1b[4:3;58:2::1:2:3ma\x1b[0;4m\x1b[4:2mb\x1b[0;4m\x1b[4:4;58:5:9mc\x1b[0;4m\x1b[4:5;58:5:9md";
    assert!(styled.starts_with(start), "{styled:?}");
    assert!(styled.contains("d\x1b[0m\x1b[58:5:9me\x1b[0;4m\x1b[58:5:9mf\x1b[0m\x1b[58:5:9mg"), "{styled:?}");
    let mut back = Term::new(12, 1);
    back.feed(styled.lines().next().unwrap().as_bytes());
    assert_eq!(back.styled(), lined.styled());
    lined.feed(b"\r\x1b[K");
}

#[test]
fn a_shells_marks_are_line_numbers_of_the_pane_that_scrolling_and_resizing_leave_alone() {
    let (a, b, c, d) = ("\x1b]133;A\x07", "\x1b]133;B\x07", "\x1b]133;C\x07", "\x1b]133;D;3\x07");
    let mut term = term(10, 4, &format!("{a}$ {b}ls\r\n{c}one\r\ntwo\r\n{d}{a}$ {b}"));
    let marks = "0 0 prompt\n0 2 command\n1 0 output\n3 0 end 3\n3 0 prompt\n3 2 command\n";
    assert_eq!(term.marks(), marks);
    // The lines scroll into history, are wrapped anew, come back: their numbers are theirs, and
    // the line a number names is the same text.
    let line = |term: &Term, number: usize| {
        let text = term.peek().history.clone() + &term.lines().0;
        text.lines().nth(number).unwrap_or_default().trim_end().to_string()
    };
    term.feed(b"a long command that wraps\r\n");
    assert!(term.top() > 0 && term.marks() == marks);
    assert_eq!(
        (line(&term, 0), line(&term, 1), line(&term, 3)),
        ("$ ls".into(), "one".into(), "$ a long command that wraps".into())
    );
    term.resize(4, 2);
    term.resize(30, 9);
    assert!(term.marks() == marks);
    assert_eq!((line(&term, 1), line(&term, 3)), ("one".into(), "$ a long command that wraps".into()));
    // A mark in a line that wraps says how far into the line it is.
    let wrapped = crate::tests::term(4, 3, &format!("{a}$ {b}abcdef{c}"));
    assert_eq!(wrapped.marks(), "0 0 prompt\n0 2 command\n0 8 output\n");
    // How far is counted in bytes of the line's text, those history has of it too, and not the
    // cell a wide character left empty at a row's end: wrapped anew, a mark is where it was.
    let mut gone = crate::tests::term(5, 2, &format!("{a}$ {b}x\r\n{c}012345678901234567890{d}"));
    assert!(gone.marks().ends_with("1 0 output\n1 21 end 3\n"), "{}", gone.marks());
    let mut wide = crate::tests::term(5, 3, &format!("{c}1234界{d}$"));
    assert_eq!((wide.marks().as_str(), wide.wraps().as_str()), ("0 0 output\n0 7 end 3\n", "1"));
    wide.resize(12, 3);
    gone.resize(30, 4);
    gone.resize(5, 2);
    assert_eq!((wide.marks().as_str(), wide.wraps().as_str()), ("0 0 output\n0 7 end 3\n", ""));
    assert!(gone.marks().ends_with("1 0 output\n1 21 end 3\n"), "{}", gone.marks());
    // Marks on the screen after one that history has: each is on its own text at a new width.
    let mut both = crate::tests::term(10, 3, &format!("{c}one\r\ntwo\r\nthree\r\nfour\r\n{a}$ {b}abcdefgh{c}"));
    let each = "0 0 output\n4 0 prompt\n4 2 command\n4 10 output\n";
    assert_eq!(both.marks(), each);
    both.resize(7, 3);
    assert_eq!(both.marks(), each);
    // A mark is on its text: when a line above it becomes two (the first row of a wrapped line
    // is erased), the mark's line is one further on, as its text is.
    let mut split = crate::tests::term(6, 6, &format!("abcdefghij\r\n{a}$ {b}go\r\n{c}RESULT\r\n{d}"));
    assert_eq!(split.marks(), "1 0 prompt\n1 2 command\n2 0 output\n3 0 end 3\n");
    split.feed(b"\x1b7\x1b[1;1H\x1b[2K\x1b8");
    assert_eq!(split.marks(), "2 0 prompt\n2 2 command\n3 0 output\n4 0 end 3\n");
    // What is erased takes its marks to where the erased part begins, in their order: an output
    // on the first row, the screen cleared, and the next prompt is no part of that output.
    let cleared = crate::tests::term(9, 3, &format!("{c}old!{d}\x1b[H\x1b[2J{a}NEXT>{b}"));
    assert_eq!(cleared.marks(), "0 0 output\n0 0 end 3\n0 0 prompt\n0 5 command\n");
    let reset = crate::tests::term(9, 3, &format!("x\r\n{c}old!{d}\x1bc{a}NEXT>"));
    assert_eq!(reset.marks(), "0 0 output\n0 0 end 3\n0 0 prompt\n");
    // A mark is never inside a character: where a wide one, written later, left the row's last
    // cell empty, a mark that waited there is before the character.
    let padded = crate::tests::term(5, 2, &format!("{c}abcd {d}\x1b[1;5H界"));
    assert_eq!((padded.plain().as_str(), padded.marks().as_str()), ("abcd\n界\n", "0 0 output\n0 4 end 3\n"));
    // Erased up to the cursor, an output's end right after the erased part is erased with it.
    let before = crate::tests::term(9, 3, &format!("{c}old!{d}\x1b[1;4H\x1b[1J\x1b[HNEW!"));
    assert_eq!(before.marks(), "0 0 output\n0 0 end 3\n");
    // Erased in its line alone (to the line's end, from its start, all of it, so many cells), the
    // same: the marks of what is gone mark nothing of what is written there next.
    let erased = |how: &str| crate::tests::term(9, 3, &format!("ab{c}cde{d}{how}new")).marks();
    assert_eq!(erased("\r\x1b[2K"), "0 0 output\n0 0 end 3\n");
    assert_eq!(erased("\x1b[1;4H\x1b[K"), "0 2 output\n0 3 end 3\n");
    assert_eq!(erased("\x1b[1;4H\x1b[1K"), "0 0 output\n0 5 end 3\n");
    assert_eq!(erased("\x1b[1;2H\x1b[2X"), "0 1 output\n0 5 end 3\n");
    // Cells put into a row or taken out of it take the marks after them along: a mark stays
    // before its character, and one on a cell taken out is where the part taken out begins.
    assert_eq!(erased("\r\x1b[@"), "0 3 output\n0 6 end 3\n");
    assert_eq!(erased("\x1b[1;2H\x1b[2P"), "0 1 output\n0 3 end 3\n");
    assert_eq!(erased("\x1b[1;2H\x1b[9@"), "0 9 output\n0 9 end 3\n");
    // A mark that waits at a row's end stays on that side of the wrap when only the height
    // changes; and marks on rows pushed off the bottom are after what stays, in their order.
    let mut waits = crate::tests::term(4, 3, &format!("{c}abcd{d}$"));
    waits.resize(4, 4);
    waits.feed(b"\x1b[2;1H\x1b[LNEW");
    assert_eq!((waits.plain().as_str(), waits.marks().as_str()), ("abcd\nNEW\n$\n\n", "0 0 output\n0 4 end 3\n"));
    let pushed = crate::tests::term(8, 3, &format!("x\r\np:{c}KEEP\r\nz{d}\x1b[H\x1b[L"));
    assert_eq!(pushed.marks(), "2 2 output\n2 8 end 3\n");
    // Only a prompt says that what was marked below it is gone, and only the four letters mark.
    let up = crate::tests::term(
        10,
        5,
        &format!("{a}$ {b}one\r\n{c}alpha\r\n{d}{a}$ {b}two\r\n{c}beta\x1b[A{d}\x1b]133;Any\x07"),
    );
    assert!(up.marks().ends_with("3 0 output\n2 4 end 3\n"), "{}", up.marks());
    // The screen is cleared: what was marked on it is there no more. Its marks stay, in their
    // order, at the line that took their place, so none is paired with another command's.
    let top = term.top();
    term.feed(format!("{c}out\r\n{d}{a}$ {b}clear\r\n{c}\x1b[H\x1b[2J{d}{a}> ").as_bytes());
    let ends: Vec<String> = term.marks().lines().rev().take(4).map(str::to_string).collect();
    assert_eq!(
        ends,
        [format!("{top} 0 prompt"), format!("{top} 0 end 3"), format!("{top} 0 output"), format!("{top} 0 command")]
    );
    // A prompt drawn again in its place is one prompt; the alternate screen has none; a thousand are kept.
    let count = term.marks().lines().count();
    term.feed(format!("\r{a}> {b}\x1b[?1049h{a}\x1b[?1049l").as_bytes());
    assert_eq!(term.marks().lines().count(), count + 1);
    term.feed(format!("{c}x\r\n").repeat(1200).as_bytes());
    assert_eq!(term.marks().lines().count(), 1000);
}

#[test]
fn a_link_is_on_its_cells_through_scrolling_erasing_and_wrapping_and_ends_at_history() {
    let link = |address: &str, text: &str| format!("\x1b]8;id=1;{address}\x1b\\{text}\x1b]8;;\x07");
    let mut term = term(10, 3, &format!("a{}b\r\n{}", link("http://x.example/", "link"), link("file:///y", "漢字")));
    assert_eq!(term.links(), "0 1 4 http://x.example/\n1 0 4 file:///y\n");
    // The screens stay text and colours: a link is in no screen file.
    assert!(!term.styled().contains("\x1b]") && term.plain() == "alinkb\n漢字\n\n");
    // Written over or erased, a cell is no longer part of the link; an address that is not
    // plain printable text is no link.
    term.feed(format!("\x1b[1;3HX\x1b[2;3H\x1b[K\x1b[3;1H{}", link("http://bad example/", "no")).as_bytes());
    // (Nor is one with a control byte in it, which a string loses on its way: what is left of
    // the address is another address.)
    term.feed(format!("{}{}", link("http://a\x01b/", "no"), link("http://d\x7fx/", "no")).as_bytes());
    assert_eq!(term.links(), "0 1 1 http://x.example/\n0 3 2 http://x.example/\n1 0 2 file:///y\n");
    // A link that wraps is one run of cells in reading order, and still one after the text is
    // wrapped anew; its line's number is a mark's kind of number.
    let mut long = crate::tests::term(6, 4, &format!("x\r\nab{}", link("http://w/", "0123456789")));
    assert_eq!(long.links(), "1 2 10 http://w/\n");
    long.resize(4, 6);
    assert!(long.links() == "1 2 10 http://w/\n", "{}", long.links());
    long.resize(20, 4);
    assert!(long.links() == "1 2 10 http://w/\n", "{}", long.links());
    // What scrolls into history takes its links' cells with it: history is text.
    long.feed(b"\r\n1\r\n2\r\n3\r\n4");
    assert_eq!((long.links().as_str(), long.top()), ("", 2));
    // The table of addresses is bounded: those no cell uses any more make room.
    let mut many = Term::new(10, 2);
    for n in 0..1500 {
        many.feed(format!("\r{}", link(&format!("http://h/{n}"), "x")).as_bytes());
    }
    assert_eq!((many.links().as_str(), many.peek().refused.len()), ("0 0 1 http://h/1499\n", 0));
    // A cell that is off the screen for a while keeps its address: a row a resize set aside
    // comes back with the link it had, however many links were made meanwhile.
    let mut aside = crate::tests::term(10, 3, &format!("{}\r\n\r\nb", link("http://A/", "a")));
    aside.resize(10, 1);
    for n in 0..1200 {
        aside.feed(format!("\r{}", link(&format!("http://h/{n}"), "x")).as_bytes());
    }
    aside.resize(10, 3);
    assert!(aside.links().starts_with("0 0 1 http://A/\n"), "{}", aside.links());
    // A blank that is part of a link is text to wrapping anew; a soft reset ends the link that
    // was being printed.
    let mut blank = crate::tests::term(4, 2, &format!("{}\r", link("http://s/", "X ")));
    blank.resize(5, 2);
    assert!(blank.links() == "0 0 2 http://s/\n", "{}", blank.links());
    blank.feed(b"\x1b]8;;http://open/\x1b\\\x1b[!pZ");
    assert_eq!(blank.links(), "0 1 1 http://s/\n");
    // The list is no longer than a reader reads: it ends before the run that would make it so.
    let wide = "http://".to_string() + &"w".repeat(2041);
    let mut full = Term::new(2, 200);
    full.feed((0..199).map(|n| format!("\x1b[{};1H{}", n + 1, link(&wide, "x"))).collect::<String>().as_bytes());
    // (And there it ends: a shorter run after that one is not in it either.)
    full.feed(format!("\x1b[200;1H{}", link("http://s/", "x")).as_bytes());
    assert_eq!((full.links().len() <= crate::LINKS_LIST_MAX, full.links().lines().count()), (true, 127));
    assert!(!full.links().contains("http://s/"));
}

#[test]
fn queries_are_answered_with_nobody_watching() {
    let term = term(80, 24, "\x1b[c\x1b[>c\x1b[5n\x1b[3;7H\x1b[6n\x1b[?2004$p\x1b[18t");
    assert_eq!(
        String::from_utf8_lossy(&term.peek().reply),
        "\x1b[?62;4;22c\x1b[>70;0;0c\x1b[0n\x1b[3;7R\x1b[?2004;2$y\x1b[8;24;80t"
    );
}

#[test]
fn what_fux_is_and_can_do_is_answered_under_its_own_name_and_no_more() {
    let said = |text: &str| String::from_utf8_lossy(&term(20, 5, text).peek().reply).into_owned();
    let hex = |text: &str| text.bytes().map(|b| format!("{b:02X}")).collect::<String>();
    // Its name and version (XTVERSION); a second device attributes answer that is no other
    // terminal's; neither with parameters that ask something else.
    let mut named = Term::new(20, 5);
    named.told.name = "fux-next 1.2".into();
    named.feed(b"\x1b[>q\x1bc\x1b[>0q\x1b[>1q");
    assert_eq!(named.peek().reply, b"\x1bP>|fux-next 1.2\x1b\\\x1bP>|fux-next 1.2\x1b\\");
    assert_eq!(said("\x1b[>c\x1b[>0c\x1b[>1c"), "\x1b[>70;0;0c\x1b[>70;0;0c");
    // A capability by its terminfo name: each name is answered by itself, so one that is not
    // known cannot hide one that is; true colour and the curly underline are there.
    let asked = format!("\x1bP+q{};{};{}\x1b\\", hex("Tc"), hex("smglr"), hex("Smulx"));
    let answer = format!(
        "\x1bP1+r{}\x1b\\\x1bP0+r{}\x1b\\\x1bP1+r{}={}\x1b\\",
        hex("Tc"),
        hex("smglr"),
        hex("Smulx"),
        hex("\x1b[4:%p1%dm")
    );
    assert_eq!(said(&asked), answer);
    assert_eq!(said("\x1bP+qzz\x1b\\"), "\x1bP0+r\x1b\\");
    // What the emulator keeps is told (DECRQSS): the colours and the underline in force, the
    // scroll region, the cursor's shape; anything else is "not valid".
    assert_eq!(said("\x1b[4:3;38;2;1;2;3m\x1bP$qm\x1b\\"), "\x1bP1$r0;4:3;38:2::1:2:3m\x1b\\");
    // (The looks in xterm's order, which is the screen file's: a style is written one way.)
    assert_eq!(said("\x1b[2;3;4;7m\x1bP$qm\x1b\\"), "\x1bP1$r0;4;7;2;3m\x1b\\");
    assert!(term(20, 5, "\x1b[2;3;4;7mx").styled().starts_with("\x1b[0;4;7;2;3mx"));
    assert_eq!(
        said("\x1b[2;4r\x1b[5 q\x1bP$qr\x1b\\\x1bP$q q\x1b\\\x1bP$qs\x1b\\"),
        "\x1bP1$r2;4r\x1b\\\x1bP1$r5 q\x1b\\\x1bP0$r\x1b\\"
    );
    // However many names are asked for, each is answered by itself (the last here is no name),
    // until 16 KiB of answers wait, as for every answer: the rest are dropped, and that is said.
    let each = format!("{}\x1bP0+r\x1b\\", "\x1bP0+r41\x1b\\".repeat(40));
    assert_eq!(said(&format!("\x1bP+q{}\x1b\\", "41;".repeat(40))), each);
    let out = term(20, 5, &format!("\x1bP+q{}\x1b\\", "41;".repeat(3000))).take(0);
    assert!(out.reply.len() > 16000 && out.reply.len() <= 16 << 10 && out.refused == [crate::DROPPED]);
    // What DECRQSS says of the looks in force, fed back, gives the same looks; and the table's
    // own sequences are ones the emulator takes (the curly underline, its colour, the shape).
    for looks in ["1;2;3", "4:2;58:5:9", "4:5;58:2::1:2:3;7", "31;42", "95;107;9", "38;5;200;48;2;9;8;7;5;8", "21"] {
        let mut first = term(20, 5, &format!("\x1b[{looks}mx\x1bP$qm\x1b\\"));
        let answer = String::from_utf8_lossy(&first.take(0).reply).into_owned();
        let body = answer.trim_start_matches("\x1bP1$r").trim_end_matches("\x1b\\");
        let again = term(20, 5, &format!("\x1b[{body}x"));
        assert_eq!(again.styled(), first.styled(), "{looks}: {answer:?}");
    }
    let shaped = term(20, 5, "\x1b[4:3m\x1b[58:2::1:2:3mx\x1b[5 q\x1b[0 q");
    assert!(
        shaped.styled().starts_with("\x1b[0;4m\x1b[4:3;58:2::1:2:3mx") && shaped.screen.cursor_shape == 0,
        "{:?}",
        shaped.styled()
    );
    // The README's table is this table, row for row, and the agents' page lists the same names.
    let (readme, page) = (include_str!("../../README.md"), include_str!("../../src/help-agents.txt"));
    let rows: Vec<String> = crate::CAPS
        .iter()
        .map(|(name, value)| match value.replace('\x1b', "\\E").as_str() {
            "" => format!("| `{name}` | yes |"),
            shown => format!("| `{name}` | `{shown}` |"),
        })
        .collect();
    assert!(
        readme.contains(&format!("| Name | Value |\n|---|---|\n{}\n\n", rows.join("\n"))),
        "the README's table of capabilities"
    );
    let names: Vec<&str> = crate::CAPS.iter().map(|(name, _)| *name).collect();
    assert!(
        page.split_whitespace().collect::<Vec<_>>().join(" ").contains(&format!("{}.", names.join(" "))),
        "the agents' page"
    );
}

#[test]
fn a_cursor_waiting_at_the_right_margin_moves_like_any_other() {
    // After a character in the last column the cursor waits there; a back-tab still goes back.
    let term = term(80, 3, "\x1b[1;80Hb\x1b[Z.");
    assert_eq!((term.screen.cursor.row, term.screen.cursor.col), (0, 73));
}

#[test]
fn a_title_and_a_clipboard_leave_as_text_and_bytes_and_a_string_has_one_bound() {
    use crate::{base64_decode, base64_encode};
    // The title loses its control characters; the clipboard is what the base64 says; text goes on after both.
    let mut term = term(40, 2, "\x1b]2;my \u{9b}title\x07\x1b]52;c;aGVsbG8sIHdvcmxk\x1b\\after");
    let out = term.take(0);
    assert_eq!((term.title(), out.clipboard.as_deref()), ("my title", Some(&b"hello, world"[..])));
    assert_eq!(term.plain(), "after\n\n");
    // A title put aside comes back, as a program that borrows the title for a while expects.
    term.feed(b"\x1b[22;2t\x1b]2;borrowed\x07\x1b[23;2t");
    assert_eq!(term.title(), "my title");
    // A program is never told what is on the clipboard, and is refused in words.
    term.feed(b"\x1b]52;c;?\x07\x1b]52;c;not base64!\x07");
    let out = term.take(0);
    assert!(out.reply.is_empty() && out.clipboard.is_none() && out.refused.len() == 2);
    // Neither is padding in the middle base64, nor a command with nothing to set; a pop with nothing put aside changes nothing.
    term.feed(b"\x1b]52;c;QU=JD\x07\x1b]52;c\x07\x1b[23;2t\x1b[23;2t");
    let out = term.take(0);
    assert!(out.clipboard.is_none() && term.title() == "my title");
    // Asked for its history alone (as the holder asks between two rounds of writing), it keeps
    // what the program said once for the round that writes it.
    let mut asked = self::term(4, 1, "\x1b]52;c;aGk=\x07gone\r\n");
    let (early, whole) = (asked.take(1), asked.take(0));
    assert_eq!(
        (early.history.as_str(), early.clipboard, whole.clipboard.as_deref()),
        ("gone\n", None, Some(&b"hi"[..]))
    );
    // A string of any kind that never ends is dropped at its bound, and said; what follows is text again.
    term.feed(b"\x1b]0;");
    term.feed(&vec![b'x'; (4 << 20) + 2]);
    term.feed(b"\x07ok");
    let out = term.take(0);
    assert!(term.title() == "my title" && out.refused.len() == 1 && term.plain().contains("afterok"));
    // A string that never ends is still said to be dropped, when it passes its bound.
    term.feed(b"\x1b]0;");
    term.feed(&vec![b'x'; (4 << 20) + 2]);
    assert_eq!(term.take(0).refused.len(), 1);
    // What is encoded decodes to itself, whatever its length.
    for bytes in [&b""[..], b"a", b"ab", b"abc", &[0, 255, 128, 7]] {
        assert_eq!(base64_decode(&base64_encode(bytes)).as_deref(), Some(bytes));
    }
}

#[test]
fn the_right_margin_is_xterms() {
    // A bare line feed after a full line goes one line down, not two.
    assert_eq!(term(5, 4, "abcde\nZ").plain(), "abcde\n    Z\n\n\n");
    // The wait at the margin is saved and restored with the cursor, and an erase ends it.
    assert_eq!(term(5, 3, "abcde\x1b7\x1b[H\x1b8Z").plain(), "abcde\nZ\n\n");
    assert_eq!(term(5, 3, "abcde\x1b[KZ").plain(), "abcdZ\n\n\n");
    // A repeat runs on through the wrap.
    let repeated = term(5, 4, "X\x1b[11b");
    assert_eq!(
        (repeated.plain().as_str(), repeated.screen.cursor.row, repeated.screen.cursor.col),
        ("XXXXX\nXXXXX\nXX\n\n", 2, 2)
    );
    // It repeats only a character printed just before it: not across a move, a control or itself.
    assert_eq!(term(8, 2, "A\x1b[2;1H\x1b[3b-\x1b[2b\x1b[2b\rB\r\x1b[b").plain(), "A\nB--\n");
    // With wrapping off a wide character that has no room is dropped; a C1 control as UTF-8 is nothing.
    assert_eq!(term(8, 1, "\x1b[?7l0123456一|a\u{9b}2C").plain(), "0123456C\n");
    // A long way to write a C1 control is no C1 control: it shows the mark of bytes that are no character.
    // A sequence that was cancelled leaves REP nothing to repeat, and a character dropped is what it repeats.
    let mut bad = Term::new(20, 1);
    bad.feed(b"a\xe0\x82\x9bb\xed\xa0\x80c\xe4\xb8e\xffA\x1b[9\x18\x1b[b");
    assert_eq!(bad.plain(), "a\u{fffd}b\u{fffd}c\u{fffd}e\u{fffd}A\n");
    assert_eq!(term(8, 1, "\x1b[?7l0123456一\x1b[b").plain(), "0123456\n");
    // A wrap waits even while wrapping is off: turned on again, the next character goes to the next row.
    assert_eq!(term(4, 2, "\x1b[?7labcdef\x1b[?7hg").plain(), "abcf\ng\n");
    // Asking again for the alternate screen of 1047 keeps what is on it; DECALN lets the whole screen scroll.
    assert_eq!(term(8, 2, "\x1b[?1047hX\x1b[?1047hY").plain(), "XY\n\n");
    assert_eq!(term(3, 3, "\x1b[2;3r\x1b[3;2H\x1b#8X\n\n\nY").plain(), "EEE\nEEE\n Y\n");
}

#[test]
fn the_cursor_stops_at_the_scroll_regions_edge_from_either_side() {
    let down = term(10, 6, "\x1b[2;4r\x1b[1;1H\x1b[9BX");
    let up = term(10, 6, "\x1b[2;4r\x1b[6;1H\x1b[9AX");
    assert_eq!((down.plain().as_str(), up.plain().as_str()), ("\n\n\nX\n\n\n", "\nX\n\n\n\n\n"));
}

#[test]
fn what_is_malformed_changes_nothing_and_what_is_cut_leaves_no_half() {
    // A longer spelling of é is not é; a colour with a part missing is no colour.
    let mut term = Term::new(20, 3);
    term.feed(b"A\xe0\x83\xa9B\x1b[31mR\x1b[38;2;255mX");
    assert_eq!(term.styled().lines().next(), Some("A\u{fffd}B\x1b[0;31mRX\x1b[m"));
    // `?` after a parameter is no private mode, and 33 parameters are no command (and the log hears of it).
    term.feed(format!("\x1b[0;?25l\x1b[{}0m", "1;".repeat(32)).as_bytes());
    assert!(term.screen.cursor_shown && !term.peek().refused.is_empty());
    // Deleting across two wide characters leaves blanks, not a character made of two halves.
    term.feed("\x1b[m\x1b[2;1H漢界X\x1b[2;2H\x1b[2P".as_bytes());
    assert_eq!(term.plain().lines().nth(1), Some("  X"));
    // Tab stops a program set are still there after a resize.
    term.feed(b"\x1b[3g\x1b[3;5H\x1bH");
    term.resize(21, 3);
    term.feed(b"\r\tT");
    assert_eq!(term.plain().lines().nth(2), Some("    T"));
}

#[test]
fn the_alternate_screen_gives_the_main_one_back_and_keeps_no_history() {
    let mut term = term(20, 4, "one\r\ntwo\x1b[?1049h\x1b[2Jfull\r\n\n\n\n\n\nscreen");
    assert!(term.screen.alt && term.peek().history.is_empty());
    term.feed(b"\x1b[?1049l!");
    assert_eq!(term.plain(), "one\ntwo!\n\n\n");
}

/// Numbers that are the same on every run: a test over random sizes fails the same way twice.
fn numbers(seed: u64) -> impl FnMut(usize) -> usize {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    move |below| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 33) as usize % below.max(1)
    }
}

/// Lines of words, some longer than any screen here, with wide, joined and marked characters.
fn some_text(next: &mut impl FnMut(usize) -> usize) -> String {
    let words =
        ["a", "to", "the", "wrapped", "一二三", "e\u{301}", "\u{1f468}\u{200d}\u{1f469}", "\u{1f1ef}\u{1f1f5}", "x"];
    let words =
        words.into_iter().chain(["\x1b[1;31mred\x1b[m", "\x1b[4:3;58:5:9mthe\x1b[44m \x1b[m", "\x1b[7m一\x1b[27m"]);
    let words: Vec<&str> = words.collect();
    let line = |next: &mut dyn FnMut(usize) -> usize| {
        (0..next(40)).map(|_| words[next(words.len())]).collect::<Vec<_>>().join(" ")
    };
    (0..2 + next(12)).map(|_| line(next)).collect::<Vec<_>>().join("\r\n")
}

/// Styled text without its looks: no SGR codes, and none of the U+200B that keep characters apart.
fn bare(styled: &str) -> String {
    let parts = styled.split("\x1b[").enumerate();
    let parts = parts.map(|(i, part)| if i == 0 { part } else { part.split_once('m').map_or("", |(_, text)| text) });
    parts.collect::<String>().replace('\u{200b}', "")
}

/// Whether styled history has history's lines (blanks that have a colour are kept at a line's end).
fn same_lines(term: &Term) -> bool {
    let ends = |text: &str| text.split('\n').map(|line| line.trim_end_matches(' ')).collect::<Vec<_>>().join("\n");
    let out = term.peek();
    !out.styled.contains("\x1b]") && ends(&bare(&out.styled)) == ends(&out.history)
}

/// Text as its lines, without the blanks at a line's end (where a cursor stood, blanks may be
/// kept) and without the empty lines at its end.
fn lines_of(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(|line| line.trim_end_matches(' ')).collect();
    lines.join("\n").trim_end_matches('\n').to_string()
}

#[test]
fn the_command_being_typed_is_not_wrapped_anew_where_the_shell_marks_its_prompt() {
    // A shell that marks its prompt (OSC 133) draws its command line again after a resize,
    // counting rows up from the cursor as it left them. So while a command is being typed
    // (the last marks are a prompt and where its command begins, and the cursor is there), that
    // line's rows are cut or padded, not wrapped anew: the shell's own drawing lands on them.
    // Everything above is wrapped as ever.
    let (a, b, d) = ("\x1b]133;A\x07", "\x1b]133;B\x07", "\x1b]133;D;0\x07");
    let whole = |term: &Term| lines_of(&(term.peek().history.clone() + &term.lines().0));
    let typed = format!("a line of output that is long\r\n{a}p> {b}echo abcdefghijabcdefghij");
    let mut marked = term(20, 6, &typed);
    assert_eq!(marked.plain(), "a line of output tha\nt is long\np> echo abcdefghijab\ncdefghij\n\n\n");
    marked.resize(12, 6);
    // (The output above is whole, in more rows; the command's two rows are two rows, cut, and
    // one line still.)
    assert_eq!(marked.plain(), "a line of ou\ntput that is\n long\np> echo abcd\ncdefghij\n\n");
    assert_eq!((marked.screen.cursor.row, marked.screen.cursor.col, marked.wraps().as_str()), (4, 8, "1 2 4"));
    // What the shell then draws (bash: to the line's start, erase, a row up, erase, the line
    // again) is one command line, once.
    let redraw = |rows: usize| format!("\r\x1b[K{}\r{a}p> {b}echo abcdefghijabcdefghij", "\r\x1b[A\x1b[K".repeat(rows));
    marked.feed(redraw(1).as_bytes());
    assert_eq!(marked.plain(), "a line of ou\ntput that is\n long\np> echo abcd\nefghijabcdef\nghij\n");
    // Made narrower twice before the shell has drawn: its rows are the same two rows still.
    let mut twice = term(20, 6, &typed);
    twice.resize(14, 6);
    twice.resize(9, 6);
    twice.feed(redraw(1).as_bytes());
    assert_eq!(twice.plain().matches("p> echo").count(), 1, "{:?}", twice.plain());
    // The height alone changes nothing of it.
    let mut taller = term(20, 6, &typed);
    let before = whole(&taller);
    taller.resize(20, 7);
    assert!(taller.wraps() == "1 3" && whole(&taller) == before);
    // Nothing that may be finished output is ever cut: every case below is wrapped anew, whole.
    let zeros = "0".repeat(50);
    for other in [
        // no marks; the command runs (its output has begun); the prompt's mark alone;
        typed.replace(a, "").replace(b, ""),
        format!("{typed}\x1b]133;C\x07"),
        format!("{a}p> go\r\n{zeros}"),
        // output on the line after the command's (a shell that sends no mark for it);
        format!("{a}p> {b}go\r\n{zeros}"),
        // output that did not end its line, with the prompt after it on that line;
        format!("abcdefghijklmnopqrstuvwxyz0123{d}{a}p> {b}"),
        format!("{a}>{b}run\r\nABCDEFGHIJKLMNOPQRST{d}{a}>{b}x"),
        // the cursor somewhere else; marks whose text was cleared away.
        format!("RESULT-OF-SOMETHING-LONG\r\n{a}>{b}x\x1b[1;1H"),
        format!("old\r\nxx{a}p{b}\x1b[H\x1b[2JRESULT-OF-SOMETHING-LONG"),
    ] {
        let mut term = term(20, 6, &other);
        let before = whole(&term);
        term.resize(7, 6);
        assert_eq!(whole(&term), before, "{other:?}");
    }
}

#[test]
fn the_main_screen_under_the_alternate_screen_is_told_as_it_will_be_shown_again() {
    // Colours, a colour of the pane's own and a wrapped line: covered, the main screen is what
    // the screens said of it before; uncovered, nothing is told of it apart.
    let mut term = term(8, 4, "\x1b]10;#102030\x07\x1b[31mred\x1b[m\r\na line that wraps");
    let before = (term.plain(), term.styled(), term.wraps());
    assert_eq!((term.covered(), before.2.as_str()), (None, "2 3"));
    term.feed(b"\x1b[?1049h\x1b[Hfull\r\nscreen");
    assert!(term.plain().starts_with("full\nscreen\n") && term.covered() == Some(before.clone()));
    // (Which rows go on is told of both: the program's screen has none.)
    assert_eq!(term.wraps(), "");
    // A resize wraps it anew under the program, and it is told anew: as the screens then have
    // it when the program leaves.
    term.resize(12, 4);
    let wrapped = term.covered().expect("it is covered still");
    assert!(wrapped.2 == "2" && wrapped.0.starts_with("red\na line that \nwraps\n"), "{wrapped:?}");
    term.feed(b"\x1b[?1049l");
    assert_eq!((term.covered(), (term.plain(), term.styled(), term.wraps())), (None, wrapped));
}

#[test]
fn a_resize_wraps_the_main_screen_anew_and_loses_nothing() {
    // The case that lost 40 characters of 123: a line of 120, narrowed to 40 columns and widened back.
    let long: String = (0..120).map(|n| char::from(b'a' + n % 26)).collect();
    let mut wide = term(120, 10, &format!("{long}\r\nend"));
    wide.resize(40, 10);
    assert!(wide.plain().starts_with(&format!("{}\n{}\n{}\nend\n", &long[..40], &long[40..80], &long[80..])));
    wide.resize(120, 10);
    assert_eq!(
        (wide.plain(), wide.screen.cursor.row, wide.screen.cursor.col),
        (format!("{long}\nend\n\n\n\n\n\n\n\n\n"), 1, 3)
    );
    // A row erased from its start, or moved away, is no longer what the row above goes on in:
    // widened, the two do not become one line. And one column is a size like any other.
    let mut parted = term(4, 3, "abcdef\x1b[2;1H\x1b[Kxy");
    parted.resize(8, 3);
    assert!(parted.plain() == "abcd\nxy\n\n", "{:?}", parted.plain());
    let mut emptied = term(4, 4, "abcdefghij\x1b[2;1H\x1b[2K");
    emptied.resize(8, 4);
    assert!(emptied.plain() == "abcd\n\nij\n\n", "{:?}", emptied.plain());
    let mut moved = term(4, 3, "abcdef\x1b[2;1H\x1b[Lxy");
    moved.resize(8, 3);
    assert!(moved.plain() == "abcd\nxy\nef\n", "{:?}", moved.plain());
    let mut thin = term(6, 4, "a\u{4e00}b\u{4e8c}c");
    thin.resize(1, 4);
    thin.resize(6, 4);
    assert!(thin.lines().2);
    assert_eq!(thin.peek().history.clone() + &thin.lines().0.replace([' ', '\n'], ""), "abc");
    // A line whose first rows are in history, and whose rest is then cleared away, ends there:
    // what is printed next is a line of its own.
    let mut cleared = term(10, 2, "x\r\naaaaaaaaaabbbbb\r\n\x1b[H\x1b[2J$ ls\r\nfile\r\n");
    assert_eq!(cleared.take(0).history, "x\naaaaaaaaaa\n$ ls\n");
    // A line editor draws a long line again row by row: written over, it is one line still.
    let mut drawn = term(5, 2, "abcdefg\x1b[1;5HX\x1b[2;1Hfg");
    drawn.resize(10, 2);
    assert!(drawn.plain() == "abcdXfg\n\n", "{:?}", drawn.plain());
    // A saved cursor that waits at a row's end stays there; one in the blank part of a row is
    // kept inside the row and does not wait; the cursor behind the alternate screen comes back
    // to its column; a mark that comes after a resize still finds its letter.
    let mut waits = term(4, 5, "ABCD\x1b7EF");
    waits.resize(2, 5);
    waits.feed(b"\x1b8");
    assert_eq!((waits.screen.cursor.row, waits.screen.cursor.col, waits.plain().as_str()), (1, 1, "AB\nCD\nEF\n\n\n"));
    let mut beside = term(20, 3, "ab\x1b[1;15H\x1b7\x1b[3;1H");
    beside.resize(10, 3);
    beside.feed(b"\x1b8X");
    assert_eq!(beside.plain(), "ab       X\n\n\n");
    let mut hidden = term(8, 5, "A\x1b[1;8H\x1b[?1049h");
    hidden.resize(3, 5);
    hidden.resize(8, 5);
    hidden.feed(b"\x1b[?1049l");
    assert_eq!((hidden.screen.cursor.row, hidden.screen.cursor.col), (0, 7));
    let mut later = term(8, 3, "e");
    later.resize(6, 3);
    later.feed("\u{301}".as_bytes());
    assert_eq!(later.plain(), "e\u{301}\n\n\n");
    // (Only while the cursor is where the letter left it, as when nothing was resized.)
    let mut away = term(8, 3, "e\x1b[2;1H");
    away.resize(6, 3);
    away.feed("\u{301}".as_bytes());
    assert_eq!(away.plain(), "e\n\n\n");
    // A row that ends in coloured blanks stays one row, coloured to its new end; a blank that
    // was given a colour of its own is in `screen.styled`.
    let mut bar = term(8, 3, "\x1b[44m\x1b[K\x1b[m\r\nx");
    bar.resize(4, 3);
    assert!(bar.plain() == "\nx\n\n" && bar.peek().history.is_empty());
    assert_eq!(bar.styled().lines().next(), Some("\x1b[0;44m    \x1b[m"));
    assert!(term(4, 1, "\x1b[31m \x1b[m").styled().starts_with("\x1b[0;31m "));
    let mut painted = term(4, 2, "\x1b[41m \x1b[m\r\n");
    painted.resize(5, 2);
    assert_eq!(painted.styled(), "\x1b[0;41m \x1b[m\n\n");
    for seed in 0..300 {
        let next = &mut numbers(seed);
        let text = some_text(next);
        let mut term = term(2 + next(60), 1 + next(30), &text);
        let printed = bare(&text.replace("\r\n", "\n"));
        let everything = |term: &Term| lines_of(&(term.peek().history.clone() + &term.lines().0));
        assert_eq!(everything(&term), lines_of(&printed), "seed {seed}: as printed");
        assert!(same_lines(&term), "seed {seed}: styled history as printed");
        // A saved cursor is put somewhere on the screen, and then the cursor, on text or beside it
        // (not in the last column, where a cell may be the gap a wide character left: no place in the text).
        let place = |term: &Term| term.peek().history.clone() + &term.lines().1;
        term.feed(format!("\x1b[{};{}H\x1b7", 1 + next(term.screen.rows), 1 + next(term.screen.cols - 1)).as_bytes());
        let saved_before = place(&term);
        term.feed(format!("\x1b[{};{}H", 1 + next(term.screen.rows), 1 + next(term.screen.cols - 1)).as_bytes());
        let (was, at, history) =
            (term.plain(), (term.screen.cursor.row, term.screen.cursor.col), term.peek().history.len());
        let (before, size) = (place(&term), (term.screen.cols, term.screen.rows));
        let (mut there, mut quiet) = (true, true);
        for _ in 0..1 + next(4) {
            // (1) Every character printed is in the screen or in history, once and in order; (3) the cursor
            // stays on its character; (4) no character is cut in half.
            let kept = term.peek().history.len();
            term.resize(2 + next(60), 1 + next(30));
            assert_eq!(
                everything(&term),
                lines_of(&printed),
                "seed {seed}: at {}x{}",
                term.screen.cols,
                term.screen.rows
            );
            assert!(term.lines().2, "seed {seed}: a character in half at {}x{}", term.screen.cols, term.screen.rows);
            // (7) Styled history has history's lines, whatever was sent there and taken back.
            assert!(same_lines(&term), "seed {seed}: styled history at {}x{}", term.screen.cols, term.screen.rows);
            // (Unless its own row had to go to history, to make room for text below it: then
            // it is at the top, and on another character from then on.)
            there &= term.screen.cursor.row > 0 || term.peek().history.len() == kept;
            quiet &= term.peek().history.len() == kept;
            if there {
                assert_eq!(
                    place(&term),
                    before,
                    "seed {seed}: the cursor at {}x{}",
                    term.screen.cols,
                    term.screen.rows
                );
            }
        }
        // (2) Back at the size it had, with nothing printed between: as it was, the rows that
        // went to history on the way taken back from it (if the cursor's own row did not go).
        term.resize(size.0, size.1);
        if there {
            assert_eq!(term.peek().history.len(), history, "seed {seed}: history, back at {size:?}");
            assert!(same_lines(&term), "seed {seed}: styled history, back at {size:?}");
            assert_eq!(
                (term.plain(), (term.screen.cursor.row, term.screen.cursor.col)),
                (was, at),
                "seed {seed}: back at {size:?}"
            );
            // The saved cursor too, if its row never left the screen and it was on a character or
            // right after one (in the blank part of a row or on an empty row it is only kept
            // inside the screen).
            term.feed(b"\x1b8");
            let kept = !quiet || place(&term) == saved_before || saved_before.ends_with([' ', '\n']);
            assert!(kept, "seed {seed}: the saved cursor");
        }
    }
}

#[test]
fn a_resize_cuts_the_alternate_screen_and_a_size_no_terminal_has_is_the_nearest_it_may_have() {
    for seed in 0..100 {
        let next = &mut numbers(seed);
        let text = some_text(next);
        let mut term = term(2 + next(60), 1 + next(30), &text);
        let everything = |term: &Term| lines_of(&(term.peek().history.clone() + &term.lines().0));
        let main = everything(&term);
        term.feed(format!("\x1b[?1049h\x1b[H{}", some_text(next).replace('a', "ALT")).as_bytes());
        for _ in 0..1 + next(4) {
            // (4) No character is cut in half on either screen, and the alternate one's rows stay rows.
            term.resize(2 + next(60), 1 + next(30));
            assert!(term.lines().2, "seed {seed}");
        }
        // (5) Leaving the alternate screen gives the main one back at the new size, with all its text;
        // nothing of the alternate screen went to history.
        term.feed(b"\x1b[?1049l");
        assert_eq!(everything(&term), main, "seed {seed}: at {}x{}", term.screen.cols, term.screen.rows);
        assert!(!term.peek().history.contains("ALT") && term.plain().lines().count() == term.screen.rows);
        // (6) A size no terminal may have is the nearest one it may, as for a new terminal: none is refused.
        for (cols, rows) in [(0, 5), (5, 0), (501, 5), (5, 501)] {
            term.resize(cols, rows);
            assert_eq!((term.screen.cols, term.screen.rows), (cols.clamp(1, 500), rows.clamp(1, 500)));
        }
        assert!(term.peek().refused.is_empty() && term.lines().2, "seed {seed}");
    }
}

#[test]
fn a_picture_is_placed_in_cells_and_leaves_when_it_scrolls_off_or_is_written_over() {
    // 30 by 12 pixels with 10 by 20 cells: 3 columns, 1 row.
    let mut term = term(20, 4, "ab\x1bPq\"1;1;30;12#0!30~-!30~\x1b\\");
    let picture = term.screen.pictures.first().expect("a picture");
    assert_eq!((picture.row, picture.col, picture.cols, picture.rows), (0, 2, 3, 1));
    assert!(picture.bytes.as_ref().is_some_and(|d| d.starts_with(b"\x1bPq") && d.ends_with(b"\x1b\\")));
    assert_eq!((term.screen.cursor.row, term.screen.cursor.col), (1, 0));
    // It stays through a full-screen program, an erase beside it and a resize that fits it.
    term.feed(b"\x1b[?1049h\x1b[2Jless\x1b[?1049l\x1b[1;10H\x1b[K\x1b[2;1H");
    term.resize(10, 3);
    assert_eq!(term.screen.pictures.len(), 1);
    term.feed(b"\x1b[1;4Hx");
    assert!(term.screen.pictures.is_empty());
    term.feed(b"\x1bPq~~\x1b\\\n\n\n\n\n");
    assert!(term.screen.pictures.is_empty() && term.peek().refused.is_empty());
    // Its size is the larger of what it says and what it paints: here 40 pixels, 4 cells, not 1.
    term.feed(b"\x1b[H\x1bPq\"1;1;10;6!40~\x1b\\");
    assert_eq!(term.screen.pictures.first().map(|p| (p.cols, p.rows)), Some((4, 1)));
    // And as high as its lowest painted pixel: 20 rows of pixels in four bands are one row of cells.
    term.feed(b"\x1b[H\x1bPq~-~-~-A?\x1b\\");
    assert_eq!(term.screen.pictures.last().map(|p| (p.cols, p.rows)), Some((1, 1)));
    // It leaves the top row by row and is there while one row of it is: of 5 rows on a screen
    // of 4 the last 3 stay, with the cursor under them.
    let mut term = self::term(20, 4, "\x1bPq\"1;1;30;100~\x1b\\");
    let placed = |term: &Term| term.screen.pictures.first().map(|p| (p.row, p.cols, p.rows, p.off));
    assert_eq!((placed(&term), term.screen.cursor.row), (Some((0, 3, 3, 2)), 3));
    term.feed(b"\n\n");
    assert_eq!(placed(&term), Some((0, 3, 1, 4)));
    term.feed(b"\n");
    assert_eq!(placed(&term), None);
    // A screen too narrow for it keeps it; rows a resize takes from it come back with the resize
    // that gives them back, and rows that scrolled away do not.
    term.feed(b"\x1b[H\x1bPq\"1;1;30;60~\x1b\\");
    term.resize(2, 4);
    term.resize(20, 2);
    assert_eq!(placed(&term), Some((0, 3, 1, 2)));
    term.resize(20, 4);
    assert_eq!(placed(&term), Some((0, 3, 3, 0)));
    term.feed(b"\n");
    term.resize(20, 3);
    term.resize(20, 4);
    assert_eq!(placed(&term), Some((0, 3, 2, 1)));
    // Rows of it below a screen made lower are its own still when the screen is made higher; and
    // one beside no text keeps its column on a screen too narrow to show it, and is there again
    // on a wider one.
    let mut term = self::term(20, 6, "\x1b[2;1H\x1bPq\"1;1;10;60~\x1b\\\x1b[H");
    term.resize(20, 2);
    assert_eq!(placed(&term), Some((1, 1, 3, 0)));
    term.resize(20, 6);
    assert_eq!(placed(&term), Some((1, 1, 3, 0)));
    let mut term = self::term(20, 4, "\x1b[1;8H\x1bPq\"1;1;20;1!20@\x1b\\");
    for cols in [5, 20] {
        term.resize(cols, 4);
        assert_eq!(term.screen.pictures.first().map(|p| (p.row, p.col, p.cols)), Some((0, 7, 2)), "{cols} columns");
    }
    // A screen filled with E's (ESC # 8) is written over whole, as by any other fill: the picture goes.
    term.feed(b"\x1b#8");
    assert!(term.plain().starts_with("EEEEE") && term.screen.pictures.is_empty(), "{:?}", term.screen.pictures);
    // Its bytes go when it goes: placed and erased a hundred times with nothing taken between, none waits.
    let mut gone = self::term(4, 3, &"\x1b[H\x1bPq~\x1b\\\x1b[2J".repeat(100));
    assert_eq!(gone.new_pictures().len(), 0);
}

fn drawn(w: usize, h: usize, colours: &[&str], rule: impl Fn(usize, usize) -> Option<usize>) -> String {
    let mut out = format!("q\"1;1;{w};{h}");
    for band in 0..h.div_ceil(6) {
        for (colour, defined) in colours.iter().enumerate() {
            out.push_str(&format!("#{colour}{defined}"));
            let painted = |x: usize, k: usize| band * 6 + k < h && rule(x, band * 6 + k) == Some(colour);
            let sixel = |x: usize| (0..6).fold(0, |bits, k| bits | u8::from(painted(x, k)) << k);
            let sixels: Vec<u8> = (0..w).map(|x| b'?' + sixel(x)).collect();
            for run in sixels.chunk_by(|a, b| a == b) {
                out.push_str(&format!("!{}{}", run.len(), char::from(run[0])));
            }
            out.push('$');
        }
        out.push('-');
    }
    out
}

/// The pixels of a part of a picture, each with its colour.
fn pixels(part: &crate::sixel::Part) -> Vec<Option<crate::sixel::Colour>> {
    let pen = |x: usize, y: usize| usize::from(part.pixels[y * part.stride + x]).checked_sub(1);
    (0..part.h).flat_map(|y| (0..part.w).map(move |x| pen(x, y).map(|pen| part.colours[pen]))).collect()
}

#[test]
fn a_part_cut_out_of_a_picture_has_exactly_its_pixels_and_too_much_is_refused() {
    use crate::sixel::{Colour, cut, read};
    let all = [0, 0, 1000, 1000];
    // Three pictures from rules: stripes of three colours with holes, 47 by 30 pixels (five
    // bands, odd sizes); colours said as hue, lightness and saturation; and one with a register
    // it never gave a colour. Each part read back is the rule's pixels of that rectangle.
    let rgb = [";2;100;0;0", ";2;0;100;0", ";2;0;0;100"];
    let hls = [";1;120;50;100", ";1;240;50;100", ";1;0;50;100", ";1;77;100;0", ";1;300;25;50"];
    let seen: Vec<Colour> =
        [[100, 0, 0], [0, 100, 0], [0, 0, 100], [100, 100, 100], [13, 38, 38]].map(Colour::Rgb).into();
    let own = [seen[0], seen[1], Colour::Register(2)];
    let stripes = |x: usize, y: usize| Some((x / 5 + y / 7) % 4).filter(|c| *c < 3);
    let squares = |x: usize, y: usize| Some((x / 3 + 2 * (y / 4)) % 5);
    type Picture<'a> = (usize, usize, &'a [&'a str], &'a [Colour], &'a dyn Fn(usize, usize) -> Option<usize>);
    let pictures: [Picture; 4] = [
        (47, 30, &rgb, &seen, &stripes),
        (31, 17, &hls, &seen, &squares),
        (20, 13, &[rgb[0], rgb[1], ""], &own, &stripes),
        (1, 1, &rgb, &seen, &|_, _| Some(1)),
    ];
    for (w, h, colours, seen, rule) in pictures {
        let picture = drawn(w, h, colours, rule);
        for [left, top, width, height] in [all, [0, 0, 23, 18], [0, 12, 47, 18], [5, 7, 20, 13], [46, 29, 9, 9]] {
            let (right, bottom) = (w.min(left + width), h.min(top + height));
            let part = cut(picture.as_bytes(), [left, top, width, height], 1 << 20).expect("a part");
            if left >= w || top >= h {
                assert_eq!(part, "", "{w} by {h}: nothing of it is at {left} {top}");
                continue;
            }
            let part = read(part.as_bytes(), all).expect("the part is a picture");
            assert_eq!((part.w, part.h), (right - left, bottom - top), "{w} by {h} at {left} {top}");
            let rule = |y: usize| (left..right).map(move |x| rule(x, y).map(|colour| seen[colour]));
            assert!(pixels(&part) == (top..bottom).flat_map(rule).collect::<Vec<_>>(), "{w} by {h} at {left} {top}");
        }
    }
    // Written by hand: a row painted over, a colour changed after it was used, a repeat past the
    // part's edge, and nothing said of the size.
    let by_hand = "0;0q#0;2;0;0;0!12~$#1;2;100;100;0!6?!6~-#0;2;100;0;0!3~#0;2;0;0;100!999999999999~";
    let part =
        read(cut(by_hand.as_bytes(), [4, 0, 4, 12], 1 << 20).expect("a part").as_bytes(), all).expect("a picture");
    let (black, yellow, blue) = (Colour::Rgb([0, 0, 0]), Colour::Rgb([100, 100, 0]), Colour::Rgb([0, 0, 100]));
    let rows = [[black, black, yellow, yellow]; 6].into_iter().chain([[blue; 4]; 6]);
    assert!(pixels(&part) == rows.flatten().map(Some).collect::<Vec<_>>(), "{:?}", pixels(&part));
    // Whatever the bytes, a part read back is what was read of the picture there, and no larger
    // than was allowed.
    let (mut next, mut compared) = (numbers(3), 0);
    for _ in 0..400 {
        let letters = b"q\"#!$-;;0123456789??@AFN^no~~ ";
        let picture: Vec<u8> = (0..next(200)).map(|_| letters[next(letters.len())]).collect();
        let part = [next(8), next(20), 1 + next(40), 1 + next(40)];
        let (whole, cut) = (read(&picture, part), cut(&picture, part, 300));
        let (Ok(whole), Ok(cut)) = (&whole, &cut) else {
            assert!(whole.is_err() || whole.is_ok_and(|whole| whole.write(1 << 20).is_ok_and(|all| all.len() > 300)));
            continue;
        };
        let again = read(cut.as_bytes(), all).expect("the part is a picture");
        let sizes = [(again.w, again.h), (whole.w, whole.h)].map(|(w, h)| (w.min(h * w), h.min(w * h)));
        assert!(cut.len() <= 300 && sizes[0] == sizes[1] && pixels(&again) == pixels(whole));
        compared += usize::from(!cut.is_empty());
    }
    assert!(compared > 100, "{compared}");
    // What a part is charged for is what is painted inside it. The last row of a picture 1200
    // pixels high, 100 of the 600 rows of one with six colours in every band, a row under 81
    // paintings of the rows above it and 300 colours used only above it, and 16 paintings of one
    // band are all drawn; a part of no pixels is no picture, and not refused.
    let tall = format!("q#0;2;100;0;0{}", ["!600~"; 200].join("-"));
    let band: String = (0..6).map(|k| format!("#{k}!600{}$", char::from(b'?' + (1 << k)))).collect();
    let above: String = (0..300).map(|n| format!("#1;2;{};{};0!10~$", n % 101, n / 101)).collect();
    let drawn_all = [
        (tall, [0, 1180, 600, 20], 600 * 20),
        (format!("q{}", [band.as_str(); 100].join("-")), [0, 500, 600, 100], 600 * 100),
        (format!("q{above}{}----#2;2;0;0;100!10~", "!10~$".repeat(81)), [0, 20, 10, 20], 60),
        (format!("q{}", "!10~$".repeat(16)), [0, 0, 10, 6], 60),
    ];
    for (picture, part, painted) in drawn_all {
        let part = read(cut(picture.as_bytes(), part, 1 << 20).expect("a part").as_bytes(), all).expect("a picture");
        assert_eq!(part.pixels.iter().filter(|pen| **pen != 0).count(), painted);
    }
    assert_eq!(cut(b"q~", [0, 0, 0, 6], 100), Ok(String::new()));
    // (A register past the last that paints only outside the part is no part of it either.)
    let under = read(cut(b"q#256~-#1@", [0, 6, 1, 1], 100).expect("a part").as_bytes(), all).expect("a picture");
    assert_eq!(pixels(&under), [Some(Colour::Register(1))]);
    // What is too much is refused in a few words: pixels, colours, a register past the last,
    // painting over and over, and more sixel than a picture may be.
    let colours: String = (0..257).map(|n| format!("#0;2;{};{};0~", n % 101, n / 101)).collect();
    let refused = [
        (String::from("q~"), [0, 0, 4096, 4096], "pixels"),
        (format!("q{colours}"), [0, 0, 300, 6], "256 colours"),
        (String::from("q#256~"), all, "register"),
        (format!("q{}", "!10~$".repeat(100)), [0, 0, 10, 6], "too many times"),
        (drawn(47, 30, &rgb, stripes), [0, 0, 47, 30], "4 MiB"),
    ];
    for (picture, part, why) in refused {
        let max = if why == "4 MiB" { 100 } else { 1 << 20 };
        assert!(cut(picture.as_bytes(), part, max).is_err_and(|said| said.contains(why)), "{why}");
    }
}

#[test]
fn the_words_of_how_input_is_sent_read_back_and_wrapped_rows_and_lines_gone_are_told() {
    let mut term = Term::new(12, 4);
    term.feed("\x1b[?1h\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[4 q\x1b[31m漢x\x1b[m e\u{301}\x1b[2;3H".as_bytes());
    assert_eq!(term.words(), "appcursor paste mousesgr mouse1002");
    let mut back = Term::new(12, 4);
    back.screen.set_words(&term.words());
    assert_eq!(back.words(), term.words());
    // The styled rows, fed row by row, are the same screen.
    for (row, line) in term.styled().lines().enumerate() {
        back.feed(format!("\x1b[{};1H{line}", row + 1).as_bytes());
    }
    assert_eq!((back.plain(), back.styled()), (term.plain(), term.styled()));
    // Which rows go on from the row above is what the plain text cannot say; and how many
    // whole lines have gone into history numbers the line the first row is of.
    let mut long = crate::tests::term(4, 3, "abcdefghij\r\nk");
    assert_eq!((long.plain().as_str(), long.wraps().as_str(), long.tail.lines), ("efgh\nij\nk\n", "0 1", 0));
    long.feed(b"\r\nl\r\nm");
    assert_eq!((long.plain().as_str(), long.wraps().as_str(), long.tail.lines), ("k\nl\nm\n", "", 1));
    // Rows a resize sent to history and took back are counted out and in again.
    let mut back = crate::tests::term(4, 4, "abcdefgh\r\nx\r\ny");
    back.resize(4, 2);
    assert!((back.plain().as_str(), back.tail.lines) == ("x\ny\n", 1));
    back.resize(4, 4);
    assert!((back.wraps().as_str(), back.tail.lines) == ("1", 0));
    // The form of the mouse's reports is told whether or not any report is on.
    let term = crate::tests::term(20, 2, "\x1b[?1002h\x1b[?1002$p\x1b[?1000$p\x1b[?1006$p");
    assert_eq!(String::from_utf8_lossy(&term.peek().reply), "\x1b[?1002;1$y\x1b[?1000;2$y\x1b[?1006;2$y");
    assert_eq!(crate::tests::term(20, 2, "\x1b[?1006h").words(), "mousesgr");
    // A clipboard's padding must be the padding of its length, and a number too large is no keyboard flags.
    use crate::base64_decode;
    assert_eq!(
        (base64_decode("b2xk"), base64_decode("AA=="), base64_decode("AA")),
        (Some(b"old".to_vec()), Some(vec![0]), Some(vec![0]))
    );
    assert_eq!((base64_decode("AA="), base64_decode("AB=="), base64_decode("b2xk=")), (None, None, None));
    let mut flags = Term::new(4, 1);
    flags.told.keyboard = crate::KEYS;
    flags.feed(b"\x1b[=65536u\x1b[?u\x1b[=65535u\x1b[?u");
    assert_eq!(flags.take(0).reply, b"\x1b[?0u\x1b[?31u");
    // A row that goes on in the next keeps the blank at its end, in both screens; when what a
    // line of history went on in is erased, history's line ends there and then, and the screen's
    // first row is the next line, and not listed.
    assert_eq!(crate::tests::term(5, 2, "abc  DEF").styled(), "abc  \nDEF\n");
    let mut spaced = crate::tests::term(3, 2, "ab cd");
    assert_eq!((spaced.plain().as_str(), spaced.wraps().as_str()), ("ab \ncd\n", "1"));
    spaced.feed(b"efg\x1b[1;1H\x1b[2KXYZ");
    assert_eq!((spaced.take(0).history.as_str(), spaced.plain().as_str()), ("ab \n", "XYZ\nfg\n"));
    assert_eq!((spaced.wraps().as_str(), spaced.top()), ("", 1));
}

#[test]
fn a_resize_under_the_alternate_screen_keeps_the_main_screens_lines() {
    // Blank rows are kept down to the saved cursor's row, the one that will come back: the row
    // under the last line with a cursor saved there (1049), no blank row with none saved (1047).
    for (mode, kept) in [(1049, "l1\nl2\nl3\nl4\nl5\n\n"), (1047, "l1\nl2\nl3\nl4\nl5\n")] {
        let mut term = term(10, 8, &format!("l1\r\nl2\r\nl3\r\nl4\r\nl5\r\n\x1b[?{mode}h"));
        term.resize(10, 3);
        term.feed(format!("\x1b[?{mode}l").as_bytes());
        assert_eq!(term.peek().history.clone() + &term.plain(), kept, "{mode}");
    }
    // A row with text below the cursor is kept as well: rows leave by the top, into history.
    let mut below = Term::new(10, 6);
    below.feed(b"TOP\x1b[5;1HBOTTOM\x1b[1;1H\x1b[?1049h");
    below.resize(10, 3);
    below.feed(b"\x1b[?1049l");
    assert!((below.peek().history.clone() + &below.plain()).contains("TOP") && below.plain().contains("BOTTOM"));
}

#[test]
fn text_is_cut_into_characters_exactly_as_unicodes_own_test_file_says() {
    for line in include_str!("../tests/GraphemeBreakTest.txt").lines() {
        let Some((case, _)) = line.split_once('#').filter(|(case, _)| !case.trim().is_empty()) else { continue };
        // "÷ 0020 × 0308 ÷": the mark before a character says whether it joins the one before.
        let mut joiner = crate::width::Joiner::default();
        let mut words = case.split_whitespace();
        while let (Some(mark), Some(code)) = (words.next(), words.next()) {
            let c = char::from_u32(u32::from_str_radix(code, 16).unwrap()).unwrap();
            assert_eq!(joiner.joins(c), mark == "×", "{line}");
        }
    }
}

#[test]
fn a_character_takes_the_columns_the_width_rule_gives_it_and_one_cell() {
    let named = [
        ("a flag", "\u{1f1ef}\u{1f1f5}", 2),
        ("a family", "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}", 2),
        ("a skin tone", "\u{1f44d}\u{1f3fd}", 2),
        ("Hangul from jamo", "\u{1100}\u{1161}\u{11a8}", 2),
        ("a Devanagari conjunct", "\u{915}\u{94d}\u{937}", 2),
        ("a letter with a mark that takes room", "\u{915}\u{93e}", 2),
        ("an emoji asked for as text", "\u{1f600}\u{fe0e}", 2),
        ("a heart asked for as a picture", "\u{2764}\u{fe0f}", 2),
        ("a keycap", "1\u{fe0f}\u{20e3}", 2),
        ("three marks", "e\u{301}\u{302}\u{303}", 1),
        ("an ambiguous one", "\u{a7}", 1),
        ("a joiner after a letter", "x\u{200d}", 1),
    ];
    for (name, text, columns) in named {
        // In `screen`: the text whole, and the next character right after its columns.
        let first = term(12, 2, &format!("a{text}|"));
        assert_eq!((first.plain(), first.screen.cursor.col), (format!("a{text}|\n\n"), columns + 2), "{name}");
        assert_eq!(first.screen.cell_at(0, 1).map(|cell| cell.wide()), Some(columns == 2), "{name}");
        // Through `screen.styled`, read back: the same cells.
        let mut second = Term::new(12, 2);
        second.feed(first.styled().lines().next().unwrap().as_bytes());
        assert_eq!((second.plain(), second.screen.cursor.col), (first.plain(), first.screen.cursor.col), "{name}");
        // At a row's end it wraps whole, a joiner at the very end too.
        let end = term(4, 2, &format!("abc{text}|"));
        let wrapped = if columns == 2 { format!("abc\n{text}|\n") } else { format!("abc{text}\n|\n") };
        assert_eq!(end.plain(), wrapped, "{name}");
    }
    // Characters put side by side stay two when read back; one that is too long loses its end, and says so.
    let apart = term(8, 1, "\u{1f1ef}\x1b[1;2H\u{1f1f5}\x1b[1;5H\u{915}\u{94d}\x1b[1;6H\u{937}");
    let mut back = Term::new(8, 1);
    back.feed(apart.styled().lines().next().unwrap().as_bytes());
    assert_eq!(
        (0..8).map(|x| back.screen.cell_at(0, x)).collect::<Vec<_>>(),
        (0..8).map(|x| apart.screen.cell_at(0, x)).collect::<Vec<_>>()
    );
    let mut long = term(8, 1, &format!("e{}", "\u{301}".repeat(12)));
    assert_eq!(long.plain().chars().count(), 10 + 1);
    assert_eq!(long.take(0).refused.len(), 1);
    // What joins a character that found no room goes with it, and joins nothing else.
    assert_eq!(term(1, 2, "A\u{754c}\u{301}").plain(), "A\n\n");
    assert_eq!(term(4, 2, "\x1b[?7lABC\u{1f468}\u{200d}\u{1f469}").plain(), "ABC\n\n");
    // One that grows wide: in insert mode it puts in a second column; at a row's end it wraps
    // whole and leaves the cell it was in as it found it.
    assert_eq!(term(8, 2, "ABCD\r\x1b[4h\u{2764}\u{fe0f}").plain(), "\u{2764}\u{fe0f}ABCD\n\n");
    assert_eq!(term(4, 2, "abcd\x1b[1;4H\u{2764}\u{fe0f}").plain(), "abcd\n\u{2764}\u{fe0f}\n");
    // Text goes on while the cursor is where the last character left it and that character is
    // still there: a change of style, a title, a sequence that came to nothing or DEL between a
    // letter and its accent, or inside a family, moves nothing (a highlighter colours a letter
    // and not its accent). After a move or an erase a mark has no character.
    for between in ["\x7f", "\u{9b}", "\x1b[1?m", "\x1b[31m", "\x1b]2;title\x07", "\x1b[?25l", "\x07"] {
        assert_eq!(term(4, 1, &format!("a{between}\u{301}")).plain(), "a\u{301}\n", "{between:?}");
        assert_eq!(term(8, 1, &format!("\u{1f468}\u{200d}{between}\u{1f469}|")).screen.cursor.col, 3, "{between:?}");
    }
    assert_eq!(term(4, 1, "a\r\x1b[C\u{301}").plain(), "a\u{301}\n");
    assert_eq!(term(4, 1, "a\x1b[C\u{301}").plain(), "a\n");
    assert_eq!(term(4, 1, "a\x08\x1b[X\x1b[C\u{301}|").plain(), " |\n");
    // A blank joined to the character before it (a mark that goes before its letter) is part of
    // that character at a row's end too: the plain screen and the styled one have the same text.
    let joined = term(8, 1, "a\u{600} ");
    assert_eq!((joined.plain().as_str(), bare(&joined.styled()).as_str()), ("a\u{600} \n", "a\u{600} \n"));
    // A wide mark (a voiced mark of kana) takes no column.
    assert_eq!(term(8, 1, "\u{304b}\u{3099}|").screen.cursor.col, 3);
    // REP's copies are each a character.
    assert_eq!(term(12, 1, "\u{1100}\x1b[3b|").screen.cursor.col, 9);
    // A mark every terminal gives a column has one here (a halfwidth voiced mark; a skin tone alone).
    assert_eq!((term(8, 1, "\u{ff76}\u{ff9e}|").screen.cursor.col, term(8, 1, "\u{1f3fd}|").screen.cursor.col), (3, 3));
    // A cluster ends at its first empty place, for U+FE0F as for the columns added up.
    assert_eq!(crate::width::width('1', &['\0', '\u{fe0f}']), 1);
    // A program that asks is told clusters are always on.
    assert_eq!(term(8, 1, "\x1b[?2027$p").take(0).reply, b"\x1b[?2027;3$y");
}

#[test]
fn a_screen_read_from_nothing_is_one_blank_cell_with_the_cursor_shown() {
    let screen = Screen::read("", 0);
    assert_eq!((screen.rows, screen.cols, screen.cursor_shown), (1, 1, true));
    assert_eq!((screen.cursor.row, screen.cursor.col, screen.cursor_shape), (0, 0, 0));
    assert!(screen.cell_at(0, 0).is_some_and(|cell| cell.ch == ' '));
}

#[test]
fn a_row_that_goes_on_from_the_one_above_still_does_when_characters_are_put_in_or_taken_out_at_its_start() {
    // (Each with the line's text after it; a wide character first in the row is taken out whole.)
    let cases = [
        ("\x1b[P", "aaaaaaaaaabcde", "bcde"),
        ("\x1b[@", "aaaaaaaaaa abcde", " abcde"),
        ("\x1b[2@\x1b[2P", "aaaaaaaaaaabcde", "abcde"),
        ("\x1b[99@", "aaaaaaaaaa", ""),
        ("\x1b[99P", "aaaaaaaaaa", ""),
    ];
    for (edit, line, row) in cases {
        let term = term(10, 3, &format!("aaaaaaaaaaabcde\x1b[2;1H{edit}"));
        assert_eq!((term.wraps().as_str(), term.lines().0.trim_end()), ("1", line), "{edit:?}");
        assert_eq!(term.plain().lines().nth(1), Some(row), "{edit:?}");
    }
    let wide = term(10, 3, "aaaaaaaaaa日bc\x1b[2;1H\x1b[P");
    assert_eq!((wide.wraps().as_str(), wide.plain().as_str(), wide.lines().2), ("1", "aaaaaaaaaa\n bc\n\n", true));
    // Not at its start, nothing of the row's first cell moves; and an erased row goes on no more.
    let erased = term(10, 3, "aaaaaaaaaaabcde\x1b[2;2H\x1b[P\x1b[@\x1b[2K");
    assert_eq!(erased.wraps(), "");
}

#[test]
fn in_insert_mode_a_character_that_wraps_is_put_in_before_what_is_on_the_next_row_however_it_came_to_be_wide() {
    // (An emoji wide from the start, and one that U+FE0F widens in the row's last column.)
    for emoji in ["\u{1f600}", "\u{2764}\u{fe0f}"] {
        let term = term(8, 2, &format!("\x1b[2;1HAB\x1b[1;8H\x1b[4h{emoji}"));
        assert_eq!(term.plain(), format!("       \n{emoji}AB\n"), "{emoji}");
    }
}

#[test]
fn a_row_shortened_keeps_no_sign_of_where_its_end_was() {
    // A wide character with no room leaves the row's last cell as no text. With a character taken
    // out at the row's start (CSI P) that cell is no longer the last: put back at the end (CSI @),
    // it is a blank like any other.
    let term = term(8, 3, "aaaaaaa\u{65e5}\x1b[1;1H\x1b[P\x1b[@");
    assert_eq!(term.lines().0.trim_end(), " aaaaaa \u{65e5}");
}

#[test]
fn a_row_that_goes_on_from_the_one_above_still_does_when_a_character_is_printed_into_its_start_in_insert_mode() {
    // (CSI 4 h: what is printed pushes the rest of the row along, as CSI @ does before a character is written.)
    let narrow = term(10, 3, "aaaaaaaaaaabcde\x1b[2;1H\x1b[4hX");
    assert_eq!((narrow.wraps().as_str(), narrow.lines().0.trim_end()), ("1", "aaaaaaaaaaXabcde"));
    let wide = term(10, 3, "aaaaaaaaaaabcde\x1b[2;1H\x1b[4h日");
    assert_eq!((wide.wraps().as_str(), wide.lines().0.trim_end()), ("1", "aaaaaaaaaa日abcde"));
    // No later cell says so in its place: a character taken out at the row's start brings no second mark back.
    let twice = term(10, 3, "aaaaaaaaaaabcde\x1b[2;1H\x1b[4hXY\x1b[2;2H\x1b[4lZ\x1b[2;1H\x1b[2P");
    assert_eq!((twice.wraps().as_str(), twice.lines().0.trim_end()), ("1", "aaaaaaaaaaabcde"));
    // A shell's mark in that row is on the same line still.
    let marked = term(10, 3, "aaaaaaaaaaabc\x1b]133;B\x07de\x1b[2;1H\x1b[4hX");
    assert!(marked.marks().starts_with("0 "), "{}", marked.marks());
}

#[test]
fn a_mark_scrolled_off_the_bottom_of_the_scroll_region_is_at_the_end_of_its_last_row_as_any_mark_there() {
    // (ESC M on the first row scrolls the rows down: the row of the prompt's mark leaves at the bottom.)
    let pushed = "x\r\ny\r\n\x1b]133;A\x07z\x1b[H\x1bM\x1b[3;1H";
    let marks = |then: &str| term(8, 3, &format!("{pushed}{then}")).marks();
    assert_eq!(marks(""), "2 8 prompt\n");
    // The row erased takes the mark to its start, and one erased from the cursor on to the cursor.
    assert_eq!((marks("\x1b[2K"), marks("\x1b[3G\x1b[K")), ("2 0 prompt\n".into(), "2 2 prompt\n".into()));
    // Characters put in before it leave it at the row's end, and ones taken out take it along.
    assert_eq!(
        (marks("\x1b[@"), marks("\x1b[3@"), marks("\x1b[2P")),
        ("2 8 prompt\n".into(), "2 8 prompt\n".into(), "2 6 prompt\n".into())
    );
}

#[test]
fn the_test_pattern_leaves_the_shells_marks_at_the_screens_start_and_the_rows_a_resize_lent_in_history() {
    // (ESC # 8 writes over the whole screen, as CSI 2 J erases it: what the marks marked is gone. CSI J from
    // the screen's first cell, which many a `clear` sends, and CSI 1 J up to its last erase it whole too.)
    for whole in ["\x1b#8", "\x1b[2J", "\x1b[H\x1b[J", "\x1b[99;99H\x1b[1J"] {
        let mut marked = term(10, 4, "one\r\n\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07out\r\n");
        assert_eq!(marked.marks(), "1 0 prompt\n1 2 command\n2 0 output\n");
        marked.feed(whole.as_bytes());
        assert_eq!(marked.marks(), "0 0 prompt\n0 0 command\n0 0 output\n", "{whole:?}");
        let mut lent = term(4, 4, "a\r\nb\r\nc\r\nd");
        lent.resize(4, 2);
        assert!(lent.lent() && lent.plain() == "c\nd\n");
        lent.feed(whole.as_bytes());
        lent.feed(b"\x1b[HEEEE\r\nEEEE");
        assert!(!lent.lent(), "{whole:?}");
        lent.resize(4, 4);
        assert_eq!((lent.plain().as_str(), lent.tail.lines), ("EEEE\nEEEE\n\n\n", 2), "{whole:?}");
        // On the alternate screen it is that screen's alone: the main one's marks and rows are as they were.
        let mut under = term(4, 4, "a\r\nb\r\n\x1b]133;A\x07c\r\nd");
        under.resize(4, 2);
        let marks = under.marks();
        under.feed(format!("\x1b[?1049h{whole}").as_bytes());
        assert!(under.lent() && under.marks() == marks && marks == "2 0 prompt\n", "{whole:?}");
    }
    // An erase of less than the whole screen leaves the rows a resize lent to come back.
    for part in ["\x1b[2;1H\x1b[J", "\x1b[H\x1b[1J", "\x1b[2;3H\x1b[1J", "\x1b[2K"] {
        let mut lent = term(4, 4, "a\r\nb\r\nc\r\nd");
        lent.resize(4, 2);
        lent.feed(part.as_bytes());
        assert!(lent.lent(), "{part:?}");
    }
}

#[test]
fn erasing_half_a_wide_character_erases_it_whole_for_the_marks_and_the_pictures_too() {
    // (Both cells of the character are blanked: the mark after it and the picture on its first cell
    // are on what was erased, as its cells are.)
    let term = term(8, 3, "\x1b]133;C\x07日\x1b]133;D\x07x\x1b[H\x1bPq~\x1b\\\x1b[1;2H\x1b[X");
    assert_eq!((term.plain().as_str(), term.marks().as_str()), ("  x\n\n\n", "0 0 output\n0 0 end\n"));
    assert!(term.screen().pictures.is_empty());
    // And from the other side: erased up to its first cell, the mark right after its second is at the start.
    let term = self::term(8, 3, "ab日\x1b]133;D\x07x\x1b[1;4H\x1bPq~\x1b\\\x1b[1;3H\x1b[1K");
    assert_eq!((term.plain().as_str(), term.marks().as_str()), ("    x\n\n\n", "0 0 end\n"));
    assert!(term.screen().pictures.is_empty());
}

#[test]
fn text_that_wraps_on_the_last_row_below_the_scroll_region_stays_there_and_joins_no_other_line() {
    // (As in xterm: nothing scrolls and the cursor stays on that row, which is the line it was.)
    let term = term(4, 3, "\x1b[1;2r\x1b[2;1HUP\x1b[3;1HabcdE");
    let cursor = term.screen().cursor;
    assert_eq!((term.plain().as_str(), term.wraps().as_str(), cursor.row, cursor.col), ("\nUP\nEbcd\n", "", 2, 1));
    assert_eq!(term.lines().0, "\nUP\nEbcd\n");
}

#[test]
fn a_sequence_with_an_intermediate_byte_too_many_is_none_of_the_ones_known() {
    let mut term = term(4, 2, "A\x1b##8");
    assert_eq!(term.plain(), "A\n\n");
    term.feed(b"\x1b[?25$$p\x1b[2 !q\x1b[!!p");
    assert!(term.peek().reply.is_empty() && term.screen().cursor_shape == 0);
    // (With one, each is what it was.)
    term.feed(b"\x1b[?25$p\x1b[2 q\x1b#8");
    assert_eq!((term.peek().reply.as_slice(), term.screen().cursor_shape), (&b"\x1b[?25;1$y"[..], 2));
    assert_eq!(term.plain(), "EEEE\nEEEE\n");
}

#[test]
fn a_styled_row_longer_than_the_widest_pane_is_cut_off_as_at_any_width() {
    for cols in [3, 499, 500, 9999, usize::MAX] {
        let wide = cols.min(500);
        let screen = Screen::read(&format!("{}Z\n", "a".repeat(wide)), cols);
        assert_eq!(screen.cell_at(0, wide - 1).map(|cell| cell.ch), Some('a'), "{cols}");
        assert_eq!(screen.cell_at(0, wide).map(|cell| cell.ch), Some(' '), "{cols}");
    }
}

#[test]
fn a_mark_goes_with_its_cell_when_a_character_is_printed_before_it_in_insert_mode() {
    // (As CSI @ moves it. One printed at the mark itself is after it, as with insert mode off.)
    assert_eq!(term(8, 2, "ab\x1b]133;B\x07cd\r\x1b[4hX").marks(), "0 3 command\n");
    assert_eq!(term(8, 2, "ab\x1b]133;B\x07cd\r\x1b[4h日").marks(), "0 5 command\n");
    assert_eq!(term(8, 2, "ab\x1b]133;B\x07cd\x1b[1;3H\x1b[4hX").marks(), "0 2 command\n");
    // (And when what comes next makes that character a column wider.)
    assert_eq!(term(8, 2, "ab\x1b]133;B\x07cd\r\x1b[4h\u{2764}\u{fe0f}").marks(), "0 8 command\n");
}

#[test]
fn an_erase_that_takes_the_whole_screen_with_half_a_wide_character_keeps_in_history_what_a_resize_took() {
    let mut term = term(4, 3, "top\r\n界x\r\nz");
    term.resize(4, 2);
    // (From the second half of the wide character on: the character goes whole, and so the screen.)
    term.feed(b"\x1b[1;2H\x1b[J");
    term.resize(4, 3);
    assert_eq!(term.plain().trim(), "");
}

#[test]
fn a_wrap_that_waits_on_the_alternate_screen_is_done_after_a_resize_that_keeps_the_width() {
    let mut term = term(4, 3, "\x1b[?1049hABCD");
    term.resize(4, 4);
    term.feed(b"E");
    let ch = |row, col| term.screen.cell_at(row, col).map(|cell| cell.ch);
    assert_eq!((ch(0, 3), ch(1, 0)), (Some('D'), Some('E')));
}

#[test]
fn a_wrap_that_waits_on_the_alternate_screen_is_forgotten_when_its_row_is_cut() {
    let mut term = term(4, 3, "\x1b[?1049hTOP\r\nMID\r\nABCD");
    term.resize(4, 2);
    term.feed(b"E");
    let row =
        |row: usize| (0..4).filter_map(|col| term.screen.cell_at(row, col)).map(|cell| cell.ch).collect::<String>();
    assert_eq!((row(0).as_str(), row(1).as_str()), ("TOP ", "MIDE"));
}

#[test]
fn a_picture_on_the_alternate_screen_keeps_its_column_on_a_screen_too_narrow_to_show_it() {
    let mut term = term(10, 4, "\x1b[?1049h\x1b[1;8H\x1bPq@\x1b\\");
    for cols in [5, 10] {
        term.resize(cols, 4);
        assert_eq!(term.screen.pictures.first().map(|p| (p.row, p.col)), Some((0, 7)), "{cols} columns");
    }
}

#[test]
fn a_picture_drawn_at_the_bottom_of_a_scroll_region_is_above_the_cursor_as_at_the_screens() {
    // A screen of 10 rows with cells of 10 by 20 pixels: the picture is `height` pixels high and
    // is drawn at row `at` (counted from 1) of the scroll region `region`.
    let placed = |region: &str, at: usize, height: usize| {
        let term = term(20, 10, &format!("{region}\x1b[{at};1H\x1bP0;0;0q\"1;1;10;{height}#0!10~-!10~\x1b\\"));
        (term.screen.pictures.first().map(|p| (p.row, p.rows, p.off)), term.screen.cursor.row)
    };
    // The region is the whole screen: the picture's two rows are above the cursor.
    assert_eq!(placed("", 10, 40), (Some((7, 2, 0)), 9));
    // The region ends above the screen's last row (apt's progress bar sets such a one): the same.
    assert_eq!(placed("\x1b[1;9r", 9, 40), (Some((6, 2, 0)), 8));
    assert_eq!(placed("\x1b[3;9r", 9, 40), (Some((6, 2, 0)), 8));
    // One as high as the room above the region's last row fills it; one with room below it scrolls nothing.
    assert_eq!(placed("\x1b[3;9r", 6, 120), (Some((2, 6, 0)), 8));
    assert_eq!(placed("\x1b[1;9r", 3, 40), (Some((2, 2, 0)), 4));
    // One taller than the region lies across its edge when it scrolls, and goes; below the region
    // nothing scrolls, and its rows past the screen's last are only not shown.
    assert_eq!(placed("\x1b[1;9r", 9, 200), (None, 8));
    assert_eq!(placed("\x1b[1;9r", 10, 40), (Some((9, 2, 0)), 9));
}

#[test]
fn a_program_that_asks_about_a_mode_that_is_not_private_is_answered() {
    // Insert mode (4) has a state, 2 for reset and 1 for set; no other is known (0), and each is
    // answered by the number it was asked with, 0 too. Unanswered, the program waits.
    let asked = term(8, 1, "\x1b[4$p\x1b[4h\x1b[4$p\x1b[4l\x1b[4$p\x1b[2$p\x1b[1049$p\x1b[0$p\x1b[$p");
    let told = "\x1b[4;2$y\x1b[4;1$y\x1b[4;2$y\x1b[2;0$y\x1b[1049;0$y\x1b[0;0$y\x1b[0;0$y";
    assert_eq!(String::from_utf8_lossy(&asked.peek().reply), told);
}

#[test]
fn a_picture_kept_past_the_edge_of_a_smaller_screen_goes_with_what_is_erased_up_to_that_edge() {
    // A picture at column 70 of 80 is kept when the screen has 40: each erase is fed there, and
    // what is left is asked on 80 columns again.
    let after = |erase: &str| {
        let mut term = term(80, 4, "\x1b[1;71H\x1bPq~\x1b\\");
        term.resize(40, 4);
        term.feed(erase.as_bytes());
        let narrow = term.screen.pictures.len();
        term.resize(80, 4);
        (narrow, term.screen.pictures.first().map(|p| (p.row, p.col)))
    };
    // The whole screen cleared (`clear` sends this) and its row erased to the end: it is gone, and stays gone.
    assert_eq!(after("\x1b[H\x1b[2J"), (0, None));
    assert_eq!(after("\x1b[1;5H\x1b[K"), (0, None));
    // Not erased, it comes back: another row's erase, an erase that ends before its row's end and
    // a character on its row's last cell do not reach it.
    assert_eq!(after("\x1b[2;1H\x1b[K\x1b[1;5H\x1b[1K\x1b[1;40Hx"), (1, Some((0, 70))));
    // On the alternate screen, one below the last row of a screen made lower: an erase up to the
    // cursor leaves it, the whole screen cleared takes it.
    let mut term = term(10, 6, "\x1b[?1049h\x1b[5;1H\x1bPq~\x1b\\");
    term.resize(10, 2);
    term.feed(b"\x1b[2;3H\x1b[1J");
    assert_eq!(term.screen.pictures.first().map(|p| p.row), Some(4));
    term.feed(b"\x1b[2J");
    term.resize(10, 6);
    assert_eq!(term.screen.pictures.first().map(|p| p.row), None);
    // From the last row: the display erased from the cursor on takes it, whatever the cursor's
    // column (the rows below are whole rows); the line erased leaves it, as the line's end is not the display's.
    let below = |erase: &str| {
        let mut term = self::term(10, 6, "\x1b[?1049h\x1b[5;1H\x1bPq~\x1b\\");
        term.resize(10, 2);
        term.feed(erase.as_bytes());
        term.resize(10, 6);
        term.screen.pictures.first().map(|p| p.row)
    };
    assert_eq!(
        (below("\x1b[2;3H\x1b[J"), below("\x1b[2;1H\x1b[2K"), below("\x1b[2;1H\x1b[99X")),
        (None, Some(4), Some(4))
    );
    // (Erased up to the first half of a wide character in the last cells, the display is erased to its end.)
    assert_eq!(below("\x1b[2;9H\u{754c}\x1b[2;9H\x1b[1J"), None);
}
