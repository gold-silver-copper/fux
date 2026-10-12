//! Shell marks for the shells fux starts: bash, zsh and fish are started so
//! that they mark each command (OSC 133 `C` as its output begins, `D;STATUS`
//! as it ends), after reading the user's own startup files as they would.
//! A host then knows when a command typed into them is done, how, and which
//! output was its, with nothing set up (`wait --done`, `--last-output`).
//!
//! Each shell is given a startup file fux writes beside its socket, which
//! reads the user's own first: bash's `--rcfile` sources `~/.bashrc`, zsh's
//! `ZDOTDIR` shim puts the user's back and sources their `.zshenv`, fish's
//! `--init-command` only adds two event handlers; bash with `--norc` reads
//! no file, so takes them from `PS0` and `PROMPT_COMMAND`. A shell started
//! with a script or a command is left as it is, as is any other program.
use std::path::{Path, PathBuf};

/// bash: the user's own startup, then the marks. bash 4.4 and later print
/// `PS0` as a command's output begins; older ones (macOS's 3.2) mark it
/// from a DEBUG trap instead, armed at the end of each prompt's
/// `PROMPT_COMMAND`, so it fires once, for the command typed.
const BASHRC: &str = r#"# fux: OSC 133 shell marks, after your own startup. Off: `set shell-marks off`.
if [ -r ~/.bashrc ]; then . ~/.bashrc; fi
__fux_end() { local s=$?; printf '\033]133;D;%s\007' "$s"; return $s; }
case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
  "declare -a"*) PROMPT_COMMAND=(__fux_end "${PROMPT_COMMAND[@]}") ;;
  *) PROMPT_COMMAND="__fux_end${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
esac
if [ "${BASH_VERSINFO[0]}" -gt 4 ] || { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]}" -ge 4 ]; }; then
  PS0="${PS0}"$'\033]133;C\007'
else
  # Armed last thing before the prompt, so the trap fires for the first
  # command typed, not for PROMPT_COMMAND's own.
  __fux_arm() { __fux_armed=1; }
  __fux_start() { [ -n "$__fux_armed" ] || return 0; __fux_armed=; printf '\033]133;C\007'; }
  case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
    "declare -a"*) PROMPT_COMMAND+=(__fux_arm) ;;
    *) PROMPT_COMMAND="${PROMPT_COMMAND};__fux_arm" ;;
  esac
  trap '__fux_start' DEBUG
fi
"#;

/// zsh: read first of all startup files, from the shim `ZDOTDIR`; it puts
/// the user's `ZDOTDIR` back, so the rest of their files are read from
/// there as always, and sources their `.zshenv`.
const ZSHENV: &str = r#"# fux: OSC 133 shell marks, after your own startup. Off: `set shell-marks off`.
if [[ -n "${FUX_ZDOTDIR+x}" ]]; then ZDOTDIR=$FUX_ZDOTDIR; unset FUX_ZDOTDIR; else unset ZDOTDIR; fi
[[ -r "${ZDOTDIR:-$HOME}/.zshenv" ]] && source "${ZDOTDIR:-$HOME}/.zshenv"
if [[ -o interactive ]]; then
  __fux_preexec() { print -n '\e]133;C\a' }
  __fux_precmd() { print -n "\e]133;D;$?\a" }
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
    // Marks of the caller's own, given as variables, are left to work.
    if env.iter().any(|(k, _)| k == "PS0" || k == "PROMPT_COMMAND") {
        return None;
    }
    let name = Path::new(program.trim_start_matches('-'))
        .file_name()?
        .to_str()?;
    // Only an interactive shell: no script or command, startup files read.
    let flags_only = |allowed: &[&str]| args.iter().all(|a| allowed.contains(&a.as_str()));
    match name {
        // Without startup files bash takes the marks from its variables,
        // which nothing then sets (4.4 and later, which print PS0).
        "bash"
            if flags_only(&["-i", "--norc", "--noprofile"])
                && args.iter().any(|a| a == "--norc") =>
        {
            let mut vars = vec![("PS0".to_owned(), "\\e]133;C\\a".to_owned())];
            vars.push((
                "PROMPT_COMMAND".to_owned(),
                r#"printf '\033]133;D;%s\007' "$?""#.to_owned(),
            ));
            Some(Marked {
                argv: argv.to_vec(),
                env: vars,
            })
        }
        "bash" if flags_only(&["-i", "--noprofile"]) => {
            let rc = write(dir, "bashrc", BASHRC)?;
            let mut argv = vec![program.clone(), "--rcfile".into()];
            argv.push(rc.to_string_lossy().into_owned());
            argv.extend(args.iter().cloned());
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
            "bash -l",
            "sh",
            "zsh -c ls",
            "vim",
            "fish -c ls",
        ] {
            assert!(marked(&words(other), &[], &dir).is_none(), "{other}");
        }
        let argv = marked(&words("bash"), &[], &dir).unwrap_or_default().argv;
        assert_eq!(argv.get(1).map(String::as_str), Some("--rcfile"));
        let own = [("PROMPT_COMMAND".to_owned(), "mine".to_owned())];
        assert!(
            marked(&words("bash"), &own, &dir).is_none(),
            "the caller's own marks"
        );
        let zdot = [("ZDOTDIR".to_owned(), "/home/me/.config/zsh".to_owned())];
        let vars = marked(&words("zsh"), &zdot, &dir).unwrap_or_default().env;
        assert!(vars.contains(&("FUX_ZDOTDIR".to_owned(), "/home/me/.config/zsh".to_owned())));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
