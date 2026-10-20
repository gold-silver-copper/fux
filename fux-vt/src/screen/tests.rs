use super::*;

#[test]
fn sgr_colour_parameters_name_the_sixteen_palette_colours() -> Result<(), Error> {
    let mut p = crate::Parser::with_options(Size::of(2, 10), 0, crate::Options::new())?;
    let layers = |p: &crate::Parser| {
        let a = p.screen().attributes;
        (a.foreground(), a.background())
    };
    for (first, index, selector) in [(30, 0, 38), (40, 0, 48), (90, 8, 38), (100, 8, 48)] {
        for offset in 0..8u8 {
            let n = first + u16::from(offset);
            p.process(format!("\x1b[0;{n}m").as_bytes())?;
            let colour = Color::Idx(index + offset);
            let want = match selector {
                38 => (colour, Color::Default),
                _ => (Color::Default, colour),
            };
            assert_eq!(layers(&p), want, "{n}");
        }
    }
    for n in [0, 29, 38, 39, 48, 49, 89, 98, 99, 108, u16::MAX] {
        p.process(format!("\x1b[0;{n}m").as_bytes())?;
        assert_eq!(layers(&p), (Color::Default, Color::Default), "{n}");
    }
    Ok(())
}

/// SGR's flag arms are what `Attributes::FLAGS` says: each row's number
/// sets its bit, and its other number clears it, alone.
#[test]
fn sgr_sets_and_clears_each_flag_as_its_row_says() -> Result<(), Error> {
    let mut p = crate::Parser::with_options(Size::of(2, 10), 0, crate::Options::new())?;
    for (bit, on, off, name) in Attributes::FLAGS {
        p.process(format!("\x1b[0;{on}m").as_bytes())?;
        assert_eq!(p.screen().attributes.flags, bit, "{name} on");
        p.process(format!("\x1b[0;1;2;3;7;8;9;{off}m").as_bytes())?;
        assert_eq!(p.screen().attributes.flags & bit, 0, "{name} off");
    }
    Ok(())
}

/// Recycling a slot clears only the cells before its `used` mark, so every
/// edit must keep the cells past it blank: printing, wide glyphs and
/// clusters, erasing in the pen's colours or not, inserting and deleting,
/// scrolling in and out of regions, resizing.
#[test]
fn cells_past_a_rows_used_mark_stay_blank() -> Result<(), Error> {
    let pieces: [&[u8]; 24] = [
        b"hello",
        b"\r\n",
        b"\n",
        "\u{754c}x".as_bytes(),
        "e\u{301}".as_bytes(),
        "\u{1f44d}\u{1f3fd}".as_bytes(),
        b"\x1b[41m",
        b"\x1b[m",
        b"\x1b[K",
        b"\x1b[1K",
        b"\x1b[2J",
        b"\x1b[3X",
        b"\x1b[2@",
        b"\x1b[2P",
        b"\x1b[L",
        b"\x1b[M",
        b"\x1b[2;4r",
        b"\x1b[r",
        b"\x1bM",
        b"\x1b[S",
        b"\x1b[T",
        b"\x1b[4h",
        b"\x1b[4l",
        b"\x1b[9;3H",
    ];
    let mut state = 0x5eed_u64;
    for reflow in [false, true] {
        let options = crate::Options::new().set(crate::Feature::Reflow, reflow);
        let mut p = crate::Parser::with_options(Size::of(6, 10), 4, options)?;
        for step in 0..3_000u32 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let pick = usize::try_from(state >> 59).unwrap_or(0);
            if step % 500 == 499 {
                let cols = if step % 1000 == 999 { 10 } else { 7 };
                p.resize(Size::of(6, cols))?;
            } else if let Some(piece) = pieces.get(pick % pieces.len()) {
                p.process(piece)?;
            }
            let s = p.screen();
            assert!(s.primary.blank_past_used(), "step {step}");
            assert!(s.alternate.blank_past_used(), "step {step}");
        }
    }
    Ok(())
}

/// What each grid counts its links to cost is within the bound, and never
/// short of what the links its rows and the open link have cost, whatever
/// the rows went through: printing over links, inserting and deleting
/// characters, erasing, scrolling into and out of history, both screens,
/// resizes with and without reflow, RIS, and making room when the links
/// fill their bound.
#[test]
fn the_links_counted_cover_the_rows() -> Result<(), Error> {
    let check = |p: &crate::Parser, step: &str| {
        let s = p.screen();
        for grid in [&s.primary, &s.alternate] {
            let open = match &grid.links.open {
                crate::link::Held::At(link) => Some(link),
                crate::link::Held::Pending | crate::link::Held::Refused => None,
            };
            let (held, live) = grid.links.costs(grid.held_links().chain(open));
            assert!(live <= held, "after {step}: {live} > {held}");
            assert!(held <= crate::link::LINK_BYTES, "after {step}");
        }
    };
    let long: String = std::iter::repeat_n('u', 2000).collect();
    for reflow in [false, true] {
        let options = crate::Options::new()
            .with(crate::Feature::Hyperlinks)
            .set(crate::Feature::Reflow, reflow);
        let mut p = crate::Parser::with_options(Size::of(4, 10), 6, options)?;
        let steps: [&[u8]; 12] = [
            b"\x1b]8;;a\x07abc\x1b]8;id=x;b\x07de\x1b]8;;\x07f\r\n",
            b"\x1b]8;;c\x07ghij\x1b[1;2H\x1b[2@\x1b[3P",
            b"\x1b[1;1Hx\x1b]8;;d\x07yz\x1b[K",
            b"\x1b[4h\x1b]8;id=x;b\x07ins\x1b[4l\r\n",
            b"\x1b[2J\x1b[H\x1b]8;;e\x07one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix",
            b"\x1b]8;;f\x07\x1b[?1049hmore\x1b[?1049l",
            b"\x1b[2;3r\x1b[2;1H\x1b[L\x1b[M\x1b[r",
            "界e\u{301}\x1b[C\u{301}".as_bytes(),
            b"\x1b]8;;\x07plain\r\n\r\n\r\n\r\n\r\n\r\n\r\n",
            b"\x1b]8;;g\x07wrapping past the edge\r\n",
            b"\x1b[3;1H\x1b[J",
            b"\x1bc\x1b]8;;h\x07after",
        ];
        for (i, step) in steps.iter().enumerate() {
            p.process(step)?;
            check(&p, &format!("step {i}"));
            p.resize(Size::of(3, 7))?;
            check(&p, &format!("step {i}, narrower"));
            p.resize(Size::of(4, 10))?;
            check(&p, &format!("step {i}, back"));
        }
        // More links than fit: room is made, history first.
        for n in 0..2200 {
            p.process(format!("\x1b]8;;{long}{n}\x07x\x1b]8;;\x07\r\n").as_bytes())?;
        }
        check(&p, "filling the links");
    }
    Ok(())
}

/// Each cell of a grid as a reader sees it, its style's number left out:
/// what sweeping the styles, which renumbers them, must keep.
fn looks(grid: &Grid) -> Vec<crate::grid::tests::Seen> {
    grid.seen()
        .into_iter()
        .map(|(id, version, wrapped, prompt, cells)| {
            let cells = cells
                .into_iter()
                .map(|(mut cell, attributes, text, link)| {
                    cell.set_style(0);
                    (cell, attributes, text, link)
                })
                .collect();
            (id, version, wrapped, prompt, cells)
        })
        .collect()
}

/// A sweep renumbers the styles and nothing a reader sees changes: two
/// parsers given the same output, one with both grids swept after every
/// piece, look the same throughout, through new colours, erasing in
/// colour, scrolling into history and in regions, insertion, deletion,
/// wide glyphs, long clusters, links, both screens and resizes.
#[test]
fn sweeping_the_styles_changes_nothing_a_reader_sees() -> Result<(), Error> {
    let pieces: [&[u8]; 22] = [
        b"hello",
        b"\r\n",
        b"\n",
        "\u{754c}x".as_bytes(),
        "e\u{301}\u{302}\u{303}\u{304}\u{305}\u{306}\u{307}\u{308}\u{309}".as_bytes(),
        b"\x1b[m",
        b"\x1b[K",
        b"\x1b[1K",
        b"\x1b[2J",
        b"\x1b[3X",
        b"\x1b[2@",
        b"\x1b[2P",
        b"\x1b[L",
        b"\x1b[M",
        b"\x1b[2;4r",
        b"\x1b[r",
        b"\x1b[S",
        b"\x1b[T",
        b"\x1b[9;3H",
        b"\x1b]8;;https://a\x1b\\link\x1b]8;;\x1b\\",
        b"\x1b[?1049h",
        b"\x1b[?1049l",
    ];
    let mut state = 0x5157_u64;
    let mut below = |n: usize| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(state >> 33)
            .unwrap_or(0)
            .checked_rem(n)
            .unwrap_or(0)
    };
    for reflow in [false, true] {
        let options = crate::Options::new()
            .set(crate::Feature::Reflow, reflow)
            .with(crate::Feature::Hyperlinks);
        let mut plain = crate::Parser::with_options(Size::of(5, 9), 6, options)?;
        let mut swept = crate::Parser::with_options(Size::of(5, 9), 6, options)?;
        for step in 0..4_000u32 {
            let piece: Vec<u8> = match below(8) {
                0 => format!("\x1b[48;2;{};1;{}m", below(4), below(256)).into_bytes(),
                1 => format!("\x1b[48;5;{}m", below(8)).into_bytes(),
                2 if below(10) == 0 => {
                    let rows = u16::try_from(below(6)).unwrap_or(0).saturating_add(1);
                    let cols = u16::try_from(below(10)).unwrap_or(0).saturating_add(1);
                    plain.resize(Size::of(rows, cols))?;
                    swept.resize(Size::of(rows, cols))?;
                    Vec::new()
                }
                _ => pieces
                    .get(below(pieces.len()))
                    .copied()
                    .unwrap_or(b"a")
                    .to_vec(),
            };
            plain.process(&piece)?;
            swept.process(&piece)?;
            let s = swept.screen_mut();
            s.primary.sweep();
            s.alternate.sweep();
            let (a, b) = (plain.screen(), swept.screen());
            assert!(
                looks(&a.primary) == looks(&b.primary)
                    && looks(&a.alternate) == looks(&b.alternate),
                "step {step}"
            );
            assert_eq!(a.attributes(), b.attributes(), "step {step}");
        }
    }
    Ok(())
}

/// Past the floor, new styles sweep the table, and the cells keep their
/// attributes: a truecolour run, every glyph a colour of its own, scrolled
/// through history many times over, reads back colour by colour, and the
/// table stays bounded however many styles were made.
#[test]
fn styles_in_use_survive_the_sweeps_that_new_ones_bring() -> Result<(), Error> {
    let (rows, cols, history) = (4u16, 16u16, 30usize);
    let mut p = crate::Parser::new(Size::of(rows, cols), history)?;
    let colour = |n: u32| {
        let [_, r, g, b] = n.to_be_bytes();
        (r, g, b)
    };
    let glyphs = 40_000u32;
    for n in 0..glyphs {
        let (r, g, b) = colour(n);
        p.process(format!("\x1b[48;2;{r};{g};{b}mx").as_bytes())?;
    }
    let grid = &p.screen().primary;
    assert!(grid.style_count() <= 2 * 4096, "{}", grid.style_count());
    // Every retained cell, oldest first, is one of the last glyphs printed,
    // in its colour.
    let retained = grid.retained_len();
    let cells = u32::try_from(retained.saturating_mul(usize::from(cols))).unwrap_or(0);
    let mut n = glyphs.saturating_sub(cells);
    for i in 0..retained {
        let row = grid.row_at(i).ok_or(Error::InvalidRange)?;
        for cell in row.cells() {
            let (r, g, b) = colour(n);
            assert_eq!(cell.contents(), "x", "glyph {n}");
            assert_eq!(cell.bgcolor(), Color::Rgb([r, g, b].into()), "glyph {n}");
            n = n.saturating_add(1);
        }
    }
    assert_eq!(n, glyphs);
    Ok(())
}

/// A command that clears the main screen starts its output again at the
/// top; one whose full-screen program clears the alternate screen keeps
/// its output's start on the main screen.
#[test]
fn only_a_main_screen_clear_moves_a_commands_output() -> Result<(), Error> {
    let start = |clear: &[u8]| -> Result<String, Error> {
        let marks = crate::Options::new().with(crate::Feature::PromptMarks);
        let mut p = crate::Parser::with_options(Size::of(6, 20), 50, marks)?;
        p.process(b"$ run\r\n\x1b]133;C\x07BEFORE\r\n")?;
        p.process(clear)?;
        p.process(b"AFTER\r\n\x1b]133;D;0\x07$ ")?;
        let screen = p.screen();
        let (from, _) = screen.commands().last_output.ok_or(Error::InvalidRange)?;
        let row = screen.row_by_id(from.row).ok_or(Error::InvalidRange)?;
        Ok(row.cells_text().trim_end().to_owned())
    };
    assert_eq!(start(b"\x1b[?1049h\x1b[2Jmenu\x1b[?1049l")?, "BEFORE");
    assert_eq!(start(b"\x1b[2J\x1b[H")?, "AFTER");
    Ok(())
}

/// A picture the screen no longer shows holds no pixels: placing another
/// lets go of one that text covered, and RIS of every one, as it clears
/// the cells they covered.
#[test]
fn pictures_no_longer_shown_are_let_go() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    let mut p = crate::Parser::with_options(Size::of(6, 10), 10, options)?;
    let sixel = b"\x1bPq#1;2;100;0;0!6~\x1b\\";
    p.process(sixel)?;
    p.process(b"\x1b[1;1Hx\x1b[3;5H")?;
    assert_eq!(p.screen().pictures().count(), 0);
    p.process(sixel)?;
    assert_eq!(p.screen().pictures.len(), 1, "the covered one goes");
    p.process(b"\x1bc")?;
    assert_eq!(p.screen().pictures.len(), 0, "RIS lets every one go");
    Ok(())
}

/// The pictures hold at most `PICTURE_BYTES` of pixels, the one being
/// drawn included (`threats.md` B01): 64 pictures of the largest size,
/// 4096 by 2160, drawn to their last pixel, and one more left unfinished.
#[test]
fn pictures_hold_no_more_than_their_budget() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    let mut p = crate::Parser::with_options(Size::of(24, 80), 8 << 20, options)?;
    let mut hostile = picture(4096, 2160);
    hostile.truncate(hostile.len().saturating_sub(2));
    for _ in 0..64 {
        p.process(&hostile)?;
        assert!(p.picture_bytes() <= PICTURE_BYTES, "while drawing");
        // A line feed after it, so the next is placed below it: the cursor
        // ends on a picture's last row, and one placed over another takes
        // its place.
        p.process(b"\x1b\\\n")?;
        assert!(p.screen().picture_bytes() <= PICTURE_BYTES);
    }
    assert_eq!(p.screen().pictures().count(), 2, "two of the largest fit");
    p.process(&hostile)?;
    assert!(p.picture_bytes() <= PICTURE_BYTES, "one more being drawn");
    Ok(())
}

/// A sixel picture `width` by `height` pixels, square ones, of one red
/// pixel drawn at its bottom right and the rest clear: its pixels take
/// `width × height × 4` bytes.
fn picture(width: u32, height: u32) -> Vec<u8> {
    let last = height.saturating_sub(1);
    let bands: String = std::iter::repeat_n('-', usize::try_from(last / 6).unwrap_or(0)).collect();
    let bit = char::from(0x3f_u8.saturating_add(1 << (last % 6)));
    let left = width.saturating_sub(1);
    format!("\x1bP0;1q\"1;1#1;2;100;0;0{bands}!{left}?{bit}\x1b\\").into_bytes()
}

/// The numbers of the pictures the screen shows.
fn shown(p: &crate::Parser) -> Vec<u64> {
    p.screen().pictures().map(|s| s.id).collect()
}

/// Pictures summing to exactly the budget fit, the last of them as it is
/// drawn beside the others; a byte more lets the oldest go (B01).
#[test]
fn pictures_fill_their_budget_exactly() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    let mut p = crate::Parser::with_options(Size::of(24, 80), 8 << 20, options)?;
    // 4096 by 2048 pixels: 32 MiB, three to the budget.
    for _ in 0..3 {
        p.process(&picture(4096, 2048))?;
        p.process(b"\n")?;
    }
    assert_eq!(shown(&p), [1, 2, 3], "three thirds of the budget");
    p.process(&picture(1, 1))?;
    assert_eq!(shown(&p), [2, 3, 4], "a byte past it");
    Ok(())
}

/// Past the budget, pictures no longer shown go first, then the oldest
/// (B01): one covered by text goes before an older one still shown, and
/// so does one on the other screen.
#[test]
fn pictures_no_longer_shown_go_first() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    let mut p = crate::Parser::with_options(Size::of(24, 80), 8 << 20, options)?;
    // 100 by 120 pixels: 48,000 bytes, 10 by 6 cells at 10 by 20; room for
    // two of them.
    p.set_picture_bytes(100_000);
    let one = picture(100, 120);
    p.process(b"\x1b[1;1H")?;
    p.process(&one)?;
    p.process(b"\x1b[1;30H")?;
    p.process(&one)?;
    p.process(b"\x1b[2;31Hx")?;
    assert_eq!(shown(&p), [1], "text covers the second");
    p.process(b"\x1b[10;1H")?;
    p.process(&one)?;
    assert_eq!(shown(&p), [1, 3], "the covered one went, not the older");
    // The oldest shown, then one on the other screen, now hidden.
    p.process(b"\x1bc\x1b[1;1H")?;
    p.process(&one)?;
    p.process(b"\x1b[?1049h")?;
    p.process(&one)?;
    p.process(b"\x1b[?1049l\x1b[10;1H")?;
    p.process(&one)?;
    assert_eq!(shown(&p), [4, 6], "the other screen's went");
    p.process(b"\x1b[?1049h")?;
    assert_eq!(shown(&p), [], "and is gone from it");
    Ok(())
}

/// A lowered budget lets pictures go at once to fit it, and a picture
/// being drawn that alone is past it lets go of what it drew.
#[test]
fn a_lowered_picture_budget_lets_pictures_go() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    let mut p = crate::Parser::with_options(Size::of(24, 80), 8 << 20, options)?;
    let one = picture(100, 120);
    p.process(b"\x1b[1;1H")?;
    p.process(&one)?;
    p.process(b"\x1b[1;30H")?;
    p.process(&one)?;
    p.set_picture_bytes(48_000);
    assert_eq!(shown(&p), [2], "one fits");
    let mut unfinished = picture(100, 120);
    unfinished.truncate(unfinished.len().saturating_sub(2));
    p.process(b"\x1b[10;1H")?;
    p.process(&unfinished)?;
    p.set_picture_bytes(47_999);
    assert_eq!(shown(&p), [], "the kept one went first");
    assert!(p.picture_bytes() <= 47_999, "the one being drawn let go");
    p.process(b"\x1b\\")?;
    assert_eq!(shown(&p), [], "with nothing drawn");
    Ok(())
}

/// A screen keeps 64 pictures, the 64th too, and the 65th lets the oldest
/// go (threats.md B02): each one cell, in a cell of its own.
#[test]
fn a_screen_keeps_64_pictures_and_the_65th_lets_the_oldest_go() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    let mut p = crate::Parser::with_options(Size::of(24, 80), 8 << 20, options)?;
    let one = picture(1, 1);
    for n in 0..65u16 {
        p.process(format!("\x1b[{};{}H", n / 8 + 1, (n % 8) * 2 + 1).as_bytes())?;
        p.process(&one)?;
        if n == 63 {
            assert_eq!(shown(&p), (1..=64).collect::<Vec<u64>>(), "64 kept");
        }
    }
    assert_eq!(shown(&p), (2..=65).collect::<Vec<u64>>(), "the 65th");
    Ok(())
}

/// Pictures exactly at the budget are kept, a byte past it lets the oldest
/// go, by the budget set as by one drawn (threats.md B01).
#[test]
fn pictures_at_the_budget_are_kept_and_a_byte_past_lets_one_go() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    let mut p = crate::Parser::with_options(Size::of(24, 80), 8 << 20, options)?;
    // 100 by 120 pixels: 48,000 bytes, 10 by 6 cells at 10 by 20.
    let one = picture(100, 120);
    p.process(b"\x1b[1;1H")?;
    p.process(&one)?;
    p.process(b"\x1b[1;30H")?;
    p.process(&one)?;
    p.set_picture_bytes(96_000);
    assert_eq!(shown(&p), [1, 2], "at the budget");
    p.set_picture_bytes(95_999);
    assert_eq!(shown(&p), [2], "a byte past it");
    p.set_picture_bytes(96_000);
    p.process(b"\x1b[10;1H")?;
    p.process(&one)?;
    assert_eq!(shown(&p), [2, 3], "one drawn at the budget");
    Ok(())
}

/// A picture placed takes the cells it covers from those under it, and
/// only those: one beside it on any side, its cells touching, stays; one
/// a cell of it covers goes (threats.md 5.2, `overlaps`).
#[test]
fn a_picture_takes_only_the_pictures_it_covers() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    // 20 by 40 pixels: 2 cells by 2 at 10 by 20, placed at row 5 col 5.
    let two = picture(20, 40);
    let place = |at: &[u8]| -> Result<Vec<u64>, Error> {
        let mut p = crate::Parser::with_options(Size::of(24, 80), 8 << 20, options)?;
        p.process(b"\x1b[5;5H")?;
        p.process(&two)?;
        p.process(at)?;
        p.process(&two)?;
        Ok(shown(&p))
    };
    // Beside it: left, right, above and below, touching.
    for at in [&b"\x1b[5;3H"[..], b"\x1b[5;7H", b"\x1b[3;5H", b"\x1b[7;5H"] {
        assert_eq!(place(at)?, [1, 2], "{:?}", String::from_utf8_lossy(at));
    }
    // Over one of its cells from each side.
    for at in [&b"\x1b[5;4H"[..], b"\x1b[5;6H", b"\x1b[4;5H", b"\x1b[6;6H"] {
        assert_eq!(place(at)?, [2], "{:?}", String::from_utf8_lossy(at));
    }
    Ok(())
}

/// A kept picture holds just its pixels, though its room grew by doubling
/// as it was drawn (threats.md B03; REB-11): the budget counts its room,
/// which is then its size. Drawn a column further right a sixel at a
/// time, to 101 columns of one band (6 positions down).
#[test]
fn a_kept_picture_holds_just_its_pixels() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    let mut p = crate::Parser::with_options(Size::of(24, 80), 8 << 20, options)?;
    let mut rightward = b"\x1bPq~".to_vec();
    for n in 1..=100 {
        rightward.extend(format!("$!{n}?~").bytes());
    }
    rightward.extend(b"\x1b\\");
    p.process(&rightward)?;
    assert_eq!(p.screen().picture_bytes(), 101 * 6 * 4);
    Ok(())
}

/// A picture that only declares its size keeps no pixels for it, however
/// large (`threats.md` B05): its extent reads as the background (colour
/// register 0 for P2 0 and 2) or clear (P2 1), when shown.
#[test]
fn a_declared_extent_keeps_no_pixels() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    for (p2, pixel) in [(0, [0, 0, 0, 255]), (1, [0, 0, 0, 0]), (2, [0, 0, 0, 255])] {
        let mut p = crate::Parser::with_options(Size::of(24, 80), 8 << 20, options)?;
        p.process(format!("\x1bP0;{p2}q\"1;1;4096;2160\x1b\\").as_bytes())?;
        assert_eq!(p.screen().picture_bytes(), 0, "P2 {p2}");
        let shown = p.screen().pictures().next().ok_or(Error::InvalidRange)?;
        let size = (shown.picture.width(), shown.picture.height());
        assert_eq!(size, (4096, 2160), "P2 {p2}");
        assert_eq!(shown.picture.pixel(4095, 2159), Some(pixel), "P2 {p2}");
    }
    Ok(())
}

/// A picture's cells take the placeholder as a fill, not as text (DEC STD
/// 070-9, 11.1: "No other information is carried from the ANSI text
/// state"): insert mode shifts nothing, the pen and protection don't reach
/// them, a cluster before doesn't take the first, and no print records
/// them.
#[test]
fn a_picture_s_cells_are_not_printed() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    // 20 by 20 pixels: 2 cells by 1 at 10 by 20.
    let two = picture(20, 20);
    let row = |p: &crate::Parser| {
        p.screen()
            .primary
            .live_row(0)
            .map(|r| r.cells_text())
            .unwrap_or_default()
    };
    let ph = PLACEHOLDER;
    let mut p = crate::Parser::with_options(Size::of(4, 10), 0, options)?;
    p.record_prints(true);
    p.process(b"abcdef\r\x1b[4h\x1b[1;7m\x1b[1\"q")?;
    p.process(&two)?;
    // A picture's cells read as blanks.
    assert_eq!(row(&p).trim_end(), "  cdef", "insert mode");
    let first = p.screen().primary.live_row(0).and_then(|r| r.cell(0));
    assert_eq!(first.map(|c| c.contents() == ph), Some(true));
    let attributes = first.map(|c| c.attributes());
    assert_eq!(attributes, Some(crate::Attributes::default()), "no pen");
    let prints = p.take_prints();
    assert!(
        !prints.iter().any(|run| run.text.contains(ph)),
        "no print of a placeholder"
    );
    let mut p = crate::Parser::with_options(Size::of(4, 10), 0, options)?;
    p.process("\u{600}".as_bytes())?;
    p.process(&two)?;
    assert_eq!(p.screen().pictures().count(), 1, "after a prepend");
    Ok(())
}

/// A host's cell is taken as at least a sixel band, 2 by 6 pixels (P5).
#[test]
fn a_host_cell_is_at_least_a_sixel_band() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    for (cell, taken) in [
        ((1, 5), (2, 6)),
        ((2, 6), (2, 6)),
        ((3, 7), (3, 7)),
        ((1, 9), (2, 9)),
    ] {
        let mut p = crate::Parser::with_options(Size::of(24, 80), 0, options)?;
        p.set_host_graphics(crate::Graphics {
            cell: Some(cell),
            sixel: true,
        });
        p.process(&picture(6, 12))?;
        let shown = p.screen().pictures().next().ok_or(Error::InvalidRange)?;
        assert_eq!(shown.cell, taken, "{cell:?}");
    }
    Ok(())
}

/// `Prints` keeps text printed and drawn over within one feed up to its
/// cap, 16 KiB on a small screen, and past it records nothing more and
/// says so (`threats.md` B10); a take begins again.
#[test]
fn prints_keep_text_drawn_over_up_to_their_cap() -> Result<(), Error> {
    let cap = 16 << 10;
    for (bytes, lost) in [(cap - 1, false), (cap, false), (cap + 1, true)] {
        let mut p = crate::Parser::new(Size::of(2, 10), 0)?;
        p.record_prints(true);
        // Runs of up to 8 bytes, each drawn over by the next, a and b in
        // turn: 2,048 runs at most, within theirs.
        let (mut left, mut a) = (bytes, true);
        while left > 0 {
            let n = left.min(8);
            let c = if a { 'a' } else { 'b' };
            let run: String = std::iter::repeat_n(c, n).chain(['\r']).collect();
            p.process(run.as_bytes())?;
            (left, a) = (left - n, !a);
        }
        let prints = p.take_prints();
        assert_eq!(prints.lost(), lost, "{bytes} bytes");
        let kept: usize = prints.iter().map(|r| r.text.len()).sum();
        assert_eq!(kept, bytes.min(cap), "{bytes} bytes");
        assert!(prints.heap() <= cap + crate::PRINT_RUNS * 32);
        p.process(b"c")?;
        let next = p.take_prints();
        assert!(!next.lost(), "a take begins again");
        assert_eq!(next.iter().map(|r| r.text).collect::<String>(), "c");
    }
    Ok(())
}

/// `Prints` keeps 4,096 runs; the 4,097th is lost. On 50 by 200 its text
/// may be 4 bytes a cell, 40,000 (`threats.md` B10).
#[test]
fn prints_keep_at_most_their_runs() -> Result<(), Error> {
    for (runs, lost) in [(4095, false), (4096, false), (4097, true)] {
        let mut p = crate::Parser::new(Size::of(50, 200), 0)?;
        p.record_prints(true);
        // A run on each of two rows in turn: none joins the one before.
        for i in 0..runs {
            let (row, c) = (1 + i % 2, if i / 2 % 2 == 0 { 'x' } else { 'y' });
            p.process(format!("\x1b[{row};1H{c}").as_bytes())?;
        }
        let prints = p.take_prints();
        assert_eq!(prints.lost(), lost, "{runs} runs");
        assert_eq!(prints.iter().count(), runs.min(4096));
    }
    let mut p = crate::Parser::new(Size::of(50, 200), 0)?;
    p.record_prints(true);
    assert_eq!(p.take_prints().cap(), 40_000);
    Ok(())
}

/// A resize with prints untaken loses them: their rows and columns are
/// the old size's. With none, nothing is lost.
#[test]
fn a_resize_loses_prints_untaken() -> Result<(), Error> {
    let mut p = crate::Parser::new(Size::of(4, 10), 0)?;
    p.record_prints(true);
    p.resize(Size::of(4, 12))?;
    p.process(b"a")?;
    assert!(!p.take_prints().lost(), "nothing untaken");
    p.process(b"b")?;
    p.resize(Size::of(4, 10))?;
    assert!(p.take_prints().lost());
    Ok(())
}

/// A run of `Prints`: its text and columns.
type Run = (String, (u16, u16));

/// What `bytes` print, each run's text and columns.
fn printed(bytes: &[&str]) -> Result<Vec<Run>, Error> {
    let mut p = crate::Parser::new(Size::of(4, 20), 0)?;
    p.record_prints(true);
    for piece in bytes {
        p.process(piece.as_bytes())?;
    }
    let prints = p.take_prints();
    Ok(prints.iter().map(|r| (r.text.to_owned(), r.cols)).collect())
}

/// Characters that join a cell's cluster are printed, as they change it
/// (PAR-13, TXT-6; `Prints`: "each run of text that changed cells"): a
/// combining mark, and a flag's second regional indicator, which widens
/// its cell; the same however the input is split.
#[test]
fn prints_hold_what_joins_a_cluster() -> Result<(), Error> {
    let whole = "e\u{301} \u{1F1EF}\u{1F1F5}";
    let want = vec![(whole.to_owned(), (0, 4))];
    assert_eq!(printed(&[whole])?, want);
    let split: Vec<String> = whole.chars().map(String::from).collect();
    let split: Vec<&str> = split.iter().map(String::as_str).collect();
    assert_eq!(printed(&split)?, want, "a character a call");
    // A mark joined after the cursor moved away and back joins the run
    // that took its cell; one after a take is a run of its own.
    let after = printed(&["a\u{1b}[C\u{1b}[D\u{301}"])?;
    assert_eq!(after, vec![("a\u{301}".to_owned(), (0, 1))]);
    let mut p = crate::Parser::new(Size::of(4, 20), 0)?;
    p.record_prints(true);
    p.process("\u{1F44D}".as_bytes())?;
    p.take_prints();
    p.process("\u{1F3FD}".as_bytes())?;
    let tone: Vec<_> = p
        .take_prints()
        .iter()
        .map(|r| (r.text.to_owned(), r.cols))
        .collect();
    assert_eq!(tone, vec![("\u{1F3FD}".to_owned(), (0, 2))], "a skin tone");
    Ok(())
}

/// The checker's 6a (REB-4): a shell command set aside by a nested
/// program's mark holds both nonces; one dropped from under it shows.
#[cfg(feature = "checks")]
#[test]
fn the_checker_finds_a_command_set_aside_without_its_nonces() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::PromptMarks);
    let mut p = crate::Parser::with_options(Size::of(4, 20), 0, options)?;
    p.require_mark_nonce(Some(b"k3y"), Some(b"n3st"));
    p.process(b"\x1b]133;C;nonce=k3y\x07\x1b]133;A;nonce=n3st\x07")?;
    assert!(p.check().is_clean(), "{}", p.check());
    p.screen_mut().nested_nonce = None;
    assert!(p.check().faults_of("screen.nest").next().is_some());
    Ok(())
}

/// The checksum a DECRQCRA reply holds, read cell by cell as a reader
/// sees each (`Grid::cell`, its text read as UTF-8 and its attributes
/// whole): what `Screen::rectangle_checksum` summed before it decoded the
/// stored cells.
fn checksum_cell_by_cell(p: &crate::Parser, lines: Span, columns: Span) -> u16 {
    let g = p.screen().grid();
    let mut sum = 0u16;
    for y in lines.each() {
        for x in columns.each().take_while(|x| *x < g.row_end(y)) {
            let Some(cell) = g.cell(y, x) else {
                continue;
            };
            let mut chars = cell.contents().chars();
            let base = match chars.next() {
                None if cell.is_wide_continuation() => ESC,
                None => 0x20,
                Some(c) => match u16::try_from(u32::from(c)) {
                    Ok(code @ 0x20..=0xff) => code,
                    _ => dec_code(c).map_or(ESC, u16::from),
                },
            };
            let marks = chars.fold(0u16, |sum, c| {
                sum.wrapping_add(u16::try_from(u32::from(c) & 0xffff).unwrap_or(0))
            });
            sum = sum
                .wrapping_add(base)
                .wrapping_add(marks)
                .wrapping_add(rendition_of(cell.attributes()));
        }
    }
    sum.wrapping_neg()
}

/// DECRQCRA's sum of the stored cells, their text decoded from its bytes
/// and each style's rendition looked up once a run, is the sum cell by
/// cell (`checksum_cell_by_cell`), on random screens of styles, wide
/// glyphs, DEC Special Graphics and Supplemental, CJK, clusters short and
/// long, blanks and erases, and double-width lines.
#[test]
fn rectangle_checksums_of_stored_cells_are_the_sum_cell_by_cell() -> Result<(), Error> {
    let pieces: [&str; 30] = [
        "hello",
        "\r\n",
        "\u{754c}\u{4e2d}x",
        "e\u{301}",
        "a\u{301}\u{302}\u{303}\u{304}\u{305}\u{306}",
        "\u{1f44d}\u{1f3fd}",
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
        "\u{e9}\u{ff}\u{a0}\u{b0}",
        "\u{152}\u{178}\u{153}\u{3c0}\u{2500}\u{2502}",
        "\x1b(0`afgjklmnopqrstuvwxyz{|}~_\x1b(B",
        "\x1b(0lqqk\x1b(B",
        "\x1b(%5W]wA($\x1b(B",
        "\x1b[1m",
        "\x1b[4;7m",
        "\x1b[5;8m",
        "\x1b[31;42m",
        "\x1b[m",
        "\x1b[K",
        "\x1b[3X",
        "\x1b[2@",
        "\x1b[2P",
        "\x1b[2J",
        "\x1b#6",
        "\x1b#3",
        "\x1b#5",
        "\x1b[H",
        "\x1b[5;7H",
        "\x1b[3;12H",
        "\t",
        "\x07\x01",
    ];
    let options = crate::Options::new().with(crate::Feature::RectangleChecksums);
    let mut r = crate::test_rng::Rng(0x00c5_c5a1);
    for case in 0..200 {
        let size = Size::of(1 + r.small(8), 1 + r.small(20));
        let mut p = crate::Parser::with_options(size, 0, options)?;
        let mut feed = String::new();
        for _ in 0..r.below(60) {
            feed.push_str(r.pick(&pieces).unwrap_or(""));
        }
        p.process(feed.as_bytes())?;
        let (rows, cols) = (size.rows(), size.cols());
        for _ in 0..8 {
            let (top, bottom) = {
                let (a, b) = (r.small(rows), r.small(rows));
                (a.min(b), a.max(b))
            };
            let (left, right) = {
                let (a, b) = (r.small(cols), r.small(cols));
                (a.min(b), a.max(b))
            };
            let (Some(lines), Some(columns)) = (
                size.lines().part(top, bottom),
                size.columns().part(left, right),
            ) else {
                continue;
            };
            let want = checksum_cell_by_cell(&p, lines, columns);
            let ask = format!(
                "\x1b[7;1;{};{};{};{}*y",
                top + 1,
                left + 1,
                bottom + 1,
                right + 1
            );
            let mut replies = Vec::new();
            p.process_with_replies(ask.as_bytes(), |reply| replies.push(reply.to_vec()))?;
            let expected = format!("\x1bP7!~{want:04X}\x1b\\").into_bytes();
            assert_eq!(replies, vec![expected], "case {case}: {feed:?} {ask:?}");
        }
    }
    Ok(())
}

/// DECCARA and DECRARA a run of one style at a time, and DECERA, DECSERA,
/// DECSED and DECSEL a span at a time, leave each grid as the cell-by-cell
/// ways did, style numbers, versions and table included: two parsers given
/// the same random output, one going cell by cell (`PER_CELL`), over
/// glyphs in many styles and fresh truecolours (so the table sweeps
/// mid-rectangle), wide glyphs, long clusters, double-width and -height
/// lines, protected glyphs and wide ones (DECSCA and SPA), DECSACE's
/// stream and rectangle, both screens and resizes.
#[test]
fn rectangles_a_run_at_a_time_are_as_cell_by_cell() -> Result<(), Error> {
    let pieces: &[&[u8]] = &[
        b"hello",
        b"ab cd",
        b"\r\n",
        b"\x1b[H",
        b"\x1b[3;4H",
        b"\x1b[2C",
        "\u{754c}x\u{4e2d}".as_bytes(),
        "e\u{301}\u{302}\u{303}\u{304}\u{305}\u{306}\u{307}\u{308}\u{309}".as_bytes(),
        "a\u{301}".as_bytes(),
        b"\x1b[1m",
        b"\x1b[4;7m",
        b"\x1b[m",
        b"\x1b[K",
        b"\x1b[2X",
        b"\x1b#6",
        b"\x1b#3",
        b"\x1b#4",
        b"\x1b#5",
        b"\x1b[1\"q",
        b"\x1b[0\"q",
        b"\x1bVpro\x1bW",
        "\x1b[1\"q\u{4e2d}y\u{754c}\x1b[0\"q".as_bytes(),
        b"\x1b[?J",
        b"\x1b[?1J",
        b"\x1b[?2J",
        b"\x1b[?K",
        b"\x1b[?1K",
        b"\x1b[?2K",
        b"\x1b[2*x",
        b"\x1b[*x",
        b"\x1b[?1049h",
        b"\x1b[?1049l",
        b"\x1b[S",
    ];
    let mut r = crate::test_rng::Rng(0x0ab0_0000_0000_0950);
    let (mut sweeps, mut runs) = (0u64, 0u32);
    for case in 0..6u32 {
        let size = Size::of(r.small(7).saturating_add(2), r.small(14).saturating_add(4));
        let mut fast = crate::Parser::new(size, 4)?;
        let mut slow = crate::Parser::new(size, 4)?;
        for step in 0..6_000u32 {
            let piece: Vec<u8> = match r.below(10) {
                0 | 1 => {
                    // Now and then the table one style short of a sweep,
                    // so the rectangle's first new style sweeps it.
                    if r.chance(10) {
                        for p in [&mut fast, &mut slow] {
                            let s = p.screen_mut();
                            s.primary.fill_styles_to_sweep();
                            s.alternate.fill_styles_to_sweep();
                        }
                    }
                    let ops = (0..r.below(4))
                        .map(|_| r.pick(&[0, 1, 4, 5, 7, 22, 24, 27, 31, 39, 44, 49, 3]))
                        .map(|n| format!(";{}", n.unwrap_or(0)))
                        .collect::<String>();
                    let (t, l) = (r.small(10), r.small(20));
                    let (b, w) = (t.saturating_add(r.small(10)), l.saturating_add(r.small(20)));
                    runs = runs.saturating_add(1);
                    match r.pick(&['r', 't', 'z', '{']).unwrap_or('r') {
                        end @ ('r' | 't') => format!("\x1b[{t};{l};{b};{w}{ops}${end}"),
                        end => format!("\x1b[{t};{l};{b};{w}${end}"),
                    }
                    .into_bytes()
                }
                2 | 3 => (0..6)
                    .map(|_| {
                        let [_, red, green, blue] = r.next().to_le_bytes()[..4] else {
                            return String::new();
                        };
                        format!("\x1b[38;2;{red};{green};{blue}mz")
                    })
                    .collect::<String>()
                    .into_bytes(),
                4 if r.chance(5) => {
                    let size =
                        Size::of(r.small(7).saturating_add(2), r.small(14).saturating_add(4));
                    fast.resize(size)?;
                    slow.resize(size)?;
                    Vec::new()
                }
                _ => r.pick(pieces).unwrap_or(b"a").to_vec(),
            };
            let before = fast.screen().primary.sweeps() + fast.screen().alternate.sweeps();
            fast.process(&piece)?;
            crate::grid::tests::PER_CELL.with(|c| c.set(true));
            let done = slow.process(&piece);
            crate::grid::tests::PER_CELL.with(|c| c.set(false));
            done?;
            let after = fast.screen().primary.sweeps() + fast.screen().alternate.sweeps();
            if piece.ends_with(b"$r") || piece.ends_with(b"$t") {
                sweeps = sweeps.saturating_add(after.saturating_sub(before));
            }
            let (a, b) = (fast.screen(), slow.screen());
            assert!(
                a.primary.seen() == b.primary.seen() && a.alternate.seen() == b.alternate.seen(),
                "case {case} step {step}: {:?}",
                String::from_utf8_lossy(&piece)
            );
            assert_eq!(
                a.primary.style_count(),
                b.primary.style_count(),
                "case {case} step {step}"
            );
            assert_eq!(a.attributes(), b.attributes(), "case {case} step {step}");
        }
    }
    // The rectangles swept the table now and then, as they must for the
    // sweep's way to be checked.
    assert!(sweeps > 0 && runs > 1_000, "{sweeps} sweeps in {runs}");
    Ok(())
}

/// No reply: what a test of a rectangle edit's calls reads.
struct Quiet;
impl crate::Sink for Quiet {
    fn reply(&mut self, _: &[u8]) {}
}

/// A rectangle edit of more cells than a call of `process_until_stop` may
/// change goes over calls, its rows left for the next call, done first:
/// DECCARA, DECRARA (both of the stream and, after DECSACE 2, the
/// rectangle), DECERA and DECSERA, on a page of styles, wide glyphs,
/// clusters and protected glyphs at 2000x2000, cost past the budget
/// (`budget::LIMIT`), so the first call stops after the sequence, the
/// grid left as `process_with` leaves it, which does every row in one
/// call; the glyph printed after it lands on what the edit made.
#[test]
fn a_rectangle_edit_past_the_budget_goes_over_calls() -> Result<(), Error> {
    let size = Size::of(2000, 2000);
    let cells = crate::budget::units(4_000_000u32);
    let cost = cells.saturating_mul(crate::budget::weight::cells + crate::budget::weight::edits);
    assert!(cost > crate::budget::LIMIT);
    // A page of E's (DECALN), each row's first columns in styles.
    let mut page = String::from("\x1b#8");
    for y in 0..2000u32 {
        page.push_str(&format!("\x1b[{};1H", y + 1));
        for x in 0..12u32 {
            let style = [1, 4, 7, 31]
                .get(usize::try_from((x + y) % 4).unwrap_or(0))
                .copied()
                .unwrap_or(0);
            let protect = (x / 3 + y) % 2;
            page.push_str(&format!(
                "\x1b[0;{style}m\x1b[{protect}\"q\u{4e2d}e\u{301}x"
            ));
        }
    }
    page.push_str("\x1b[0\"q\x1b[m\x1b[3;7H");
    let mut written = crate::Parser::new(size, 0)?;
    written.process(page.as_bytes())?;
    let sequences: [&[u8]; 6] = [
        b"\x1b[;;;;1;5$r",
        b"\x1b[2*x\x1b[2;3;2000;2000;7;31$r",
        b"\x1b[5;9;2000;2000;1;4$t",
        b"\x1b[2*x\x1b[;;;;7$t",
        b"\x1b[;;;$z",
        b"\x1b[;;;${",
    ];
    for sequence in sequences {
        let input = [sequence, b"\x1b[Hz".as_slice()].concat();
        let mut whole = written.clone();
        whole.process(&input)?;
        // Each row's cells as stored and its wrap, versions apart: a row
        // changed in a later call has a later one.
        let cells = |p: &crate::Parser| -> Vec<_> {
            let g = &p.screen().primary;
            (0..2000u16)
                .map(|y| (g.live_wrapped(y), g.live_cells(y).to_vec()))
                .collect()
        };
        let mut parser = written.clone();
        let first = parser.process_until_stop(&input, &mut Quiet)?;
        // The first call stops after the sequence, its rows partly done.
        assert_eq!(
            first,
            Some((sequence.len(), crate::Stop::Budget)),
            "{sequence:?}"
        );
        assert!(cells(&parser) != cells(&whole), "{sequence:?}");
        let mut rest = input.get(sequence.len()..).unwrap_or_default();
        let mut calls = 1;
        while let Some((taken, stop)) = parser.process_until_stop(rest, &mut Quiet)? {
            assert_eq!(stop, crate::Stop::Budget);
            rest = rest.get(taken..).unwrap_or_default();
            calls += 1;
            assert!(calls < 100, "{calls} calls");
        }
        assert!(cells(&parser) == cells(&whole), "{sequence:?}");
    }
    Ok(())
}
