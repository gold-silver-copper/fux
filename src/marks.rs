//! Shell marks for the shells fux starts: bash, zsh and fish are started so
//! that they mark each command (OSC 133 `C` as its output begins, `D;STATUS`
//! as it ends), after reading the user's own startup files as they would.
//! A host then knows when a command typed into them is done, how, and which
//! output was its, with nothing set up (`wait --done`, `--last-output`).
//!
//! Each shell is given a startup file fux writes beside its socket, which
//! reads the user's own first: bash's `--rcfile` sources `~/.bashrc`, zsh's
//! `ZDOTDIR` shim puts the user's back and sources their `.zshenv`, fish's
//! `--init-command` only adds two event handlers; bash with `--norc`, or a
//! login bash, reads no file of fux's, so takes them from `PS0` and
//! `PROMPT_COMMAND`. A shell started with a script or a command is left as
//! it is, as is any other program.
use std::path::{Path, PathBuf};

/// bash: the user's own startup, then the marks. bash 4.4 and later print
/// `PS0` as a command's output begins; older ones (macOS's 3.2) mark it
/// from a DEBUG trap instead, armed last thing before each prompt, so it
/// fires once, for the command typed. A line run with no start mark (one
/// bash could not parse, or a comment: the history count moved) gets one as
/// it ends. Marks go to stderr, as the prompt does; fux's PROMPT_COMMAND is
/// not exported, so shells started from this one do not inherit it.
const BASHRC: &str = r#"# fux: OSC 133 shell marks, after your own startup. Off: `set shell-marks off`.
if [ -r ~/.bashrc ]; then . ~/.bashrc; fi
"#;

/// A login bash's startup, read as `bash -l` reads it (a login shell takes
/// no --rcfile, so fux starts bash interactive with this one instead).
const BASH_LOGIN: &str = r#"# fux: a login shell's startup, as bash -l reads it, then OSC 133 marks.
if [ -r /etc/profile ]; then . /etc/profile; fi
for __fux_f in ~/.bash_profile ~/.bash_login ~/.profile; do
  if [ -r "$__fux_f" ]; then . "$__fux_f"; break; fi
done
unset __fux_f
"#;

/// The marks, after either startup.
const BASH_MARKS: &str = r#"__fux_end() {
  local s=$? h=${HISTCMD-0}
  if [ -z "${__fux_c+x}" ] && [ "$h" != "${__fux_h-$h}" ]; then printf '\033]133;C\007' >&2; fi
  unset __fux_c; __fux_h=$h; __fux_armed=
  printf '\033]133;D;%s\007' "$s" >&2
  return $s
}
case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
  "declare -a"*) PROMPT_COMMAND=(__fux_end "${PROMPT_COMMAND[@]}") ;;
  *) PROMPT_COMMAND="__fux_end${PROMPT_COMMAND:+$'\n'$PROMPT_COMMAND}" ;;
esac
if [ "${BASH_VERSINFO[0]}" -gt 4 ] || { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]}" -ge 4 ]; }; then
  PS0="${PS0-}"$'\033]133;C\007''${__fux_c=}'
else
  __fux_arm() { __fux_armed=1; }
  __fux_start() { case "$BASH_COMMAND" in __fux_*) return 0 ;; esac; [ -n "${__fux_armed-}" ] || return 0; __fux_armed=; __fux_c=; printf '\033]133;C\007' >&2; }
  case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
    "declare -a"*) PROMPT_COMMAND+=(__fux_arm) ;;
    *) PROMPT_COMMAND="${PROMPT_COMMAND}"$'\n'"__fux_arm" ;;
  esac
  trap '__fux_start' DEBUG
fi
export -n PROMPT_COMMAND PS0
"#;

/// bash that reads no file of fux's (`--norc`, or a login shell, which
/// reads only the profile), from its variables, before and after the
/// caller's own PROMPT_COMMAND: it ends a command (giving a start mark to a
/// line run without one), and keeps the variables to itself. `PS0` marks
/// the next command's start, or before bash 4.4, a DEBUG trap armed last
/// does (fux's commands, an empty line, and a line that starts with a
/// subshell, which the trap does not see, mark nothing).
const BARE_END: &str = r#"__fux_s=$?; __fux_a=; export -n PS0 PROMPT_COMMAND; __fux_n=${HISTCMD-0}; if [ -z "${__fux_c+x}" ] && [ "$__fux_n" != "${__fux_h-$__fux_n}" ]; then printf '\033]133;C\007' >&2; fi; unset __fux_c; __fux_h=$__fux_n; printf '\033]133;D;%s\007' "$__fux_s" >&2; __fux_r() { return "$1"; }; __fux_r "$__fux_s""#;
const BARE_ARM: &str = r#"if [ "${BASH_VERSINFO[0]}${BASH_VERSINFO[1]}" -lt 44 ]; then __fux_t() { case "$BASH_COMMAND" in __fux_*) return 0 ;; esac; [ -n "${__fux_a-}" ] || return 0; __fux_a=; __fux_c=; printf '\033]133;C\007' >&2; }; trap __fux_t DEBUG; __fux_a=1; fi"#;

/// zsh: read first of all startup files, from the shim `ZDOTDIR`; it puts
/// the user's `ZDOTDIR` back, so the rest of their files are read from
/// there as always, and sources their `.zshenv`.
const ZSHENV: &str = r#"# fux: OSC 133 shell marks, after your own startup. Off: `set shell-marks off`.
if [[ -n "${FUX_ZDOTDIR+x}" ]]; then ZDOTDIR=$FUX_ZDOTDIR; unset FUX_ZDOTDIR; else unset ZDOTDIR; fi
[[ -r "${ZDOTDIR:-$HOME}/.zshenv" ]] && source "${ZDOTDIR:-$HOME}/.zshenv"
if [[ -o interactive ]]; then
  __fux_preexec() { print -n '\e]133;C\a' >&2 }
  __fux_precmd() { print -n "\e]133;D;$?\a" >&2 }
  preexec_functions=(__fux_preexec $preexec_functions)
  precmd_functions=(__fux_precmd $precmd_functions)
fi
"#;

/// fish: two event handlers, after its own startup.
const FISH: &str = "function __fux_preexec --on-event fish_preexec; printf '\\e]133;C\\a'; end; \
     function __fux_postexec --on-event fish_postexec; printf '\\e]133;D;%s\\a' $status; end";

/// How a shell is started so that it marks its commands: its words, and
/// variables to add.
#[derive(Debug, Default)]
pub struct Marked {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// `argv` and the variables to start it with so that it marks its commands,
/// if it is a shell fux knows started interactively; the startup files it
/// reads are written under `dir`. `None` leaves it as it is.
pub fn marked(argv: &[String], env: &[(String, String)], dir: &Path) -> Option<Marked> {
    let (program, args) = argv.split_first()?;
    let name = Path::new(program.trim_start_matches('-'))
        .file_name()?
        .to_str()?;
    // Only an interactive shell: no script or command, startup files read.
    let flags_only = |allowed: &[&str]| args.iter().all(|a| allowed.contains(&a.as_str()));
    match name {
        // Without fux's startup file (`--norc`, or a login shell) bash
        // takes the marks from its variables, around the caller's own.
        "bash"
            if flags_only(&["-i", "--norc", "--noprofile"])
                && args.iter().any(|a| a == "--norc") =>
        {
            let theirs = |key: &str| {
                (env.iter().rev())
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.clone())
            };
            let command = match theirs("PROMPT_COMMAND") {
                Some(own) => format!("{BARE_END}\n{own}\n{BARE_ARM}"),
                None => format!("{BARE_END}\n{BARE_ARM}"),
            };
            let start = format!(
                "{}\\e]133;C\\a${{__fux_c=}}",
                theirs("PS0").unwrap_or_default()
            );
            Some(Marked {
                argv: argv.to_vec(),
                env: vec![
                    ("PROMPT_COMMAND".to_owned(), command),
                    ("PS0".to_owned(), start),
                ],
            })
        }
        // A login shell reads the profile files instead, here from fux's
        // rc file, a login shell taking none.
        "bash" if flags_only(&["-i", "--noprofile", "-l", "--login"]) => {
            let login = args.iter().any(|a| a == "-l" || a == "--login");
            let (name, startup) = if login {
                ("bash_login", BASH_LOGIN)
            } else {
                ("bashrc", BASHRC)
            };
            let rc = write(dir, name, &format!("{startup}{BASH_MARKS}"))?;
            let mut argv = vec![program.clone(), "--rcfile".into()];
            argv.push(rc.to_string_lossy().into_owned());
            argv.extend(
                args.iter()
                    .filter(|a| *a != "-l" && *a != "--login")
                    .cloned(),
            );
            Some(Marked {
                argv,
                env: Vec::new(),
            })
        }
        "zsh" if flags_only(&["-i", "-l", "--login"]) => {
            write(&dir.join("zsh"), ".zshenv", ZSHENV)?;
            let shim = dir.join("zsh").to_string_lossy().into_owned();
            // The user's own ZDOTDIR, from what the shell would be given.
            let theirs = (env.iter().rev())
                .find(|(k, _)| k == "ZDOTDIR")
                .map(|(_, v)| v.clone())
                .or_else(|| std::env::var("ZDOTDIR").ok());
            let mut vars = vec![("ZDOTDIR".to_owned(), shim)];
            vars.extend(theirs.map(|v| ("FUX_ZDOTDIR".to_owned(), v)));
            Some(Marked {
                argv: argv.to_vec(),
                env: vars,
            })
        }
        "fish" if flags_only(&["-i", "-l", "--login"]) => {
            let mut argv = argv.to_vec();
            argv.extend(["--init-command".to_owned(), FISH.to_owned()]);
            Some(Marked {
                argv,
                env: Vec::new(),
            })
        }
        _ => None,
    }
}

/// Writes `text` to `dir/name`, `dir` made private if missing: its path.
fn write(dir: &Path, name: &str, text: &str) -> Option<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .ok()?;
    let path = dir.join(name);
    // Unchanged files are left, so a shell starting reads a whole one.
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
        let part = dir.join(format!(".{name}.part"));
        std::fs::write(&part, text).ok()?;
        std::fs::rename(&part, &path).ok()?;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::marked;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    /// Interactive shells fux knows are marked; anything else, a shell with
    /// a script, a command or no startup files included, is left alone.
    #[test]
    fn only_interactive_shells_are_marked() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("marks-test-{}", std::process::id()));
        for shell in [
            "bash",
            "/bin/bash",
            "-bash",
            "bash --norc -i",
            "zsh",
            "zsh -l",
            "fish",
        ] {
            assert!(marked(&words(shell), &[], &dir).is_some(), "{shell}");
        }
        for other in [
            "bash script.sh",
            "bash -c ls",
            "sh",
            "zsh -c ls",
            "vim",
            "fish -c ls",
        ] {
            assert!(marked(&words(other), &[], &dir).is_none(), "{other}");
        }
        let argv = marked(&words("bash"), &[], &dir).unwrap_or_default().argv;
        assert_eq!(argv.get(1).map(String::as_str), Some("--rcfile"));
        // The caller's own PROMPT_COMMAND goes along: bare bash keeps it.
        let own = [("PROMPT_COMMAND".to_owned(), "mine".to_owned())];
        let bare = marked(&words("bash --norc"), &own, &dir)
            .unwrap_or_default()
            .env;
        assert!(
            bare.iter()
                .any(|(k, v)| k == "PROMPT_COMMAND" && v.contains("\nmine\n"))
        );
        let zdot = [("ZDOTDIR".to_owned(), "/home/me/.config/zsh".to_owned())];
        let vars = marked(&words("zsh"), &zdot, &dir).unwrap_or_default().env;
        assert!(vars.contains(&("FUX_ZDOTDIR".to_owned(), "/home/me/.config/zsh".to_owned())));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
