//! A pane's recipe: one executable POSIX `sh` script, `WORKSPACE/.recipes/NAME`,
//! that starts the pane's program again exactly: its directory, the
//! environment named at start (never the inherited one), and its arguments,
//! every word single-quoted. It is where neither a kill nor clearing dead
//! panes removes it, so `fux3 start NAME` with no program runs it again.
use crate::{Failure, io};
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

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

/// Whether `name` may name an environment variable in a recipe.
pub fn check_env_name(name: &str) -> Result<(), Failure> {
    let mut chars = name.chars();
    let good = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    match good {
        true => Ok(()),
        false => Err(Failure::new(format!(
            "bad variable name {name:?}: [A-Za-z_][A-Za-z0-9_]*"
        ))),
    }
}

/// The recipe of a program started in `cwd` with `env` and `argv`, or why
/// not: past [`MAX_RECIPE`].
pub fn render(
    cwd: &Path,
    env: &[(String, OsString)],
    argv: &[OsString],
) -> Result<Vec<u8>, Failure> {
    let mut out = b"#!/bin/sh\n".to_vec();
    out.extend_from_slice(b"cd -- ");
    out.extend_from_slice(&quote(cwd.as_os_str().as_bytes()));
    out.extend_from_slice(b" || exit 126\n");
    for (name, value) in env {
        let _ = write!(out, "{name}=");
        out.extend_from_slice(&quote(value.as_bytes()));
        let _ = writeln!(out, "; export {name}");
    }
    out.extend_from_slice(b"exec");
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

/// Where the recipe of `name` in the workspace directory `workspace` is.
pub fn path(workspace: &Path, name: &str) -> PathBuf {
    workspace.join(DIR).join(name)
}

/// Writes the recipe of `name`, whole: aside, then renamed in, mode 0700.
pub fn write(workspace: &Path, name: &str, text: &[u8]) -> Result<(), Failure> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let dir = workspace.join(DIR);
    match fuxix::file::private_dir(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(io(dir.display(), e)),
    }
    crate::session::check_private(&dir)?;
    let aside = dir.join(format!(".{name}.new"));
    let _ = std::fs::remove_file(&aside);
    let written = std::fs::File::options()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .custom_flags(fuxix::file::NOFOLLOW)
        .open(&aside)
        .and_then(|mut file| {
            file.write_all(text)?;
            file.set_permissions(std::fs::Permissions::from_mode(0o700))
        })
        .and_then(|()| std::fs::rename(&aside, dir.join(name)));
    written.map_err(|e| {
        let _ = std::fs::remove_file(&aside);
        io(
            format!("writing the recipe {}", dir.join(name).display()),
            e,
        )
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
        let text = render(&odd, &env, &words).map_err(|e| e.0)?;
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
