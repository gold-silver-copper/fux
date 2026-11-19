//! Making a pane's files and writing them. The holder renders a screen
//! into bytes on its own thread; a publisher thread writes them, so a slow
//! or full disk never stops the holder answering its program or reading
//! `ctl`. Each screen file is replaced whole (written aside, then
//! renamed), never torn; `history` is appended. Every file is named
//! relative to the pane's directory, held open: the pane may be renamed
//! or moved while it is written.
use crate::files::{self, Head, MAX_SCREEN_FILE, Placed};
use fux_vt::sixel::Picture;
use fux_vt::{Attributes, Screen};
use fuxix::file::at;
use std::fs::File;
use std::io::Write;
use std::sync::{Arc, Condvar, Mutex};

/// The most history text waiting for the publisher; past it the oldest
/// waiting text goes, and a line in `history` says how much.
const MAX_HISTORY_WAITING: usize = 4 << 20;

/// The plain text of the screen: each row's text, trailing blanks gone.
pub fn plain(screen: &Screen, head: &Head) -> Vec<u8> {
    let mut out = head.line().into_bytes();
    out.push(b'\n');
    for (_, row) in screen.window().iter() {
        let text = row_text(&row);
        out.extend_from_slice(text.trim_end_matches(' ').as_bytes());
        out.push(b'\n');
    }
    out
}

/// A row's glyphs as shown, a column each (a wide glyph takes two).
fn row_text(row: &fux_vt::Row<'_>) -> String {
    let mut text = String::new();
    for cell in row.cells().filter(|c| !c.is_wide_continuation()) {
        text.push_str(cell.shown());
    }
    text
}

/// The screen in colour: each row's cells with the SGR changes between
/// them, built from the emulator's cells alone (never from the program's
/// bytes), so showing it in a terminal sets colours and nothing else. A
/// row ends with its pen back at the default. Then a line per picture.
pub fn styled(screen: &Screen, head: &Head, pictures: &[Placed]) -> Vec<u8> {
    let mut out = head.line().into_bytes();
    out.push(b'\n');
    for (_, row) in screen.window().iter() {
        let cells: Vec<_> = row.cells().collect();
        let used = cells
            .iter()
            .rposition(|c| !c.contents().is_empty() || c.attributes() != Attributes::default())
            .map_or(0, |last| last.saturating_add(1));
        let mut pen = Attributes::default();
        for cell in cells
            .iter()
            .take(used)
            .filter(|c| !c.is_wide_continuation())
        {
            cell.attributes().write_sgr_change(pen, &mut out, true);
            pen = cell.attributes();
            out.extend_from_slice(cell.shown().as_bytes());
        }
        Attributes::default().write_sgr_change(pen, &mut out, true);
        out.push(b'\n');
    }
    for picture in pictures {
        out.extend_from_slice(picture.line().as_bytes());
        out.push(b'\n');
    }
    out
}

/// One publish: the three screen files, whole.
pub struct Files {
    pub screen: Vec<u8>,
    pub styled: Vec<u8>,
    pub status: Vec<u8>,
    pub errors: Option<Vec<u8>>,
    /// The program's end, written once, after the screens that show it.
    pub exit: Option<Vec<u8>>,
}

/// One publish, as the holder hands it over.
#[derive(Default)]
pub struct Publish {
    pub files: Option<Files>,
    pub history: Vec<u8>,
    pub new_images: Vec<(String, Arc<Picture>)>,
    pub gone_images: Vec<String>,
}

/// What waits for the publisher.
#[derive(Default)]
struct Work {
    /// Every publish handed over and not yet taken, as one.
    waiting: Publish,
    history_lost: usize,
    stop: bool,
    /// Publishes handed over, and how many of them are written.
    handed: u64,
    written: u64,
    /// Whether the last publish written was written whole.
    written_ok: bool,
    /// Whether `exit` is on disk.
    exit_done: bool,
}

struct Shared {
    work: Mutex<Work>,
    written: Condvar,
    wake: Condvar,
}

/// The publisher thread, and the way to hand it work.
pub struct Publisher {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Publisher {
    /// Starts publishing into the directory held open as `dir`, `history`
    /// kept under `history_cap` bytes.
    pub fn start(dir: File, history_cap: usize) -> std::io::Result<Publisher> {
        let shared = Arc::new(Shared {
            work: Mutex::new(Work::default()),
            wake: Condvar::new(),
            written: Condvar::new(),
        });
        let theirs = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("publish".to_owned())
            .spawn(move || run(&theirs, &dir, history_cap))?;
        Ok(Publisher {
            shared,
            thread: Some(thread),
        })
    }

    fn with_work(&self, change: impl FnOnce(&mut Work)) {
        if let Ok(mut work) = self.shared.work.lock() {
            change(&mut work);
            self.shared.wake.notify_one();
        }
    }

    /// Hands over one publish, whole: screens not yet written are replaced
    /// by these, history text is kept in order, a picture both waiting to be
    /// written and gone is never written.
    pub fn hand(&self, publish: Publish) {
        self.with_work(|work| {
            work.handed = work.handed.saturating_add(1);
            let waiting = &mut work.waiting;
            if let Some(files) = publish.files {
                // What the replaced publish carried once is carried on.
                let old = waiting.files.take();
                let (old_errors, old_exit) = old.map_or((None, None), |f| (f.errors, f.exit));
                waiting.files = Some(Files {
                    errors: files.errors.or(old_errors),
                    exit: files.exit.or(old_exit),
                    ..files
                });
            }
            waiting.history.extend_from_slice(&publish.history);
            let over = waiting.history.len().saturating_sub(MAX_HISTORY_WAITING);
            if over > 0 {
                let kept = from_a_line(&waiting.history, over).to_vec();
                let lost = waiting.history.len().saturating_sub(kept.len());
                waiting.history = kept;
                work.history_lost = work.history_lost.saturating_add(lost);
            }
            waiting.new_images.extend(publish.new_images);
            for name in publish.gone_images {
                // One never written needs no removal: its write is dropped.
                let unwritten = waiting.new_images.iter().any(|(n, _)| *n == name);
                waiting.new_images.retain(|(n, _)| *n != name);
                if !unwritten {
                    waiting.gone_images.push(name);
                }
            }
        });
    }

    /// Whether the `exit` file is on disk.
    pub fn exit_done(&self) -> bool {
        self.shared.work.lock().is_ok_and(|work| work.exit_done)
    }

    /// Waits, at most `most`, until every publish handed over is written:
    /// whether it was, whole.
    pub fn wait_written(&self, most: std::time::Duration) -> bool {
        let Ok(work) = self.shared.work.lock() else {
            return false;
        };
        let target = work.handed;
        match self
            .shared
            .written
            .wait_timeout_while(work, most, |w| w.written < target)
        {
            Ok((work, timeout)) => !timeout.timed_out() && work.written_ok,
            Err(_) => false,
        }
    }

    /// Writes what waits, then stops; later work is never written.
    pub fn stop(&mut self) {
        self.with_work(|work| work.stop = true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(shared: &Shared, dir: &File, history_cap: usize) {
    let mut history_len = history_len(dir, 0);
    // Pictures not yet on disk: no screen naming one is whole until it is.
    let mut images_left: Vec<(String, Arc<Picture>)> = Vec::new();
    // What a failed write gives back, tried again with the next publish.
    let (mut errors_left, mut gone_left) = (None, Vec::new());
    loop {
        let Ok(mut work) = shared.work.lock() else {
            return;
        };
        let idle = |work: &Work| {
            let waiting = &work.waiting;
            let images = waiting.new_images.is_empty() && waiting.gone_images.is_empty();
            waiting.files.is_none() && waiting.history.is_empty() && images && !work.stop
        };
        while idle(&work) {
            work = match shared.wake.wait(work) {
                Ok(work) => work,
                Err(_) => return,
            };
        }
        let taken = std::mem::take(&mut work.waiting);
        let lost = std::mem::take(&mut work.history_lost);
        let (stop, handed) = (work.stop, work.handed);
        drop(work);
        let mut writer = Writer {
            dir,
            failure: None,
            exit_done: false,
        };
        let files = taken.files.map(|files| Files {
            errors: files.errors.or_else(|| errors_left.take()),
            ..files
        });
        let mut gone = std::mem::take(&mut gone_left);
        gone.extend(taken.gone_images);
        images_left.extend(taken.new_images);
        images_left.retain(|(name, _)| !gone.contains(name));
        // History before the screens: a line that scrolled off is in
        // `history` before a screen without it is, so a reader of `screen`
        // and then `history` may see it twice and never misses it.
        if !taken.history.is_empty() || lost > 0 {
            history_len = writer.append_history(&taken.history, lost, history_len, history_cap);
        }
        (errors_left, gone_left) = writer.publish(&mut images_left, files, gone);
        if let Ok(mut work) = shared.work.lock() {
            work.written = handed;
            work.written_ok = writer.failure.is_none();
            work.exit_done |= writer.exit_done;
            shared.written.notify_all();
        }
        if stop {
            return;
        }
    }
}

/// `history`'s length, or `known` if it can't be asked.
fn history_len(dir: &File, known: usize) -> usize {
    at::open(dir, files::HISTORY, at::READ | fuxix::file::NONBLOCK)
        .and_then(|f| f.metadata())
        .map_or(known, |m| usize::try_from(m.len()).unwrap_or(usize::MAX))
}

struct Writer<'a> {
    dir: &'a File,
    failure: Option<String>,
    exit_done: bool,
}

impl Writer<'_> {
    fn note(&mut self, what: &str, error: impl std::fmt::Display) {
        if self.failure.is_none() {
            self.failure = Some(format!("{what}: {error}"));
        }
    }

    /// Writes `name` whole: aside, then renamed over the old one.
    fn replace(&mut self, name: &str, bytes: &[u8]) -> bool {
        let aside = format!(".{}.new", name.replace('/', "-"));
        // A stale or planted file aside goes first; ours is made anew, so a
        // FIFO or a link put there can't block or redirect the write.
        let _ = at::remove_file(self.dir, &aside);
        let written = at::open(self.dir, &aside, at::WRITE | at::NEW)
            .and_then(|mut file| file.write_all(bytes))
            .and_then(|()| at::rename(self.dir, &aside, self.dir, name));
        match written {
            Ok(()) => true,
            Err(e) => {
                let _ = at::remove_file(self.dir, &aside);
                self.note(&format!("writing {name}"), e);
                false
            }
        }
    }

    /// Writes one publish: pictures, then the screens naming them, then
    /// `errors` and `status`, then removes pictures no screen names. What
    /// could not be written and must not be lost is given back: `errors`,
    /// and pictures to remove once a styled screen is written.
    fn publish(
        &mut self,
        images: &mut Vec<(String, Arc<Picture>)>,
        files: Option<Files>,
        gone: Vec<String>,
    ) -> (Option<Vec<u8>>, Vec<String>) {
        // Pictures first, so a screen naming one finds its file; one not
        // written is tried again with the next publish.
        let mut pictures_written = true;
        images.retain(|(name, picture)| {
            let mut sixel = Vec::new();
            fux_vt::sixel::encode(
                picture,
                (0, 0, picture.width(), picture.height()),
                &mut sixel,
            );
            let wrote = match sixel.len() <= files::MAX_IMAGE_FILE {
                true => self.replace(&format!("{}/{name}", files::IMAGES), &sixel),
                false => {
                    let why = format!("{} bytes, at most {}", sixel.len(), files::MAX_IMAGE_FILE);
                    self.note(&format!("writing picture {name}"), why);
                    false
                }
            };
            pictures_written &= wrote;
            !wrote
        });
        let mut styled_written = false;
        let mut errors_left = None;
        if let Some(files) = files {
            // A styled screen naming a picture not written is not shown.
            if !pictures_written {
                self.note(
                    "writing screen.styled",
                    "a picture it names was not written",
                );
            } else if files.styled.len() > MAX_SCREEN_FILE {
                let why = format!("{} bytes, at most {MAX_SCREEN_FILE}", files.styled.len());
                self.note("writing screen.styled", why);
            } else {
                styled_written = self.replace(files::STYLED, &files.styled);
            }
            if files.screen.len() > MAX_SCREEN_FILE {
                let why = format!("{} bytes, at most {MAX_SCREEN_FILE}", files.screen.len());
                self.note("writing screen", why);
            } else {
                self.replace(files::SCREEN, &files.screen);
            }
            if let Some(errors) = files.errors
                && !self.replace(files::ERRORS, &errors)
            {
                errors_left = Some(errors);
            }
            // Status last, saying how the rest went: a failure here is
            // in the next one.
            let mut status = files.status;
            if let Some(failure) = &self.failure {
                let _ = writeln!(status, "publish failed: {failure}");
            }
            self.replace(files::STATUS, &status);
            // Only beside screens written whole: else tried with the next.
            if let Some(exit) = &files.exit
                && self.failure.is_none()
            {
                self.exit_done = self.replace(files::EXIT, exit);
                // `status` was written saying all went well: it is
                // written again, saying this did not.
                if let Some(failure) = &self.failure {
                    let _ = writeln!(status, "publish failed: {failure}");
                    self.replace(files::STATUS, &status);
                }
            }
        }
        // Removed only once a styled screen no longer naming them is
        // written; until then, tried again with the next.
        if !styled_written {
            return (errors_left, gone);
        }
        for name in gone {
            let _ = at::remove_file(self.dir, format!("{}/{name}", files::IMAGES));
        }
        (errors_left, Vec::new())
    }

    /// Appends to `history` what the holder hands over: whole records,
    /// each a line of at most half of `cap` ([`record`]). Each file holds
    /// at most half of `cap`: a record that does not fit starts a new one,
    /// `history` becoming `history.1` (the older one goes). So rotation
    /// happens only between records, a reader following `history` by name
    /// (`tail -F`) sees every record once, and the two files together
    /// stay under `cap`.
    fn append_history(&mut self, text: &[u8], lost: usize, len: usize, cap: usize) -> usize {
        let half = cap / 2;
        // What both files can't hold goes first, from a record on; then a
        // record saying what a slow disk lost, if anything.
        let kept = from_a_line(text, text.len().saturating_sub(cap));
        let mut marked = Vec::new();
        if lost > 0 {
            let _ = writeln!(
                marked,
                "[fux3: {lost} bytes of history were lost: the disk was slow]"
            );
        }
        marked.extend_from_slice(kept);
        let mut rest = marked.as_slice();
        let mut len = len;
        while !rest.is_empty() {
            let room = half.saturating_sub(len);
            let fits = rest.get(..room.min(rest.len())).unwrap_or_default();
            // The records that fit whole; into an empty file, whatever
            // fits (no record is longer, so this cuts none).
            let piece = match fits.iter().rposition(|b| *b == b'\n') {
                Some(end) => fits.get(..=end).unwrap_or(fits),
                None if len == 0 => fits,
                None => &[],
            };
            if piece.is_empty() {
                match at::rename(self.dir, files::HISTORY, self.dir, files::HISTORY_OLDER) {
                    Ok(()) => len = 0,
                    Err(e) => {
                        self.note("rotating history", e);
                        return len;
                    }
                }
                continue;
            }
            if !self.append(piece) {
                // A part written counts: the file's length says how much.
                return history_len(self.dir, len);
            }
            len = len.saturating_add(piece.len());
            rest = rest.get(piece.len()..).unwrap_or_default();
        }
        len
    }

    fn append(&mut self, text: &[u8]) -> bool {
        // Without waiting, and only into a regular file: a FIFO put there
        // can't stall the publisher.
        let flags = at::WRITE | at::APPEND | at::CREATE | fuxix::file::NONBLOCK;
        let appended = at::open(self.dir, files::HISTORY, flags).and_then(|mut file| {
            let meta = file.metadata()?;
            if !meta.is_file() {
                return Err(std::io::Error::other("history is not a regular file"));
            }
            // A part of a record written before the disk filled is taken
            // back: the file holds whole records only.
            file.write_all(text)
                .inspect_err(|_| drop(file.set_len(meta.len())))
        });
        match appended {
            Ok(()) => true,
            Err(e) => {
                self.note("appending to history", e);
                false
            }
        }
    }
}

/// What ends a piece of a history line too long to be one record.
pub const CONTINUED: &str = " [fux3: continued]";

/// Adds `fragment` to the history line being gathered in `line`, and
/// moves every finished record to `out`. A record is a line, its newline
/// included, of at most `limit` bytes; a line longer than that becomes
/// pieces, each ending with [`CONTINUED`] before its newline, so taking
/// that mark and the newline after it out gives the line back. `end`
/// says the line ends with this fragment.
pub fn record(line: &mut String, out: &mut Vec<u8>, fragment: &str, end: bool, limit: usize) {
    line.push_str(fragment);
    let most = limit
        .saturating_sub(CONTINUED.len().saturating_add(1))
        .max(4);
    while line.len() >= limit {
        let cut = line.floor_char_boundary(most);
        let (piece, tail) = line.split_at_checked(cut).unwrap_or((line, ""));
        out.extend_from_slice(piece.as_bytes());
        out.extend_from_slice(CONTINUED.as_bytes());
        out.push(b'\n');
        *line = tail.to_owned();
    }
    if end {
        out.extend_from_slice(line.as_bytes());
        // A line that itself ends with the mark's text is written as a
        // piece with nothing after it, so that taking marks out gives it
        // back too.
        if line.ends_with(CONTINUED) {
            out.extend_from_slice(CONTINUED.as_bytes());
            out.push(b'\n');
        }
        out.push(b'\n');
        line.clear();
    }
}

/// `text` from the first line that starts at or after `at`.
fn from_a_line(text: &[u8], at: usize) -> &[u8] {
    if at == 0 || text.get(at.saturating_sub(1)) == Some(&b'\n') {
        return text.get(at..).unwrap_or_default();
    }
    let start = (text
        .get(at..)
        .and_then(|rest| rest.iter().position(|b| *b == b'\n')))
    .map_or(text.len(), |n| at.saturating_add(n).saturating_add(1));
    text.get(start..).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fux_vt::{Parser, Size};

    fn screen_of(bytes: &[u8]) -> Result<Parser, String> {
        let size = Size::new(4, 20).map_err(|e| format!("{e:?}"))?;
        let mut parser = Parser::with_options(size, 4096, fux_vt::Options::new())
            .map_err(|e| format!("{e:?}"))?;
        parser.process_all(bytes, &mut ());
        Ok(parser)
    }

    fn head() -> Head {
        Head {
            id: std::iter::repeat_n('0', 16).collect(),
            version: 1,
            cols: 20,
            rows: 4,
            cursor_row: 0,
            cursor_col: 0,
            cursor_shown: true,
        }
    }

    /// Taking the marks out of history's records gives every line back: a
    /// long one cut into pieces, and one that itself ends with the mark.
    #[test]
    fn history_records_give_their_lines_back() {
        let long: String = std::iter::repeat_n("é", 300).collect();
        let literal = format!("alpha{CONTINUED}");
        let (mut line, mut out) = (String::new(), Vec::new());
        for text in [long.as_str(), literal.as_str(), "", "beta"] {
            record(&mut line, &mut out, text, true, 64);
        }
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(text.lines().all(|record| record.len() < 64), "{text}");
        let back = text.replace(&format!("{CONTINUED}\n"), "");
        assert_eq!(back, format!("{long}\n{literal}\n\nbeta\n"));
    }

    /// Whatever a program prints, the styled screen holds no control but
    /// SGR sequences and newlines: shown in a terminal it can set colours
    /// and nothing else.
    #[test]
    fn a_hostile_program_cannot_reach_the_reader() -> Result<(), String> {
        let hostile = b"\x1b]0;owned\x07\x1b[6n\x1b]52;c;aGk=\x07\x1bP+q544e\x1b\\\
            \xc2\x9b31m\x1b[31mred\x1b[0m \x07\x1b[?1049h\x1b[2Jalt\x1b[?1049l";
        let parser = screen_of(hostile)?;
        for bytes in [
            styled(parser.screen(), &head(), &[]),
            plain(parser.screen(), &head()),
        ] {
            let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
            let mut rest = text.as_str();
            while let Some(at) = rest.find(|c: char| c.is_control() && c != '\n') {
                let tail = rest.get(at..).unwrap_or_default();
                let sgr = tail.strip_prefix("\x1b[").and_then(|t| {
                    let end = t.find(|c: char| !(c.is_ascii_digit() || c == ';' || c == ':'))?;
                    t.get(end..)?.strip_prefix('m')
                });
                match sgr {
                    Some(after) => rest = after,
                    None => return Err(format!("a control that is no SGR: {tail:?}")),
                }
            }
            assert!(text.contains("red"));
        }
        Ok(())
    }
}
