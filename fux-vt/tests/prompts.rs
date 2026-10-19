//! Prompt marks (OSC 133, `references/modern/osc133_semantic_prompts.md` and
//! iTerm2's "Shell Integration/FinalTerm"): `A` marks the row a prompt
//! starts on, after a fresh line; the mark goes with its row.

use fux_vt::{Feature, Options, Parser, Size};
#[path = "corpus/pieces.rs"]
mod pieces;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
const MARKS: Options = Options::new().with(Feature::PromptMarks);

/// The retained rows a prompt starts on, counted from the oldest.
fn marks(p: &Parser) -> Vec<usize> {
    let s = p.screen();
    s.rows()
        .filter(|r| r.starts_prompt())
        .map(|r| r.index())
        .collect()
}

fn text(p: &Parser, row: u16) -> String {
    let s = p.screen();
    (0..s.size().cols())
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
    let mut p = Parser::with_options(Size::new(8, 10)?, 10, MARKS)?;
    p.process(SESSION)?;
    assert_eq!(marks(&p), [0, 3, 4]);
    assert!(p.screen().starts_prompt(0) && !p.screen().starts_prompt(1));
    // B, C, D and a continuation prompt move nothing and mark nothing.
    let mut q = Parser::with_options(Size::new(8, 10)?, 10, MARKS)?;
    q.process(b"ab\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;1\x07\x1b]133;P;k=c\x07c")?;
    assert_eq!(marks(&q), Vec::<usize>::new());
    assert_eq!(text(&q, 0), "abc");
    // The same whatever pieces the output came in, and with every option.
    for size in [1, 2, 5] {
        let options = MARKS.with(Feature::Events).with(Feature::Hyperlinks);
        let mut q = Parser::with_options(Size::new(8, 10)?, 10, options)?;
        for piece in pieces::pieces(SESSION, size) {
            q.process(piece)?;
        }
        assert_eq!(marks(&q), marks(&p), "pieces of {size}");
    }
    Ok(())
}

/// `A` and `L` do a fresh line first: a new line unless the cursor is in
/// the first column (`references/modern/osc133_semantic_prompts.md`,
/// "Commands"; Ghostty does it so).
#[test]
fn a_and_l_start_a_fresh_line() -> Result {
    let mut p = Parser::with_options(Size::new(4, 10)?, 0, MARKS)?;
    p.process(b"out\x1b]133;A\x07$ \x1b]133;L\x07x\x1b]133;L\x07\x1b]133;L\x07y")?;
    assert_eq!(text(&p, 0), "out");
    assert_eq!(text(&p, 1), "$");
    assert_eq!(text(&p, 2), "x");
    assert_eq!(text(&p, 3), "y");
    assert_eq!(marks(&p), [1]);
    // At the bottom, the fresh line scrolls.
    let mut p = Parser::with_options(Size::new(2, 10)?, 1 << 10, MARKS)?;
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
    // Two rows of up to five cells: 24 bytes a row and 8 a cell.
    let mut p = Parser::with_options(Size::new(3, 6)?, 2 * 64, MARKS)?;
    p.process(b"\x1b]133;A\x07$ one\r\nout\r\n\x1b]133;A\x07$ two\r\nout\r\n\x1b]133;A\x07$")?;
    assert_eq!(marks(&p), [0, 2, 4]);
    // Past the history, the first row leaves.
    p.process(b"\r\nx\r\ny")?;
    assert_eq!(marks(&p), [0, 2]);
    // IL in the screen's rows moves the marks below it.
    let mut p = Parser::with_options(Size::new(4, 6)?, 0, MARKS)?;
    p.process(b"a\r\n\x1b]133;A\x07b\x1b[1;1H\x1b[L")?;
    assert_eq!(marks(&p), [2]);
    p.process(b"\x1b[M\x1b[M")?;
    assert_eq!(marks(&p), [0]);
    p.process(b"\x1b[M")?;
    assert_eq!(marks(&p), Vec::<usize>::new());
    // EL and ED 0 at the prompt keep it; ED 2 does not.
    let mut p = Parser::with_options(Size::new(3, 6)?, 0, MARKS)?;
    p.process(b"\x1b]133;A\x07$ ls\r\n\x1b]133;A\x07$ \r\x1b[K\x1b[J")?;
    assert_eq!(marks(&p), [0, 1]);
    p.process(b"\x1b[1;1H\x1b[J")?;
    assert_eq!(marks(&p), [0]);
    p.process(b"\x1b[2J")?;
    assert_eq!(marks(&p), Vec::<usize>::new());
    // RIS forgets them.
    let mut p = Parser::with_options(Size::new(3, 6)?, 0, MARKS)?;
    p.process(b"\x1b]133;A\x07\x1bc")?;
    assert_eq!(marks(&p), Vec::<usize>::new());
    Ok(())
}

/// A resize keeps the marks of the rows it keeps; a reflow puts each on
/// the row its prompt's first character goes to.
#[test]
fn marks_survive_resize_and_reflow() -> Result {
    let input = b"\x1b]133;A\x07$ long command\r\noutput\r\n\x1b]133;A\x07$ ";
    let mut p = Parser::with_options(Size::new(4, 20)?, 1 << 12, MARKS)?;
    p.process(input)?;
    p.resize(Size::new(4, 6)?)?;
    assert_eq!(marks(&p), [0, 2]);
    let mut p = Parser::with_options(Size::new(4, 20)?, 1 << 12, MARKS.with(Feature::Reflow))?;
    p.process(input)?;
    // "$ long command" takes three rows of six.
    p.resize(Size::new(4, 6)?)?;
    assert_eq!(marks(&p), [0, 4]);
    p.resize(Size::new(4, 20)?)?;
    assert_eq!(marks(&p), [0, 2]);
    // A mark on a line's second row goes where that row's first cell goes:
    // narrower, its own row; wider, the line's first. A mark is a row's, so
    // from there on it is the first row's.
    let mut p = Parser::with_options(Size::new(3, 4)?, 10, MARKS.with(Feature::Reflow))?;
    p.process(b"abcde\r\x1b]133;A\x07")?;
    assert_eq!(marks(&p), [1]);
    p.resize(Size::new(3, 2)?)?;
    assert_eq!(marks(&p), [2]);
    p.resize(Size::new(3, 8)?)?;
    assert_eq!(marks(&p), [0]);
    p.resize(Size::new(3, 2)?)?;
    assert_eq!(marks(&p), [0]);
    Ok(())
}

/// Without `Feature::PromptMarks`, OSC 133 is ignored: no mark, no fresh
/// line, and no OSC payload kept.
#[test]
fn without_the_option_prompt_marks_are_ignored() -> Result {
    let mut p = Parser::new(Size::new(4, 10)?, 0)?;
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
    let mut p = Parser::with_options(Size::new(6, 10)?, 0, MARKS)?;
    p.process(b"x\x1b]133;N\x07$ ")?;
    assert_eq!(marks(&p), [1]);
    p.process(b"\r\n\x1b]133;P;k=c\x07> \x1b]133;P;k=r\x07")?;
    assert_eq!(marks(&p), [1], "continuation and right prompts");
    p.process(b"\r\nab\x1b]133;P;k=i\x07$ \r\n\x1b]133;P\x07$ ")?;
    assert_eq!(marks(&p), [1, 3, 4], "primary prompts, no fresh line");
    assert_eq!(text(&p, 3), "ab$");
    Ok(())
}

/// `N` "may first implicitly terminate a previous command"
/// (`references/modern/osc133_semantic_prompts.md`, "Commands"): with a
/// command running (`C`), it ends it as a `D` with no status would, then
/// starts a prompt as `A` does; with none running it ends nothing.
#[test]
fn n_ends_the_command_running() -> Result {
    let mut p = Parser::with_options(Size::new(6, 10)?, 0, MARKS)?;
    p.process(b"\x1b]133;N\x07")?;
    assert_eq!(p.screen().commands().finished, 0);
    p.process(b"$ ls\r\n\x1b]133;C\x07out\x1b]133;N\x07$ ")?;
    let commands = p.screen().commands();
    assert!(!commands.running());
    assert_eq!(commands.finished, 1);
    assert_eq!(commands.last_status, None);
    assert!(commands.last_output.is_some());
    assert!(commands.prompt);
    assert_eq!(marks(&p), [0, 2]);
    Ok(())
}

/// With a nonce required, a mark counts only if it carries it: one a
/// program prints (`cat` of a file with marks in it) is no mark.
#[test]
fn marks_without_the_nonce_are_ignored() -> Result {
    let mut p = Parser::with_options(Size::new(8, 20)?, 10, MARKS)?;
    p.require_mark_nonce(Some(b"k3y"), None);
    p.process(b"\x1b]133;A\x07$ \x1b]133;C\x07fake\r\n\x1b]133;D;9\x07")?;
    assert_eq!(marks(&p), Vec::<usize>::new());
    assert_eq!(p.screen().commands().finished, 0);
    p.process(b"\r\n\x1b]133;A;nonce=k3y\x07$ ")?;
    assert!(p.screen().commands().prompt);
    p.process(b"ls\r\n\x1b]133;C;nonce=k3y\x07a\r\n\x1b]133;D;0;nonce=k3y\x07")?;
    let commands = p.screen().commands();
    assert_eq!((commands.finished, commands.last_status), (1, Some(0)));
    assert!(!commands.prompt);
    Ok(())
}

/// A program the shell runs, given the nested nonce (a remote shell's
/// marks, relayed), marks commands within the shell's: they count while
/// the shell's command runs, on the alternate screen too, and the shell's
/// own end ends it, whatever the program left running. Its marks before
/// that command, and forged ones, are none.
#[test]
fn a_nested_programs_marks_count_within_the_shells_command() -> Result {
    let mut p = Parser::with_options(Size::new(8, 20)?, 10, MARKS)?;
    p.require_mark_nonce(Some(b"k3y"), Some(b"n3st"));
    p.process(b"\x1b]133;A;nonce=k3y\x07$ \x1b]133;C;nonce=n3st\x07\x1b]133;D;9;nonce=n3st\x07")?;
    assert_eq!(p.screen().commands().finished, 0, "no shell command runs");
    p.process(b"koh\r\n\x1b]133;C;nonce=k3y\x07\x1b[?1049h")?;
    p.process(b"\x1b]133;A;nonce=n3st\x07r$ \x1b]133;C;nonce=n3st\x07")?;
    let commands = p.screen().commands();
    assert!(commands.running() && commands.full_screen);
    p.process(b"\x1b]133;D;1\x07\x1b]133;D;1;nonce=k3y2\x07")?;
    assert_eq!(p.screen().commands().finished, 0, "forged");
    p.process(b"hi\r\n\x1b]133;D;1;nonce=n3st\x07\x1b]133;A;nonce=n3st\x07r$ ")?;
    let commands = p.screen().commands();
    assert_eq!((commands.finished, commands.last_status), (1, Some(1)));
    assert!(commands.prompt && !commands.running());
    // The program exits mid-command; the shell's end is the one that counts.
    p.process(b"\x1b]133;C;nonce=n3st\x07\x1b[?1049l\x1b]133;D;0;nonce=k3y\x07")?;
    let commands = p.screen().commands();
    assert_eq!((commands.finished, commands.last_status), (2, Some(0)));
    assert!(!commands.running() && !commands.full_screen);
    p.process(b"\x1b]133;A;nonce=k3y\x07$ \x1b]133;C;nonce=n3st\x07")?;
    assert!(!p.screen().commands().running(), "no shell command runs");
    Ok(())
}

/// A state report (`133;S;activity=STATE`) counts under the marks' nonce
/// rule, the nested nonce's only within the shell's command, and holds
/// until the next mark; one naming no state it knows is none, and a
/// report changes no command.
#[test]
fn a_state_report_holds_until_the_next_mark() -> Result {
    use fux_vt::Reported;
    let mut p = Parser::with_options(Size::new(8, 20)?, 10, MARKS)?;
    p.require_mark_nonce(Some(b"k3y"), Some(b"n3st"));
    p.process(b"\x1b]133;A;nonce=k3y\x07$ \x1b]133;S;activity=reading;nonce=n3st\x07")?;
    assert_eq!(
        p.screen().commands().reported(),
        None,
        "no shell command runs"
    );
    p.process(b"koh\r\n\x1b]133;C;nonce=k3y\x07\x1b[?1049h\x1b]133;C;nonce=n3st\x07")?;
    let before = p.screen().commands();
    p.process(b"\x1b]133;S;activity=reading\x07\x1b]133;S;activity=dozing;nonce=n3st\x07")?;
    assert_eq!(p.screen().commands().reported(), None, "unsigned, unknown");
    p.process(b"\x1b]133;S;nonce=n3st;activity=reading\x1b\\")?;
    let commands = p.screen().commands();
    assert_eq!(commands.reported(), Some(Reported::Reading));
    assert_eq!(
        (commands.started, commands.running()),
        (before.started, true)
    );
    p.process(b"\x1b]133;D;0;nonce=n3st\x07")?;
    assert_eq!(
        p.screen().commands().reported(),
        None,
        "a mark came after it"
    );
    p.process(b"\x1b]133;S;activity=busy;nonce=n3st\x07")?;
    assert_eq!(p.screen().commands().reported(), Some(Reported::Busy));
    Ok(())
}

/// A `C` while a command runs (a relay placing that command's start anew)
/// moves its start but begins no command: `started` counts those begun.
#[test]
fn a_start_while_a_command_runs_begins_none() -> Result {
    let mut p = Parser::with_options(Size::new(8, 20)?, 10, MARKS)?;
    p.process(b"\x1b]133;A\x07$ \x1b]133;C\x07out\r\n\x1b]133;C\x07")?;
    let commands = p.screen().commands();
    assert_eq!((commands.started, commands.finished), (1, 0));
    p.process(b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;C\x07")?;
    assert_eq!(p.screen().commands().started, 2);
    Ok(())
}

/// A late `C` (a line the shell could not run) starts the output after
/// the line last submitted, not after the prompt's first line: the
/// continuation prompts and the lines typed at them are not its output.
#[test]
fn a_late_start_follows_the_line_submitted_last() -> Result {
    let mut p = Parser::with_options(Size::new(8, 20)?, 10, MARKS)?;
    p.process(b"\x1b]133;A\x07$ for i in 1 2\r\n")?;
    p.process(b"> do echo $i\r\n> done)")?;
    p.note_submit();
    p.process(b"\r\nsyntax error\r\n\x1b]133;C;late\x07\x1b]133;D;2\x07")?;
    let (start, _) = p.screen().commands().last_output.ok_or("no output")?;
    let row = p.screen().row_index(start.row).ok_or("row gone")?;
    assert_eq!((row, start.col), (3, 0));
    assert_eq!(text(&p, 3), "syntax error");
    Ok(())
}

/// A late `C` looks for its prompt, and for the line submitted, on the
/// screen alone (threats.md B29): one whose prompt and line scrolled into
/// history starts its output at the cursor, where a `C` would, rather than
/// after a prompt it would read all history to find.
#[test]
fn a_late_start_past_the_screen_starts_at_the_cursor() -> Result {
    let mut p = Parser::with_options(Size::new(3, 20)?, 1 << 16, MARKS)?;
    p.process(b"\x1b]133;A\x07$ cat\r\n")?;
    p.note_submit();
    p.process(b"one\r\ntwo\r\nthree\r\nfour")?;
    p.process(b"\x1b]133;C;late\x07\x1b]133;D;0\x07")?;
    let (start, _) = p.screen().commands().last_output.ok_or("no output")?;
    let cursor = p.screen().window().row(2).map(|r| r.id());
    assert_eq!((Some(start.row), start.col), (cursor, 4));
    Ok(())
}

/// While recording, every run printed that changed cells is kept, with
/// its row and columns, though drawn over in the same output; a redraw
/// of the same text is none.
#[test]
fn prints_are_recorded_though_drawn_over() -> Result {
    let mut p = Parser::with_options(Size::new(4, 20)?, 10, Options::new())?;
    p.process(b"same")?;
    p.record_prints(true);
    p.process(b"\rsame\r\nREADY\rxxxxx\x1b[1mbo\x1b[0mld")?;
    let prints = p.take_prints();
    let row = |i: usize| p.screen().rows().nth(i).map(|r| r.id());
    let texts: Vec<(Option<fux_vt::RowId>, (u16, u16), &str)> = prints
        .iter()
        .map(|t| (Some(t.row), t.cols, t.text))
        .collect();
    assert_eq!(
        texts,
        [(row(1), (0, 5), "READY"), (row(1), (0, 9), "xxxxxbold")]
    );
    assert!(p.take_prints().is_empty());
    Ok(())
}

/// A run of ASCII drawn over cells that already hold some of it records
/// only the glyphs that changed cells, as printing them one at a time
/// does (Prints: "each run of text that changed cells"): `abX`, then
/// `abc` over it, is `abX` and `c`; `aXcY` over `abcd`, `X` and `Y`
/// (FA-TXT-2).
#[test]
fn an_ascii_run_records_only_the_glyphs_that_changed_cells() -> Result {
    for (input, want) in [
        (&b"abX\rabc"[..], &[((0, 3), "abX"), ((2, 3), "c")][..]),
        (
            b"abcd\raXcY",
            &[((0, 4), "abcd"), ((1, 2), "X"), ((3, 4), "Y")],
        ),
    ] {
        let mut p = Parser::with_options(Size::new(1, 8)?, 10, Options::new())?;
        p.record_prints(true);
        p.process(input)?;
        let prints = p.take_prints();
        let runs: Vec<((u16, u16), &str)> = prints.iter().map(|t| (t.cols, t.text)).collect();
        assert_eq!(runs, want, "{}", input.escape_ascii());
    }
    Ok(())
}

/// What is recorded may hold 4 bytes a cell of the screen it is printed
/// on: a resize with nothing recorded yet makes room for the new size, as
/// taking the prints does, and loses nothing (FA-PAR-5).
#[test]
fn a_resize_with_nothing_recorded_keeps_room_for_the_new_size() -> Result {
    let mut p = Parser::with_options(Size::new(10, 10)?, 10, Options::new())?;
    p.record_prints(true);
    p.resize(Size::new(200, 100)?)?;
    let prints = p.take_prints();
    assert_eq!((prints.cap(), prints.lost()), (4 * 200 * 100, false));
    Ok(())
}

/// `tick` numbers something between pieces of output: after every row's
/// number so far, before the next piece's.
#[test]
fn a_tick_falls_between_pieces() -> Result {
    let mut p = Parser::with_options(Size::new(4, 20)?, 10, Options::new())?;
    p.process(b"one")?;
    let first = p
        .screen()
        .rows()
        .next()
        .map(|r| r.version())
        .ok_or("no row")?;
    let tick = p.tick();
    p.process(b"\r\ntwo")?;
    let second = p
        .screen()
        .rows()
        .nth(1)
        .map(|r| r.version())
        .ok_or("no row")?;
    assert!(first < tick && tick < second);
    Ok(())
}

/// A row's prompt mark is part of what a host redraws from (`threats.md`
/// P12, section 6's invariant 28): marking a row, or taking its mark away,
/// gives it a new version, and the mark shows in `dirty_rows_since`; a
/// mark that stays as it was changes nothing.
#[test]
fn a_prompt_mark_changes_its_row_s_version() -> Result {
    let mut p = Parser::with_options(Size::new(4, 10)?, 0, MARKS)?;
    p.process(b"\x1b[2;1H")?;
    let version = |p: &Parser| p.screen().rows().nth(1).map(|r| r.version());
    let dirty = |p: &Parser, mark| {
        (p.screen().dirty_rows_since(mark))
            .map(|r| r.index())
            .collect::<Vec<_>>()
    };
    let (before, mark) = (version(&p), p.screen().mark());
    // A at the first column: no fresh line, the blank row marked.
    p.process(b"\x1b]133;A\x07")?;
    assert!(p.screen().starts_prompt(1));
    assert!(version(&p) > before, "marked with no new version");
    assert_eq!(dirty(&p, mark), [1]);
    // P with no `k`: the row marked again, as it was.
    let (before, mark) = (version(&p), p.screen().mark());
    p.process(b"\x1b]133;P\x07")?;
    assert_eq!(version(&p), before, "a mark as it was changed the row");
    assert_eq!(dirty(&p, mark), Vec::<usize>::new());
    // ED 2 takes it away from the blank row: a change of its own.
    let (before, mark) = (version(&p), p.screen().mark());
    p.process(b"\x1b[2J")?;
    assert!(!p.screen().starts_prompt(1));
    assert!(version(&p) > before, "unmarked with no new version");
    assert!(dirty(&p, mark).contains(&1));
    Ok(())
}
