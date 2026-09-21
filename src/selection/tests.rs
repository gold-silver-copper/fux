use super::*;
use crate::testing::*;
use fux_vt::Parser;

#[test]
fn unicode_selection_normalizes_wide_continuations_and_preserves_combining_marks() -> Outcome {
    let mut parser = Parser::new(3, 12, 20)?;
    parser.process("A界e\u{301}Z".as_bytes())?;
    let grid = Grid::capture(parser.screen(), 0)?;
    assert_eq!(grid.text((0, 2), (0, 3))?, "界e\u{301}");
    assert_eq!(grid.text((0, 3), (0, 2))?, "界e\u{301}");
    assert_eq!(grid.text((0, 0), (0, 4))?, "A界e\u{301}Z");
    assert_eq!(grid.text((0, 1), (0, 1))?, "界");
    Ok(())
}
#[test]
fn wrapped_rows_join_and_hard_breaks_remain_newlines() -> Outcome {
    let mut parser = Parser::new(4, 5, 20)?;
    parser.process(b"abcdefgh\r\nijk")?;
    let grid = Grid::capture(parser.screen(), 0)?;
    assert_eq!(grid.text((0, 0), (2, 2))?, "abcdefgh\nijk");
    assert_eq!(grid.text((1, 0), (1, 4))?, "fgh");
    Ok(())
}
#[test]
fn history_capture_is_immutable_and_clamps_to_retained_rows() -> Outcome {
    let mut parser = Parser::new(3, 8, 2)?;
    parser.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive")?;
    let mark = parser.screen().mark();
    let grid = Grid::capture(parser.screen(), usize::MAX)?;
    assert_eq!(grid.offset, 2);
    assert_eq!(parser.screen().mark(), mark);
    assert_eq!(
        parser
            .screen()
            .window(0, 3, 8)
            .text((0, 0), (2, 7), 24, 100)?,
        "three\nfour\nfive"
    );
    assert_eq!(grid.text((0, 0), (2, 7))?, "one\ntwo\nthree");
    Ok(())
}
#[test]
fn clipped_viewports_never_select_a_backing_only_row_or_half_a_wide_glyph() -> Outcome {
    let mut parser = Parser::new(2, 2, 0)?;
    parser.process("界".as_bytes())?;
    let grid = Grid::capture(parser.screen(), 0)?.clip((1, 1))?;
    assert_eq!(grid.size, (1, 1));
    assert_eq!(grid.text((0, 0), (1, 1))?, "");
    assert!(Grid::capture(parser.screen(), 0)?.clip((0, 1)).is_err());
    Ok(())
}
#[test]
fn clipboard_policy_and_encoded_limit_are_explicit() -> Outcome {
    let mut settings = Settings::default();
    assert!(
        validate_clipboard(&settings, "x")
            .err()
            .need()?
            .contains("disabled")
    );
    settings.clipboard = ClipboardPolicy::WriteOnly;
    assert!(validate_clipboard(&settings, &"a".repeat(MAX_COPY_BYTES)).is_ok());
    assert!(validate_clipboard(&settings, &"a".repeat(MAX_COPY_BYTES + 1)).is_err());
    let mut parser = Parser::new(1, 40_000, 0)?;
    parser.process(
        "A\u{1d185}\u{1d185}\u{1d185}\u{1d185}\u{1d185}"
            .repeat(40_000)
            .as_bytes(),
    )?;
    assert!(
        Grid::capture(parser.screen(), 0)?
            .text((0, 0), (0, 39_999))
            .is_err()
    );
    Ok(())
}
fn selection(parser: &Parser, a: (u16, u16), b: (u16, u16)) -> Result<Selection, String> {
    let grid = Grid::capture(parser.screen(), 0)?;
    Ok(Selection {
        leaf: Entity::PLACEHOLDER,
        cursor: b,
        anchor: grid.identity(a),
        grid,
        revision: 0,
        dragging: false,
        mouse_origin: false,
    })
}
fn renew(selection: &mut Selection, parser: &Parser) -> Result<bool, String> {
    let (grid, cursor, invalid) =
        selection.renew(parser.screen(), selection.grid.offset, selection.grid.size)?;
    selection.grid = grid;
    selection.cursor = cursor;
    if invalid {
        selection.anchor = None;
    }
    Ok(invalid)
}
fn selected(selection: &Selection) -> Result<String, String> {
    selection.grid.text(
        selection.grid.position(selection.anchor.need()?).need()?,
        selection.cursor,
    )
}
#[test]
fn retained_selection_follows_scrolling_and_ignores_unrelated_output() -> Outcome {
    let mut parser = Parser::new(6, 12, 4)?;
    parser.process(b"AAA\r\nBBB\r\nCCC\r\nDDD\r\nEEE\r\nREADY")?;
    let mut selection = selection(&parser, (1, 0), (3, 2))?;
    parser.process(b"\x1b[6;1Hother")?;
    assert!(!renew(&mut selection, &parser)?);
    parser.process(b"\r\nnext\r\nlast")?;
    assert!(!renew(&mut selection, &parser)?);
    assert_eq!(selection.grid.offset, 2);
    assert_eq!(selected(&selection)?, "BBB\nCCC\nDDD");
    Ok(())
}
#[test]
fn span_versions_allow_unrelated_cells_and_styles_but_reject_overwritten_text() -> Outcome {
    let mut parser = Parser::new(3, 12, 2)?;
    parser.process(b"ABCdef")?;
    let mut selection = selection(&parser, (0, 0), (0, 2))?;
    parser.process(b"\x1b[1;10HX\x1b[H\x1b[31mABC")?;
    assert!(!renew(&mut selection, &parser)?);
    assert_eq!(selected(&selection)?, "ABC");
    parser.process(b"\x1b[1;2HZ")?;
    assert!(renew(&mut selection, &parser)?);
    Ok(())
}
#[test]
fn interior_row_loss_invalidates_even_when_both_endpoints_survive() -> Outcome {
    let mut parser = Parser::new(5, 12, 2)?;
    parser.process(b"AAA\r\nBBB\r\nCCC\r\nDDD\r\nEEE")?;
    let mut selection = selection(&parser, (0, 0), (4, 2))?;
    let first = *selection.grid.ids.first().need()?;
    let last = *selection.grid.ids.last().need()?;
    parser.process(b"\x1b[2;4r\x1b[3;1H\x1b[L")?;
    assert!(parser.screen().row_by_id(first).is_some());
    assert!(parser.screen().row_by_id(last).is_some());
    assert!(renew(&mut selection, &parser)?);
    Ok(())
}
#[test]
fn partial_scrolling_preserves_contiguous_retained_selected_rows() -> Outcome {
    let mut parser = Parser::new(5, 12, 2)?;
    parser.process(b"AAA\r\nBBB\r\nCCC\r\nDDD\r\nEEE")?;
    let mut selection = selection(&parser, (2, 0), (3, 2))?;
    parser.process(b"\x1b[2;5r\x1b[S")?;
    assert!(!renew(&mut selection, &parser)?);
    assert_eq!(selected(&selection)?, "CCC\nDDD");
    assert_eq!(selection.cursor, (2, 2));
    Ok(())
}
#[test]
fn eviction_of_unselected_top_is_safe_but_loss_of_required_rows_is_not() -> Outcome {
    let mut parser = Parser::new(3, 12, 2)?;
    parser.process(b"AAA\r\nBBB\r\nCCC")?;
    let mut safe = selection(&parser, (2, 0), (2, 2))?;
    let mut lost = selection(&parser, (0, 0), (0, 2))?;
    parser.process(b"\r\nDDD\r\nEEE\r\nFFF")?;
    assert!(!renew(&mut safe, &parser)?);
    assert_eq!(selected(&safe)?, "CCC");
    assert!(renew(&mut lost, &parser)?);
    Ok(())
}
#[test]
fn pane_removal_ends_copy_mode_with_a_notice() -> Outcome {
    let mut parser = Parser::new(3, 12, 2)?;
    parser.process(b"ABC")?;
    let mut world = World::new();
    let pane = world.spawn_empty().id();
    let leaf = world.spawn(PaneView { pane }).id();
    let mut selection = selection(&parser, (0, 0), (0, 2))?;
    selection.leaf = leaf;
    let id = world
        .spawn((
            Viewer {
                rows: 4,
                cols: 12,
                zoom: false,
                scrollback: 0,
                notice: None,
            },
            selection,
        ))
        .id();
    world.despawn(leaf);
    refresh_visible(&mut world, id, (3, 12));
    assert!(world.get::<Selection>(id).is_none());
    assert!(
        world
            .get::<Viewer>(id)
            .need()?
            .notice
            .as_ref()
            .need()?
            .text
            .contains("selection cleared: pane removed")
    );
    Ok(())
}

#[test]
fn browsing_resize_clipping_reset_and_buffer_switches_clear_anchors() -> Outcome {
    for bytes in [b"\x1bc".as_slice(), b"\x1b[?47h", b"\x1b[?1049h"] {
        let mut parser = Parser::new(3, 12, 2)?;
        parser.process(b"ABC")?;
        let mut selection = selection(&parser, (0, 0), (0, 2))?;
        parser.process(bytes)?;
        assert!(renew(&mut selection, &parser)?);
    }
    let mut parser = Parser::new(3, 12, 2)?;
    parser.process(b"OLD\r\nABC\r\nDEF\r\nGHI")?;
    let selection = selection(&parser, (0, 0), (0, 2))?;
    assert!(selection.renew(parser.screen(), 1, (3, 12))?.2);
    assert!(selection.renew(parser.screen(), 0, (2, 10))?.2);
    parser.resize(4, 12)?;
    assert!(selection.renew(parser.screen(), 0, (3, 12))?.2);
    Ok(())
}
