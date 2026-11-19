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
}

/// What the publisher says back.
#[derive(Default)]
pub struct Report {
    /// The last failure, until a publish succeeds.
    pub failure: Option<String>,
    /// Files written or replaced, ever.
    pub writes: u64,
    /// Bytes written, ever.
    pub bytes: u64,
}

struct Shared {
    work: Mutex<Work>,
    wake: Condvar,
    report: Mutex<Report>,
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
            report: Mutex::new(Report::default()),
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

    /// Hands over a publish: one not yet written is replaced, as only the
    /// newest screen matters.
    pub fn files(&self, files: Files) {
        self.with_work(|work| {
            let errors = files
                .errors
                .is_none()
                .then(|| work.files.take().and_then(|f| f.errors))
                .flatten();
            work.files = Some(Files {
                errors: files.errors.or(errors),
                ..files
            });
        });
    }

    /// Hands over text for `history`, kept in order.
    pub fn history(&self, text: &[u8]) {
        self.with_work(|work| {
            work.history.extend_from_slice(text);
            let over = work.history.len().saturating_sub(MAX_HISTORY_WAITING);
            if over > 0 {
                work.history = work.history.get(over..).unwrap_or_default().to_vec();
                work.history_lost = work.history_lost.saturating_add(over);
            }
        });
    }

    /// A picture to write once as `images/NAME`.
    pub fn image(&self, name: String, picture: Arc<Picture>) {
        self.with_work(|work| work.new_images.push((name, picture)));
    }

    /// A picture's file to remove, after the next screens.
    pub fn image_gone(&self, name: String) {
        self.with_work(|work| {
            work.new_images.retain(|(n, _)| *n != name);
            work.gone_images.push(name);
        });
    }

    /// What the publisher said last.
    pub fn report(&self) -> (Option<String>, u64, u64) {
        match self.shared.report.lock() {
            Ok(report) => (report.failure.clone(), report.writes, report.bytes),
            Err(_) => (Some("the publisher failed".to_owned()), 0, 0),
        }
    }

    /// Whether everything handed over has been written.
    pub fn idle(&self) -> bool {
        self.shared.work.lock().is_ok_and(|work| {
            work.files.is_none() && work.history.is_empty() && work.new_images.is_empty()
        })
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
        let taken = std::mem::take(&mut *work);
        work.stop = taken.stop;
        drop(work);
        let had_files = taken.files.is_some();
        let mut writer = Writer {
            dir,
            failure: None,
            writes: 0,
            bytes: 0,
        };
        writer.publish(taken.new_images, taken.files, taken.gone_images);
        if !taken.history.is_empty() || taken.history_lost > 0 {
            let mut text = Vec::new();
            if taken.history_lost > 0 {
                let lost = taken.history_lost;
                let _ = writeln!(
                    text,
                    "[fux3: {lost} bytes of history were lost: the disk was slow]"
                );
            }
            text.extend_from_slice(&taken.history);
            history_len = writer.append_history(&text, history_len, history_cap);
        }
        if let Ok(mut report) = shared.report.lock() {
            report.writes = report.writes.saturating_add(writer.writes);
            report.bytes = report.bytes.saturating_add(writer.bytes);
            if writer.failure.is_some() || had_files {
                report.failure = writer.failure;
            }
        }
        if taken.stop {
            return;
        }
    }
}

struct Writer<'a> {
    dir: &'a Path,
    failure: Option<String>,
    writes: u64,
    bytes: u64,
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
        let mut options = std::fs::File::options();
        options.write(true).create(true).truncate(true);
        let written = fuxix::file::private_file(&mut options)
            .open(&aside)
            .and_then(|mut file| file.write_all(bytes))
            .and_then(|()| std::fs::rename(&aside, &path));
        match written {
            Ok(()) => {
                self.writes = self.writes.saturating_add(1);
                let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                self.bytes = self.bytes.saturating_add(len);
                true
            }
            Err(e) => {
                let _ = std::fs::remove_file(&aside);
                self.note(&format!("writing {name}"), e);
                false
            }
        }
    }

    fn publish(
        &mut self,
        images: Vec<(String, Arc<Picture>)>,
        files: Option<Files>,
        gone: Vec<String>,
    ) {
        // Pictures first, so a screen naming one finds its file.
        for (name, picture) in images {
            let mut sixel = Vec::new();
            fux_vt::sixel::encode(
                &picture,
                (0, 0, picture.width(), picture.height()),
                &mut sixel,
            );
            self.replace(&format!("{}/{name}", files::IMAGES), &sixel);
        }
        if let Some(files) = files {
            for (name, bytes) in [
                (files::STYLED, &files.styled),
                (files::SCREEN, &files.screen),
            ] {
                if bytes.len() > MAX_SCREEN_FILE {
                    let why = format!("{} bytes, at most {MAX_SCREEN_FILE}", bytes.len());
                    self.note(&format!("writing {name}"), why);
                    continue;
                }
                self.replace(name, bytes);
            }
            if let Some(errors) = &files.errors {
                self.replace(files::ERRORS, errors);
            }
            // Status last, saying how the rest went: a failure here is
            // in the next one.
            let mut status = files.status;
            if let Some(failure) = &self.failure {
                let _ = writeln!(status, "publish failed: {failure}");
            }
            self.replace(files::STATUS, &status);
        }
        // Removed only once the screens no longer name them.
        for name in gone {
            let _ = std::fs::remove_file(self.dir.join(files::IMAGES).join(name));
        }
    }

    /// Appends to `history`; past `cap` it is replaced by its newer half,
    /// starting at a line.
    fn append_history(&mut self, text: &[u8], len: usize, cap: usize) -> usize {
        let path = self.dir.join(files::HISTORY);
        if len.saturating_add(text.len()) > cap {
            let mut all = std::fs::read(&path).unwrap_or_default();
            all.extend_from_slice(text);
            let keep = cap / 2;
            let cut = all.len().saturating_sub(keep);
            let start = all
                .get(cut..)
                .and_then(|rest| rest.iter().position(|b| *b == b'\n'))
                .map_or(cut, |at| cut.saturating_add(at).saturating_add(1));
            let kept = all.get(start..).unwrap_or_default().to_vec();
            return match self.replace(files::HISTORY, &kept) {
                true => kept.len(),
                false => len,
            };
        }
        let mut options = std::fs::File::options();
        options.append(true).create(true);
        let appended = fuxix::file::private_file(&mut options)
            .open(&path)
            .and_then(|mut file| file.write_all(text));
        match appended {
            Ok(()) => {
                self.writes = self.writes.saturating_add(1);
                let n = u64::try_from(text.len()).unwrap_or(u64::MAX);
                self.bytes = self.bytes.saturating_add(n);
                len.saturating_add(text.len())
            }
            Err(e) => {
                self.note("appending to history", e);
                len
            }
        }
    }
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
