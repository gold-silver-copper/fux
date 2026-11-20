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
    // A string of any kind that never ends is dropped at its bound, and said; what follows is text again.
    term.feed(b"\x1b]0;");
    term.feed(&vec![b'x'; (4 << 20) + 2]);
    term.feed(b"\x07ok");
    let out = term.take();
    assert!(out.title.is_none() && out.refused.len() == 1 && term.plain().contains("afterok"));
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

#[test]
fn a_resize_keeps_the_text_and_sends_what_no_longer_fits_to_history() {
    let mut term = term(10, 4, "a\r\nb\r\nc\r\nd");
    term.resize(5, 2);
    assert_eq!((term.peek().history.as_str(), term.plain().as_str()), ("a\nb\n", "c\nd\n"));
    term.resize(8, 3);
    assert_eq!((term.plain().as_str(), term.row), ("c\nd\n\n", 1));
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
fn a_wide_character_cut_by_a_resize_goes_whole_on_both_screens() {
    // One on the screen shown, one on the screen behind the alternate one.
    let mut term = Term::new(4, 2);
    term.feed("AB界\x1b[?1049hCD界".as_bytes());
    term.resize(3, 2);
    assert_eq!(term.plain(), "\nCD\n");
    term.feed(b"\x1b[?1049l");
    assert_eq!(term.plain(), "AB\n\n");
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
