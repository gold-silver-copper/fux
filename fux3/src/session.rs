//! The session directory: one private directory holding a directory per
//! workspace, each holding a directory per pane.
//!
//! A pane is its directory and its id; a workspace is its directory; a
//! name is only where one is listed right now. So everything here holds a
//! directory open and names files relative to it, and never looks a path
//! up twice. A pane's holder locks its directory for as long as it lives,
//! and that lock is the only proof a pane is alive. The session's own lock
//! (`.lock`) serializes giving and taking names and clearing dead panes;
//! reads never take it.
use crate::{Failure, files, io};
use fuxix::file::at;
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The most panes a workspace holds.
pub const MAX_PANES: usize = 128;
/// The most entries a listing reads before it refuses.
const MAX_ENTRIES: usize = 1024;
/// The most entries read to find where a directory is listed now.
const MAX_SIBLINGS: usize = 1 << 16;
/// How long a change waits for the session's lock.
const LOCK_WAIT: Duration = Duration::from_secs(5);
/// The session's lock file, a dot-name no pane can have.
const LOCK: &str = ".lock";
/// The file that says a directory is a pane's: its id and a newline,
/// written by its holder before anything else.
pub const MARKER: &str = "id";
/// What a pane's directory is called while it is made, before it has its
/// name, and once it has lost it, while it is taken apart: each followed
/// by the pane's id. Dot-names, so nothing lists them as panes.
const STAGING: &str = ".new-";
const GONE: &str = ".gone-";

/// Refuses a name outside `[a-z0-9][a-z0-9_-]{0,31}`: no dot-names, so
/// nothing of the session's own (`.lock`, `.view`) is ever a pane.
pub fn check_name(name: &str) -> Result<(), Failure> {
    let lead = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let mut chars = name.chars();
    let good = chars.next().is_some_and(lead)
        && chars.all(|c| lead(c) || c == '_' || c == '-')
        && name.len() <= 32;
    match good {
        true => Ok(()),
        false => Err(Failure::new(format!(
            "bad name {name:?}: a pane or workspace name must match [a-z0-9][a-z0-9_-]{{0,31}}"
        ))),
    }
}

/// The lock on a pane's directory, held open as `dir`: whether it was
/// taken, so no holder lives.
pub fn try_lock(dir: &File) -> Result<bool, Failure> {
    match dir.try_lock() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(e)) => Err(io("locking a pane's directory", e)),
    }
}

/// Whether `a` and `b` are one file.
fn same(a: &File, b: &File) -> bool {
    matches!((a.metadata(), b.metadata()), (Ok(a), Ok(b)) if a.dev() == b.dev() && a.ino() == b.ino())
}

/// Checks that `dir`, shown as `shown`, is a directory of this user's; with
/// `alone`, one nobody else can open. The session's directory is, so what
/// is in it needs no mode of its own: `mkdir` makes a workspace.
fn check_held(dir: &File, shown: impl std::fmt::Display, alone: bool) -> Result<(), Failure> {
    let meta = dir.metadata().map_err(|e| io(&shown, e))?;
    if meta.uid() != fuxix::process::geteuid() {
        return Err(Failure::new(format!(
            "{shown} belongs to uid {}",
            meta.uid()
        )));
    }
    if alone && meta.mode() & 0o077 != 0 {
        return Err(Failure::new(format!(
            "{shown} has mode {:o}, and must be 700",
            meta.mode() & 0o777
        )));
    }
    Ok(())
}

/// Opens the directory `path` to hold it: a link, or anything but a
/// directory of this user's alone, is refused.
pub fn open_private(path: &std::path::Path) -> Result<File, Failure> {
    use std::os::unix::fs::OpenOptionsExt;
    let flags = fuxix::file::NOFOLLOW | fuxix::file::DIRECTORY | fuxix::file::NONBLOCK;
    let dir = (File::options().read(true).custom_flags(flags).open(path))
        .map_err(|e| io(format!("{} (a directory, no link)", path.display()), e))?;
    check_held(&dir, path.display(), true)?;
    Ok(dir)
}

/// The directory `name` in `dir`, made private if `make` and missing,
/// held open; a link, or anything but a directory of this user's, is
/// refused.
pub fn private_dir(dir: &File, name: &str, make: bool) -> Result<File, Failure> {
    let made = match make {
        true => at::make_dir(dir, name),
        false => Ok(()),
    };
    match made {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(io(name, e)),
        _ => {}
    }
    let held =
        at::open_dir(dir, name).map_err(|e| io(format!("{name} (a directory, no link)"), e))?;
    check_held(&held, name, false)?;
    Ok(held)
}

/// Where the directory `dir` is listed now: its parent, held open, and
/// its name there. None once it is listed nowhere (removed).
pub fn place(dir: &File) -> Option<(File, String)> {
    let parent = at::open_dir(dir, "..").ok()?;
    let inode = dir.metadata().ok()?.ino();
    let names = at::names(&parent, MAX_SIBLINGS).ok()?;
    let is = |name: &std::ffi::OsString| at::open_dir(&parent, name).is_ok_and(|d| same(&d, dir));
    // By the entry's inode number first; where a filesystem gives entries
    // other numbers than its files have (some overlays), by looking at each.
    let found = (names.iter().find(|(name, i)| *i == inode && is(name)))
        .or_else(|| names.iter().find(|(name, _)| is(name)))?;
    let name = found.0.clone().into_string().ok()?;
    Some((parent, name))
}

/// Renames the directory held as `dir` from `from` in `parent` to `to` in
/// `to_parent`, onto nothing. `from` was looked up a moment ago and a
/// `mv` may have put another directory there since: if what got the new
/// name is not `dir`, it is given its name back and the rename refused.
fn rename_held(
    dir: &File,
    parent: &File,
    from: &str,
    to_parent: &File,
    to: &str,
) -> Result<(), Failure> {
    at::rename_new(parent, from, to_parent, to).map_err(|e| io(format!("renaming {from}"), e))?;
    if at::open_dir(to_parent, to).is_ok_and(|there| same(&there, dir)) {
        return Ok(());
    }
    // Its name back, unless yet another took that meanwhile.
    Err(Failure::new(
        match at::rename_new(to_parent, to, parent, from) {
            Ok(()) => format!("{from} was moved meanwhile; nothing was changed"),
            Err(_) => format!("{from} was moved meanwhile; what took its name is now {to}"),
        },
    ))
}

/// Renames the pane held as `dir`, id `id`, listed as `name` in `parent`,
/// out of its name: to `.gone-ID`, or with a number after it where a copy
/// of the pane already has that. The name it has now.
fn lose_name(dir: &File, parent: &File, name: &str, id: &str) -> Result<String, Failure> {
    let free = |gone: &String| at::open(parent, gone, at::READ | fuxix::file::NONBLOCK).is_err();
    let gone = (0..8)
        .map(|n| match n {
            0 => format!("{GONE}{id}"),
            n => format!("{GONE}{id}-{n}"),
        })
        .find(free)
        .ok_or_else(|| Failure::new(format!("{GONE}{id} is taken, eight times over")))?;
    rename_held(dir, parent, name, parent, &gone)?;
    Ok(gone)
}

/// The session directory when `FUX3_DIR` does not name one: in memory on
/// Linux (`$XDG_RUNTIME_DIR` is a tmpfs), on disk on macOS.
fn default_root() -> Result<PathBuf, Failure> {
    let set = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let missing = |name: &str| Failure::new(format!("{name} is not set, and FUX3_DIR neither"));
    #[cfg(target_os = "macos")]
    let base = PathBuf::from(set("HOME").ok_or_else(|| missing("HOME"))?).join("Library/Caches");
    #[cfg(not(target_os = "macos"))]
    let base = PathBuf::from(set("XDG_RUNTIME_DIR").ok_or_else(|| missing("XDG_RUNTIME_DIR"))?);
    if !base.is_dir() {
        return Err(Failure::new(format!("{} is no directory", base.display())));
    }
    Ok(base.join("fux3"))
}

/// A session: its private directory, held open, holding one directory
/// per workspace. `root` is its path, for showing and for other programs.
#[derive(Clone, Debug)]
pub struct Session {
    pub root: PathBuf,
    dir: Arc<File>,
}

/// A workspace: a directory of the session's, held open from when it was
/// first named, holding one directory per pane. `name` and `dir` are what
/// it was called then, for showing and for other programs; what is done
/// to it is done through the handle.
#[derive(Clone, Debug)]
pub struct Workspace {
    pub session: Session,
    pub name: String,
    pub dir: PathBuf,
    held: Arc<File>,
}

/// The workspace a pane named without one is in.
pub const DEFAULT_WORKSPACE: &str = "main";

/// The session's lock, while held.
pub struct SessionLock {
    _file: File,
}

/// The names in `dir` a pane or workspace may have, sorted.
fn names_in(dir: &File) -> Result<Vec<String>, Failure> {
    let entries = at::names(dir, MAX_ENTRIES).map_err(|e| io("listing", e))?;
    let mut names: Vec<String> = (entries.into_iter())
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| check_name(name).is_ok())
        .collect();
    names.sort();
    Ok(names)
}

/// Splits `[WORKSPACE/]NAME`, the workspace `main` if none is given; both
/// names checked.
pub fn split_pane(text: &str) -> Result<(String, String), Failure> {
    let (workspace, name) = text.split_once('/').unwrap_or((DEFAULT_WORKSPACE, text));
    check_name(workspace)?;
    check_name(name)?;
    Ok((workspace.to_owned(), name.to_owned()))
}

impl Session {
    /// The session `FUX3_DIR` names, else this user's default, made if
    /// missing and refused unless it is private.
    pub fn open() -> Result<Session, Failure> {
        let root = match std::env::var_os("FUX3_DIR").filter(|v| !v.is_empty()) {
            Some(root) => PathBuf::from(root),
            None => default_root()?,
        };
        if !root.is_absolute() {
            let why = format!("FUX3_DIR must be absolute, not {}", root.display());
            return Err(Failure::new(why));
        }
        match fuxix::file::private_dir(&root) {
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
                return Err(io(root.display(), e));
            }
            _ => {}
        }
        Session::at(root)
    }

    /// The session whose directory is `root`: opened once, a link refused.
    pub fn at(root: PathBuf) -> Result<Session, Failure> {
        let dir = Arc::new(open_private(&root)?);
        Ok(Session { root, dir })
    }

    /// The workspace `name`, made if `make` and missing, refused unless it
    /// is a private directory. It is held from here on: what is done to
    /// the workspace is done to this directory, whatever has its name later.
    pub fn workspace(&self, name: &str, make: bool) -> Result<Workspace, Failure> {
        check_name(name)?;
        let held = private_dir(&self.dir, name, make)
            .map_err(|f| Failure::new(format!("workspace {f}")))?;
        Ok(Workspace {
            session: self.clone(),
            name: name.to_owned(),
            dir: self.root.join(name),
            held: Arc::new(held),
        })
    }

    /// The workspaces, by name: every directory here a workspace may be
    /// named, empty or not.
    pub fn workspaces(&self) -> Result<Vec<String>, Failure> {
        let mut names = names_in(&self.dir)?;
        names.retain(|name| at::open_dir(&self.dir, name).is_ok());
        Ok(names)
    }

    /// Takes the session's lock, waiting a bounded time.
    pub fn lock(&self) -> Result<SessionLock, Failure> {
        let shown = self.root.join(LOCK);
        let file = at::open(&self.dir, LOCK, at::READ_WRITE | at::CREATE)
            .map_err(|e| io(shown.display(), e))?;
        let start = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(SessionLock { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(e)) => return Err(io(shown.display(), e)),
            }
            if start.elapsed() >= LOCK_WAIT {
                let why = format!("{} was held for {LOCK_WAIT:?}", shown.display());
                return Err(Failure::new(why));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Every live pane as `WORKSPACE/NAME`, after clearing every dead
    /// one's leftovers in every workspace. Workspaces, empty ones too, stay.
    pub fn live(&self) -> Result<Vec<String>, Failure> {
        let lock = self.lock()?;
        self.clear_all(&lock)
    }

    /// Clears every workspace's dead panes, under the session's lock: the
    /// live ones as `WORKSPACE/NAME`.
    pub fn clear_all(&self, lock: &SessionLock) -> Result<Vec<String>, Failure> {
        let mut live = Vec::new();
        for name in self.workspaces()? {
            // One workspace that can't be read blocks no other.
            let Ok(panes) = (self.workspace(&name, false)).and_then(|w| w.clear_dead(lock)) else {
                continue;
            };
            for pane in panes {
                live.push(format!("{name}/{pane}"));
            }
        }
        Ok(live)
    }
}

impl Workspace {
    /// The workspace's directory, held open.
    pub fn held(&self) -> &File {
        &self.held
    }

    /// Which directory this is, as `fux3 start --workspace-id` takes it.
    pub fn id(&self) -> String {
        (self.held.metadata())
            .map_or_else(|_| "?".to_owned(), |m| format!("{}:{}", m.dev(), m.ino()))
    }

    /// Refuses once the directory held is listed in the session no more:
    /// removed, whatever has its name now.
    pub fn check(&self) -> Result<(), Failure> {
        match place(&self.held) {
            Some((parent, _)) if same(&parent, &self.session.dir) => Ok(()),
            _ => Err(Failure::new(format!("workspace {} was removed", self.name))),
        }
    }

    /// The names with a directory here that a pane may have, live or not,
    /// sorted.
    pub fn names(&self) -> Result<Vec<String>, Failure> {
        let mut names = names_in(&self.held)?;
        names.retain(|name| at::open_dir(&self.held, name).is_ok());
        Ok(names)
    }

    /// The directory of the pane called `name` now, held open.
    pub fn pane(&self, name: &str) -> Result<File, Failure> {
        check_name(name)?;
        at::open_dir(&self.held, name)
            .ok()
            .filter(is_pane)
            .ok_or_else(|| Failure::new(format!("no pane {}/{name}", self.name)))
    }

    /// The live panes here, after clearing every dead one's leftovers.
    pub fn live(&self) -> Result<Vec<String>, Failure> {
        let lock = self.session.lock()?;
        self.clear_dead(&lock)
    }

    /// Clears every dead pane here and gives the names of the live ones.
    /// Dead is: a directory named as a pane, or as one being made or
    /// taken apart, whose lock nobody holds. Only what a holder made goes
    /// ([`clear_pane`]): anything else here, named like a pane or not,
    /// stays.
    pub fn clear_dead(&self, _lock: &SessionLock) -> Result<Vec<String>, Failure> {
        let entries = at::names(&self.held, MAX_ENTRIES).map_err(|e| io(&self.name, e))?;
        let mut live = Vec::new();
        for name in entries
            .into_iter()
            .filter_map(|(n, _)| n.into_string().ok())
        {
            let named = check_name(&name).is_ok();
            if !named && !name.starts_with(STAGING) && !name.starts_with(GONE) {
                continue;
            }
            let Ok(dir) = at::open_dir(&self.held, &name) else {
                continue;
            };
            match try_lock(&dir) {
                Ok(true) => {
                    // A pane loses its name before it is taken apart, as
                    // in its own kill: no reader finds a name half empty,
                    // and a clearing that dies leaves what the next takes.
                    let name = match marker(&dir) {
                        Some(id) if named => match lose_name(&dir, &self.held, &name, &id) {
                            Ok(gone) => gone,
                            Err(_) => continue,
                        },
                        _ => name,
                    };
                    // Still the one locked, and empty now: it goes. A
                    // directory with a pane's name and no marker is not
                    // ours, empty or not.
                    let emptied = clear_pane(&dir, !check_name(&name).is_ok());
                    if emptied && at::open_dir(&self.held, &name).is_ok_and(|d| same(&d, &dir)) {
                        let _ = at::remove_dir(&self.held, &name);
                    }
                }
                Ok(false) | Err(_) if named => live.push(name),
                Ok(false) | Err(_) => {}
            }
        }
        live.sort();
        Ok(live)
    }

    /// Makes the directory of a pane to be, under a name nothing lists,
    /// and locks it: its marker written, nothing else in it yet. Under
    /// the session's lock, so that no clearing takes it half made.
    pub fn stage(&self, id: &str) -> Result<File, Failure> {
        let staged = format!("{STAGING}{id}");
        let _lock = self.session.lock()?;
        at::make_dir(&self.held, &staged).map_err(|e| match self.check() {
            Ok(()) => io(format!("making a pane in workspace {}", self.name), e),
            Err(removed) => removed,
        })?;
        let dir = at::open_dir(&self.held, &staged).map_err(|e| io(&staged, e))?;
        if !try_lock(&dir)? {
            return Err(Failure::new(format!("{staged} is locked by another")));
        }
        files::write_new(&dir, MARKER, format!("{id}\n").as_bytes())?;
        Ok(dir)
    }

    /// Gives the pane in `dir`, whole, the name `name` here: one rename
    /// under the session's lock, which never replaces anything. A live
    /// pane's name, a full workspace, or anything else at the name is
    /// refused, saying why.
    pub fn name_pane(&self, dir: &File, name: &str) -> Result<(), Failure> {
        check_name(name)?;
        let lock = self.session.lock()?;
        // Every workspace's dead panes go, as with every `fux3` command.
        self.session.clear_all(&lock)?;
        let live = self.clear_dead(&lock)?;
        let shown = format!("{}/{name}", self.name);
        if live.iter().any(|n| n == name) {
            return Err(Failure::new(format!("pane {shown} exists")));
        }
        if live.len() >= MAX_PANES {
            return Err(Failure::new(format!(
                "{} panes in workspace {}, at most {MAX_PANES}",
                live.len(),
                self.name
            )));
        }
        self.check()?;
        if at::open(&self.held, name, at::READ | fuxix::file::NONBLOCK).is_ok() {
            let there = self.dir.join(name);
            let why = "is there and holds what no pane made: it was left alone";
            return Err(Failure::new(format!("{} {why}", there.display())));
        }
        let (parent, staged) =
            place(dir).ok_or_else(|| Failure::new("the pane's directory went"))?;
        rename_held(dir, &parent, &staged, &self.held, name)
            .map_err(|f| Failure::new(format!("naming pane {shown}: {f}")))
    }
}

/// Whether `dir` is a pane's directory: it holds the marker its holder
/// wrote. Nothing else is ever cleared as a dead pane, so a session
/// directory pointed at a place holding other files loses none.
pub fn is_pane(dir: &File) -> bool {
    marker(dir).is_some()
}

/// The id in the marker of the directory `dir`, if it holds one as a
/// holder writes it: sixteen hex digits and a newline, nothing else. A
/// file that happens to be called `id` makes no directory a pane's.
fn marker(dir: &File) -> Option<String> {
    let text = String::from_utf8(files::read_at(dir, MARKER, 17).ok()?).ok()?;
    let id = text.strip_suffix('\n')?;
    let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    (id.len() == 16 && id.bytes().all(hex)).then(|| id.to_owned())
}

/// Takes the pane in `dir` out of its name, by one rename, to a name
/// nothing lists: its parent and that name, or None if it is listed
/// nowhere. Called under the session's lock.
pub fn unname(
    dir: &File,
    id: &str,
    _lock: &SessionLock,
) -> Result<Option<(File, String)>, Failure> {
    let Some((parent, name)) = place(dir) else {
        return Ok(None);
    };
    let gone = match name.starts_with(GONE) {
        true => name,
        false => lose_name(dir, &parent, &name, id)?,
    };
    Ok(Some((parent, gone)))
}

/// Whether `name` is a picture's file name, `N.sixel`.
fn sixel(name: &str) -> bool {
    let number = name.strip_suffix(".sixel");
    number.is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Takes a dead pane's directory apart, through its handle and with its
/// lock held by the caller: whether it is empty now. Only a directory
/// with the marker (or, `unnamed`, one with nothing in it) is touched,
/// and only what a holder makes is removed: with anything else in it, a
/// pane moved into it among them, that and the marker stay, for the next
/// clearing to try again. No directory but `images` is entered.
pub fn clear_pane(dir: &File, unnamed: bool) -> bool {
    let Ok(names) = at::names(dir, MAX_ENTRIES) else {
        return false;
    };
    if !is_pane(dir) {
        return unnamed && names.is_empty();
    }
    let mut known = true;
    for (name, _) in &names {
        let name = name.to_string_lossy();
        if name == files::IMAGES {
            known &= at::open_dir(dir, files::IMAGES).is_ok_and(|images| {
                let all = at::names(&images, MAX_ENTRIES).unwrap_or_default();
                let mut emptied = true;
                for (image, _) in all {
                    let ours = sixel(&image.to_string_lossy());
                    emptied &= ours && at::remove_file(&images, &image).is_ok();
                }
                emptied
            }) && at::remove_dir(dir, files::IMAGES).is_ok();
        } else if name != MARKER {
            let aside = name.strip_prefix('.').and_then(|n| n.strip_suffix(".new"));
            let made = files::MADE.contains(&&*name)
                || aside.is_some_and(|n| {
                    files::REPLACED.contains(&n) || n.strip_prefix("images-").is_some_and(sixel)
                });
            known &= made && at::remove_file(dir, &*name).is_ok();
        }
    }
    known && at::remove_file(dir, MARKER).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_never_dot_names_or_paths() {
        for good in ["build", "a", "0-x_y"] {
            assert!(check_name(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            ".view",
            ".lock",
            "a/b",
            "..",
            "Build",
            &std::iter::repeat_n('x', 33).collect::<String>(),
        ] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }
}
