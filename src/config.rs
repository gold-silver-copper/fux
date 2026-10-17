//! The server's settings: what every pane and client of it shares, set by
//! the `set` word from a client's config or command line. Key bindings and
//! the prefix are each client's own (`crate::keymap`).
use crate::code::Code;
use crate::log::JOURNAL_BYTES;
use crate::words;
use std::path::PathBuf;
use std::time::Duration;

/// `settings! { /// doc  field "name": Type = default, parse; … }` expands
/// to the struct `Config` of those fields, its `Default`, `NAMES` (every
/// setting's name, in order) and `Config::set`, which reads a setting's
/// text with its `parse` (a `&str` to `Result<Type, E>`, `E` a `String`,
/// refused as `config`, or a `Refused` with its own code). The row
/// `clipboard "clipboard": bool = true, on;` gives the field `pub clipboard:
/// bool`, `true` in `Default`, `"clipboard"` in `NAMES`, and in `set` the
/// arm `"clipboard" => self.clipboard = on(value).map_err(|e|
/// named(e.into()))?`.
macro_rules! settings {
    ($($(#[$doc:meta])* $field:ident $name:literal: $ty:ty = $default:expr, $parse:expr;)*) => {
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct Config {
            $($(#[$doc])* pub $field: $ty,)*
        }

        impl Default for Config {
            fn default() -> Config {
                Config {
                    $($field: $default,)*
                }
            }
        }

        /// Every setting's name, in order.
        pub const NAMES: &[&str] = &[$($name),*];

        impl Config {
            /// Sets setting `name` to `value`, its text: a number, `on` or
            /// `off` (or `true` or `false`), a command line for `shell`,
            /// names for `update-environment`. A setting refused says why.
            pub fn set(&mut self, name: &str, value: &str) -> Result<(), Refused> {
                let named = |Refused(code, why)| Refused(code, format!("{name}: {why}"));
                match name {
                    $($name => self.$field = $parse(value).map_err(|e| named(e.into()))?,)*
                    other => {
                        let all = words::listed(NAMES, "and");
                        return Err(format!("no setting {other:?}; the settings are {all}").into());
                    }
                }
                Ok(())
            }
        }
    };
}

settings! {
    shell "shell": Shell = default_shell(), Shell::parse;
    /// The bytes of history each pane keeps, for copy mode and `capture`.
    history_bytes "history-bytes": usize = 8 << 20, number(0, 1 << 30);
    /// How long an action or a wait waits at most, unless it says.
    wait "wait-seconds": Duration = WAIT, |v| number(0, MAX_WAIT)(v).map(Duration::from_secs);
    /// The bytes each pane's journal keeps (`crate::log`).
    journal_bytes "journal-bytes": usize = JOURNAL_BYTES, number(MIN_JOURNAL, MAX_JOURNAL);
    /// The bytes all panes' journals keep together.
    journal_total "journal-total": usize = 16 * JOURNAL_BYTES, number(MIN_JOURNAL, MAX_JOURNAL);
    /// Copies are also sent to the client's terminal as OSC 52.
    clipboard "clipboard": bool = true, on;
    /// How many paste buffers are kept.
    buffers "buffers": usize = 16, number(1, MAX_BUFFERS);
    /// A pane's bell rings in the terminals showing its workspace.
    bell "bell": bool = true, on;
    /// Each client's terminal title is its focused pane's.
    titles "titles": bool = false, on;
    /// The shells fux starts mark their commands (OSC 133): `crate::marks`.
    shell_marks "shell-marks": bool = true, on;
    /// Variables a new pane takes from whoever starts it (a request's
    /// caller, the client whose key it is) rather than from the server,
    /// which may have started long ago. One the starter has not is unset.
    update_environment "update-environment": Vec<String> = KEPT.map(str::to_owned).to_vec(), names;
}

/// Why a setting is refused: its code (`config`, or its value's own, as
/// `open_quote` for a shell's line), and why.
#[derive(Debug, PartialEq, Eq)]
pub struct Refused(pub Code, pub String);

impl From<String> for Refused {
    fn from(why: String) -> Self {
        Self(Code::Config, why)
    }
}

/// What `update-environment` names at first.
const KEPT: [&str; 8] = [
    "DISPLAY",
    "KRB5CCNAME",
    "SSH_AGENT_PID",
    "SSH_ASKPASS",
    "SSH_AUTH_SOCK",
    "SSH_CONNECTION",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
];

pub const MAX_BUFFERS: usize = 1000;
/// How long an action or a wait waits at most by default: a build or a
/// program's startup is waited through, while a wait that cannot be met
/// still answers well inside what a tool running a command waits for it.
pub const WAIT: Duration = Duration::from_secs(30);
/// The longest `wait-seconds`: a day.
pub const MAX_WAIT: usize = 86_400;
/// The most bytes journals may be given: each, or all together.
pub const MAX_JOURNAL: usize = 1 << 31;
/// The fewest bytes a journal may be given: waits and events read it.
pub const MIN_JOURNAL: usize = 1 << 20;

/// The user's login shell from `/etc/passwd`, for when `SHELL` is unset
/// (a service, a sandbox, a cleared environment): bash or zsh there marks
/// its commands, where `/bin/sh` would not.
fn login_shell() -> Option<String> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    shell_of(&passwd, fuxix::process::geteuid())
}

/// The shell of user `uid` in `passwd`, the text of `/etc/passwd`.
fn shell_of(passwd: &str, uid: u32) -> Option<String> {
    let uid = uid.to_string();
    passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        (fields.get(2) == Some(&uid.as_str()))
            .then(|| fields.get(6).map(|s| s.trim().to_owned()))
            .flatten()
            .filter(|s| !s.is_empty())
    })
}

/// `SHELL`, else the login shell, else `/bin/sh`: the program alone, with
/// no arguments.
fn default_shell() -> Shell {
    let program = (std::env::var("SHELL").ok())
        .filter(|s| !s.is_empty())
        .or_else(login_shell)
        .unwrap_or_else(|| "/bin/sh".into());
    Shell {
        program,
        args: Vec::new(),
    }
}

/// A whole number from `min` to `max`, read from its text.
fn number<T: TryFrom<usize>>(min: usize, max: usize) -> impl Fn(&str) -> Result<T, String> {
    move |value| {
        let n: i128 = value.parse().map_err(|_| "not a number".to_owned())?;
        (usize::try_from(n).ok())
            .filter(|n| (min..=max).contains(n))
            .and_then(|n| T::try_from(n).ok())
            .ok_or_else(|| format!("out of range, {min} to {max}"))
    }
}

/// `on` or `off` (or `true` or `false`).
fn on(value: &str) -> Result<bool, String> {
    match value {
        "on" | "true" => Ok(true),
        "off" | "false" => Ok(false),
        other => Err(format!("{other:?} is not on or off (true or false)")),
    }
}

/// The shell fux starts in a pane: a program, never empty, then its
/// arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell {
    program: String,
    args: Vec<String>,
}

impl Shell {
    /// A command line, split as a shell splits it: a program first.
    pub fn parse(value: &str) -> Result<Shell, Refused> {
        let words =
            words::split(value).map_err(|why| Refused(Code::from(fuxth::Code::OpenQuote), why))?;
        match words.split_first() {
            Some((program, args)) if !program.is_empty() => Ok(Shell {
                program: program.clone(),
                args: args.to_vec(),
            }),
            Some(_) | None => Err(Refused::from("a program is needed".to_owned())),
        }
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    /// Its command line, the program first.
    pub fn argv(&self) -> Vec<String> {
        let all = std::iter::once(&self.program).chain(&self.args);
        all.cloned().collect()
    }
}

/// Variables' names, separated by blanks.
fn names(value: &str) -> Result<Vec<String>, String> {
    (value.split_whitespace())
        .map(|name| crate::command::var_name(name).map(|()| name.to_owned()))
        .collect()
}

/// The config file, a fuxth program each client runs as it attaches (and
/// a server's first client, as it starts it): `$XDG_CONFIG_HOME/fux/config.fux`,
/// else `~/.config/fux/config.fux`.
pub fn default_path() -> Option<PathBuf> {
    let set = |name: &str| std::env::var_os(name).filter(|d| !d.is_empty());
    let config = set("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("fux").join("config.fux"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_take_their_values_as_text() {
        let mut config = Config::default();
        assert_eq!(config.set("history-bytes", "5000"), Ok(()));
        assert_eq!(config.history_bytes, 5000);
        assert_eq!(config.set("bell", "false"), Ok(()));
        assert!(!config.bell);
        assert_eq!(config.set("shell", "/bin/zsh -l"), Ok(()));
        assert_eq!(config.shell.argv(), ["/bin/zsh", "-l"]);
        assert!(config.set("history-bytes", "lots").is_err());
        let below = config.set("history-bytes", "-1").map_err(|e| e.1);
        assert!(below.is_err_and(|e| e.contains("out of range")));
        assert!(config.set("buffers", "0").is_err());
        assert!(config.set("nonsense", "1").is_err());
        assert!(config.set("update-environment", "OK BAD-NAME").is_err());
    }

    /// With `SHELL` unset, the shell is the user's own line's in
    /// `/etc/passwd`, not the first line's.
    #[test]
    fn the_login_shell_is_the_users_own() {
        let passwd = "root:x:0:0::/root:/bin/bash\n\
                      me:x:1001:1001::/home/me:/usr/bin/zsh\n\
                      bare:x:1002:1002::/home/bare:\n";
        assert_eq!(shell_of(passwd, 1001).as_deref(), Some("/usr/bin/zsh"));
        assert_eq!(shell_of(passwd, 0).as_deref(), Some("/bin/bash"));
        assert_eq!(shell_of(passwd, 1002), None);
        assert_eq!(shell_of(passwd, 7), None);
    }
}
