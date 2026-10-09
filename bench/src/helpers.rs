//! The programs `feel` runs, as subcommands of this binary (`fux-bench
//! __NAME`): the pane programs it measures with.
use std::io::Write;
use std::os::fd::AsFd;

/// What a pane program prints when it is ready, in raw mode: three
/// snowmen, which nothing else prints.
pub const READY: &str = "\u{2603}\u{2603}\u{2603}";

/// The glyph `__echo` prints for the key `b`: a circled letter, ⓐ for `a`
/// to ⓩ for `z`, which nothing else prints; `None` for any other byte.
pub fn circled(b: u8) -> Option<char> {
    let offset = b.checked_sub(b'a').filter(|o| *o < 26)?;
    char::from_u32(0x24d0_u32.saturating_add(u32::from(offset)))
}

/// The marker `__serve` prints after workload `n`: six of one glyph,
/// which none of the workloads has and which differs from the last one's,
/// so the client is painted all six (the end-to-end method of PR #78).
pub fn marker(n: usize) -> String {
    const GLYPHS: [char; 4] = ['\u{2603}', '\u{2605}', '\u{265e}', '\u{2691}'];
    let glyph = GLYPHS
        .get(n.checked_rem(GLYPHS.len()).unwrap_or(0))
        .copied()
        .unwrap_or('\u{2603}');
    std::iter::repeat_n(glyph, 6).collect()
}

/// Puts the terminal on stdin in raw mode.
fn raw() -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut modes = fuxix::terminal::attributes(stdin.as_fd()).map_err(|e| e.to_string())?;
    modes.make_raw();
    fuxix::terminal::set_attributes(stdin.as_fd(), &modes).map_err(|e| e.to_string())
}

fn out(bytes: &[u8]) -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(bytes).map_err(|e| e.to_string())?;
    stdout.flush().map_err(|e| e.to_string())
}

/// Reads stdin, a piece at a time, until it ends; `each` gets every piece.
fn each_read(mut each: impl FnMut(&[u8]) -> Result<(), String>) -> Result<bool, String> {
    let stdin = std::io::stdin();
    let mut buffer = [0u8; 4096];
    loop {
        match fuxix::io::read(stdin.as_fd(), &mut buffer) {
            Ok(0) | Err(_) => return Ok(true),
            Ok(n) => each(buffer.get(..n).unwrap_or_default())?,
        }
    }
}

/// `__echo`: in raw mode, answers each key `a` to `z` with a carriage
/// return and its circled letter, at once.
pub fn echo() -> Result<bool, String> {
    raw()?;
    out(format!("\x1b[2J\x1b[H{READY}\r\n").as_bytes())?;
    each_read(|keys| {
        let mut reply = Vec::new();
        for &key in keys {
            if let Some(glyph) = circled(key) {
                let mut buf = [0; 4];
                reply.push(b'\r');
                reply.extend_from_slice(glyph.encode_utf8(&mut buf).as_bytes());
            }
        }
        if reply.is_empty() {
            Ok(())
        } else {
            out(&reply)
        }
    })
}

/// `__flood`: lines of changing text, as fast as they are taken, until
/// the terminal goes away.
pub fn flood() -> Result<bool, String> {
    let mut block = Vec::with_capacity(1 << 16);
    let mut line = 0u64;
    let mut stdout = std::io::stdout().lock();
    loop {
        block.clear();
        while block.len() < 1 << 16 {
            line = line.wrapping_add(1);
            let _ = write!(
                block,
                "flood {line:>12} the quick brown fox jumps over the lazy dog {:>16x}\r\n",
                line.wrapping_mul(0x9e37_79b9_7f4a_7c15)
            );
        }
        if stdout.write_all(&block).is_err() || stdout.flush().is_err() {
            return Ok(true);
        }
    }
}

/// `__fill ROWS DONE`: `ROWS` lines filling a 120-column screen, then
/// creates the file `DONE`, and waits for its terminal to go away.
pub fn fill(argv: &[String]) -> Result<bool, String> {
    let [rows, done] = argv else {
        return Err("__fill ROWS DONE".into());
    };
    let rows: u64 = rows
        .parse()
        .map_err(|_| format!("{rows:?} is not a number"))?;
    let mut text = Vec::new();
    for row in 0..rows {
        let _ = write!(text, "{row:>8} ");
        let letters = (b'a'..=b'z')
            .cycle()
            .skip(usize::try_from(row % 26).unwrap_or(0));
        text.extend(letters.take(110));
        text.extend_from_slice(b"\r\n");
    }
    out(&text)?;
    std::fs::write(done, b"").map_err(|e| format!("{done}: {e}"))?;
    each_read(|_| Ok(()))
}

/// What `__serve` has read of a request: digits and the Enter ending them,
/// outside any sequence a terminal sends a program (its answers to the
/// queries in the workloads).
#[derive(Default)]
pub struct Requests {
    digits: String,
    state: Sequence,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Sequence {
    #[default]
    None,
    Escape,
    Csi,
    /// An OSC, DCS, APC, PM or SOS string, until BEL or ST.
    String,
    StringEscape,
}

impl Requests {
    /// Reads `bytes`; each request completed in them.
    pub fn read(&mut self, bytes: &[u8]) -> Vec<usize> {
        let mut done = Vec::new();
        for &b in bytes {
            self.state = match (self.state, b) {
                (Sequence::None, 0x1b) => Sequence::Escape,
                (Sequence::None, b'0'..=b'9') => {
                    self.digits.push(char::from(b));
                    Sequence::None
                }
                (Sequence::None, b'\r' | b'\n') => {
                    if let Ok(n) = self.digits.parse() {
                        done.push(n);
                    }
                    self.digits.clear();
                    Sequence::None
                }
                (Sequence::Escape, b'[') => Sequence::Csi,
                (Sequence::Escape, b']' | b'P' | b'_' | b'^' | b'X') => Sequence::String,
                (Sequence::Csi, 0x40..=0x7e) | (Sequence::Escape | Sequence::None, _) => {
                    Sequence::None
                }
                (Sequence::String, 0x07) | (Sequence::StringEscape, b'\\') => Sequence::None,
                (Sequence::String | Sequence::StringEscape, 0x1b) => Sequence::StringEscape,
                (Sequence::StringEscape, _) => Sequence::String,
                (state @ (Sequence::Csi | Sequence::String), _) => state,
            };
        }
        done
    }
}

/// `__serve LIST`: in raw mode, for each request `N` and Enter, resets
/// the terminal (RIS), writes the Nth file named in the file `LIST`, one
/// path a line, and then `marker(N)`. (A list, as the paths themselves
/// would make a command line longer than a terminal's line takes.)
pub fn serve(argv: &[String]) -> Result<bool, String> {
    let [list] = argv else {
        return Err("__serve LIST".into());
    };
    let list = std::fs::read_to_string(list).map_err(|e| format!("{list}: {e}"))?;
    let loads = list
        .lines()
        .map(|f| std::fs::read(f).map_err(|e| format!("{f}: {e}")))
        .collect::<Result<Vec<_>, _>>()?;
    raw()?;
    out(format!("\x1b[2J\x1b[H{READY}\r\n").as_bytes())?;
    let mut requests = Requests::default();
    each_read(|bytes| {
        for n in requests.read(bytes) {
            let Some(load) = loads.get(n) else {
                continue;
            };
            // One write, as `cat` of one file would make it: the PTY still
            // splits it into reads of its own.
            let mut all = Vec::with_capacity(load.len().saturating_add(32));
            all.extend_from_slice(b"\x1bc");
            all.extend_from_slice(load);
            all.extend_from_slice(marker(n).as_bytes());
            out(&all)?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn requests_are_read_around_a_terminals_answers() {
        let mut r = super::Requests::default();
        assert_eq!(r.read(b"12\r"), vec![12]);
        // DA1's answer, a cursor report, an OSC 11 answer and DECRQSS's.
        let answers = b"\x1b[?62;22c\x1b[2;2R\x1b]11;rgb:0000/0000/0000\x07\x1bP1$r0m\x1b\\";
        assert_eq!(r.read(answers), Vec::<usize>::new());
        assert_eq!(r.read(b"3"), Vec::<usize>::new());
        assert_eq!(r.read(b"\x1b[?1;2c4\r"), vec![34]);
    }

    #[test]
    fn markers_differ_from_one_workload_to_the_next() {
        assert_eq!(super::marker(0).chars().count(), 6);
        assert_ne!(super::marker(0), super::marker(1));
        assert_eq!(super::circled(b'a'), Some('\u{24d0}'));
        assert_eq!(super::circled(b'z'), Some('\u{24e9}'));
        assert_eq!(super::circled(b'A'), None);
    }
}
