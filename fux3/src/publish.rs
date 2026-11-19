//! Making a pane's files and writing them. The holder renders a screen
//! into bytes on its own thread; a publisher thread writes them, so a slow
//! or full disk never stops the holder answering its program or reading
//! `ctl`. Each screen file is replaced whole (written aside, then
//! renamed), never torn; `history` is appended.
use crate::files::{self, Head, MAX_SCREEN_FILE, Placed};
use fux_vt::sixel::Picture;
use fux_vt::{Attributes, Screen};
use std::io::Write;
use std::path::{Path, PathBuf};
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
    files: Option<Files>,
    history: Vec<u8>,
    history_lost: usize,
    new_images: Vec<(String, Arc<Picture>)>,
    gone_images: Vec<String>,
    stop: bool,
    /// Publishes handed over, and how many of them are written.
    handed: u64,
    written: u64,
    /// Whether the last publish written was written whole.
    written_ok: bool,
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
    /// Starts publishing into `dir`, `history` kept under `history_cap`
    /// bytes.
    pub fn start(dir: PathBuf, history_cap: usize) -> std::io::Result<Publisher> {
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
            if let Some(files) = publish.files {
                // What the replaced publish carried once is carried on.
                let old = work.files.take();
                let (old_errors, old_exit) = old.map_or((None, None), |f| (f.errors, f.exit));
                work.files = Some(Files {
                    errors: files.errors.or(old_errors),
                    exit: files.exit.or(old_exit),
                    ..files
                });
            }
            work.history.extend_from_slice(&publish.history);
            let over = work.history.len().saturating_sub(MAX_HISTORY_WAITING);
            if over > 0 {
                let kept = from_a_line(&work.history, over).to_vec();
                let lost = work.history.len().saturating_sub(kept.len());
                work.history = kept;
                work.history_lost = work.history_lost.saturating_add(lost);
            }
            work.new_images.extend(publish.new_images);
            for name in publish.gone_images {
                // One never written needs no removal: its write is dropped.
                let waiting = work.new_images.iter().any(|(n, _)| *n == name);
                work.new_images.retain(|(n, _)| *n != name);
                if !waiting {
                    work.gone_images.push(name);
                }
            }
        });
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

fn run(shared: &Shared, dir: &Path, history_cap: usize) {
    let mut history_len = std::fs::metadata(dir.join(files::HISTORY))
        .map_or(0, |m| usize::try_from(m.len()).unwrap_or(usize::MAX));
    // What a failed write gives back, tried again with the next publish.
    let (mut errors_left, mut gone_left) = (None, Vec::new());
    loop {
        let Ok(mut work) = shared.work.lock() else {
            return;
        };
        while work.files.is_none()
            && work.history.is_empty()
            && work.new_images.is_empty()
            && work.gone_images.is_empty()
            && !work.stop
        {
            work = match shared.wake.wait(work) {
                Ok(work) => work,
                Err(_) => return,
            };
        }
        let taken = Work {
            files: work.files.take(),
            history: std::mem::take(&mut work.history),
            history_lost: std::mem::take(&mut work.history_lost),
            new_images: std::mem::take(&mut work.new_images),
            gone_images: std::mem::take(&mut work.gone_images),
            stop: work.stop,
            handed: work.handed,
            written: 0,
            written_ok: false,
        };
        drop(work);
        let mut writer = Writer { dir, failure: None };
        let files = taken.files.map(|files| Files {
            errors: files.errors.or_else(|| errors_left.take()),
            ..files
        });
        let mut gone = std::mem::take(&mut gone_left);
        gone.extend(taken.gone_images);
        (errors_left, gone_left) = writer.publish(taken.new_images, files, gone);
        if !taken.history.is_empty() || taken.history_lost > 0 {
            history_len =
                writer.append_history(&taken.history, taken.history_lost, history_len, history_cap);
        }
        if let Ok(mut work) = shared.work.lock() {
            work.written = taken.handed;
            work.written_ok = writer.failure.is_none();
            shared.written.notify_all();
        }
        if taken.stop {
            return;
        }
    }
}

struct Writer<'a> {
    dir: &'a Path,
    failure: Option<String>,
}

impl Writer<'_> {
    fn note(&mut self, what: &str, error: impl std::fmt::Display) {
        if self.failure.is_none() {
            self.failure = Some(format!("{what}: {error}"));
        }
    }

    /// Writes `name` whole: aside, then renamed over the old one.
    fn replace(&mut self, name: &str, bytes: &[u8]) -> bool {
        let path = self.dir.join(name);
        let aside = self.dir.join(format!(".{}.new", name.replace('/', "-")));
        // A stale or planted file aside goes first; ours is made anew, so a
        // FIFO or a link put there can't block or redirect the write.
        let _ = std::fs::remove_file(&aside);
        let mut options = std::fs::File::options();
        options.write(true).create_new(true);
        let written = fuxix::file::private_file(&mut options)
            .open(&aside)
            .and_then(|mut file| file.write_all(bytes))
            .and_then(|()| std::fs::rename(&aside, &path));
        match written {
            Ok(()) => true,
            Err(e) => {
                let _ = std::fs::remove_file(&aside);
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
        images: Vec<(String, Arc<Picture>)>,
        files: Option<Files>,
        gone: Vec<String>,
    ) -> (Option<Vec<u8>>, Vec<String>) {
        // Pictures first, so a screen naming one finds its file.
        let mut pictures_written = true;
        for (name, picture) in images {
            let mut sixel = Vec::new();
            fux_vt::sixel::encode(
                &picture,
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
        }
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
            if let Some(exit) = &files.exit {
                self.replace(files::EXIT, exit);
            }
        }
        // Removed only once a styled screen no longer naming them is
        // written; until then, tried again with the next.
        if !styled_written {
            return (errors_left, gone);
        }
        for name in gone {
            let _ = std::fs::remove_file(self.dir.join(files::IMAGES).join(name));
        }
        (errors_left, Vec::new())
    }

    /// Appends to `history`. Each file holds at most half of `cap`: past
    /// it, `history` becomes `history.1` (the older one goes) and a new one
    /// starts, so a reader following `history` by name (`tail -F`) sees
    /// every line once, and the two files together stay under `cap`.
    fn append_history(&mut self, text: &[u8], lost: usize, len: usize, cap: usize) -> usize {
        let half = cap / 2;
        // What both files can't hold goes first, from a line on; then a line
        // saying what a slow disk lost, if anything, goes first of the rest.
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
            // The most that fits, to a line's end; a line longer than a
            // whole file is cut, at a character's start.
            let fits = rest.get(..room.min(rest.len())).unwrap_or_default();
            let piece = match fits.len() == rest.len() {
                true => fits,
                false => match fits.iter().rposition(|b| *b == b'\n') {
                    Some(at) => fits.get(..=at).unwrap_or(fits),
                    None if len == 0 => {
                        let end = (1..=fits.len())
                            .rev()
                            .find(|&at| rest.get(at).is_none_or(|b| b & 0xc0 != 0x80));
                        fits.get(..end.unwrap_or(fits.len())).unwrap_or(fits)
                    }
                    None => &[],
                },
            };
            if piece.is_empty() {
                match std::fs::rename(
                    self.dir.join(files::HISTORY),
                    self.dir.join(files::HISTORY_OLDER),
                ) {
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
                return std::fs::metadata(self.dir.join(files::HISTORY))
                    .map_or(len, |m| usize::try_from(m.len()).unwrap_or(usize::MAX));
            }
            len = len.saturating_add(piece.len());
            rest = rest.get(piece.len()..).unwrap_or_default();
        }
        len
    }

    fn append(&mut self, text: &[u8]) -> bool {
        let mut options = std::fs::File::options();
        options.append(true).create(true);
        let appended = fuxix::file::private_file(&mut options)
            .open(self.dir.join(files::HISTORY))
            .and_then(|mut file| file.write_all(text));
        match appended {
            Ok(()) => true,
            Err(e) => {
                self.note("appending to history", e);
                false
            }
        }
    }
}

/// `text` from the first line that starts at or after `at`.
fn from_a_line(text: &[u8], at: usize) -> &[u8] {
    if at == 0 {
        return text;
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
