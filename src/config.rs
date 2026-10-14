//! The server's settings: what every pane and client of it shares, set by
//! the `set` word from a client's config or command line. Key bindings and
//! the prefix are each client's own (`crate::keymap`).
use crate::words;
use std::path::PathBuf;

/// Why a setting is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    UnknownOption {
        option: String,
    },
    NotANumber {
        option: String,
    },
    OutOfRange {
        option: String,
        min: usize,
        max: usize,
    },
    NotOnOff {
        option: String,
        value: String,
    },
    NoProgram,
    /// A name `update-environment` is given that no variable has.
    NotAVariable {
        name: String,
    },
    Words(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::UnknownOption { option } => write!(
                f,
                "no setting {option:?}; the settings are shell, history-lines, clipboard, \
                 buffers, bell, titles, shell-marks and update-environment"
            ),
            Error::NotANumber { option } => write!(f, "{option}: not a number"),
            Error::OutOfRange { option, min, max } => {
                write!(f, "{option}: out of range, {min} to {max}")
            }
            Error::NotOnOff { option, value } => {
                write!(f, "{option}: {value:?} is not on or off (true or false)")
            }
            Error::NoProgram => f.write_str("shell: a program is needed"),
            Error::NotAVariable { name } => write!(
                f,
                "update-environment: {name:?} is no variable's name (letters, digits and _, \
                 not first a digit)"
            ),
            Error::Words(why) => write!(f, "shell: {why}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub shell: Vec<String>,
    pub history_lines: usize,
    /// Copies are also sent to the client's terminal as OSC 52.
    pub clipboard: bool,
    /// How many paste buffers are kept.
    pub buffers: usize,
    /// A pane's bell rings in the terminals showing its workspace.
    pub bell: bool,
    /// Each client's terminal title is its focused pane's.
    pub titles: bool,
    /// The shells fux starts mark their commands (OSC 133): `crate::marks`.
    pub shell_marks: bool,
    /// Variables a new pane takes from whoever starts it (a request's
    /// caller, the client whose key it is) rather than from the server,
    /// which may have started long ago. One the starter has not is unset.
    pub update_environment: Vec<String>,
}

/// The largest history a pane keeps.
pub const MAX_HISTORY: usize = 250_000;
pub const MAX_BUFFERS: usize = 1000;

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

impl Default for Config {
    fn default() -> Self {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(login_shell)
            .unwrap_or_else(|| "/bin/sh".into());
        Self {
            shell: vec![shell],
            history_lines: 10_000,
            clipboard: true,
            buffers: 16,
            bell: true,
            titles: false,
            shell_marks: true,
            update_environment: [
                "DISPLAY",
                "KRB5CCNAME",
                "SSH_AGENT_PID",
                "SSH_ASKPASS",
                "SSH_AUTH_SOCK",
                "SSH_CONNECTION",
                "WAYLAND_DISPLAY",
                "XAUTHORITY",
            ]
            .map(str::to_owned)
            .to_vec(),
        }
    }
}

impl Config {
    /// Sets `option` to `value`, its text: a number, `on` or `off` (or
    /// `true` or `false`), a command line for `shell`, names for
    /// `update-environment`.
    pub fn set(&mut self, option: &str, value: &str) -> Result<(), Error> {
        let name = || option.to_owned();
        let number = |min: usize, max: usize| -> Result<usize, Error> {
            let n: i128 = value
                .parse()
                .map_err(|_| Error::NotANumber { option: name() })?;
            (usize::try_from(n).ok())
                .filter(|n| (min..=max).contains(n))
                .ok_or(Error::OutOfRange {
                    option: name(),
                    min,
                    max,
                })
        };
        let on = || match value {
            "on" | "true" => Ok(true),
            "off" | "false" => Ok(false),
            other => Err(Error::NotOnOff {
                option: name(),
                value: other.to_owned(),
            }),
        };
        match option {
            "shell" => {
                let argv = words::split(value).map_err(|e| Error::Words(e.to_string()))?;
                if argv.first().is_none_or(|p| p.is_empty()) {
                    return Err(Error::NoProgram);
                }
                self.shell = argv;
            }
            "history-lines" => self.history_lines = number(0, MAX_HISTORY)?,
            "clipboard" => self.clipboard = on()?,
            "bell" => self.bell = on()?,
            "titles" => self.titles = on()?,
            "shell-marks" => self.shell_marks = on()?,
            "update-environment" => {
                let names: Vec<String> = value.split_whitespace().map(str::to_owned).collect();
                if let Some(bad) = names.iter().find(|n| !crate::command::shell_name(n)) {
                    return Err(Error::NotAVariable { name: bad.clone() });
                }
                self.update_environment = names;
            }
            "buffers" => self.buffers = number(1, MAX_BUFFERS)?,
            other => {
                return Err(Error::UnknownOption {
                    option: other.to_owned(),
                });
            }
        }
        Ok(())
    }
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
        assert_eq!(config.set("history-lines", "5000"), Ok(()));
        assert_eq!(config.history_lines, 5000);
        assert_eq!(config.set("bell", "false"), Ok(()));
        assert!(!config.bell);
        assert_eq!(config.set("shell", "/bin/zsh -l"), Ok(()));
        assert_eq!(config.shell, ["/bin/zsh", "-l"]);
        assert!(config.set("history-lines", "lots").is_err());
        let below = config.set("history-lines", "-1").map_err(|e| e.to_string());
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
