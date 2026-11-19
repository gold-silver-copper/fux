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
    let all = term.history.clone() + &term.plain();
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
    for text in [rest, term.plain(), term.history.clone()] {
        assert!(text.chars().all(|c| c == '\n' || !c.is_control()), "a control character reached a file");
    }
    assert!(term.pictures.len() <= PICTURES_MAX && term.reply.len() <= 4096);
}

#[test]
fn queries_are_answered_with_nobody_watching() {
    let term = term(80, 24, "\x1b[c\x1b[>c\x1b[5n\x1b[3;7H\x1b[6n\x1b[?2004$p\x1b[18t");
    assert_eq!(
        String::from_utf8_lossy(&term.reply),
        "\x1b[?62;4;22c\x1b[>1;10;0c\x1b[0n\x1b[3;7R\x1b[?2004;2$y\x1b[8;24;80t"
    );
}

#[test]
fn the_alternate_screen_gives_the_main_one_back_and_keeps_no_history() {
    let mut term = term(20, 4, "one\r\ntwo\x1b[?1049h\x1b[2Jfull\r\n\n\n\n\n\nscreen");
    assert!(term.alt && term.history.is_empty());
    term.feed(b"\x1b[?1049l!");
    assert_eq!(term.plain(), "one\ntwo!\n\n\n");
}

#[test]
fn a_resize_keeps_the_text_and_sends_what_no_longer_fits_to_history() {
    let mut term = term(10, 4, "a\r\nb\r\nc\r\nd");
    term.resize(5, 2);
    assert_eq!((term.history.as_str(), term.plain().as_str()), ("a\nb\n", "c\nd\n"));
    term.resize(8, 3);
    assert_eq!((term.plain().as_str(), term.row), ("c\nd\n\n", 1));
}

#[test]
fn a_picture_is_placed_in_cells_and_leaves_when_it_scrolls_off_or_is_written_over() {
    // 30 by 12 pixels with 10 by 20 cells: 3 columns, 1 row.
    let mut term = term(20, 4, "ab\x1bPq\"1;1;30;12#0!30~-!30~\x1b\\");
    let picture = term.pictures.first().expect("a picture");
    assert_eq!((picture.row, picture.col, picture.cols, picture.rows), (0, 2, 3, 1));
    assert!(picture.data.as_ref().is_some_and(|d| d.starts_with(b"\x1bPq") && d.ends_with(b"\x1b\\")));
    assert_eq!((term.row, term.col), (1, 0));
    term.feed(b"\x1b[1;4Hx");
    assert!(term.pictures.is_empty());
    term.feed(b"\x1bPq~~\x1b\\\n\n\n\n\n");
    assert!(term.pictures.is_empty() && term.refused.is_none());
}

#[test]
fn a_layout_reads_back_as_written_and_its_tiles_fill_the_area_without_overlap() {
    for text in ["* row(a col(b c):2) focus=c\n  d focus=d zoom\n", "* col(row(a b) row(c d e):3 f) focus=a\n"] {
        let layout = parse(text).expect("a layout");
        assert_eq!(layout.print(), text);
        let area = Rect { x: 0, y: 0, w: 83, h: 31 };
        let tiles = layout.tabs.first().expect("a tab").tiles(area);
        let covered: usize = tiles.iter().map(|(_, r)| r.w * r.h).sum();
        let lines: usize = area.w * area.h - covered;
        assert!(lines < area.h * tiles.len(), "only the lines between a row's parts are uncovered");
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
    assert_eq!(layout.print(), "* col(a new) focus=a\n  b focus=b\n");
    assert!(layout.edit(&["fly"]).is_err() && Layout::default().edit(&["zoom"]).is_err());
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
        (b"\x1bx", "M-x"),
        (b"\x1b", "esc"),
        (b"/", "slash"),
        ("é".as_bytes(), "é"),
        (b"\x1b[200~", "paste-start"),
        (b"\x1b[?62;4c", "terminal:?62;4"),
    ] {
        assert_eq!(name(bytes), Some((bytes.len(), expected.to_string())));
    }
    assert_eq!(crate::keys::next(b"\x1b[1;", false), None, "half a sequence waits for the rest");
}
