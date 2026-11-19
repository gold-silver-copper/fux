//! A pane as the viewer sees it: only its files. `status` and
//! `screen.styled` are looked at by their metadata (each publish renames a
//! new file in, so its inode changes) and read again only when that
//! changed; keys go to `in`, commands to `ctl`, a paste through a private
//! file that `ctl`'s `paste` names.
use crate::layout::{Live, PaneRef};
use crate::render::Cell;
use fux3::Failure;
use fux3::files::{self, Head, Placed};
use fuxix::Errno;
use std::collections::VecDeque;
use std::fs::File;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// The most bytes of keys waiting for a pane's `in`.
const MAX_QUEUED: usize = 64 << 10;
/// The largest `status` read.
const MAX_STATUS: usize = 64 << 10;

/// What changes when a file is replaced: its inode, size and time.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp(u64, u64, i64, i64);

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some(Stamp(
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
    ))
}

/// A published screen, as cells.
pub struct Screen {
    pub head: Head,
    pub rows: Vec<Vec<Cell>>,
    pub pictures: Vec<Placed>,
}

pub struct Pane {
    pub name: String,
    pub dir: PathBuf,
    pub status: Vec<(String, String)>,
    pub screen: Option<Screen>,
    stamps: (Option<Stamp>, Option<Stamp>),
    parser: Option<fux_vt::Parser>,
    input: Option<File>,
    queued: VecDeque<u8>,
}

/// Opens a FIFO of `dir` to write, without waiting: refused if it is no
/// FIFO or nobody reads it (the holder is gone).
fn open_fifo(path: &Path) -> Result<File, Failure> {
    let file = File::options()
        .write(true)
        .custom_flags(fuxix::file::NOFOLLOW | fuxix::file::NONBLOCK | fuxix::file::NOCTTY)
        .open(path)
        .map_err(|e| fux3::io(path.display(), e))?;
    let fifo = file.metadata().is_ok_and(|m| m.file_type().is_fifo());
    match fifo {
        true => Ok(file),
        false => Err(Failure::new(format!("{} is not a FIFO", path.display()))),
    }
}

/// Writes one command line to the `ctl` of the pane in `dir`, whole or not
/// at all.
pub fn ctl_line(dir: &Path, line: &str) -> Result<(), Failure> {
    let mut bytes = line.trim_end_matches('\n').as_bytes().to_vec();
    bytes.push(b'\n');
    if bytes.len() > files::PIPE_BUF
        || line.contains('\n') && line.trim_end_matches('\n').contains('\n')
    {
        return Err(Failure::new(format!(
            "a ctl line is one line of at most {} bytes",
            files::PIPE_BUF
        )));
    }
    let path = dir.join(files::CTL);
    let file = open_fifo(&path)?;
    match fuxix::io::write(&file, &bytes) {
        Ok(n) if n == bytes.len() => Ok(()),
        Ok(_) | Err(Errno::AGAIN) => Err(Failure::new(format!(
            "{} is full: the holder is not reading it",
            path.display()
        ))),
        Err(errno) => Err(fux3::io(path.display(), errno)),
    }
}

/// `status` read now, past any look's.
pub fn read_status(dir: &Path) -> Result<Vec<(String, String)>, Failure> {
    let bytes = files::read_small(&dir.join(files::STATUS), MAX_STATUS)?;
    Ok(files::parse_status(&String::from_utf8_lossy(&bytes)))
}

impl Pane {
    pub fn new(name: &str, dir: PathBuf) -> Pane {
        Pane {
            name: name.to_owned(),
            dir,
            status: Vec::new(),
            screen: None,
            stamps: (None, None),
            parser: None,
            input: None,
            queued: VecDeque::new(),
        }
    }

    pub fn value(&self, key: &str) -> Option<&str> {
        files::status_value(&self.status, key)
    }

    /// This life of the pane, once its status has said.
    pub fn id(&self) -> Option<&str> {
        self.value("id").filter(|id| crate::layout::is_id(id))
    }

    pub fn pane_ref(&self) -> Option<PaneRef> {
        Some(PaneRef {
            name: self.name.clone(),
            id: self.id()?.to_owned(),
        })
    }

    pub fn live(&self) -> Option<Live> {
        let labels = (self.status.iter())
            .filter(|(key, _)| key == "label")
            .filter_map(|(_, rest)| rest.split_once(' '))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        Some(Live {
            pane: self.pane_ref()?,
            labels,
        })
    }

    /// `running`, `exited CODE`, `signalled NAME` or `kill-incomplete WHAT`.
    pub fn state(&self) -> &str {
        self.value("state").unwrap_or("running")
    }

    /// Looks at `status`, and at the screen too if it is shown, reading
    /// each again only if it was replaced: true if either was.
    pub fn look(&mut self, shown: bool) -> Result<bool, Failure> {
        let mut changed = false;
        let status_stamp = stamp(&self.dir.join(files::STATUS));
        if status_stamp != self.stamps.1 {
            self.stamps.1 = status_stamp;
            let was = self.id().map(str::to_owned);
            self.status = read_status(&self.dir)?;
            // A new life under this name: nothing meant for the old one,
            // nor its `in`, carries over.
            if was.is_some() && self.id() != was.as_deref() {
                (self.input, self.screen) = (None, None);
                self.queued.clear();
            }
            changed = true;
        }
        if !shown {
            return Ok(changed);
        }
        let path = self.dir.join(files::STYLED);
        let screen_stamp = stamp(&path);
        if screen_stamp != self.stamps.0 {
            self.stamps.0 = screen_stamp;
            let bytes = files::read_small(&path, files::MAX_SCREEN_FILE)?;
            let published = files::parse_screen(&String::from_utf8_lossy(&bytes))?;
            self.screen = Some(self.cells(published)?);
            changed = true;
        }
        Ok(changed)
    }

    /// The cells of a published screen: its rows fed to a terminal of its
    /// size, each after a cursor move, autowrap off, so each cell's whole
    /// pen is known and written back by fux-vt.
    fn cells(&mut self, published: files::Published) -> Result<Screen, Failure> {
        let (cols, rows) = (published.head.cols, published.head.rows);
        let size = fux_vt::Size::new(rows.max(1), cols.max(1))
            .map_err(|e| Failure::new(format!("{e:?}")))?;
        let fits = (self.parser.as_ref()).is_some_and(|p| p.screen().size() == size);
        if !fits {
            let parser = fux_vt::Parser::with_options(size, 0, fux_vt::Options::new())
                .map_err(|e| Failure::new(format!("{e:?}")))?;
            self.parser = Some(parser);
        }
        let parser = self
            .parser
            .as_mut()
            .ok_or_else(|| Failure::new("no parser"))?;
        parser.process_all(b"\x1b[0m\x1b[?7l\x1b[2J", &mut ());
        for (row, text) in published.rows.iter().enumerate() {
            let at = format!("\x1b[{};1H", row.saturating_add(1));
            parser.process_all(at.as_bytes(), &mut ());
            parser.process_all(text.as_bytes(), &mut ());
            parser.process_all(b"\x1b[0m", &mut ());
        }
        let mut out = Vec::with_capacity(usize::from(rows));
        for (_, row) in parser.screen().window().iter() {
            let cells = row.cells().map(|cell| {
                let mut sgr = Vec::new();
                cell.attributes()
                    .write_sgr(&mut sgr, fux_vt::SgrForm::Paint { styles: true });
                Cell::new(
                    cell.shown(),
                    &String::from_utf8_lossy(&sgr),
                    cell.is_wide_continuation(),
                )
            });
            out.push(cells.collect());
        }
        Ok(Screen {
            head: published.head,
            rows: out,
            pictures: published.pictures,
        })
    }

    /// Queues bytes for the program; refused out loud past the bound.
    pub fn type_bytes(&mut self, bytes: &[u8]) -> Result<(), Failure> {
        if self.queued.len().saturating_add(bytes.len()) > MAX_QUEUED {
            return Err(Failure::new(format!(
                "keys dropped: {} is not taking them",
                self.name
            )));
        }
        self.queued.extend(bytes);
        self.flush()
    }

    pub fn has_queued(&self) -> bool {
        !self.queued.is_empty()
    }

    /// Writes what is queued to `in`, at most a pipe's atomic write at a
    /// time, until it would wait.
    pub fn flush(&mut self) -> Result<(), Failure> {
        while !self.queued.is_empty() {
            if self.input.is_none() {
                match open_fifo(&self.dir.join(files::IN)) {
                    Ok(file) => self.input = Some(file),
                    Err(failure) => {
                        let lost = self.queued.len();
                        self.queued.clear();
                        return Err(Failure::new(format!(
                            "{lost} bytes for {} not delivered: {failure}",
                            self.name
                        )));
                    }
                }
            }
            let Some(file) = &self.input else {
                return Ok(());
            };
            let (front, _) = self.queued.as_slices();
            let piece = front
                .get(..front.len().min(files::PIPE_BUF))
                .unwrap_or_default();
            match fuxix::io::write(file, piece) {
                Ok(n) => {
                    for _ in 0..n {
                        self.queued.pop_front();
                    }
                }
                Err(Errno::AGAIN | Errno::INTR) => return Ok(()),
                Err(errno) => {
                    self.input = None;
                    let lost = self.queued.len();
                    self.queued.clear();
                    return Err(Failure::new(format!(
                        "{lost} bytes for {} not delivered: {errno}",
                        self.name
                    )));
                }
            }
        }
        Ok(())
    }

    /// Hands `text` to the program as one paste: written to a private file
    /// in `view`, whose path is given back, and named to `ctl`'s `paste`.
    pub fn paste(&self, view: &Path, text: &str, number: u64) -> Result<PathBuf, Failure> {
        let name = format!("paste-{}-{number}", std::process::id());
        crate::layout::replace(view, &name, &format!(".{name}.new"), text)?;
        let path = view.join(&name);
        if let Err(failure) = ctl_line(&self.dir, &format!("paste {}", path.display())) {
            let _ = std::fs::remove_file(&path);
            return Err(failure);
        }
        Ok(path)
    }

    /// The key mode the program asked for, as `status` says.
    pub fn key_mode(&self) -> fux_vt::keys::encode::KeyMode {
        (self.value("keys").and_then(fux3::holder::parse_key_mode)).unwrap_or(
            fux_vt::keys::encode::KeyMode::Legacy {
                application: false,
                keypad: false,
                other_keys: None,
                backarrow: false,
            },
        )
    }

    /// The bytes a key gives this pane's program: Enter is CR LF while the
    /// program has newline mode on.
    pub fn key_bytes(&self, stroke: fux_vt::keys::Keystroke) -> Vec<u8> {
        let mut out = Vec::new();
        let enter = stroke.press
            == fux_vt::keys::KeyPress::new(fux_vt::keys::Key::Enter, fux_vt::keys::Modifiers::NONE);
        if enter && self.value("newline-mode") == Some("on") {
            out.extend_from_slice(b"\r\n");
            return out;
        }
        fux_vt::keys::encode::key_bytes(stroke, self.key_mode(), &mut out);
        out
    }
}
