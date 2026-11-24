//! Properties of the layout, of the key names beside the emulator they agree with, of what
//! the viewer draws of a picture, and of what `changes` says.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use crate::layout::{Layout, Rect, parse};
use crate::term::{Term, files};

/// The key at the start of the bytes, as the one parser tells it: how long it is and its name.
fn next(bytes: &[u8], whole: bool, answers: bool) -> Option<(usize, String)> {
    crate::keys::next(bytes, whole, answers).map(|(length, name, _)| (length, name))
}

fn term(cols: usize, rows: usize, bytes: &str) -> Term {
    let mut term = Term::new(cols, rows);
    term.feed(bytes.as_bytes());
    term
}

#[test]
fn a_question_about_a_colour_is_answered_from_what_was_told_and_never_made_up() {
    // Told nothing, the terminal says nothing: the answer to the next question comes alone.
    let asks = "\x1b]11;?\x07\x1b]10;?\x1b\\\x1b]4;1;?;7;?;300;?\x1b\\\x1b[c";
    let da = "\x1b[?62;4;22c".to_string();
    assert_eq!(String::from_utf8(term(20, 2, asks).take(0).reply).unwrap(), da);
    // Told, it answers in the order asked, each answer ended as its question was, and is silent
    // about what it was not told (a palette entry, a number that is no entry).
    let mut told = Term::new(20, 2);
    files::parse_colours("foreground #d0d0d0\nbackground #1c1c1c\n1 #cc0000\nnonsense\n", told.told());
    told.feed(asks.as_bytes());
    let answers = "\x1b]11;rgb:1c1c/1c1c/1c1c\x07\x1b]10;rgb:d0d0/d0d0/d0d0\x1b\\\x1b]4;1;rgb:cccc/0000/0000\x1b\\";
    assert_eq!(String::from_utf8(told.take(0).reply).unwrap(), answers.to_string() + &da);
    // `10` with two parts asks for the foreground and then the background, and `11`'s second
    // part for the cursor's colour, which is told if there is a line for it; a line that is no
    // colour (not six hex digits) is no answer.
    told.feed(b"\x1b]10;?;?\x07\x1b]11;?;?\x07");
    let both = "\x1b]10;rgb:d0d0/d0d0/d0d0\x07\x1b]11;rgb:1c1c/1c1c/1c1c\x07\x1b]11;rgb:1c1c/1c1c/1c1c\x07";
    assert_eq!(String::from_utf8(told.take(0).reply).unwrap(), both);
    told.told().cursor = Some([0xff, 0x80, 0x00]);
    told.feed(b"\x1b]12;?\x07");
    assert_eq!(String::from_utf8(told.take(0).reply).unwrap(), "\x1b]12;rgb:ffff/8080/0000\x07");
    files::parse_colours("background #+12345\nforeground #d0d0d\n7 cc0000\n", told.told());
    told.feed(b"\x1b]11;?\x07\x1b]10;?\x07\x1b]4;7;?\x07");
    assert!(told.take(0).reply.is_empty());
    // A person's file: blanks do not matter, capitals neither, and of two lines for a colour the last counts.
    files::parse_colours("background #000000\n  background\t#FDF6E3  # paper\r\n", told.told());
    told.feed(b"\x1b]11;?\x07");
    assert_eq!(String::from_utf8(told.take(0).reply).unwrap(), "\x1b]11;rgb:fdfd/f6f6/e3e3\x07");
    assert_eq!(files::render(&told, false).map_or(String::new(), |files| files.screen), "\n\n");
    // The viewer reads its terminal's answers as answers, not as keys.
    assert_eq!(
        next(b"\x1b]11;rgb:1c1c/1c1c/1c1c\x1b\\x", false, true),
        Some((25, "answer:11;rgb:1c1c/1c1c/1c1c".into()))
    );
    assert_eq!(next(b"\x1b]4;1;rgb:cc/00/00\x07", false, true), Some((19, "answer:4;1;rgb:cc/00/00".into())));
    // An answer that comes in pieces waits for its end; when nothing was asked, Alt-] is a key at once.
    assert_eq!((next(b"\x1b]11;rgb:1c", false, true), next(b"\x1b]", false, true)), (None, None));
    assert_eq!(next(b"\x1b]x\x07", false, false), Some((2, "M-]".into())));
    // The viewer reads its terminal's answer about a mode as an answer.
    assert_eq!(next(b"\x1b[?2027;2$y", false, false), Some((11, "mode:2027;2".into())));
    // And its window's gain and loss of the keys as that, not as a key.
    assert_eq!(next(b"\x1b[Ox", false, false), Some((3, "focus:O".into())));
}

#[test]
fn a_program_says_how_it_wants_keys_told_and_is_answered_no_more_than_the_terminal_takes() {
    // With no terminal known to take the protocol, the answer is 0 whatever was asked, and a
    // program that was told so is not given the flags later, when a terminal that takes them
    // comes: what it was told stays true until it asks again.
    let mut early = term(9, 2, "\x1b[>1u\x1b[?u");
    early.told().keyboard = crate::term::KEYS;
    early.feed(b"\x1b[?u");
    assert_eq!((early.take(0).reply.as_slice(), early.shown().keys()), (&b"\x1b[?0u\x1b[?0u"[..], 0));
    early.feed(b"\x1b[=1u\x1b[?u");
    early.told().keyboard = 0;
    assert_eq!((early.take(0).reply.as_slice(), early.shown().keys()), (&b"\x1b[?1u"[..], 1));
    let asked = |bytes: &str| {
        let mut term = Term::new(9, 2);
        term.told().keyboard = crate::term::KEYS;
        term.feed(bytes.as_bytes());
        (String::from_utf8(term.take(0).reply).unwrap(), files::modes(&mut term))
    };
    // The protocol's own examples: flags are set, set aside and come back; setting adds or
    // takes bits; going back past the first is no flags; the two screens have their own.
    assert_eq!(asked("\x1b[?u\x1b[>1u\x1b[?u\x1b[<u\x1b[?u").0, "\x1b[?0u\x1b[?1u\x1b[?0u");
    assert_eq!(asked("\x1b[=1u\x1b[>5u\x1b[?u\x1b[<1u\x1b[?u\x1b[<9u\x1b[?u").0, "\x1b[?5u\x1b[?1u\x1b[?0u");
    assert_eq!(asked("\x1b[=1;1u\x1b[=4;2u\x1b[?u\x1b[=1;3u\x1b[?u\x1b[=0u\x1b[?u").0, "\x1b[?5u\x1b[?4u\x1b[?0u");
    assert_eq!(asked("\x1b[>1u\x1b[?1049h\x1b[?u\x1b[>4u\x1b[?u\x1b[?1049l\x1b[?u").0, "\x1b[?0u\x1b[?4u\x1b[?1u");
    // All five of the protocol's flags can be in force, and no number past them.
    assert_eq!(asked("\x1b[>31u\x1b[?u\x1b[=256u\x1b[?u\x1b[=127u\x1b[?u").0, "\x1b[?31u\x1b[?0u\x1b[?31u");
    // A flag the terminal that sends the keys lacks is not in force, and the program is told so.
    let mut lacking = Term::new(4, 1);
    lacking.told().keyboard = 5;
    lacking.feed(b"\x1b[>31u\x1b[?u\x1b[=2;2u\x1b[?u");
    assert_eq!(lacking.take(0).reply, b"\x1b[?5u\x1b[?5u");
    // What a key's sequence says beyond its name: which key, pressed, held or let go, and
    // whether it is a modifier by itself. Bytes in the old form say none of it.
    let event = |raw: &[u8]| crate::keys::next(raw, true, false).and_then(|(.., event)| event);
    assert_eq!(event(b"\x1b[97;1:3u"), Some(((97, b'u'), None, 3, false)));
    assert_eq!(event(b"\x1b[98;5u"), Some(((98, b'u'), None, 1, false)));
    assert_eq!(event(b"\x1b[97:65;2:2;65u"), Some(((97, b'u'), Some((65, b'u')), 2, false)));
    // (A sign let go is told by its key's number and its own: pressed, it was told by its own alone.)
    assert_eq!(event(b"\x1b[49:33;2:3u").map(|key| (key.0, key.1)), event(b"!").map(|key| ((49, b'u'), Some(key.0))));
    assert_eq!(event(b"\x1b[57358u"), Some(((57358, b'u'), None, 1, true)));
    // A key with much text is one key, also when it comes in two pieces.
    let long = format!("\x1b[97;;{}", "97:".repeat(24));
    assert_eq!(next(long.as_bytes(), false, false), None);
    assert_eq!(next(format!("{long}97u").as_bytes(), false, false), Some((long.len() + 3, "a".into())));
    assert_eq!(event(b"\x1b[57441;2u"), Some(((57441, b'u'), None, 1, true)));
    assert_eq!(event(b"\x1b[1;5:3A"), Some(((1, b'A'), None, 3, false)));
    assert_eq!(
        (event(b"A"), event(b"\x1b[<0;1;1M"), event(b"\x1ba")),
        (Some(((97, b'u'), None, 1, false)), None, None)
    );
    // Text with no key (number 0, the text after it) is no key: nothing is down, nothing to let go.
    assert_eq!((event(b"\x1b[0;;229u"), next(b"\x1b[0;;229u", true, false)), (None, Some((9, String::new()))));
    // Going back takes a count: none given is one, 0 is none.
    assert_eq!(asked("\x1b[=4u\x1b[>1u\x1b[<0u\x1b[?u\x1b[<u\x1b[?u").0, "\x1b[?1u\x1b[?4u");
    // What is kept aside is bounded at 16: of 17, the oldest (4) is gone, and the 16 come back in turn.
    let deep = format!("\x1b[=4u{}{}\x1b[?u\x1b[<u\x1b[?u", "\x1b[>1u".repeat(17), "\x1b[<u".repeat(16));
    assert_eq!(asked(&deep).0, "\x1b[?1u\x1b[?0u");
    // A cursor saved and put back the old way (`CSI s`, `CSI u`) is another matter and stays one.
    assert_eq!(asked("\x1b[>1u\x1b[s\x1b[u\x1b[?u").0, "\x1b[?1u");
    // The flags in force are a word among the modes, and a reader of those words has them.
    for screen in ["", "\x1b[?1049h"] {
        let words = asked(&format!("{screen}\x1b[>5u")).1;
        let mut reader = crate::term::Screen::default();
        files::parse_modes(&mut reader, &words);
        assert!(words.ends_with("keys5") && reader.keys() == 5, "{words}");
    }
    assert!(!asked("x").1.contains("keys"));
    // The viewer still finds its keys in that protocol's forms, and tells apart what the old forms cannot.
    let name = |bytes: &str| next(bytes.as_bytes(), true, false).map(|(length, name)| (length == bytes.len(), name));
    for (bytes, key) in [
        ("\x1b[98;5u", "C-b"),
        ("\x1b[105;5u", "C-i"),
        ("\t", "tab"),
        ("\x1b[13;2u", "S-enter"),
        ("\r", "enter"),
        ("\x1b[27u", "esc"),
        ("\x1b[97;3u", "M-a"),
        ("\x1b[97:65;6u", "C-S-a"),
        ("\x1b[65;6u", "C-S-a"),
        ("\x1b[118;1:1u", "v"),
        ("\x1b[1;5:1A", "C-up"),
        ("\x1b[3;133~", "C-delete"),
        ("\x1b[97;9u", ""),
        ("\x1b[57441u", ""),
        ("\x1b[?5u", "keyboard:5"),
        // A key has one name, in the old form and in the new: held keys in one order, a sign
        // that needs Shift by the sign, a letter of another alphabet by the key it shares with
        // ours, the keypad's keys as the keys they double.
        ("\x1b\x02", "C-M-b"),
        ("\x1b[98;7u", "C-M-b"),
        ("\x1b#", "M-#"),
        ("\x1b[51:35;4u", "M-#"),
        ("\x1bA", "M-S-a"),
        ("\x1b[97:65;4u", "M-S-a"),
        ("\x1b[1080::98;5u", "C-b"),
        // (And by itself where that key is no letter of ours: é on the key that is our `2`.)
        ("\u{e9}", "\u{e9}"),
        ("\x1b[233::50u", "\u{e9}"),
        ("\u{c9}", "S-\u{e9}"),
        ("\x1b[233:201:50;2u", "S-\u{e9}"),
        ("\x1b[5~", "pgup"),
        ("\x1b[57421u", "pgup"),
        ("\x1b[57399u", "0"),
        ("\x1b[57414u", "enter"),
    ] {
        assert_eq!(name(bytes), Some((true, key.to_string())), "{bytes:?}");
    }
}

#[test]
fn a_picture_is_passed_on_unread_only_if_its_own_pixels_are_square_and_all_in_its_tile() {
    // A picture's file, and what is drawn of it where its tile has so many pixels for it (rows
    // of its pixels above the tile, then the tile's width and height from its corner on).
    let file = std::env::temp_dir().join(format!("fux-part-{}.sixel", std::process::id()));
    let part = |picture: &str, room: [usize; 3]| {
        std::fs::write(&file, format!("\x1bP{picture}\x1b\\")).unwrap();
        crate::view::part(&(file.clone(), None, room))
    };
    // 80 pixels wide: its own bytes where the tile has 80 pixels, whatever cells a pane counted
    // it in; 64 of them, written anew, where the tile has 64. And so with its height.
    let red = "0;1q\"1;1;80;1#0;2;100;0;0!80@";
    assert_eq!(part(red, [0, 80, 20]), Ok(Some((red.to_string(), 80, 1))));
    let cut = part(red, [0, 64, 20]).unwrap().unwrap();
    assert!(cut.0.starts_with("0;1q\"1;1;64;1#") && (cut.1, cut.2) == (64, 1), "{cut:?}");
    let high = part("0;1q\"1;1;1;20~-~-~-A", [0, 80, 12]).unwrap().unwrap();
    assert!(high.0.starts_with("0;1q\"1;1;1;12#") && (high.1, high.2) == (1, 12), "{high:?}");
    // One that asks for pixels twice as high as wide is written anew with square ones.
    let tall = part("0;1q\"2;1;10;12!10~-!10~", [0, 80, 20]).unwrap().unwrap();
    assert!(tall.0.starts_with("0;1q\"1;1;10;12#"), "{tall:?}");
    // Nothing of it in the tile, or not one whole picture: nothing is drawn, and nothing said.
    assert_eq!(part(red, [20, 80, 20]), Ok(None));
    assert_eq!(part("0;1q~\x1b\\\x1bPq~", [0, 80, 20]), Ok(None));
    // No pixels to count in, or a file over a picture's bound: not drawn, and why is said.
    assert!(part(red, [0, 0, 0]).is_err_and(|why| why.contains("pixels a cell has")));
    let long = format!("q~{}", "$".repeat(crate::term::PICTURE_MAX + 5));
    assert!(part(&long, [0, 80, 20]).is_err_and(|why| why.contains("4 MiB")));
    std::fs::remove_file(&file).unwrap();
}

#[test]
fn a_picture_is_read_only_if_its_name_is_a_file_and_no_link_to_one() {
    // A pane's folder may be written by another than its holder, so a picture's name is no proof
    // of the file. A link under it (to a picture or to anything else), a FIFO and a folder are no
    // picture: none is read, none is waited at, and none is asked when it was written.
    let dir = std::env::temp_dir().join(format!("fux-named-{}", std::process::id()));
    // (What a run that failed left under that name, of a process of this number, goes first.)
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let red = "0;1q\"1;1;80;1#0;2;100;0;0!80@";
    std::fs::write(dir.join("real.sixel"), format!("\x1bP{red}\x1b\\")).unwrap();
    std::os::unix::fs::symlink(dir.join("real.sixel"), dir.join("link.sixel")).unwrap();
    std::fs::create_dir(dir.join("folder.sixel")).unwrap();
    drop(crate::sys::fifo(dir.join("fifo.sixel")).unwrap());
    let part = |name: &str| crate::view::part(&(dir.join(name), None, [0, 80, 20]));
    assert_eq!(part("real.sixel"), Ok(Some((red.to_string(), 80, 1))));
    assert!(crate::view::written(&dir.join("real.sixel")).is_some());
    for name in ["link.sixel", "folder.sixel", "fifo.sixel", "none.sixel"] {
        assert_eq!(part(name), Ok(None), "{name}");
        assert_eq!(crate::view::written(&dir.join(name)), None, "{name}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A sixel picture written from a rule (the colour of each pixel, or none), plainly: for each
/// band each colour's row, every run with its count. And its pixels, as the rule gives them.

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
    // A pane split off one that was given a share is as large as that one, not a sliver beside it.
    let mut sized = parse("* row(a:43 b:51) focus=a\n  c focus=c\n").expect("a layout");
    sized.edit(&["split", "row", "c", "beside", "a"]).expect("a split");
    assert_eq!(sized.print(), "* row(a:43 c:43 b:51) focus=c\n");
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
    // A pane of the shown tab that an earlier line has too stays with the earlier line; the mark stays on its own.
    let mut twice = parse("  a focus=a\n- b focus=b\n* row(a c) focus=c\n").expect("a layout");
    twice.settle(vec!["a".into(), "b".into(), "c".into()]);
    assert_eq!(twice.print(), "  a focus=a\n- b focus=b\n* c focus=c\n");
    // A pane that leaves for a tab of its own leaves its old tab marked, and `tab -` goes back.
    let mut broken = parse("* row(a b) focus=b\n").expect("a layout");
    broken.edit(&["break"]).expect("break");
    assert_eq!(broken.print(), "- a focus=a\n* b focus=b\n");
    broken.edit(&["tab", "-"]).expect("tab");
    assert_eq!(broken.print(), "* a focus=a\n- b focus=b\n");
}

#[test]
fn when_the_tab_shown_goes_with_no_tab_shown_before_it_the_tab_now_in_its_place_is_shown() {
    let after = |text: &str, existing: &[&str]| {
        let mut layout = parse(text).expect("a layout");
        layout.settle(existing.iter().map(|name| name.to_string()).collect());
        layout.print()
    };
    let four = "  1 focus=1\n* 2 focus=2\n  3 focus=3\n  4 focus=4\n";
    // Only the shown tab goes: the tab after it is in its place.
    assert_eq!(after(four, &["1", "3", "4"]), "  1 focus=1\n* 3 focus=3\n  4 focus=4\n");
    // An earlier tab goes with it: its place is counted among the tabs that stay, not among those that were.
    assert_eq!(after(four, &["3", "4"]), "* 3 focus=3\n  4 focus=4\n");
    // With a tab shown before it, that one is shown, wherever it is now.
    let marked = "  1 focus=1\n* 2 focus=2\n  3 focus=3\n- 4 focus=4\n";
    assert_eq!(after(marked, &["3", "4"]), "  3 focus=3\n* 4 focus=4\n");
    // The last tab is shown and goes: the tab that is last now; a pane with no tab yet gets the place.
    let last = "  1 focus=1\n  2 focus=2\n* 3 focus=3\n";
    assert_eq!(after(last, &["1", "2"]), "  1 focus=1\n* 2 focus=2\n");
    assert_eq!(after(last, &["2", "9"]), "  2 focus=2\n* 9 focus=9\n");
}

#[test]
fn keys_have_the_names_of_their_files() {
    let name = |bytes: &[u8]| next(bytes, true, false);
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
        // A capital is Shift and the small letter in the old form and in the new, whatever the alphabet.
        ("É".as_bytes(), "S-é"),
        (b"\x1b[233;2u", "S-é"),
        ("\x1bÉ".as_bytes(), "M-S-é"),
        (b"\x1b[233;4u", "M-S-é"),
        // A letter of another alphabet is named by the key it shares with ours, if the terminal tells
        // it; a sign outside ASCII is no letter, and is itself in the new form as in the old.
        (b"\x1b[1092::97u", "a"),
        ("§".as_bytes(), "§"),
        (b"\x1b[167::96u", "§"),
        (b"\x1b[200~", "paste-start"),
        (b"\x1b[?62;4c", "terminal:?62;4"),
    ] {
        assert_eq!(name(bytes), Some((bytes.len(), expected.to_string())));
    }
    assert_eq!(next(b"\x1b[20", true, false), None, "half a sequence waits for the rest, however long");
    // Alt with a character of several bytes, of which not all have come yet, waits for the rest
    // as the character by itself does.
    assert_eq!(next(b"\x1b\xc3", false, false), None);
    assert_eq!(next("\x1bé".as_bytes(), false, false), Some((3, "M-é".to_string())));
    // And so when nothing more came for a moment: half a character is no key, however late its rest
    // is. A byte that is none of its ends the wait, and is a key of its own.
    for half in [&b"\xc3"[..], b"\xe2\x82", b"\xf0\x9f\x98", b"\x1b\xc3"] {
        assert_eq!(next(half, true, false), None, "{half:?}");
    }
    assert_eq!(
        (next(b"\xe2a", true, false), next(b"\xff", true, false)),
        (Some((1, String::new())), Some((1, String::new())))
    );
}

#[test]
fn a_run_of_esc_bytes_is_the_escape_key_again_and_again_and_no_key_of_a_long_name() {
    // Many ESC at once (a paste on a terminal that does not mark pastes): each is the key `esc`,
    // told at once, until two are left, which are Alt with Escape if nothing more comes.
    let run = [0x1b; 2000];
    for whole in [true, false] {
        assert_eq!(next(&run, whole, false), Some((1, "esc".to_string())));
    }
    let (mut at, mut longest) = (0, 0);
    while let Some((length, name)) = next(&run[at..], true, false) {
        (at, longest) = (at + length, longest.max(name.len()));
    }
    assert_eq!((at, longest), (run.len(), "M-esc".len()));
    assert_eq!((next(b"\x1b\x1b", true, false), next(b"\x1b\x1b", false, false)), (Some((2, "M-esc".into())), None));
    // Alt with a key that is a sequence is still that.
    assert_eq!(next(b"\x1b\x1b[A", false, false), Some((4, "M-up".into())));
    assert_eq!(next(b"\x1b\x1b\x1b[A", false, false), Some((1, "esc".into())));
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
    // A share asked to shrink never grows, and one asked to grow never shrinks: a pane at a hundredth
    // stays there when it is made narrower, and is not brought up to the least a step leaves.
    let mut thin = parse("* row(a:1 b:99) focus=a\n").expect("thin");
    thin.edit(&["size", "row", "-5"]).expect("size");
    assert_eq!(thin.print(), "* row(a b:99) focus=a\n");
    thin.edit(&["size", "row", "+5"]).expect("size");
    thin.edit(&["size", "row", "-3"]).expect("size");
    assert_eq!(thin.print(), "* row(a:5 b:94) focus=a\n");
    // Nor does its tile: a share under a hundredth of its row is left as it is (made 1 of 100, it
    // had five columns of 500 where it had one).
    let mut sliver = parse("* row(a:1 b:9999) focus=a\n").expect("sliver");
    let area = Rect { x: 0, y: 0, w: 500, h: 3 };
    let before = sliver.tabs.first().expect("a tab").tiles(area).first().expect("a tile").1.w;
    sliver.edit(&["size", "row", "-5"]).expect("size");
    assert_eq!((before, sliver.tabs.first().expect("a tab").tiles(area).first().expect("a tile").1.w), (1, 1));
    // The focus is on d by now; the new pane still goes beside the pane it was asked for.
    let mut layout = parse("* row(a b) focus=b\n  d focus=d\n  n focus=n\n").expect("a layout");
    layout.edit(&["split", "col", "n", "beside", "a"]).expect("split");
    assert_eq!(layout.print(), "* row(col(a n) b) focus=n\n  d focus=d\n");
    assert!(layout.edit(&["split", "col", "n", "beside", "nobody"]).is_err());
    // With no anchor named the focused pane is the anchor: a pane is put beside itself neither way.
    let mut own = parse("* row(a b) focus=a\n").expect("a layout");
    assert!(own.edit(&["split", "row", "a"]).is_err() && own.edit(&["split", "row", "a", "beside", "a"]).is_err());
    assert_eq!(own.print(), "* row(a b) focus=a\n");
    // In a view with room for one of three, the one drawn is the one that has the keys.
    let crowded = parse("* row(a b c) focus=c\n").expect("a layout");
    let tiles = crowded.tabs.first().expect("a tab").tiles(Rect { x: 0, y: 0, w: 1, h: 3 });
    assert_eq!(tiles.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["c"]);
}

#[test]
fn arrange_puts_a_tabs_panes_in_a_row_a_column_a_grid_or_one_beside_the_rest() {
    let arranged = |text: &str, way: &str| {
        let mut layout = parse(text).expect("a layout");
        layout.edit(&["arrange", way]).expect("arrange");
        layout.print()
    };
    let five = "* row(a col(b:3 c) d e) focus=c\n";
    assert_eq!(arranged(five, "row"), "* row(a b c d e) focus=c\n");
    assert_eq!(arranged(five, "col"), "* col(a b c d e) focus=c\n");
    assert_eq!(arranged(five, "grid"), "* col(row(a b c) row(d e)) focus=c\n");
    assert_eq!(arranged(five, "main"), "* row(a:2 col(b c d e)) focus=c\n");
    // A grid is as many across as it takes to be no taller than wide; a row of one is that pane.
    assert_eq!(arranged("* row(a b c d) focus=a\n", "grid"), "* col(row(a b) row(c d)) focus=a\n");
    assert_eq!(
        arranged("* col(a b c d e f g h i j) focus=a\n", "grid"),
        "* col(row(a b c d) row(e f g h) row(i j)) focus=a\n"
    );
    assert_eq!(arranged("* col(a b c) focus=a\n", "grid"), "* col(row(a b) c) focus=a\n");
    assert_eq!(arranged("* col(a b) focus=b\n", "main"), "* row(a:2 b) focus=b\n");
    for way in ["row", "col", "grid", "main"] {
        assert_eq!(arranged("* a focus=a\n", way), "* a focus=a\n");
    }
}

// `changes`: what the holder says its rounds changed (`src/pane.rs`).
use crate::pane::{CHANGES_MAX, Change, Changes};
use std::collections::BTreeMap;

/// A folder as a reader of `changes` has it: what each name is, and a file's bytes.
#[derive(Clone, Debug, PartialEq)]
enum Thing {
    File(Vec<u8>),
    Folder,
    Link(String),
    Fifo,
}
type Folder = BTreeMap<String, Thing>;

fn under(name: &str, folder: &str) -> bool {
    name.strip_prefix(folder).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// What a reader does: `copy` is the folder as an earlier round left it, `lines` what `changes` says
/// the rounds after that one did, `pane` the folder as it is. Each line in its order: a `put` leaves
/// none of the copy's bytes, an `append` those before AT, a `move` gives the copy's file its other
/// name, and the rest make and remove. Then each file a `put` or an `append` named, under the name it
/// has by then, gets from the pane's file the bytes after those it kept: and no other file is read.
fn replay(mut copy: Folder, lines: &[(u64, Change)], pane: &Folder) -> Folder {
    // The files to read from, and how long the last `append` of each said it is.
    let mut lacking: BTreeMap<String, Option<u64>> = BTreeMap::new();
    for (_, change) in lines {
        match change {
            Change::Put(name) => {
                copy.insert(name.clone(), Thing::File(Vec::new()));
                lacking.insert(name.clone(), None);
            }
            Change::Append(name, at, len) => {
                let Thing::File(bytes) = copy.entry(name.clone()).or_insert(Thing::File(Vec::new())) else {
                    panic!("{name} is added to, and is no file in the copy")
                };
                bytes.truncate(*at as usize);
                lacking.insert(name.clone(), Some(*len));
            }
            Change::Move(from, to) => {
                let moved = copy.remove(from).unwrap_or_else(|| panic!("{from} is moved, and the copy has none"));
                copy.insert(to.clone(), moved);
                lacking.remove(to);
                lacking.remove(from).into_iter().for_each(|len| _ = lacking.insert(to.clone(), len));
            }
            Change::Folder(name) => _ = copy.insert(name.clone(), Thing::Folder),
            Change::Link(name, to) => _ = copy.insert(name.clone(), Thing::Link(to.clone())),
            Change::Fifo(name) => _ = copy.insert(name.clone(), Thing::Fifo),
            Change::Gone(name) => {
                copy.retain(|other, _| !under(other, name));
                lacking.retain(|other, _| !under(other, name));
            }
        }
    }
    for (name, len) in lacking {
        let (Some(Thing::File(kept)), Some(Thing::File(file))) = (copy.get_mut(&name), pane.get(&name)) else {
            panic!("{name} is to be read, and is no file at both ends")
        };
        assert!(len.is_none_or(|len| len == file.len() as u64), "{name} is {} long, not {len:?}", file.len());
        kept.extend_from_slice(&file[kept.len()..]);
    }
    copy
}

#[test]
fn a_later_change_takes_from_changes_what_it_makes_of_no_account_and_nothing_past_a_move() {
    // One line of each kind for the name `a`, an `append` that adds and one that only cuts: every one
    // after every other. `x`: the later one (the row) takes the earlier one (the column) away.
    let a = || "a".to_string();
    let kinds = [
        Change::Put(a()),
        Change::Append(a(), 4, 9),
        Change::Append(a(), 6, 6),
        Change::Move("b".into(), a()),
        Change::Folder(a()),
        Change::Link(a(), "main".into()),
        Change::Fifo(a()),
        Change::Gone(a()),
    ];
    let table = [
        // put add cut move folder link fifo gone
        "x x x . . . . .", // put: all of the file is new
        ". x . . . . . .", // append that adds: the cut before it is to less than the file is long now
        ". x x . . . . .", // append that cuts: to no more than the cut before it
        ". . . . . . . .", // move
        ". . . . . . . .", // folder
        ". . . . . x . .", // link
        ". . . . . . . .", // fifo
        "x x x . x x x x", // gone: and never what was said before a move
    ];
    for (later, row) in kinds.iter().zip(table) {
        for (earlier, goes) in kinds.iter().zip(row.split(' ')) {
            let folded = |between: Option<&Change>| {
                let mut changes = Changes::default();
                changes.fold(2, earlier.clone());
                between.into_iter().for_each(|change| changes.fold(4, change.clone()));
                changes.fold(6, later.clone());
                changes.list
            };
            let (first, last) = ((2, earlier.clone()), (6, later.clone()));
            let expected = if goes == "x" { vec![last.clone()] } else { vec![first.clone(), last.clone()] };
            assert_eq!(folded(None), expected, "{earlier} then {later}");
            // A move of another name between the two changes nothing; one of this name, to it or
            // from it, and neither goes: the earlier line is of the file that has the other name now.
            let other = Change::Move("c".into(), "d".into());
            assert_eq!(folded(Some(&other)).len(), expected.len() + 1, "{earlier} then {later}");
            for moved in [Change::Move("c".into(), a()), Change::Move(a(), "c".into())] {
                assert_eq!(folded(Some(&moved)), [first.clone(), (4, moved), last.clone()], "{earlier} then {later}");
            }
        }
    }
    // The cuts of a file that stay are each to less than the one after it, and what was added stays once.
    let mut changes = Changes::default();
    let appends = [(2, 3, 3), (2, 3, 10), (4, 10, 20), (6, 8, 8), (6, 8, 12), (8, 12, 30)];
    appends.iter().for_each(|&(number, at, len)| changes.fold(number, Change::Append(a(), at, len)));
    assert_eq!(changes.to_string(), "0\n2 append a 3 3\n6 append a 8 8\n8 append a 12 30\n");
    // (A reader that has the folder of round 4 keeps 8 bytes; one of round 6 or later, all it has.)
    assert_eq!(changes.after(4).map(<[_]>::len), Some(2));
    changes.fold(10, Change::Append(a(), 2, 2));
    assert_eq!(changes.to_string(), "0\n10 append a 2 2\n");
    // What is gone takes what was in it, and no name that only begins as its name does.
    let mut changes = Changes::default();
    ["alt", "alt/screen", "alternate"].iter().for_each(|name| changes.fold(2, Change::Put(name.to_string())));
    changes.fold(4, Change::Gone("alt".into()));
    assert_eq!(changes.to_string(), "0\n2 put alternate\n4 gone alt\n");
    // Past the bound the oldest rounds go, each whole, and the file says which was the last of them:
    // a reader that has an earlier round is told nothing, any other all it was told before.
    let mut changes = Changes::default();
    (0..CHANGES_MAX + 3).for_each(|n| changes.fold(2 + 2 * (n as u64 / 2), Change::Gone(format!("pic-{n}.sixel"))));
    assert_eq!(
        (changes.since, changes.list.len(), changes.list.first().map(|line| line.0)),
        (4, CHANGES_MAX - 1, Some(6))
    );
    assert_eq!((changes.after(2), changes.after(4).map(<[_]>::len)), (None, Some(CHANGES_MAX - 1)));
    assert_eq!(Changes::parse(&changes.to_string()), Some(changes.clone()));
    // (A round of more lines than the bound is one nobody is told: all of it goes.)
    (0..=CHANGES_MAX).for_each(|n| changes.fold(4000, Change::Put(format!("p{n}"))));
    assert_eq!((changes.since, changes.list.len()), (4000, 0));
}

#[test]
fn a_reader_of_changes_from_any_earlier_round_has_the_folder_as_it_is() {
    // A folder that a holder's changes are made to at random, each said as the holder says it, a few a
    // round: files put, added to and cut, put aside under another name, folders with files in them
    // made and gone, a link turned, pictures that come and go (their lines stay, so the bound is
    // passed). After each round, a reader that has the folder as some earlier round left it replays
    // what `changes` says since and has the folder as it is, or is told that it is not told.
    let mut seed = 7u64;
    let mut pick = |below: usize| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as usize % below
    };
    let (mut pane, mut changes, mut pictures) = (Folder::new(), Changes::default(), 0);
    let mut rounds = vec![(0u64, pane.clone())];
    let mut untold = 0;
    for round in 1..2000u64 {
        for _ in 0..pick(5) {
            let name = ["history", "history.styled", "main/screen", "alt/screen", "cursor"][pick(5)].to_string();
            let bytes: Vec<u8> = (0..pick(6)).map(|_| pick(256) as u8).collect();
            let folder = name.split_once('/').map(|(folder, _)| folder.to_string());
            let file = match pane.get(&name) {
                Some(Thing::File(file)) => Some(file.len()),
                _ => None,
            };
            let said = match (pick(10), file, folder) {
                // A file's folder is made before the file; and goes with all that is in it.
                (_, None, Some(folder)) if !pane.contains_key(&folder) => {
                    pane.insert(folder.clone(), Thing::Folder);
                    vec![Change::Folder(folder)]
                }
                (0, _, Some(folder)) => {
                    pane.retain(|name, _| !under(name, &folder));
                    vec![Change::Gone(folder)]
                }
                (1 | 2, Some(len), _) if !bytes.is_empty() => {
                    pane.insert(name.clone(), Thing::File([&file_of(&pane, &name)[..], &bytes[..]].concat()));
                    vec![Change::Append(name, len as u64, (len + bytes.len()) as u64)]
                }
                (3, Some(len @ 1..), _) => {
                    let to = pick(len);
                    pane.insert(name.clone(), Thing::File(file_of(&pane, &name)[..to].to_vec()));
                    vec![Change::Append(name, to as u64, to as u64)]
                }
                (4, Some(_), None) => {
                    let moved = pane.remove(&name).expect("the file");
                    pane.insert(format!("{name}.1"), moved);
                    pane.insert(name.clone(), Thing::File(bytes));
                    vec![Change::Move(name.clone(), format!("{name}.1")), Change::Put(name)]
                }
                (5, Some(_), _) => {
                    pane.remove(&name);
                    vec![Change::Gone(name)]
                }
                (6, ..) => {
                    let to = ["main", "alt"][pick(2)].to_string();
                    pane.insert("shown".into(), Thing::Link(to.clone()));
                    vec![Change::Link("shown".into(), to)]
                }
                (7, ..) if !pane.contains_key("in") => {
                    pane.insert("in".into(), Thing::Fifo);
                    vec![Change::Fifo("in".into())]
                }
                (8 | 9, ..) => {
                    pictures += 1;
                    let (old, new) = (format!("pic-{}.sixel", pictures - 1), format!("pic-{pictures}.sixel"));
                    pane.insert(new.clone(), Thing::File(bytes));
                    let gone = pane.remove(&old).map(|_| Change::Gone(old));
                    std::iter::once(Change::Put(new)).chain(gone).collect()
                }
                _ => {
                    pane.insert(name.clone(), Thing::File(bytes));
                    vec![Change::Put(name)]
                }
            };
            said.into_iter().for_each(|change| changes.fold(2 * round, change));
        }
        assert_eq!(Changes::parse(&changes.to_string()).as_ref(), Some(&changes));
        // The round before, and three of any before that.
        for at in [rounds.len() - 1, pick(rounds.len()), pick(rounds.len()), pick(rounds.len())] {
            let (number, copy) = &rounds[at];
            match changes.after(*number) {
                Some(lines) => {
                    assert_eq!(replay(copy.clone(), lines, &pane), pane, "from round {number} to {}", 2 * round)
                }
                None => untold += usize::from(*number < changes.since),
            }
        }
        rounds.push((2 * round, pane.clone()));
    }
    // (The bound was passed, and readers behind it were told so.)
    assert!(changes.since > 0 && untold > 100 && changes.list.len() == CHANGES_MAX, "{} {untold}", changes.since);
}

fn file_of(folder: &Folder, name: &str) -> Vec<u8> {
    match folder.get(name) {
        Some(Thing::File(bytes)) => bytes.clone(),
        _ => Vec::new(),
    }
}

#[test]
fn a_changes_that_is_not_what_a_holder_writes_to_the_byte_is_none_to_a_reader() {
    let text = "0\n2 fifo in\n2 put main/screen\n2 folder main\n2 link shown main\n4 append history 5 5\n\
        4 append history 5 90\n4 move history history.1\n6 gone alt\n";
    assert_eq!(Changes::parse(text).map(|changes| changes.to_string()).as_deref(), Some(text));
    assert_eq!(Changes::parse("6\n"), Some(Changes { since: 6, list: Vec::new() }));
    let long = |lines: usize| "0\n".to_string() + &"2 gone a\n".repeat(lines);
    assert!(Changes::parse(&long(CHANGES_MAX)).is_some() && Changes::parse(&long(CHANGES_MAX + 1)).is_none());
    let wrong = [
        ("", "nothing"),
        ("0", "a line that does not end"),
        ("0\n2 put a", "a line that does not end"),
        ("0\r\n", "another line end"),
        ("00\n", "a number spelled another way"),
        ("0\n+2 put a\n", "a number spelled another way"),
        ("1\n", "an odd round"),
        ("0\n3 put a\n", "an odd round"),
        ("2\n2 put a\n", "a round that is not after the first line's"),
        ("0\n4 put a\n2 put b\n", "rounds out of order"),
        ("0\n2 take a\n", "no verb of the seven"),
        ("0\n2 put\n", "no name"),
        ("0\n2 put a b\n", "a word too many"),
        ("0\n2  put a\n", "a blank too many"),
        ("0\n2 put a\n\n", "an empty line"),
        ("0\n2 put .a\n", "a name that begins with a dot"),
        ("0\n2 put a/../b\n", "a name that leaves the folder"),
        ("0\n2 put /a\n", "a path from the root"),
        ("0\n2 put a/\n", "a name that ends in a /"),
        ("0\n2 put caf\u{e9}\n", "a name of other letters"),
        ("0\n2 move a a\n", "a move to the same name"),
        ("0\n2 link shown main/screen\n", "a link that leads into a folder"),
        ("0\n2 link shown ..\n", "a link that leads out"),
        ("0\n2 append a 5 4\n", "more bytes kept than there are"),
        ("0\n2 append a 5\n", "no length"),
        ("0\n2 append a -1 4\n", "a number that is none"),
    ];
    for (text, why) in wrong {
        assert_eq!(Changes::parse(text), None, "{why}: {text:?}");
    }
}
