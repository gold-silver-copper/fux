//! Shell integration (OSC 133, `references/modern/osc133_semantic_prompts.md`
//! and iTerm2's "Shell Integration/FinalTerm"): `A` starts a prompt after
//! a fresh line, and `C` and `D` mark each command and its output.

use fux_vt::{Event, Feature, Options, Parser, Sink, Size};

#[path = "corpus/feed.rs"]
#[expect(dead_code, reason = "each test binary uses some of the helpers")]
mod feed;
use feed::{Feed, Look};
#[path = "corpus/pieces.rs"]
mod pieces;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
const MARKS: Options = Options::new().with(Feature::PromptMarks);

fn text(p: &Parser, row: u16) -> String {
    let s = p.screen();
    (0..s.size().cols())
        .filter_map(|x| s.cell(row, x))
        .map(|c| {
            if c.contents().is_empty() {
                " "
            } else {
                c.contents()
            }
        })
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// A shell's commands: a prompt, the command, its output.
const SESSION: &[u8] =
    b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07a\r\nb\r\n\x1b]133;D;0\x07\
    \x1b]133;A;cl=m;aid=12\x1b\\$ \x1b]133;B\x1b\\true\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ";

#[test]
fn b_c_d_and_a_continuation_move_nothing_in_any_pieces() -> Result {
    let mut p = Parser::with_options(Size::new(8, 10)?, 10, MARKS)?;
    p.process(SESSION)?;
    // B, C, D and a continuation prompt move nothing.
    let mut q = Parser::with_options(Size::new(8, 10)?, 10, MARKS)?;
    q.process(b"ab\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;1\x07\x1b]133;P;k=c\x07c")?;
    assert_eq!(text(&q, 0), "abc");
    // The same whatever pieces the output came in, and with every option.
    for size in [1, 2, 5] {
        let options = MARKS.with(Feature::Events).with(Feature::Hyperlinks);
        let mut q = Parser::with_options(Size::new(8, 10)?, 10, options)?;
        for piece in pieces::pieces(SESSION, size) {
            q.process(piece)?;
        }
        for row in 0..8 {
            assert_eq!(text(&q, row), text(&p, row), "pieces of {size}");
        }
        assert_eq!(
            q.screen().commands(),
            p.screen().commands(),
            "pieces of {size}"
        );
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
    // At the bottom, the fresh line scrolls.
    let mut p = Parser::with_options(Size::new(2, 10)?, 1 << 10, MARKS)?;
    p.process(b"a\r\nb\x1b]133;A\x07$")?;
    assert_eq!((text(&p, 0), text(&p, 1)), ("b".to_owned(), "$".to_owned()));
    assert_eq!(p.screen().history_len(), 1);
    Ok(())
}

/// Without `Feature::PromptMarks`, OSC 133 is ignored: no fresh line, and
/// no OSC payload kept.
#[test]
fn without_the_option_prompt_marks_are_ignored() -> Result {
    let mut p = Parser::new(Size::new(4, 10)?, 0)?;
    p.process(b"ab\x1b]133;A\x07cd")?;
    assert_eq!(text(&p, 0), "abcd");
    Ok(())
}

/// `N` is `A` (`references/modern/osc133_semantic_prompts.md`): a fresh
/// line, then the prompt. `P`, of any kind, starts its prompt where the
/// cursor is, without a fresh line.
#[test]
fn n_is_a_and_p_starts_no_fresh_line() -> Result {
    let mut p = Parser::with_options(Size::new(6, 10)?, 0, MARKS)?;
    p.process(b"x\x1b]133;N\x07$ ")?;
    assert_eq!(text(&p, 1), "$");
    p.process(b"\r\n\x1b]133;P;k=c\x07> \x1b]133;P;k=r\x07")?;
    assert_eq!(text(&p, 2), ">", "continuation and right prompts");
    p.process(b"\r\nab\x1b]133;P;k=i\x07$ \r\n\x1b]133;P\x07$ ")?;
    assert_eq!(text(&p, 3), "ab$");
    assert_eq!(text(&p, 4), "$");
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
    assert_eq!(text(&p, 2), "$");
    Ok(())
}

/// A mark is where the next character goes: after output that fills its
/// row, pending its wrap, past the last column, so the output ends after
/// its last character.
#[test]
fn a_mark_after_a_full_row_is_past_its_last_column() -> Result {
    let mut p = Parser::with_options(Size::new(3, 5)?, 0, MARKS)?;
    p.process(b"\x1b]133;C\x07hello\x1b]133;D;0\x07")?;
    let (start, end) = p.screen().commands().last_output.ok_or("no output")?;
    assert_eq!((start.col, end.col), (0, 5));
    Ok(())
}

/// Each `C` that begins a command, each `D` that ends a running one and an
/// `N` that ends one are events, with `Feature::Events`, in the order of
/// the output with the others: the end with the status the shell gave.
#[test]
fn each_command_started_and_ended_is_an_event() -> Result {
    #[derive(Default)]
    struct Heard(Vec<String>);
    impl Sink for Heard {
        fn event(&mut self, event: Event<'_>) {
            self.0.push(match event {
                Event::CommandStarted => "started".to_owned(),
                Event::CommandEnded(status) => format!("ended:{status:?}"),
                Event::Bell => "bell".to_owned(),
                Event::Title(_)
                | Event::IconName(_)
                | Event::Notification(_)
                | Event::Clipboard { .. }
                | Event::ColorQuery { .. } => return,
            });
        }
    }
    let options = MARKS.with(Feature::Events);
    let mut p = Parser::with_options(Size::new(8, 20)?, 10, options)?;
    let mut heard = Heard::default();
    let two = b"\x1b]133;C\x07\x07a\r\n\x1b]133;D;1\x07\x1b]133;C\x07b\r\n\x1b]133;D;0\x07";
    p.process_with(two, &mut heard)?;
    assert_eq!(
        heard.0,
        [
            "started",
            "bell",
            "ended:Some(1)",
            "started",
            "ended:Some(0)"
        ]
    );
    // A `D` with none running ends nothing, a `C` while one runs begins
    // none, and an `N` ends one, with no status.
    heard.0.clear();
    let more = b"\x1b]133;D;3\x07\x1b]133;C\x07\x1b]133;C\x07\x1b]133;N\x07";
    p.process_with(more, &mut heard)?;
    assert_eq!(heard.0, ["started", "ended:None"]);
    Ok(())
}

/// A `C` while a command runs moves its start but begins no command:
/// `started` counts those begun.
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
