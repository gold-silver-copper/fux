//! Properties of the emulator, the layout and the key names. None of them
//! fixes what a screen looks like beyond what its program asked for.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use crate::layout::{Layout, Rect, parse};
use crate::term::{PICTURES_MAX, Term};

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
fn the_styled_screen_read_back_is_the_same_screen() {
    let mut first = term(30, 6, "plain \x1b[1;31mbold red\x1b[0m \x1b[48;5;200m256\x1b[38;2;1;2;3mrgb\x1b[m\r\n");
    first.feed(
        "wide 日本語 e\u{301} \x1b[4:3mcurly\x1b[m \x1b[7mrev\x1b[27m\x1b[44m\x1b[K\r\n\x1b(0lqk\x1b(B".as_bytes(),
    );
    let mut second = Term::new(30, 6);
    for (row, line) in first.styled().lines().enumerate() {
        second.feed(format!("\x1b[{};1H{line}", row + 1).as_bytes());
    }
    assert_eq!(first.styled(), second.styled());
    assert_eq!(first.plain(), second.plain());
    assert!(first.plain().contains("wide 日本語 e\u{301} curly rev") && first.plain().contains("┌─┐"));
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
    assert!(term.pictures.len() <= PICTURES_MAX && term.peek().reply.len() <= 4096);
}

#[test]
fn a_question_about_a_colour_is_answered_from_what_was_told_and_never_made_up() {
    // Told nothing, the terminal says nothing: the answer to the next question comes alone.
    let asks = "\x1b]11;?\x07\x1b]10;?\x1b\\\x1b]4;1;?;7;?;300;?\x1b\\\x1b[c";
    let da = "\x1b[?62;4;22c".to_string();
    assert_eq!(String::from_utf8(term(20, 2, asks).take().reply).unwrap(), da);
    // Told, it answers in the order asked, each answer ended as its question was, and is silent
    // about what it was not told (a palette entry, a number that is no entry).
    let mut told = Term::new(20, 2);
    told.colours = "foreground #d0d0d0\nbackground #1c1c1c\n1 #cc0000\nnonsense\n".into();
    told.feed(asks.as_bytes());
    let answers = "\x1b]11;rgb:1c1c/1c1c/1c1c\x07\x1b]10;rgb:d0d0/d0d0/d0d0\x1b\\\x1b]4;1;rgb:cccc/0000/0000\x1b\\";
    assert_eq!(String::from_utf8(told.take().reply).unwrap(), answers.to_string() + &da);
    // Setting a colour is taken and changes nothing; a reset keeps what was told.
    told.feed(b"\x1b]11;#ffffff\x07\x1b]4;1;rgb:00/00/00\x07\x1b]104\x07\x1b]111\x07\x1bc\x1b]11;?\x07x");
    assert_eq!(String::from_utf8(told.take().reply).unwrap(), "\x1b]11;rgb:1c1c/1c1c/1c1c\x07");
    assert_eq!(told.plain(), "x\n\n");
    // The viewer reads its terminal's answers as answers, not as keys.
    use crate::keys::next;
    assert_eq!(next(b"\x1b]11;rgb:1c1c/1c1c/1c1c\x1b\\x", false), Some((25, "answer:11;rgb:1c1c/1c1c/1c1c".into())));
    assert_eq!(next(b"\x1b]4;1;rgb:cc/00/00\x07", false), Some((19, "answer:4;1;rgb:cc/00/00".into())));
    assert_eq!((next(b"\x1b]11;rgb:1c", false), next(b"\x1b]", true)), (None, Some((2, "M-]".into()))));
}

#[test]
fn queries_are_answered_with_nobody_watching() {
    let term = term(80, 24, "\x1b[c\x1b[>c\x1b[5n\x1b[3;7H\x1b[6n\x1b[?2004$p\x1b[18t");
    assert_eq!(
        String::from_utf8_lossy(&term.peek().reply),
        "\x1b[?62;4;22c\x1b[>1;10;0c\x1b[0n\x1b[3;7R\x1b[?2004;2$y\x1b[8;24;80t"
    );
}

#[test]
fn a_cursor_waiting_at_the_right_margin_moves_like_any_other() {
    // After a character in the last column the cursor waits there; a back-tab still goes back.
    let term = term(80, 3, "\x1b[1;80Hb\x1b[Z.");
    assert_eq!((term.row, term.col), (0, 73));
}

#[test]
fn a_title_and_a_clipboard_leave_as_text_and_bytes_and_a_string_has_one_bound() {
    use crate::term::{base64_decode, base64_encode};
    // The title loses its control characters; the clipboard is what the base64 says; text goes on after both.
    let mut term = term(40, 2, "\x1b]2;my \u{9b}title\x07\x1b]52;c;aGVsbG8sIHdvcmxk\x1b\\after");
    let out = term.take();
    assert_eq!((out.title.as_deref(), out.clipboard.as_deref()), (Some("my title"), Some(&b"hello, world"[..])));
    assert_eq!(term.plain(), "after\n\n");
    // A title put aside comes back, as a program that borrows the title for a while expects.
    term.feed(b"\x1b[22;2t\x1b]2;borrowed\x07\x1b[23;2t");
    assert_eq!(term.take().title.as_deref(), Some("my title"));
    // A program is never told what is on the clipboard, and is refused in words.
    term.feed(b"\x1b]52;c;?\x07\x1b]52;c;not base64!\x07");
    let out = term.take();
    assert!(out.reply.is_empty() && out.clipboard.is_none() && out.refused.len() == 2);
    // Neither is padding in the middle base64, nor a command with nothing to set; a pop with nothing put aside changes nothing.
    term.feed(b"\x1b]52;c;QU=JD\x07\x1b]52;c\x07\x1b[23;2t\x1b[23;2t");
    let out = term.take();
    assert!(out.clipboard.is_none() && out.title.is_none());
    // A string of any kind that never ends is dropped at its bound, and said; what follows is text again.
    term.feed(b"\x1b]0;");
    term.feed(&vec![b'x'; (4 << 20) + 2]);
    term.feed(b"\x07ok");
    let out = term.take();
    assert!(out.title.is_none() && out.refused.len() == 1 && term.plain().contains("afterok"));
    // A string that never ends is still said to be dropped, when it passes its bound.
    term.feed(b"\x1b]0;");
    term.feed(&vec![b'x'; (4 << 20) + 2]);
    assert_eq!(term.take().refused.len(), 1);
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
    assert_eq!((repeated.plain().as_str(), repeated.row, repeated.col), ("XXXXX\nXXXXX\nXX\n\n", 2, 2));
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
    assert!(term.cursor_shown && !term.peek().refused.is_empty());
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
    assert!(term.alt && term.peek().history.is_empty());
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
    let line = |next: &mut dyn FnMut(usize) -> usize| {
        (0..next(40)).map(|_| words[next(words.len())]).collect::<Vec<_>>().join(" ")
    };
    (0..2 + next(12)).map(|_| line(next)).collect::<Vec<_>>().join("\r\n")
}

/// Text as its lines, without the blanks at a line's end (where a cursor stood, blanks may be
/// kept) and without the empty lines at its end.
fn lines_of(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(|line| line.trim_end_matches(' ')).collect();
    lines.join("\n").trim_end_matches('\n').to_string()
}

#[test]
fn a_resize_wraps_the_main_screen_anew_and_loses_nothing() {
    // The case that lost 40 characters of 123: a line of 120, narrowed to 40 columns and widened back.
    let long: String = (0..120).map(|n| char::from(b'a' + n % 26)).collect();
    let mut wide = term(120, 10, &format!("{long}\r\nend"));
    assert!(
        wide.resize(40, 10)
            && wide.plain().starts_with(&format!("{}\n{}\n{}\nend\n", &long[..40], &long[40..80], &long[80..]))
    );
    assert!(wide.resize(120, 10));
    assert_eq!((wide.plain(), wide.row, wide.col), (format!("{long}\nend\n\n\n\n\n\n\n\n\n"), 1, 3));
    // A row erased from its start, or moved away, is no longer what the row above goes on in:
    // widened, the two do not become one line. And one column is a size like any other.
    let mut parted = term(4, 3, "abcdef\x1b[2;1H\x1b[Kxy");
    assert!(parted.resize(8, 3) && parted.plain() == "abcd\nxy\n\n", "{:?}", parted.plain());
    let mut emptied = term(4, 4, "abcdefghij\x1b[2;1H\x1b[2K");
    assert!(emptied.resize(8, 4) && emptied.plain() == "abcd\n\nij\n\n", "{:?}", emptied.plain());
    let mut moved = term(4, 3, "abcdef\x1b[2;1H\x1b[Lxy");
    assert!(moved.resize(8, 3) && moved.plain() == "abcd\nxy\nef\n", "{:?}", moved.plain());
    let mut thin = term(6, 4, "a\u{4e00}b\u{4e8c}c");
    assert!(thin.resize(1, 4) && thin.resize(6, 4) && thin.lines().2);
    assert_eq!(thin.peek().history.clone() + &thin.lines().0.replace([' ', '\n'], ""), "abc");
    // A line whose first rows are in history, and whose rest is then cleared away, ends there:
    // what is printed next is a line of its own.
    let mut cleared = term(10, 2, "x\r\naaaaaaaaaabbbbb\r\n\x1b[H\x1b[2J$ ls\r\nfile\r\n");
    assert_eq!(cleared.take().history, "x\naaaaaaaaaa\n$ ls\n");
    // A line editor draws a long line again row by row: written over, it is one line still.
    let mut drawn = term(5, 2, "abcdefg\x1b[1;5HX\x1b[2;1Hfg");
    assert!(drawn.resize(10, 2) && drawn.plain() == "abcdXfg\n\n", "{:?}", drawn.plain());
    // A saved cursor that waits at a row's end stays there; one in the blank part of a row is
    // kept inside the row and does not wait; the cursor behind the alternate screen comes back
    // to its column; a mark that comes after a resize still finds its letter.
    let mut waits = term(4, 5, "ABCD\x1b7EF");
    assert!(waits.resize(2, 5));
    waits.feed(b"\x1b8");
    assert_eq!((waits.row, waits.col, waits.plain().as_str()), (1, 1, "AB\nCD\nEF\n\n\n"));
    let mut beside = term(20, 3, "ab\x1b[1;15H\x1b7\x1b[3;1H");
    assert!(beside.resize(10, 3));
    beside.feed(b"\x1b8X");
    assert_eq!(beside.plain(), "ab       X\n\n\n");
    let mut hidden = term(8, 5, "A\x1b[1;8H\x1b[?1049h");
    assert!(hidden.resize(3, 5) && hidden.resize(8, 5));
    hidden.feed(b"\x1b[?1049l");
    assert_eq!((hidden.row, hidden.col), (0, 7));
    let mut later = term(8, 3, "e");
    assert!(later.resize(6, 3));
    later.feed("\u{301}".as_bytes());
    assert_eq!(later.plain(), "e\u{301}\n\n\n");
    // A row that ends in coloured blanks stays one row, coloured to its new end; a blank that
    // was given a colour of its own is in `screen.styled`.
    let mut bar = term(8, 3, "\x1b[44m\x1b[K\x1b[m\r\nx");
    assert!(bar.resize(4, 3) && bar.plain() == "\nx\n\n" && bar.peek().history.is_empty());
    assert_eq!(bar.styled().lines().next(), Some("\x1b[0;44m    \x1b[m"));
    assert!(term(4, 1, "\x1b[31m \x1b[m").styled().starts_with("\x1b[0;31m "));
    for seed in 0..300 {
        let next = &mut numbers(seed);
        let text = some_text(next);
        let mut term = term(2 + next(60), 1 + next(30), &text);
        let printed = text.replace("\r\n", "\n");
        let everything = |term: &Term| lines_of(&(term.peek().history.clone() + &term.lines().0));
        assert_eq!(everything(&term), lines_of(&printed), "seed {seed}: as printed");
        // A saved cursor is put somewhere on the screen, and then the cursor, on text or beside it
        // (not in the last column, where a cell may be the gap a wide character left: no place in the text).
        let place = |term: &Term| term.peek().history.clone() + &term.lines().1;
        term.feed(format!("\x1b[{};{}H\x1b7", 1 + next(term.rows), 1 + next(term.cols - 1)).as_bytes());
        let saved_before = place(&term);
        term.feed(format!("\x1b[{};{}H", 1 + next(term.rows), 1 + next(term.cols - 1)).as_bytes());
        let (was, at, history) = (term.plain(), (term.row, term.col), term.peek().history.len());
        let (before, size) = (place(&term), (term.cols, term.rows));
        let (mut there, mut quiet) = (true, true);
        for _ in 0..1 + next(4) {
            // (1) Every character printed is in the screen or in history, once and in order; (3) the cursor
            // stays on its character; (4) no character is cut in half.
            let kept = term.peek().history.len();
            assert!(term.resize(2 + next(60), 1 + next(30)));
            assert_eq!(everything(&term), lines_of(&printed), "seed {seed}: at {}x{}", term.cols, term.rows);
            assert!(term.lines().2, "seed {seed}: a character in half at {}x{}", term.cols, term.rows);
            // (Unless its own row had to go to history, to make room for text below it: then
            // it is at the top, and on another character from then on.)
            there &= term.row > 0 || term.peek().history.len() == kept;
            quiet &= term.peek().history.len() == kept;
            if there {
                assert_eq!(place(&term), before, "seed {seed}: the cursor at {}x{}", term.cols, term.rows);
            }
        }
        // (2) Back at the size it had, with nothing printed between: as it was, the rows that
        // went to history on the way taken back from it (if the cursor's own row did not go).
        assert!(term.resize(size.0, size.1));
        if there {
            assert_eq!(term.peek().history.len(), history, "seed {seed}: history, back at {size:?}");
            assert_eq!((term.plain(), (term.row, term.col)), (was, at), "seed {seed}: back at {size:?}");
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
fn a_resize_cuts_the_alternate_screen_and_a_refused_one_changes_nothing() {
    for seed in 0..100 {
        let next = &mut numbers(seed);
        let text = some_text(next);
        let mut term = term(2 + next(60), 1 + next(30), &text);
        let everything = |term: &Term| lines_of(&(term.peek().history.clone() + &term.lines().0));
        let main = everything(&term);
        term.feed(format!("\x1b[?1049h\x1b[H{}", some_text(next).replace('a', "ALT")).as_bytes());
        for _ in 0..1 + next(4) {
            // (4) No character is cut in half on either screen, and the alternate one's rows stay rows.
            assert!(term.resize(2 + next(60), 1 + next(30)) && term.lines().2, "seed {seed}");
        }
        // (6) A size that is refused changes nothing, and says so.
        let state = (term.styled(), term.row, term.col, term.cols, term.rows, term.peek().history.clone());
        for (cols, rows) in [(0, 5), (5, 0), (501, 5), (5, 501)] {
            assert!(!term.resize(cols, rows));
        }
        assert_eq!(state, (term.styled(), term.row, term.col, term.cols, term.rows, term.peek().history.clone()));
        assert_eq!(term.peek().refused.len(), 1);
        // (5) Leaving the alternate screen gives the main one back at the new size, with all its text;
        // nothing of the alternate screen went to history.
        term.feed(b"\x1b[?1049l");
        assert_eq!(everything(&term), main, "seed {seed}: at {}x{}", term.cols, term.rows);
        assert!(!term.peek().history.contains("ALT") && term.plain().lines().count() == term.rows);
    }
}

#[test]
fn a_picture_is_placed_in_cells_and_leaves_when_it_scrolls_off_or_is_written_over() {
    // 30 by 12 pixels with 10 by 20 cells: 3 columns, 1 row.
    let mut term = term(20, 4, "ab\x1bPq\"1;1;30;12#0!30~-!30~\x1b\\");
    let picture = term.pictures.first().expect("a picture");
    assert_eq!((picture.row, picture.col, picture.cols, picture.rows), (0, 2, 3, 1));
    let bytes = term.peek().pictures.first().map(|(id, bytes)| (*id, bytes.as_slice()));
    assert!(bytes.is_some_and(|(id, d)| id == picture.id && d.starts_with(b"\x1bPq") && d.ends_with(b"\x1b\\")));
    assert_eq!((term.row, term.col), (1, 0));
    // It stays through a full-screen program, an erase beside it and a resize that fits it.
    term.feed(b"\x1b[?1049h\x1b[2Jless\x1b[?1049l\x1b[1;10H\x1b[K\x1b[2;1H");
    term.resize(10, 3);
    assert_eq!(term.pictures.len(), 1);
    term.feed(b"\x1b[1;4Hx");
    assert!(term.pictures.is_empty());
    term.feed(b"\x1bPq~~\x1b\\\n\n\n\n\n");
    assert!(term.pictures.is_empty() && term.peek().refused.is_empty());
    // Its size is the larger of what it says and what it paints: here 40 pixels, 4 cells, not 1.
    term.feed(b"\x1b[H\x1bPq\"1;1;10;6!40~\x1b\\");
    assert_eq!(term.pictures.first().map(|p| (p.cols, p.rows)), Some((4, 1)));
}

#[test]
fn a_layout_reads_back_as_written_and_its_tiles_fill_the_area_without_overlap() {
    for text in ["* row(a col(b c):2) focus=c\n- d focus=d\n  e focus=e\n", "* col(row(a b) row(c d e):3 f) focus=a\n"]
    {
        let layout = parse(text).expect("a layout");
        assert_eq!(layout.print(), text);
        let area = Rect { x: 0, y: 0, w: 83, h: 31 };
        let tiles = layout.tabs.first().expect("a tab").tiles(area);
        let covered: usize = tiles.iter().map(|(_, r)| r.w * r.h).sum();
        let lines: usize = area.w * area.h - covered;
        assert!(lines < (area.w + area.h) * tiles.len(), "only the lines between parts are uncovered");
        for (i, (_, a)) in tiles.iter().enumerate() {
            assert!(a.x + a.w <= area.w && a.y + a.h <= area.h);
            for (_, b) in tiles.iter().skip(i + 1) {
                assert!(a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y, "tiles overlap");
            }
        }
    }
    assert!(parse("row(a").is_err() && parse("a b").is_err() && parse("x/y").is_err());
}

#[test]
fn a_layout_follows_the_panes_that_exist_and_edits_act_on_the_focused_pane() {
    let mut layout = parse("* row(a gone b) focus=gone\n").expect("a layout");
    layout.settle(vec!["a".into(), "b".into(), "new".into()]);
    assert_eq!(layout.print(), "* row(a b) focus=a\n  new focus=new\n");
    let edits: &[&[&str]] = &[&["split", "col", "new"], &["focus", "up"], &["focus", "right"], &["size", "row", "+10"]];
    edits.iter().for_each(|edit| layout.edit(edit).expect("an edit"));
    assert_eq!(layout.print(), "* row(col(a new):50 b:60) focus=b\n");
    layout.edit(&["break"]).expect("break");
    layout.edit(&["tab", "prev"]).expect("tab");
    assert_eq!(layout.print(), "* col(a new) focus=a\n- b focus=b\n");
    assert!(
        layout.edit(&["fly"]).is_err()
            && Layout::default().edit(&["rotate"]).is_err()
            && layout.edit(&["zoom"]).is_err()
    );
    // A pane that is not there is not placed, a huge step is a big one, and nesting has an end.
    assert!(layout.edit(&["split", "row", "nobody"]).is_err());
    layout.edit(&["size", "col", "9223372036854775807"]).expect("size");
    assert_eq!(layout.print(), "* col(a:10000 new:50) focus=a\n- b focus=b\n");
    // What an edit writes, the next reading takes: 40 splits deep is refused, as is a share out of range.
    assert!(parse("row(a:20000 b)").is_err());
    // A row with more parts than columns shows those it has room for, each with a cell, lines between.
    let crowded = parse("* row(a b c d e) focus=a\n").expect("a layout");
    let tiles = crowded.tabs.first().expect("a tab").tiles(Rect { x: 0, y: 0, w: 5, h: 2 });
    assert_eq!(tiles.iter().map(|(_, r)| (r.x, r.w)).collect::<Vec<_>>(), [(0, 1), (2, 1), (4, 1)]);
    // A pane named like a direction is reached with `pane`.
    let mut named = parse("* row(a left) focus=left\n").expect("a layout");
    named.edit(&["pane", "a"]).expect("pane");
    named.edit(&["pane", "left"]).expect("pane");
    assert_eq!(named.print(), "* row(a left) focus=left\n");
    assert!(parse(&format!("{}a{}", "row(".repeat(40), ")".repeat(40))).is_err());
}

#[test]
fn when_the_tab_shown_goes_the_tab_shown_before_it_is_shown() {
    // An edit that ends on another tab marks the one it left; an edit that stays leaves the mark.
    let mut layout = parse("* row(a b) focus=a\n  c focus=c\n  d focus=d\n").expect("a layout");
    layout.edit(&["pane", "d"]).expect("pane");
    layout.edit(&["focus", "next"]).expect("focus");
    assert_eq!(layout.print(), "- row(a b) focus=a\n  c focus=c\n* d focus=d\n");
    // The shown tab's pane goes: back to the marked tab, not to the neighbour.
    let mut closed = layout.clone();
    closed.settle(vec!["a".into(), "b".into(), "c".into()]);
    assert_eq!(closed.print(), "* row(a b) focus=a\n  c focus=c\n");
    // The marked tab goes too: the position, as before there was a mark.
    layout.settle(vec!["c".into()]);
    assert_eq!(layout.print(), "* c focus=c\n");
    // A pane that leaves for a tab of its own leaves its old tab marked, and `tab -` goes back.
    let mut broken = parse("* row(a b) focus=b\n").expect("a layout");
    broken.edit(&["break"]).expect("break");
    assert_eq!(broken.print(), "- a focus=a\n* b focus=b\n");
    broken.edit(&["tab", "-"]).expect("tab");
    assert_eq!(broken.print(), "* a focus=a\n- b focus=b\n");
}

#[test]
fn keys_have_the_names_of_their_files() {
    let name = |bytes: &[u8]| crate::keys::next(bytes, true);
    for (bytes, expected) in [
        (&b"\x02"[..], "C-b"),
        (b"v", "v"),
        (b"H", "S-h"),
        (b"\x1b[A", "up"),
        (b"\x1bOA", "up"),
        (b"\x1b[1;5D", "C-left"),
        (b"\x1b[15~", "f5"),
        (b"\x1b[15;2~", "S-f5"),
        (b"\x1b[1;2P", "S-f1"),
        (b"\x1bOP", "f1"),
        // The keypad's 0 in application mode is the program's, not F1.
        (b"\x1bOp", ""),
        (b"\x1b[", "M-["),
        (b"\x1bx", "M-x"),
        (b"\x1b", "esc"),
        (b"/", "slash"),
        ("é".as_bytes(), "é"),
        (b"\x1b[200~", "paste-start"),
        (b"\x1b[?62;4c", "terminal:?62;4"),
    ] {
        assert_eq!(name(bytes), Some((bytes.len(), expected.to_string())));
    }
    assert_eq!(crate::keys::next(b"\x1b[20", true), None, "half a sequence waits for the rest, however long");
}

#[test]
fn a_screen_file_reads_back_as_the_terminal_that_wrote_it() {
    let mut term = Term::new(12, 4);
    term.feed("\x1b[?1h\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[4 q\x1b[31m漢x\x1b[m e\u{301}\x1b[2;3H".as_bytes());
    let file = format!("pid=1 {}\n{}", term.header(), term.styled());
    let mut back = Term::from_screen(&file, |_, _| {});
    assert_eq!(back.header(), term.header());
    assert_eq!((back.plain(), back.styled()), (term.plain(), term.styled()));
}

#[test]
fn a_program_is_told_which_mouse_reports_are_on() {
    let term = term(20, 2, "\x1b[?1002h\x1b[?1002$p\x1b[?1000$p\x1b[?1006$p");
    assert_eq!(String::from_utf8_lossy(&term.peek().reply), "\x1b[?1002;1$y\x1b[?1000;2$y\x1b[?1006;2$y");
    // The form of the reports is listed on line 1 whether or not any report is on, and reads back.
    let mut form = crate::tests::term(20, 2, "\x1b[?1006h");
    assert!(form.header().ends_with("modes=cursor,mousesgr"));
    assert_eq!(Term::from_screen(&format!("{}\n", form.header()), |_, _| {}).header(), form.header());
}

#[test]
fn a_share_changes_the_same_way_however_deep_and_a_split_can_name_its_neighbour() {
    let (mut flat, mut deep) =
        (parse("* row(a b c:4) focus=a\n").expect("flat"), parse("* col(d row(a b c:4)) focus=a\n").expect("deep"));
    flat.edit(&["size", "row", "+5"]).expect("size");
    deep.edit(&["size", "row", "+5"]).expect("size");
    assert_eq!(
        (flat.print(), deep.print()),
        ("* row(a:21 b:16 c:66) focus=a\n".into(), "* col(d row(a:21 b:16 c:66)) focus=a\n".into())
    );
    // The focus is on d by now; the new pane still goes beside the pane it was asked for.
    let mut layout = parse("* row(a b) focus=b\n  d focus=d\n  n focus=n\n").expect("a layout");
    layout.edit(&["split", "col", "n", "beside", "a"]).expect("split");
    assert_eq!(layout.print(), "* row(col(a n) b) focus=n\n  d focus=d\n");
    assert!(layout.edit(&["split", "col", "n", "beside", "nobody"]).is_err());
    // In a view with room for one of three, the one drawn is the one that has the keys.
    let crowded = parse("* row(a b c) focus=c\n").expect("a layout");
    let tiles = crowded.tabs.first().expect("a tab").tiles(Rect { x: 0, y: 0, w: 1, h: 3 });
    assert_eq!(tiles.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["c"]);
}

#[test]
fn a_resize_under_the_alternate_screen_keeps_the_main_screens_lines() {
    let mut term = Term::new(10, 8);
    term.feed(b"l1\r\nl2\r\nl3\r\nl4\r\nl5\r\n\x1b[?1047h");
    term.resize(10, 3);
    term.feed(b"\x1b[?1047l");
    assert_eq!(term.peek().history.clone() + &term.plain(), "l1\nl2\nl3\nl4\nl5\n\n");
    // A row with text below the cursor is kept as well: rows leave by the top, into history.
    let mut below = Term::new(10, 6);
    below.feed(b"TOP\x1b[5;1HBOTTOM\x1b[1;1H\x1b[?1049h");
    below.resize(10, 3);
    below.feed(b"\x1b[?1049l");
    assert!((below.peek().history.clone() + &below.plain()).contains("TOP") && below.plain().contains("BOTTOM"));
}

#[test]
fn text_is_cut_into_characters_exactly_as_unicodes_own_test_file_says() {
    let mut cases = 0;
    for line in include_str!("../tests/GraphemeBreakTest.txt").lines() {
        let Some((case, _)) = line.split_once('#').filter(|(case, _)| !case.trim().is_empty()) else { continue };
        // "÷ 0020 × 0308 ÷": the mark before a character says whether it joins the one before.
        let mut joiner = crate::width::Joiner::default();
        let mut words = case.split_whitespace();
        while let (Some(mark), Some(code)) = (words.next(), words.next()) {
            let c = char::from_u32(u32::from_str_radix(code, 16).unwrap()).unwrap();
            assert_eq!(joiner.joins(c), mark == "×", "{line}");
        }
        cases += 1;
    }
    // The file is Unicode 17.0.0's, whole: 766 cases, as tools/unicode.py pins it.
    assert_eq!((cases, include_str!("../tests/GraphemeBreakTest.txt").len()), (766, 126_570));
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
        assert_eq!((first.plain(), first.col), (format!("a{text}|\n\n"), columns + 2), "{name}");
        assert_eq!(first.cell_at(0, 1).map(|cell| cell.wide()), Some(columns == 2), "{name}");
        // Through `screen.styled`, read back: the same cells.
        let mut second = Term::new(12, 2);
        second.feed(first.styled().lines().next().unwrap().as_bytes());
        assert_eq!((second.plain(), second.col), (first.plain(), first.col), "{name}");
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
        (0..8).map(|x| back.cell_at(0, x)).collect::<Vec<_>>(),
        (0..8).map(|x| apart.cell_at(0, x)).collect::<Vec<_>>()
    );
    let mut long = term(8, 1, &format!("e{}", "\u{301}".repeat(12)));
    assert_eq!(long.plain().chars().count(), 10 + 1);
    assert_eq!(long.take().refused.len(), 1);
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
        assert_eq!(term(8, 1, &format!("\u{1f468}\u{200d}{between}\u{1f469}|")).col, 3, "{between:?}");
    }
    assert_eq!(term(4, 1, "a\r\x1b[C\u{301}").plain(), "a\u{301}\n");
    assert_eq!(term(4, 1, "a\x1b[C\u{301}").plain(), "a\n");
    assert_eq!(term(4, 1, "a\x08\x1b[X\x1b[C\u{301}|").plain(), " |\n");
    // A wide mark (a voiced mark of kana) takes no column.
    assert_eq!(term(8, 1, "\u{304b}\u{3099}|").col, 3);
    // REP's copies are each a character.
    assert_eq!(term(12, 1, "\u{1100}\x1b[3b|").col, 9);
    // A mark every terminal gives a column has one here (a halfwidth voiced mark; a skin tone alone).
    assert_eq!((term(8, 1, "\u{ff76}\u{ff9e}|").col, term(8, 1, "\u{1f3fd}|").col), (3, 3));
    // A program that asks is told clusters are always on.
    assert_eq!(term(8, 1, "\x1b[?2027$p").take().reply, b"\x1b[?2027;3$y");
}
