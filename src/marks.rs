//! Shell marks for the shells fux starts: bash, zsh and fish are started so
//! that they mark each command (OSC 133 `C` as its output begins, `D;STATUS`
//! as it ends, and `A` where the next prompt starts), after reading the
//! user's own startup files as they would. A host then knows when a command
//! typed into them is done, how, which output was its, and where each
//! prompt is, with nothing set up (`line`'s `· done`, `last-output`, copy
//! mode's `[` and `]`).
//!
//! Each shell is given a startup file fux writes beside its socket, which
//! reads the user's own first: bash's `--rcfile` sources `~/.bashrc` (a
//! login bash's, the profile files; with `--norc`, nothing, on macOS: bash
//! `--norc` elsewhere stays so, as Debian's reads /etc/bash.bashrc given
//! any rc file, and takes the marks from `PS0` and `PROMPT_COMMAND`), zsh's
//! `ZDOTDIR` shim puts the user's back and sources their `.zshenv`, fish's
//! `--init-command` only adds two event handlers. A shell started with a
//! script or a command is left as it is, as is any other program.
use std::path::{Path, PathBuf};

/// bash: the user's own startup, then the marks, set by `__fux_marks`, a
/// function defined first. A function inherits no DEBUG or RETURN trap
/// (unless `set -T`), so a trap the user's startup set sees one command of
/// fux's: its call. With `set -T` it sees what the function runs too, but
/// what it prints goes nowhere (the function's output, and END's, go to
/// /dev/null; END writes its marks to a copy of stderr), and nothing of fux's reads it
/// (no command substitution, from bash 4.4). The marks run no command of
/// the shell's own either, so
/// the user's DEBUG, ERR and RETURN traps see nothing of them, and `$?` and
/// `PIPESTATUS` stay the command's, for the user's hooks too:
///
/// - `END`, first in `PROMPT_COMMAND` (its first element, an array's
///   too, so that setting it anew replaces the user's hooks, as without
///   fux), so before the user's own hooks print: subshells, which inherit
///   no DEBUG trap, print `D;STATUS` and `A`, the test of an `if` (which
///   sets off no ERR trap) that then makes `PIPESTATUS` and `$?` again.
///   The status, `PIPESTATUS` and the state (`__fux_w`, a prompt
///   shown and no command started since; `__fux_h`, the next history
///   number then) are kept in the arithmetic of a redirection of its own,
///   as no subshell can keep them: a line run with no start mark (one bash
///   could not parse, or a comment: the history number moved) gets one as
///   it ends, `C;late`, which starts its output after the prompt's line, so
///   bash's complaint is its output. Its stderr is /dev/null, so that
///   `set -x` traces none of it; the marks go to fd 3, stderr's copy.
/// - `PS1` ends a command too, in case `PROMPT_COMMAND` is set anew while
///   the shell runs (a `D` that ends no command does nothing); its hooks
///   then print before that end.
/// - bash 4.4 and later print `PS0` as a command's output begins. Older
///   ones (macOS's 3.2) mark it from a DEBUG trap, set after the function
///   (there a function's own is dropped as it returns), and armed last
///   thing before the prompt, so it fires once, for the command typed; and
///   there `HISTCMD` reads 1 in `PROMPT_COMMAND`, so `history 1` tells the
///   history number. The user's own DEBUG trap runs on in it, after the
///   mark, with its `$?`; it sees the command that sets the trap too, as
///   nothing a function sets outlasts its call there.
///
/// A variable the user made readonly is left as it is, unmarked (told from
/// its attributes, before bash 4.4 `declare -p`: an assignment tried in a subshell leaves bash 3.2's
/// subshell reading the terminal in the shell's place). Marks go to
/// stderr, as the prompt does; fux's PROMPT_COMMAND is not exported, so
/// shells started from this one do not inherit it.
const BASHRC: &str = r#"if [ -r ~/.bashrc ]; then . ~/.bashrc; fi
"#;

/// A login bash's startup, read as `bash -l` reads it (a login shell takes
/// no --rcfile, so fux starts bash interactive with this one instead).
const BASH_LOGIN: &str = r#"if [ -r /etc/profile ]; then . /etc/profile; fi
for __fux_f in ~/.bash_profile ~/.bash_login ~/.profile; do
  if [ -r "$__fux_f" ]; then . "$__fux_f"; break; fi
done
unset __fux_f
"#;

/// A command's end, `{H}` the next history number, `{K}` keeping the
/// command's `PIPESTATUS` and `{P}` making it again (see `BASHRC`). No
/// single quotes: the startup file quotes it with them.
const END: &str = r#"{ if ([ "$__fux_m" = 1 ] && printf "\033]133;C;late\007" >&3; printf "\033]133;D;%s\007\033]133;A\007" "$__fux_s" >&3; exit $((__fux_c != 1))); then {P}; fi; } >/dev/null 3>&$((__fux_s = $?, {K}__fux_c = ${#PIPESTATUS[@]}, __fux_n = {H}, __fux_m = ${__fux_w-0} && __fux_n != ${__fux_h-0}, __fux_w = 1, __fux_h = __fux_n, 2)) 2>/dev/null"#;
/// The longest pipeline whose statuses `END` gives the user's hooks in
/// `PIPESTATUS`; of a longer one they see its status alone.
const PIPES: usize = 8;
const HISTORY_NEW: &str = "${HISTCMD-0}";
const HISTORY_OLD: &str = r#"$(__fux_n=$(history 1); __fux_n=${__fux_n#"${__fux_n%%[0-9]*}"}; echo "${__fux_n%%[!0-9]*}") + 1"#;
/// Before bash 4.4: the DEBUG trap, as a `printf` format of the user's own
/// trap twice (`trap -p` in a command substitution still shows it): once
/// armed, the start mark (a `case`, as a command there would change
/// `PIPESTATUS`) and `$?` put back, then theirs. Its arming, last in
/// `PROMPT_COMMAND` and in `PS1`.
const START: &str = r#"case ${__fux_a-}${__fux_z[__fux_q = $?]-} in 1) __fux_a=0; __fux_w=0; printf "\\033]133;C\\007" >&2; (exit $__fux_q)\n%s\n;; *) %s\n;; esac"#;
const ARM: &str = "{ (:); } 2>&$((__fux_a = 1, 2))";
/// What `PS1` starts with: a command's end, should `END` not have run, and
/// the trap's arming.
const PS1_END: &str = r"\[\033]133;D;$?\007\033]133;A\007${__fux_z[__fux_a = 1]-}\]";
/// What `PS0` ends with: a command's start.
const PS0_START: &str = r"\033]133;C\007${__fux_z[__fux_w = 0]-}";

/// `END` for bash 4.4 and later, and for older ones. The command's
/// `PIPESTATUS` is kept (`__fux_p`, its length `__fux_c`) and made again
/// last, for the user's hooks after: a pipeline of as many subshells, each
/// exiting with a status, run twice (`&&`: once if it fails, as a failure
/// there would set off the ERR trap; again if not, so that it is the last
/// pipeline run).
fn ends() -> (String, String) {
    let kept: String = (0..PIPES)
        .map(|i| format!("__fux_p[{i}] = ${{PIPESTATUS[{i}]-0}}, "))
        .collect();
    let again: Vec<String> = (1..=PIPES)
        .map(|n| {
            let pipe = (0..n)
                .map(|i| format!("(exit ${{__fux_p[{i}]}})"))
                .collect::<Vec<_>>()
                .join(" | ");
            format!("{pipe} && {pipe}")
        })
        .collect();
    let mut made = String::new();
    for (n, pipe) in (1..).zip(&again) {
        if n > 1 {
            made.push_str(&format!("; elif (exit $((__fux_c != {n}))); then "));
        }
        made.push_str(pipe);
    }
    made.push_str("; else (exit \"$__fux_s\") && (:)");
    let end = END.replace("{K}", &kept).replace("{P}", &made);
    (
        end.replace("{H}", HISTORY_NEW),
        end.replace("{H}", HISTORY_OLD),
    )
}

/// A bash startup file: fux's function, `startup`, then the marks.
fn bash_rc(startup: &str) -> String {
    let (new, old) = ends();
    format!(
        r#"# fux: OSC 133 shell marks, after your own startup. Off: `str shell-marks str off set`.
__fux_marks() {{
  unset -f __fux_marks
  __fux_free() {{ local d; if [ $__fux_new = 1 ]; then d=${{!1@a}}; else d=$(declare -p "$1" 2>/dev/null); d=${{d#declare -}}; d=${{d%% *}}; fi; case $d in *r*) return 1 ;; esac; }}
  local __fux_new=0 __fux_end
  if [ "${{BASH_VERSINFO[0]}}${{BASH_VERSINFO[1]}}" -ge 44 ]; then __fux_new=1; __fux_end='{new}'; else __fux_end='{old}'; fi
  if __fux_free PROMPT_COMMAND; then
    PROMPT_COMMAND="$__fux_end${{PROMPT_COMMAND:+$'\n'$PROMPT_COMMAND}}"
    if [ $__fux_new = 0 ]; then PROMPT_COMMAND="$PROMPT_COMMAND"$'\n''{ARM}'; fi
    export -n PROMPT_COMMAND
  fi
  if [ $__fux_new = 1 ] && __fux_free PS0; then
    PS0="${{PS0-}}"'{PS0_START}'
    export -n PS0
  fi
  if __fux_free PS1; then PS1='{PS1_END}'"${{PS1-}}"; fi
  unset -f __fux_free
}} >/dev/null 2>&1
{startup}__fux_marks
([ "${{BASH_VERSINFO[0]}}${{BASH_VERSINFO[1]}}" -ge 44 ]) >/dev/null || trap -- "$(eval "set -- $(trap -p DEBUG)"; printf '{START}' "${{3-}}" "${{3-}}")" DEBUG
"#,
    )
}

/// bash that reads no file of fux's (`--norc`), from its variables.
/// PROMPT_COMMAND never comes from the environment: bash 5.3 empties
/// `PIPESTATUS` for good once it imports one. The first prompt's `PS1`
/// sets it from `__fux_pc` (an expansion that prints nothing), and starts
/// the marks' state (`END`); its first run, once the first command
/// ends, sets the PROMPT_COMMAND that stays (`__fux_pn`, or before bash
/// 4.4 `__fux_po` and the trap), puts the caller's `PS1` back, forgets
/// fux's variables, and runs it.
const BARE_FIRST: &str = r#"__fux_s=$?; if [ "${BASH_VERSINFO[0]}${BASH_VERSINFO[1]}" -ge 44 ]; then PROMPT_COMMAND=$__fux_pn; else PROMPT_COMMAND=$__fux_po; trap '{START}' DEBUG; fi; if [ -n "${__fux_ps1+x}" ]; then PS1='{PS1_END}'$__fux_ps1; else PS1='{PS1_END}\s-\v\$ '; export -n PS1; fi; unset __fux_pc __fux_pn __fux_po __fux_ps1; export -n PS0 PROMPT_COMMAND; (exit "$__fux_s") && :; eval "$PROMPT_COMMAND""#;

/// zsh: read first of all startup files, from the shim `ZDOTDIR`; it puts
/// the user's `ZDOTDIR` back, so the rest of their files are read from
/// there as always, and sources their `.zshenv`.
const ZSHENV: &str = r#"# fux: OSC 133 shell marks, after your own startup. Off: `str shell-marks str off set`.
if [[ -n "${FUX_ZDOTDIR+x}" ]]; then ZDOTDIR=$FUX_ZDOTDIR; unset FUX_ZDOTDIR; else unset ZDOTDIR; fi
[[ -r "${ZDOTDIR:-$HOME}/.zshenv" ]] && source "${ZDOTDIR:-$HOME}/.zshenv"
if [[ -o interactive ]]; then
  __fux_preexec() { print -n '\e]133;C\a' >&2 }
  __fux_precmd() { print -n "\e]133;D;$?\a\e]133;A\a" >&2 }
  preexec_functions=(__fux_preexec $preexec_functions)
  precmd_functions=(__fux_precmd $precmd_functions)
fi
"#;

/// fish: two event handlers, after its own startup.
const FISH: &str = "function __fux_preexec --on-event fish_preexec; printf '\\e]133;C\\a' >&2; end; \
     function __fux_postexec --on-event fish_postexec; printf '\\e]133;D;%s\\a\\e]133;A\\a' $status >&2; end";

/// How a shell is started so that it marks its commands: its words, and
/// variables to add and remove.
#[derive(Debug, Default)]
pub struct Marked {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Variables the shell must not be given, whoever set them.
    pub unset: Vec<String>,
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
    // A variable as the shell would be given it.
    let theirs = |key: &str| {
        (env.iter().rev())
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    match name {
        // Without fux's startup file (`--norc`, or a login shell) bash
        // takes the marks from its variables, around the caller's own.
        // Not on macOS, whose bash 3.2 cannot mark a first command so and
        // reads no system rc file (Debian's reads /etc/bash.bashrc unless
        // given --norc itself).
        "bash"
            if !cfg!(target_os = "macos")
                && flags_only(&["-i", "--norc", "--noprofile"])
                && args.iter().any(|a| a == "--norc") =>
        {
            let (new, old) = ends();
            let (new, old) = match theirs("PROMPT_COMMAND") {
                Some(own) => (format!("{new}\n{own}"), format!("{old}\n{own}\n{ARM}")),
                None => (new, format!("{old}\n{ARM}")),
            };
            let first_run = BARE_FIRST
                .replace("{START}", &START.replace("%s", ":"))
                .replace("{PS1_END}", PS1_END);
            let start = format!("{}{PS0_START}", theirs("PS0").unwrap_or_default());
            // The first prompt sets PROMPT_COMMAND from `__fux_pc`, as the
            // pattern removed from `__fux_pc` itself: all of it, so it shows
            // nothing; and starts the marks' state.
            let first = format!(
                "${{__fux_pc#\"${{PROMPT_COMMAND:=$__fux_pc}}\"}}\
                 ${{__fux_z[__fux_w = 1, __fux_h = ${{HISTCMD-0}}]-}}{}",
                theirs("PS1").unwrap_or_else(|| "\\s-\\v\\$ ".to_owned())
            );
            let mut env = vec![
                ("__fux_pc".to_owned(), first_run),
                ("__fux_pn".to_owned(), new),
                ("__fux_po".to_owned(), old),
                ("PS1".to_owned(), first),
                ("PS0".to_owned(), start),
            ];
            if let Some(own) = theirs("PS1") {
                env.push(("__fux_ps1".to_owned(), own));
            }
            Some(Marked {
                argv: argv.to_vec(),
                env,
                unset: vec!["PROMPT_COMMAND".to_owned()],
            })
        }
        // fux's rc file reads what bash would: a login shell, which takes
        // no rc file, the profile files (unless --noprofile), another
        // `~/.bashrc` (unless --norc).
        "bash" if flags_only(&["-i", "--norc", "--noprofile", "-l", "--login"]) => {
            let has = |flag: &str| args.iter().any(|a| a == flag);
            let login = has("-l") || has("--login");
            let (name, startup) = if login && !has("--noprofile") {
                ("bash_login", BASH_LOGIN)
            } else if !login && !has("--norc") {
                ("bashrc", BASHRC)
            } else {
                ("bash_bare", "")
            };
            let rc = write(dir, name, &bash_rc(startup))?;
            let mut argv = vec![program.clone(), "--rcfile".into()];
            argv.push(rc.to_string_lossy().into_owned());
            argv.extend(
                (args.iter())
                    .filter(|a| !["-l", "--login", "--norc"].contains(&a.as_str()))
                    .cloned(),
            );
            Some(Marked {
                argv,
                ..Marked::default()
            })
        }
        "zsh" if flags_only(&["-i", "-l", "--login"]) => {
            write(&dir.join("zsh"), ".zshenv", ZSHENV)?;
            let shim = dir.join("zsh").to_string_lossy().into_owned();
            // The user's own ZDOTDIR, from what the shell would be given.
            let theirs = theirs("ZDOTDIR").or_else(|| std::env::var("ZDOTDIR").ok());
            let mut vars = vec![("ZDOTDIR".to_owned(), shim)];
            vars.extend(theirs.map(|v| ("FUX_ZDOTDIR".to_owned(), v)));
            Some(Marked {
                argv: argv.to_vec(),
                env: vars,
                ..Marked::default()
            })
        }
        "fish" if flags_only(&["-i", "-l", "--login"]) => {
            let mut argv = argv.to_vec();
            argv.extend(["--init-command".to_owned(), FISH.to_owned()]);
            Some(Marked {
                argv,
                ..Marked::default()
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
        // The rc file reads what bash would have read; a login shell takes
        // no rc file, so its flag goes, the others stay.
        for (shell, rc, rest) in [
            ("bash -i", "bashrc", "-i"),
            ("bash -l", "bash_login", ""),
            ("bash --login -i", "bash_login", "-i"),
            ("bash -l --noprofile", "bash_bare", "--noprofile"),
        ] {
            let argv = marked(&words(shell), &[], &dir).unwrap_or_default().argv;
            assert!(
                argv.get(2).is_some_and(|f| f.ends_with(rc)),
                "{shell}: {argv:?}"
            );
            assert_eq!(argv.get(3..), Some(&words(rest)[..]), "{shell}");
        }
        // bash --norc stays so (Debian's would read /etc/bash.bashrc given
        // an rc file), its marks from its variables, the caller's own
        // PROMPT_COMMAND along; on macOS (bash 3.2) it reads an rc file of
        // fux's alone.
        let own = [("PROMPT_COMMAND".to_owned(), "mine".to_owned())];
        let bare = marked(&words("bash --norc -i"), &own, &dir).unwrap_or_default();
        if cfg!(target_os = "macos") {
            assert!(bare.argv.get(2).is_some_and(|rc| rc.ends_with("bash_bare")));
            assert_eq!(bare.argv.get(3..), Some(&["-i".to_owned()][..]));
        } else {
            assert_eq!(bare.argv, words("bash --norc -i"));
            assert!((bare.env.iter()).any(|(k, v)| k == "__fux_pn" && v.ends_with("\nmine")));
        }
        let zdot = [("ZDOTDIR".to_owned(), "/home/me/.config/zsh".to_owned())];
        let vars = marked(&words("zsh"), &zdot, &dir).unwrap_or_default().env;
        assert!(vars.contains(&("FUX_ZDOTDIR".to_owned(), "/home/me/.config/zsh".to_owned())));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
