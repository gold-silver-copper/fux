//! A pane's recipe: one executable POSIX `sh` script, `WORKSPACE/.recipes/NAME`,
//! that starts the pane's program again exactly: its directory, the
//! environment named at start (never the inherited one), and its arguments,
//! every word single-quoted. It is where neither a kill nor clearing dead
//! panes removes it, so `fux3 start NAME` with no program runs it again.
use crate::{Failure, io};
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// The recipes' directory in a workspace.
pub const DIR: &str = ".recipes";
/// The largest recipe written; a start whose recipe would be larger is
/// refused.
pub const MAX_RECIPE: usize = 64 << 10;

/// `bytes` as one `sh` word: single-quoted, each `'` written `'\''`. Every
/// other byte, a newline or 0xff among them, stands for itself.
pub fn quote(bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![b'\''];
    for &byte in bytes {
        match byte {
            b'\'' => out.extend_from_slice(b"'\\''"),
            other => out.push(other),
        }
    }
    out.push(b'\'');
    out
}

/// Names `sh` owns and will not take a value for (`bash`, which is
/// `/bin/sh` on macOS, makes them read-only): a recipe setting one would
/// not run, so a start naming one is refused.
const OWNED: [&str; 6] = [
    "PPID",
    "UID",
    "EUID",
    "BASHOPTS",
    "SHELLOPTS",
    "BASH_VERSINFO",
];

/// Whether `name` may name an environment variable in a recipe: a
/// variable's name, and none `sh` owns.
pub fn check_env_name(name: &str) -> Result<(), Failure> {
    let mut chars = name.chars();
    let good = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    match (good, OWNED.contains(&name)) {
        (true, false) => Ok(()),
        (true, true) => Err(Failure::new(format!(
            "--env {name}: sh keeps that variable for itself, so the recipe could not run"
        ))),
        (false, _) => Err(Failure::new(format!(
            "bad variable name {name:?}: [A-Za-z_][A-Za-z0-9_]*"
        ))),
    }
}

/// Where a recipe keeps the start's size and labels: comment lines, which
/// `sh` skips and a start with no program reads back ([`Kept`]).
const SIZE: &str = "# fux3 size ";
const LABEL: &str = "# fux3 label ";

/// The recipe of a program started in `cwd` with `env` and `argv`, and
/// what it keeps besides; or why not: past [`MAX_RECIPE`].
pub fn render(
    cwd: &Path,
    env: &[(String, OsString)],
    argv: &[OsString],
    kept: &Kept,
) -> Result<Vec<u8>, Failure> {
    let mut out = b"#!/bin/sh\n".to_vec();
    if let Some((cols, rows)) = kept.size {
        let _ = writeln!(out, "{SIZE}{cols}x{rows}");
    }
    for (key, value) in &kept.labels {
        let _ = writeln!(out, "{LABEL}{key}={value}");
    }
    out.extend_from_slice(b"cd -- ");
    out.extend_from_slice(&quote(cwd.as_os_str().as_bytes()));
    out.extend_from_slice(b" || exit 126\n");
    for (name, value) in env {
        let _ = write!(out, "{name}=");
        out.extend_from_slice(&quote(value.as_bytes()));
        let _ = writeln!(out, "; export {name}");
    }
    // A program whose name starts with `-` would be read as an option of
    // `exec`'s by bash, and `exec --` is not every `sh`'s (dash runs `--`):
    // `env --` runs it, in both.
    out.extend_from_slice(b"exec");
    if argv.first().is_some_and(|p| p.as_bytes().starts_with(b"-")) {
        out.extend_from_slice(b" env --");
    }
    for word in argv {
        out.push(b' ');
        out.extend_from_slice(&quote(word.as_bytes()));
    }
    out.push(b'\n');
    match out.len() <= MAX_RECIPE {
        true => Ok(out),
        false => Err(Failure::new(format!(
            "its recipe would be {} bytes, at most {MAX_RECIPE}",
            out.len()
        ))),
    }
}

/// What a recipe keeps of its start besides the program: the size and
/// the labels, so a pane started again from it is sized and found (by a
/// template's tab that matches on a label) as it was.
#[derive(Default)]
pub struct Kept {
    pub size: Option<(u16, u16)>,
    pub labels: Vec<(String, String)>,
}

/// Reads back what `text`, a recipe, keeps. Lines it does not know are
/// the script's.
pub fn kept(text: &[u8]) -> Kept {
    let mut kept = Kept::default();
    for line in String::from_utf8_lossy(text).lines() {
        if let Some((cols, rows)) = line.strip_prefix(SIZE).and_then(|s| s.split_once('x')) {
            kept.size = cols.parse().ok().zip(rows.parse().ok());
        } else if let Some(Ok(label)) = line.strip_prefix(LABEL).map(crate::holder::parse_label) {
            kept.labels.push(label);
        }
    }
    kept
}

/// Writes the recipe of `name` in the workspace held open as `workspace`,
/// whole: aside, then renamed in, mode 0700.
pub fn write(workspace: &std::fs::File, name: &str, text: &[u8]) -> Result<(), Failure> {
    use fuxix::file::at;
    use std::os::unix::fs::PermissionsExt;
    let dir = crate::session::private_dir(workspace, DIR, true)?;
    let aside = format!(".{name}.new");
    let _ = at::remove_file(&dir, &aside);
    let written = at::open(&dir, &aside, at::WRITE | at::NEW)
        .and_then(|mut file| {
            file.write_all(text)?;
            file.set_permissions(std::fs::Permissions::from_mode(0o700))
        })
        .and_then(|()| at::rename(&dir, &aside, &dir, name));
    written.map_err(|e| {
        let _ = at::remove_file(&dir, &aside);
        io(format!("writing the recipe {DIR}/{name}"), e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every word comes back from `sh` byte for byte, whatever it holds.
    #[test]
    fn a_recipe_gives_back_every_word_exactly() -> Result<(), String> {
        use std::os::unix::ffi::OsStringExt;
        let words: Vec<OsString> = [
            b"printf".to_vec(),
            b"%s\\0".to_vec(),
            Vec::new(),
            b"it's \"quoted\" $(echo no) `echo no` $HOME".to_vec(),
            b"two\nlines".to_vec(),
            vec![0xff, b'x'],
            std::iter::repeat_n(b'a', 5000).collect(),
        ]
        .into_iter()
        .map(OsString::from_vec)
        .collect();
        let dir = fuxix::file::scratch().map_err(|e| e.to_string())?;
        let odd = dir.join(format!("fux3 it's\nodd-{}", std::process::id()));
        std::fs::create_dir_all(&odd).map_err(|e| e.to_string())?;
        let env = vec![("FUX3_T".to_owned(), OsString::from("a'b"))];
        let text = render(&odd, &env, &words, &Kept::default()).map_err(|e| e.0)?;
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(OsString::from_vec(text))
            .output()
            .map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir(&odd);
        let mut want = Vec::new();
        for word in words.iter().skip(2) {
            want.extend_from_slice(word.as_bytes());
            want.push(0);
        }
        assert_eq!(out.stdout, want);
        Ok(())
    }
}
