//! Prompt marks (OSC 133, `references/modern/osc133_semantic_prompts.md` and
//! iTerm2's "Shell Integration/FinalTerm"): `A` marks the row a prompt
//! starts on, after a fresh line; the mark goes with its row.

use fux_vt::{Feature, Options, Parser};
#[path = "corpus/pieces.rs"]
mod pieces;
type Result = std::result::Result<(), Box<dyn std::error::Error>>;
const MARKS: Options = Options::new().with(Feature::PromptMarks);

/// The retained rows a prompt starts on, counted from the oldest.
fn marks(p: &Parser) -> Vec<usize> {
    let s = p.screen();
    let retained = usize::from(s.size().0).saturating_add(s.history_len());
    (0..retained)
        .filter(|i| {
            let offset = retained.saturating_sub(i.saturating_add(1));
            s.row_from_bottom(offset).is_some_and(|r| r.starts_prompt())
        })
        .collect()
}

fn text(p: &Parser, row: u16) -> String {
    let s = p.screen();
    (0..s.size().1)
        .filter_map(|x| s.cell(row, x))
        .map(|c| if c.has_contents() { c.contents() } else { " " })
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// A shell's commands: a prompt, the command, its output.
const SESSION: &[u8] =
    b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07a\r\nb\r\n\x1b]133;D;0\x07\
    \x1b]133;A;cl=m;aid=12\x1b\\$ \x1b]133;B\x1b\\true\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ";

#[test]
fn a_prompt_marks_its_row_and_the_others_change_nothing() -> Result {
    let mut p = Parser::with_options(8, 10, 10, MARKS)?;
    p.process(SESSION)?;
    assert_eq!(marks(&p), [0, 3, 4]);
    assert!(p.screen().starts_prompt(0) && !p.screen().starts_prompt(1));
    // B, C, D and a continuation prompt move nothing and mark nothing.
    let mut q = Parser::with_options(8, 10, 10, MARKS)?;
    q.process(b"ab\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;1\x07\x1b]133;P;k=c\x07c")?;
    assert_eq!(marks(&q), Vec::<usize>::new());
    assert_eq!(text(&q, 0), "abc");
    // The same whatever pieces the output came in, and with every option.
    for size in [1, 2, 5] {
        let options = MARKS.with(Feature::Events).with(Feature::Hyperlinks);
        let mut q = Parser::with_options(8, 10, 10, options)?;
        for piece in pieces::pieces(SESSION, size) {
            q.process(piece)?;
        }
        assert_eq!(marks(&q), marks(&p), "pieces of {size}");
    }
    Ok(())
}

/// `A` and `L` do a fresh line first: a new line unless the cursor is in
/// the first column (the semantic prompts proposal; Ghostty does it so).
#[test]
fn a_and_l_start_a_fresh_line() -> Result {
    let mut p = Parser::with_options(4, 10, 0, MARKS)?;
    p.process(b"out\x1b]133;A\x07$ \x1b]133;L\x07x\x1b]133;L\x07\x1b]133;L\x07y")?;
    assert_eq!(text(&p, 0), "out");
    assert_eq!(text(&p, 1), "$");
    assert_eq!(text(&p, 2), "x");
    assert_eq!(text(&p, 3), "y");
    assert_eq!(marks(&p), [1]);
    // At the bottom, the fresh line scrolls.
    let mut p = Parser::with_options(2, 10, 5, MARKS)?;
    p.process(b"a\r\nb\x1b]133;A\x07$")?;
    assert_eq!(marks(&p), [2]);
    assert_eq!(text(&p, 1), "$");
    Ok(())
}

/// Marks go into history with their rows, move with IL and DL, leave with
/// the rows a scroll drops, and go when ED erases their row whole; EL, and
/// ED from the cursor, leave the cursor's row its mark, as a shell erases
/// from its prompt to redraw it.
#[test]
fn marks_go_with_their_rows() -> Result {
    let mut p = Parser::with_options(3, 6, 2, MARKS)?;
    p.process(b"\x1b]133;A\x07$ one\r\nout\r\n\x1b]133;A\x07$ two\r\nout\r\n\x1b]133;A\x07$")?;
    assert_eq!(marks(&p), [0, 2, 4]);
    // Past the history, the first row leaves.
    p.process(b"\r\nx\r\ny")?;
    assert_eq!(marks(&p), [0, 2]);
    // IL in the screen's rows moves the marks below it.
    let mut p = Parser::with_options(4, 6, 0, MARKS)?;
    p.process(b"a\r\n\x1b]133;A\x07b\x1b[1;1H\x1b[L")?;
    assert_eq!(marks(&p), [2]);
    p.process(b"\x1b[M\x1b[M")?;
    assert_eq!(marks(&p), [0]);
    p.process(b"\x1b[M")?;
    assert_eq!(marks(&p), Vec::<usize>::new());
    // EL and ED 0 at the prompt keep it; ED 2 does not.
    let mut p = Parser::with_options(3, 6, 0, MARKS)?;
    p.process(b"\x1b]133;A\x07$ ls\r\n\x1b]133;A\x07$ \r\x1b[K\x1b[J")?;
    assert_eq!(marks(&p), [0, 1]);
    p.process(b"\x1b[1;1H\x1b[J")?;
    assert_eq!(marks(&p), [0]);
    p.process(b"\x1b[2J")?;
    assert_eq!(marks(&p), Vec::<usize>::new());
    // RIS forgets them.
    let mut p = Parser::with_options(3, 6, 0, MARKS)?;
    p.process(b"\x1b]133;A\x07\x1bc")?;
    assert_eq!(marks(&p), Vec::<usize>::new());
    Ok(())
}

/// A resize keeps the marks of the rows it keeps; a reflow puts each on
/// the row its prompt's first character goes to.
#[test]
fn marks_survive_resize_and_reflow() -> Result {
    let input = b"\x1b]133;A\x07$ long command\r\noutput\r\n\x1b]133;A\x07$ ";
    let mut p = Parser::with_options(4, 20, 10, MARKS)?;
    p.process(input)?;
    p.resize(4, 6)?;
    assert_eq!(marks(&p), [0, 2]);
    let mut p = Parser::with_options(4, 20, 10, MARKS.with(Feature::Reflow))?;
    p.process(input)?;
    // "$ long command" takes three rows of six.
    p.resize(4, 6)?;
    assert_eq!(marks(&p), [0, 4]);
    p.resize(4, 20)?;
    assert_eq!(marks(&p), [0, 2]);
    // A mark on a line's second row goes where that row's first cell goes:
    // narrower, its own row; wider, the line's first. A mark is a row's, so
    // from there on it is the first row's.
    let mut p = Parser::with_options(3, 4, 10, MARKS.with(Feature::Reflow))?;
    p.process(b"abcde\r\x1b]133;A\x07")?;
    assert_eq!(marks(&p), [1]);
    p.resize(3, 2)?;
    assert_eq!(marks(&p), [2]);
    p.resize(3, 8)?;
    assert_eq!(marks(&p), [0]);
    p.resize(3, 2)?;
    assert_eq!(marks(&p), [0]);
    Ok(())
}

/// Without `Feature::PromptMarks`, OSC 133 is ignored: no mark, no fresh
/// line, and no OSC payload kept.
#[test]
fn without_the_option_prompt_marks_are_ignored() -> Result {
    let mut p = Parser::new(4, 10, 0)?;
    p.process(b"ab\x1b]133;A\x07cd")?;
    assert_eq!(marks(&p), Vec::<usize>::new());
    assert_eq!(text(&p, 0), "abcd");
    Ok(())
}

/// `N` is `A` (`references/modern/osc133_semantic_prompts.md`): a fresh
/// line, then the mark. `P` of the primary kind (`k=i`, or none) marks the
/// cursor's row without a fresh line; right-side and continuation prompts
/// mark nothing.
#[test]
fn n_is_a_and_a_primary_p_marks_its_row() -> Result {
    let mut p = Parser::with_options(6, 10, 0, MARKS)?;
    p.process(b"x\x1b]133;N\x07$ ")?;
    assert_eq!(marks(&p), [1]);
    p.process(b"\r\n\x1b]133;P;k=c\x07> \x1b]133;P;k=r\x07")?;
    assert_eq!(marks(&p), [1], "continuation and right prompts");
    p.process(b"\r\nab\x1b]133;P;k=i\x07$ \r\n\x1b]133;P\x07$ ")?;
    assert_eq!(marks(&p), [1, 3, 4], "primary prompts, no fresh line");
    assert_eq!(text(&p, 3), "ab$");
    Ok(())
}
