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

/// A command's output starts where its `C` mark put it: a clear of either
/// screen moves it not.
#[test]
fn a_clear_moves_no_commands_output() -> Result<(), Error> {
    for clear in [&b"\x1b[?1049h\x1b[2Jmenu\x1b[?1049l"[..], b"\x1b[2J\x1b[H"] {
        let marks = crate::Options::new().with(crate::Feature::PromptMarks);
        let mut p = crate::Parser::with_options(Size::of(6, 20), 50, marks)?;
        p.process(b"$ run\r\n\x1b]133;C\x07BEFORE\r\n")?;
        let begun = p.screen().commands().output_start;
        p.process(clear)?;
        p.process(b"AFTER\r\n\x1b]133;D;0\x07$ ")?;
        let (from, _) = p
            .screen()
            .commands()
            .last_output
            .ok_or(Error::InvalidRange)?;
        assert_eq!(Some(from), begun, "{clear:?}");
    }
    Ok(())
}

/// The primary screen's saved cursor is the one `CSI ? 1049 l` restores,
/// seen from the alternate screen too, whatever DECSC saves there.
#[test]
fn the_main_saved_cursor_is_what_leaving_the_alternate_screen_restores() -> Result<(), Error> {
    let mut p = crate::Parser::with_options(Size::of(6, 20), 0, crate::Options::new())?;
    p.process(b"\x1b[2;4H\x1b7\x1b[5;9H\x1b[?1049h\x1b[3;3H\x1b7")?;
    assert_eq!(p.screen().main_saved_cursor(), (4, 8));
    p.process(b"\x1b[?1049l")?;
    assert_eq!(p.screen().cursor_position(), (4, 8));
    Ok(())
}

/// A parser that places pictures, its host's cells 10 by 20 pixels.
fn pictured(size: Size, history: usize) -> Result<crate::Parser, Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    let mut p = crate::Parser::with_options(size, history, options)?;
    p.set_host_graphics(
        crate::CellSize::new(10, 20).map_or(crate::Graphics::Text, crate::Graphics::Sixel),
    );
    Ok(p)
}

/// A picture the screen no longer shows holds no pixels: placing another
/// lets go of one that text covered, and RIS of every one, as it clears
/// the cells they covered.
#[test]
fn pictures_no_longer_shown_are_let_go() -> Result<(), Error> {
    let mut p = pictured(Size::of(6, 10), 10)?;
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
/// drawn included (`THREATS.md` B01): 64 pictures of the largest size,
/// 4096 by 2160, drawn to their last pixel, and one more left unfinished.
#[test]
fn pictures_hold_no_more_than_their_budget() -> Result<(), Error> {
    let mut p = pictured(Size::of(24, 80), 8 << 20)?;
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
    let mut p = pictured(Size::of(24, 80), 8 << 20)?;
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
    let mut p = pictured(Size::of(24, 80), 8 << 20)?;
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
    let mut p = pictured(Size::of(24, 80), 8 << 20)?;
    let one = picture(100, 120);
    p.process(b"\x1b[1;1H")?;
    p.process(&one)?;
    p.process(b"\x1b[1;30H")?;
    p.process(&one)?;
    let revision = p.screen().revision();
    p.set_picture_bytes(48_000);
    assert_eq!(shown(&p), [2], "one fits");
    assert!(
        p.screen().revision() > revision,
        "a picture let go is a change"
    );
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
/// go (THREATS.md B02): each one cell, in a cell of its own.
#[test]
fn a_screen_keeps_64_pictures_and_the_65th_lets_the_oldest_go() -> Result<(), Error> {
    let mut p = pictured(Size::of(24, 80), 8 << 20)?;
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
/// go, by the budget set as by one drawn (THREATS.md B01).
#[test]
fn pictures_at_the_budget_are_kept_and_a_byte_past_lets_one_go() -> Result<(), Error> {
    let mut p = pictured(Size::of(24, 80), 8 << 20)?;
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
/// a cell of it covers goes (THREATS.md section 5, `overlaps`).
#[test]
fn a_picture_takes_only_the_pictures_it_covers() -> Result<(), Error> {
    // 20 by 40 pixels: 2 cells by 2 at 10 by 20, placed at row 5 col 5.
    let two = picture(20, 40);
    let place = |at: &[u8]| -> Result<Vec<u64>, Error> {
        let mut p = pictured(Size::of(24, 80), 8 << 20)?;
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
/// as it was drawn (THREATS.md B03): the budget counts its room,
/// which is then its size. Drawn a column further right a sixel at a
/// time, to 101 columns of one band (6 positions down).
#[test]
fn a_kept_picture_holds_just_its_pixels() -> Result<(), Error> {
    let mut p = pictured(Size::of(24, 80), 8 << 20)?;
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
/// large (`THREATS.md` B05): its extent reads as the background (colour
/// register 0 for P2 0 and 2) or clear (P2 1), when shown.
#[test]
fn a_declared_extent_keeps_no_pixels() -> Result<(), Error> {
    for (p2, pixel) in [(0, [0, 0, 0, 255]), (1, [0, 0, 0, 0]), (2, [0, 0, 0, 255])] {
        let mut p = pictured(Size::of(24, 80), 8 << 20)?;
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
/// them, and a cluster before doesn't take the first.
#[test]
fn a_picture_s_cells_are_not_printed() -> Result<(), Error> {
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
    let mut p = pictured(Size::of(4, 10), 0)?;
    p.process(b"abcdef\r\x1b[4h\x1b[1;7m\x1b[1\"q")?;
    p.process(&two)?;
    // A picture's cells read as blanks.
    assert_eq!(row(&p).trim_end(), "  cdef", "insert mode");
    let first = p.screen().primary.live_row(0).and_then(|r| r.cell(0));
    assert_eq!(first.map(|c| c.contents() == ph), Some(true));
    let attributes = first.map(|c| c.attributes());
    assert_eq!(attributes, Some(crate::Attributes::default()), "no pen");
    let mut p = pictured(Size::of(4, 10), 0)?;
    p.process("\u{600}".as_bytes())?;
    p.process(&two)?;
    assert_eq!(p.screen().pictures().count(), 1, "after a prepend");
    Ok(())
}

/// No text makes a picture's cells: a mark after one has no cell, and
/// neither U+10EEEE (kitty's placeholder, plain text to fux-vt) nor U+0080
/// printed brings a picture back.
#[test]
fn text_cannot_forge_a_picture_s_cells() -> Result<(), Error> {
    let mut p = pictured(Size::of(4, 10), 0)?;
    p.process(b"\x1b[3G")?;
    p.process(&picture(20, 20))?;
    p.process("\x1b[1;5H\u{301}".as_bytes())?;
    assert_eq!(p.screen().pictures().count(), 1, "a mark after it");
    let fourth = p.screen().cell(0, 3).map(|c| c.contents() == PLACEHOLDER);
    assert_eq!(fourth, Some(true));
    p.process(b"\x1b[1;3HX")?;
    assert_eq!(p.screen().pictures().count(), 0, "text over it");
    p.process("\x1b[1;3H\u{10EEEE}\u{80}".as_bytes())?;
    assert_eq!(p.screen().pictures().count(), 0, "text can't bring it back");
    Ok(())
}

/// A host's cell is at least a sixel band, 2 by 6 pixels (THREATS.md
/// B05): a smaller one is refused, not made one. With no cell size, a
/// picture has no cells to be laid out in, and none is placed.
#[test]
fn a_picture_needs_a_host_cell_of_a_sixel_band_or_more() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    for (cell, placed) in [
        ((1, 5), false),
        ((1, 9), false),
        ((2, 6), true),
        ((3, 7), true),
    ] {
        let mut p = crate::Parser::with_options(Size::of(24, 80), 0, options)?;
        let size = crate::CellSize::new(cell.0, cell.1);
        assert_eq!(size.is_some(), placed, "{cell:?}");
        p.set_host_graphics(size.map_or(crate::Graphics::Text, crate::Graphics::Sixel));
        p.process(&picture(6, 12))?;
        let shown = p.screen().pictures().next().map(|s| s.cell);
        assert_eq!(shown, placed.then_some(cell), "{cell:?}");
        assert_eq!(p.pictures_refused(), u64::from(!placed), "{cell:?}");
    }
    Ok(())
}

/// A picture the pictures' budget refuses room is not placed, and is
/// counted; one that drew nothing, or that CAN ended, is no picture.
#[test]
fn a_picture_refused_room_is_counted() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    let mut p = crate::Parser::with_options(Size::of(24, 80), 0, options)?;
    p.set_host_graphics(
        crate::CellSize::new(10, 20).map_or(crate::Graphics::Text, crate::Graphics::Sixel),
    );
    p.process(b"\x1bP9q\x1b\\\x1bP9q~\x18")?;
    assert_eq!(p.pictures_refused(), 0);
    p.set_picture_bytes(0);
    p.process(b"\x1bP9q~\x1b\\")?;
    assert_eq!(p.screen().pictures().count(), 0);
    assert_eq!(p.pictures_refused(), 1);
    Ok(())
}

/// A picture begun while the host drew no sixel is counted as it ends,
/// if it drew anything: one empty, or that CAN ended, is none, as on a
/// host that draws them.
#[test]
fn a_picture_no_host_draws_is_counted_as_it_ends() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    let mut p = crate::Parser::with_options(Size::of(24, 80), 0, options)?;
    p.process(b"\x1bPq\x1b\\\x1bPq~\x18\x1bPq#1;2;100;0;0\x1b\\")?;
    assert_eq!(p.pictures_refused(), 0);
    p.process(b"\x1bPq~\x1b\\")?;
    assert_eq!(p.pictures_refused(), 1);
    assert_eq!(p.screen().pictures().count(), 0);
    Ok(())
}

/// A picture begun while the host drew sixel and ended once it had no cell
/// size, or drew no sixel though it has one, read across two reads, is not
/// placed, and is counted so, once.
#[test]
fn a_picture_whose_host_went_as_it_was_read_is_counted() -> Result<(), Error> {
    let options = crate::Options::from(crate::Feature::Pictures);
    let cell = crate::CellSize::new(10, 20).ok_or(Error::InvalidRange)?;
    for gone in [crate::Graphics::Text, crate::Graphics::Cell(cell)] {
        let mut p = crate::Parser::with_options(Size::of(24, 80), 0, options)?;
        p.set_host_graphics(crate::Graphics::Sixel(cell));
        let bytes = picture(6, 12);
        let (begun, rest) = bytes
            .split_at_checked(bytes.len() / 2)
            .ok_or(Error::InvalidRange)?;
        p.process(begun)?;
        p.set_host_graphics(gone);
        p.process(rest)?;
        assert_eq!(p.screen().pictures().count(), 0, "{gone:?}");
        assert_eq!(p.pictures_refused(), 1, "{gone:?}");
    }
    Ok(())
}
