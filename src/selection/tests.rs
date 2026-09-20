use super::*;
use crate::testing::*;

#[test]
fn unicode_selection_normalizes_wide_continuations_and_preserves_combining_marks()
-> crate::testing::Outcome {
    let mut parser = vt100::Parser::new(3, 12, 20);
    parser.process("A界e\u{301}Z".as_bytes());
    let grid = Grid::capture(parser.screen_mut(), 0)?;
    assert_eq!(grid.text((0, 2), (0, 3)), "界e\u{301}");
    assert_eq!(grid.text((0, 3), (0, 2)), "界e\u{301}");
    assert_eq!(grid.text((0, 0), (0, 4)), "A界e\u{301}Z");
    assert_eq!(grid.text((0, 1), (0, 1)), "界");
    Ok(())
}

#[test]
fn wrapped_rows_join_and_hard_breaks_remain_newlines() -> crate::testing::Outcome {
    let mut parser = vt100::Parser::new(4, 5, 20);
    parser.process(b"abcdefgh\r\nijk");
    let grid = Grid::capture(parser.screen_mut(), 0)?;
    assert_eq!(grid.text((0, 0), (2, 2)), "abcdefgh\nijk");
    assert_eq!(grid.text((1, 0), (1, 4)), "fgh");
    Ok(())
}

#[test]
fn history_capture_restores_live_offset_and_clamps_to_retained_rows() -> crate::testing::Outcome {
    let mut parser = vt100::Parser::new(3, 8, 2);
    parser.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
    let grid = Grid::capture(parser.screen_mut(), usize::MAX)?;
    assert_eq!(grid.offset, 2);
    assert_eq!(parser.screen().scrollback(), 0);
    assert_eq!(grid.text((0, 0), (2, 7)), "one\ntwo\nthree");
    Ok(())
}

#[test]
fn clipped_viewports_never_select_a_backing_only_row_or_half_a_wide_glyph()
-> crate::testing::Outcome {
    let mut parser = vt100::Parser::new(2, 2, 0);
    parser.process("界".as_bytes());
    let grid = Grid::capture(parser.screen_mut(), 0)?.clip((1, 1))?;
    assert_eq!(grid.size, (1, 1));
    assert_eq!(grid.text((0, 0), (1, 1)), "");
    assert!(Grid::capture(parser.screen_mut(), 0)?.clip((0, 1)).is_err());
    Ok(())
}

#[test]
fn clipboard_policy_and_encoded_limit_are_explicit() -> crate::testing::Outcome {
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
    Ok(())
}
