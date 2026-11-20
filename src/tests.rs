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
    // `10` with two parts asks for the foreground and then the background; a part that would set
    // a colour is passed over; the cursor's colour (`11`'s second part) is not told; a line that
    // is no colour (not six hex digits) is no answer.
    told.feed(b"\x1b]10;?;?\x07\x1b]10;#ffffff;?\x07\x1b]11;#ffffff;?\x07");
    let both = "\x1b]10;rgb:d0d0/d0d0/d0d0\x07\x1b]11;rgb:1c1c/1c1c/1c1c\x07\x1b]11;rgb:1c1c/1c1c/1c1c\x07";
    assert_eq!(String::from_utf8(told.take().reply).unwrap(), both);
    told.colours = "background #+12345\nforeground #d0d0d\n7 cc0000\n".into();
    told.feed(b"\x1b]11;?\x07\x1b]10;?\x07\x1b]4;7;?\x07");
    assert!(told.take().reply.is_empty());
    // A person's file: blanks do not matter, capitals neither, and of two lines for a colour the last counts.
    told.colours = "background #000000\n  background\t#FDF6E3  # paper\r\n".into();
    told.feed(b"\x1b]11;?\x07");
    assert_eq!(String::from_utf8(told.take().reply).unwrap(), "\x1b]11;rgb:fdfd/f6f6/e3e3\x07");
    told.colours = "foreground #d0d0d0\nbackground #1c1c1c\n".into();
    // Setting a colour is taken and changes nothing; a reset keeps what was told.
    told.feed(b"\x1b]11;#ffffff\x07\x1b]4;1;rgb:00/00/00\x07\x1b]104\x07\x1b]111\x07\x1bc\x1b]11;?\x07x");
    assert_eq!(String::from_utf8(told.take().reply).unwrap(), "\x1b]11;rgb:1c1c/1c1c/1c1c\x07");
    assert_eq!(told.plain(), "x\n\n");
    // The viewer reads its terminal's answers as answers, not as keys.
    use crate::keys::next;
    assert_eq!(
        next(b"\x1b]11;rgb:1c1c/1c1c/1c1c\x1b\\x", false, true),
        Some((25, "answer:11;rgb:1c1c/1c1c/1c1c".into()))
    );
    assert_eq!(next(b"\x1b]4;1;rgb:cc/00/00\x07", false, true), Some((19, "answer:4;1;rgb:cc/00/00".into())));
    // An answer that comes in pieces waits for its end; when nothing was asked, Alt-] is a key at once.
    assert_eq!((next(b"\x1b]11;rgb:1c", false, true), next(b"\x1b]", false, true)), (None, None));
    assert_eq!(next(b"\x1b]x\x07", false, false), Some((2, "M-]".into())));
}

#[test]
fn a_program_says_how_it_wants_keys_told_and_is_answered_no_more_than_the_terminal_takes() {
    // With no terminal known to take the protocol, the answer is 0 whatever was asked, and a
    // program that was told so is not given the flags later, when a terminal that takes them
    // comes: what it was told stays true until it asks again.
    let mut early = term(9, 2, "\x1b[>1u\x1b[?u");
    early.keyboard = crate::term::KEYS;
    early.feed(b"\x1b[?u");
    assert_eq!((early.take().reply.as_slice(), early.keys()), (&b"\x1b[?0u\x1b[?0u"[..], 0));
    early.feed(b"\x1b[=1u\x1b[?u");
    early.keyboard = 0;
    assert_eq!((early.take().reply.as_slice(), early.keys()), (&b"\x1b[?1u"[..], 1));
    let asked = |bytes: &str| {
        let mut term = Term::new(9, 2);
        term.keyboard = crate::term::KEYS;
        term.feed(bytes.as_bytes());
        (String::from_utf8(term.take().reply).unwrap(), term.words())
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
    lacking.keyboard = 5;
    lacking.feed(b"\x1b[>31u\x1b[?u\x1b[=2;2u\x1b[?u");
    assert_eq!(lacking.take().reply, b"\x1b[?5u\x1b[?5u");
    // What a key's sequence says beyond its name: which key, pressed, held or let go, and
    // whether it is a modifier by itself. Bytes in the old form say none of it.
    use crate::keys::event;
    assert_eq!(event(b"\x1b[97;1:3u"), Some(((97, b'u'), None, 3, false)));
    assert_eq!(event(b"\x1b[98;5u"), Some(((98, b'u'), None, 1, false)));
    assert_eq!(event(b"\x1b[97:65;2:2;65u"), Some(((97, b'u'), Some((65, b'u')), 2, false)));
    // (A sign let go is told by its key's number and its own: pressed, it was told by its own alone.)
    assert_eq!(event(b"\x1b[49:33;2:3u").map(|key| (key.0, key.1)), event(b"!").map(|key| ((49, b'u'), Some(key.0))));
    assert_eq!(event(b"\x1b[57358u"), Some(((57358, b'u'), None, 1, true)));
    // A key with much text is one key, also when it comes in two pieces.
    let long = format!("\x1b[97;;{}", "97:".repeat(24));
    assert_eq!(crate::keys::next(long.as_bytes(), false, false), None);
    assert_eq!(crate::keys::next(format!("{long}97u").as_bytes(), false, false), Some((long.len() + 3, "a".into())));
    assert_eq!(event(b"\x1b[57441;2u"), Some(((57441, b'u'), None, 1, true)));
    assert_eq!(event(b"\x1b[1;5:3A"), Some(((1, b'A'), None, 3, false)));
    assert_eq!(
        (event(b"A"), event(b"\x1b[<0;1;1M"), event(b"\x1ba")),
        (Some(((97, b'u'), None, 1, false)), None, None)
    );
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
        let mut reader = Term::new(1, 1);
        reader.set_words(&words);
        assert!(words.ends_with("keys5") && reader.keys() == 5, "{words}");
    }
    assert!(!asked("x").1.contains("keys"));
    // The viewer still finds its keys in that protocol's forms, and tells apart what the old forms cannot.
    use crate::keys::next;
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
        ("\x1b[5~", "pgup"),
        ("\x1b[57421u", "pgup"),
        ("\x1b[57399u", "0"),
        ("\x1b[57414u", "enter"),
    ] {
        assert_eq!(name(bytes), Some((true, key.to_string())), "{bytes:?}");
    }
}

#[test]
fn a_program_says_where_it_is_what_to_notice_and_how_it_draws() {
    // The directory it is in (OSC 7), with the host it names; a path's odd bytes come as %XX.
    let mut term = term(20, 2, "\x1b]7;file://box/tmp/a%20b\x07");
    assert_eq!(term.take().cwd, Some(("box".to_string(), "/tmp/a b".to_string())));
    term.feed(b"\x1b]7;file:///home\x1b\\\x1b]7;nonsense\x07\x1b]7;file:///a%00b\x07");
    assert_eq!(term.take().cwd, Some((String::new(), "/home".to_string())));
    // A path that is no path (a control character, bytes that are no text, too long) is not
    // told: only the machine named is.
    for path in ["/a%00b".to_string(), "/a\u{fffd}".to_string(), "/".repeat(5000)] {
        term.feed(format!("\x1b]7;file://far{path}\x07").as_bytes());
        assert_eq!(term.take().cwd, Some(("far".to_string(), String::new())));
    }
    // What it wants noticed: the bell (no words), and words in three forms; each one counts.
    term.feed(b"\x07");
    assert_eq!((term.peek().notice.as_deref(), term.peek().notices), (Some(""), 1));
    // (Not a progress bar or another numbered command after `9;`, nor a question in kitty's form.)
    term.feed(b"\x1b]9;4;1;50\x07\x1b]9;9;/tmp\x07\x1b]99;i=1:p=?;\x07\x1b]99;i=1:e=1;aGk=\x07");
    assert_eq!(term.peek().notices, 1);
    term.feed(b"\x1b]9;done\x07\x1b]99;i=1;hello\x1b\\\x1b]777;notify;make;all\x01 built\x07");
    assert_eq!((term.take().notice.as_deref(), term.plain().as_str()), (Some("make: all built"), "\n\n"));
    // That it wants to be told when its pane gains the keys is a word among the modes; that it
    // is in the middle of drawing is the holder's to know. Both are told when asked.
    term.feed(b"\x1b[?1004h\x1b[?2026h\x1b[?1004$p\x1b[?2026$p");
    assert_eq!((term.words().as_str(), term.sync), ("focus", true));
    assert_eq!(term.take().reply, b"\x1b[?1004;1$y\x1b[?2026;1$y");
    term.feed(b"\x1b[?2026l\x1b[?2026h\x1b[?2026l\x1bc");
    assert!(!term.sync && term.words().is_empty());
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
    // The viewer reads its terminal's answer about a mode as an answer.
    assert_eq!(crate::keys::next(b"\x1b[?2027;2$y", false, false), Some((11, "mode:2027;2".into())));
    // And its window's gain and loss of the keys as that, not as a key.
    assert_eq!(crate::keys::next(b"\x1b[Ox", false, false), Some((3, "focus:O".into())));
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
    assert!(term.resize(4, 2) && term.resize(30, 9) && term.marks() == marks);
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
    assert!(wide.resize(12, 3) && gone.resize(30, 4) && gone.resize(5, 2));
    assert_eq!((wide.marks().as_str(), wide.wraps().as_str()), ("0 0 output\n0 7 end 3\n", ""));
    assert!(gone.marks().ends_with("1 0 output\n1 21 end 3\n"), "{}", gone.marks());
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
    // A mark that waits at a row's end stays on that side of the wrap when only the height
    // changes; and marks on rows pushed off the bottom are after what stays, in their order.
    let mut waits = crate::tests::term(4, 3, &format!("{c}abcd{d}$"));
    assert!(waits.resize(4, 4));
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
    assert!(long.resize(4, 6) && long.links() == "1 2 10 http://w/\n", "{}", long.links());
    assert!(long.resize(20, 4) && long.links() == "1 2 10 http://w/\n", "{}", long.links());
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
    assert!(aside.resize(10, 1));
    for n in 0..1200 {
        aside.feed(format!("\r{}", link(&format!("http://h/{n}"), "x")).as_bytes());
    }
    assert!(aside.resize(10, 3) && aside.links().starts_with("0 0 1 http://A/\n"), "{}", aside.links());
    // A blank that is part of a link is text to wrapping anew; a soft reset ends the link that
    // was being printed.
    let mut blank = crate::tests::term(4, 2, &format!("{}\r", link("http://s/", "X ")));
    assert!(blank.resize(5, 2) && blank.links() == "0 0 2 http://s/\n", "{}", blank.links());
    blank.feed(b"\x1b]8;;http://open/\x1b\\\x1b[!pZ");
    assert_eq!(blank.links(), "0 1 1 http://s/\n");
    // The list is no longer than a reader reads: it ends before the run that would make it so.
    let wide = "http://".to_string() + &"w".repeat(2041);
    let mut full = Term::new(2, 200);
    full.feed((0..199).map(|n| format!("\x1b[{};1H{}", n + 1, link(&wide, "x"))).collect::<String>().as_bytes());
    // (And there it ends: a shorter run after that one is not in it either.)
    full.feed(format!("\x1b[200;1H{}", link("http://s/", "x")).as_bytes());
    assert_eq!((full.links().len() <= crate::term::LINKS_LIST_MAX, full.links().lines().count()), (true, 127));
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
    named.name = "fux-next 1.2".into();
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
    assert_eq!(
        said("\x1b[2;4r\x1b[5 q\x1bP$qr\x1b\\\x1bP$q q\x1b\\\x1bP$qs\x1b\\"),
        "\x1bP1$r2;4r\x1b\\\x1bP1$r5 q\x1b\\\x1bP0$r\x1b\\"
    );
    assert_eq!(said(&format!("\x1bP+q{}\x1b\\", "41;".repeat(40))), "\x1bP0+r\x1b\\");
    // What DECRQSS says of the looks in force, fed back, gives the same looks; and the table's
    // own sequences are ones the emulator takes (the curly underline, its colour, the shape).
    for looks in ["1;2;3", "4:2;58:5:9", "4:5;58:2::1:2:3;7", "31;42", "95;107;9", "38;5;200;48;2;9;8;7;5;8", "21"] {
        let mut first = term(20, 5, &format!("\x1b[{looks}mx\x1bP$qm\x1b\\"));
        let answer = String::from_utf8_lossy(&first.take().reply).into_owned();
        let body = answer.trim_start_matches("\x1bP1$r").trim_end_matches("\x1b\\");
        let again = term(20, 5, &format!("\x1b[{body}x"));
        assert_eq!(again.styled(), first.styled(), "{looks}: {answer:?}");
    }
    let shaped = term(20, 5, "\x1b[4:3m\x1b[58:2::1:2:3mx\x1b[5 q\x1b[0 q");
    assert!(
        shaped.styled().starts_with("\x1b[0;4m\x1b[4:3;58:2::1:2:3mx") && shaped.cursor_shape == 0,
        "{:?}",
        shaped.styled()
    );
    // The README's table is this table, row for row, and the agents' page lists the same names.
    let (readme, page) = (include_str!("../README.md"), include_str!("help-agents.txt"));
    let rows: Vec<String> = crate::term::CAPS
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
    let names: Vec<&str> = crate::term::CAPS.iter().map(|(name, _)| *name).collect();
    assert!(
        page.split_whitespace().collect::<Vec<_>>().join(" ").contains(&format!("{}.", names.join(" "))),
        "the agents' page"
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
    // A pane that leaves for a tab of its own leaves its old tab marked, and `tab -` goes back.
    let mut broken = parse("* row(a b) focus=b\n").expect("a layout");
    broken.edit(&["break"]).expect("break");
    assert_eq!(broken.print(), "- a focus=a\n* b focus=b\n");
    broken.edit(&["tab", "-"]).expect("tab");
    assert_eq!(broken.print(), "* a focus=a\n- b focus=b\n");
}

#[test]
fn keys_have_the_names_of_their_files() {
    let name = |bytes: &[u8]| crate::keys::next(bytes, true, false);
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
    assert_eq!(crate::keys::next(b"\x1b[20", true, false), None, "half a sequence waits for the rest, however long");
}

#[test]
fn the_words_of_how_input_is_sent_read_back_and_wrapped_rows_and_lines_gone_are_told() {
    let mut term = Term::new(12, 4);
    term.feed("\x1b[?1h\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[4 q\x1b[31m漢x\x1b[m e\u{301}\x1b[2;3H".as_bytes());
    assert_eq!(term.words(), "appcursor paste mouse1002 mousesgr");
    let mut back = Term::new(12, 4);
    back.set_words(&term.words());
    assert_eq!(back.words(), term.words());
    // The styled rows, fed row by row, are the same screen.
    for (row, line) in term.styled().lines().enumerate() {
        back.feed(format!("\x1b[{};1H{line}", row + 1).as_bytes());
    }
    assert_eq!((back.plain(), back.styled()), (term.plain(), term.styled()));
    // Which rows go on from the row above is what the plain text cannot say; and how many
    // whole lines have gone into history numbers the line the first row is of.
    let mut long = crate::tests::term(4, 3, "abcdefghij\r\nk");
    assert_eq!((long.plain().as_str(), long.wraps().as_str(), long.gone), ("efgh\nij\nk\n", "0 1", 0));
    long.feed(b"\r\nl\r\nm");
    assert_eq!((long.plain().as_str(), long.wraps().as_str(), long.gone), ("k\nl\nm\n", "", 1));
    // Rows a resize sent to history and took back are counted out and in again.
    let mut back = crate::tests::term(4, 4, "abcdefgh\r\nx\r\ny");
    assert!(back.resize(4, 2) && (back.plain().as_str(), back.gone) == ("x\ny\n", 1));
    assert!(back.resize(4, 4) && (back.wraps().as_str(), back.gone) == ("1", 0));
    // The form of the mouse's reports is told whether or not any report is on.
    let term = crate::tests::term(20, 2, "\x1b[?1002h\x1b[?1002$p\x1b[?1000$p\x1b[?1006$p");
    assert_eq!(String::from_utf8_lossy(&term.peek().reply), "\x1b[?1002;1$y\x1b[?1000;2$y\x1b[?1006;2$y");
    assert_eq!(crate::tests::term(20, 2, "\x1b[?1006h").words(), "mousesgr");
    // A clipboard's padding must be the padding of its length, and a number too large is no keyboard flags.
    use crate::term::base64_decode;
    assert_eq!(
        (base64_decode("b2xk"), base64_decode("AA=="), base64_decode("AA")),
        (Some(b"old".to_vec()), Some(vec![0]), Some(vec![0]))
    );
    assert_eq!((base64_decode("AA="), base64_decode("AB=="), base64_decode("b2xk=")), (None, None, None));
    let mut flags = Term::new(4, 1);
    flags.keyboard = crate::term::KEYS;
    flags.feed(b"\x1b[=65536u\x1b[?u\x1b[=65535u\x1b[?u");
    assert_eq!(flags.take().reply, b"\x1b[?0u\x1b[?31u");
    // A row that goes on in the next keeps the blank at its end, in both screens; when what a
    // line of history went on in is erased, history's line ends there and then, and the screen's
    // first row is the next line, and not listed.
    assert_eq!(crate::tests::term(5, 2, "abc  DEF").styled(), "abc  \nDEF\n");
    let mut spaced = crate::tests::term(3, 2, "ab cd");
    assert_eq!((spaced.plain().as_str(), spaced.wraps().as_str()), ("ab \ncd\n", "1"));
    spaced.feed(b"efg\x1b[1;1H\x1b[2KXYZ");
    assert_eq!((spaced.take().history.as_str(), spaced.plain().as_str()), ("ab \n", "XYZ\nfg\n"));
    assert_eq!((spaced.wraps().as_str(), spaced.top()), ("", 1));
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
