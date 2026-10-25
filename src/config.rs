//! The server's settings: what every pane and client of it shares, set by
//! the `set` word from a client's config or command line. Key bindings and
//! the prefix are each client's own (`crate::keymap`).
use crate::words;
use std::path::PathBuf;
use std::time::Duration;

/// `settings! { /// about  field "name": Reads = default; … }` expands to
/// the struct `Config` of those fields, each of `Reads`'s value, its
/// `Default`, `NAMES` (every setting's name, in order), `Config::set`,
/// which reads a setting's text with its `Reads`, and `table`, every
/// setting with what it takes, its default and its about, which `fux
/// help set` prints. The row `/// A bell rings.  bell "bell": bool =
/// true;` gives the field `pub bell: bool`, `true` in `Default`, `"bell"`
/// in `NAMES`, in `set` the arm `"bell" => self.bell = bool::read(value)`,
/// and in `table` the row `bell`, `on or off`, `on`, `A bell rings.`. A
/// default the server reads where it runs is told by its rule, which
/// `=> "RULE"` after it gives `table`, not by the value the process
/// asking for help would read.
macro_rules! settings {
    (@default $shown:literal; $value:expr) => {
        $shown.to_owned()
    };
    (@default; $value:expr) => {
        $value
    };
    ($($(#[doc = $about:literal])* $field:ident $name:literal: $reads:ty = $default:expr
        $(=> $shown:literal)?;)*) => {
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct Config {
            $($(#[doc = $about])* pub $field: <$reads as Reads>::Value,)*
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
            /// Sets setting `name` to `value`, its text, as its `Reads`
            /// reads it. A setting refused says why.
            pub fn set(&mut self, name: &str, value: &str) -> Result<(), String> {
                let named = |why| format!("{name}: {why}");
                match name {
                    $($name => self.$field = <$reads as Reads>::read(value).map_err(named)?,)*
                    other => {
                        let all = words::listed(NAMES, "and");
                        return Err(format!("no setting {other:?}; the settings are {all}"));
                    }
                }
                Ok(())
            }
        }

        /// Every setting, in order, as `fux help set` tells it: its name,
        /// what text it takes, its default as that text, and what it does.
        pub fn table() -> Vec<[String; 4]> {
            let defaults = Config::default();
            vec![$([
                $name.to_owned(),
                <$reads as Reads>::takes(),
                settings!(@default $($shown)?; <$reads as Reads>::show(&defaults.$field)),
                [$($about.trim()),*].join(" "),
            ],)*]
        }
    };
}

/// How a setting's text is read: the value it gives (the type itself, but
/// for `Number`'s ranges), what text it takes, and a value written back as
/// that text.
pub trait Reads {
    type Value;
    fn takes() -> String;
    fn read(text: &str) -> Result<Self::Value, String>;
    fn show(value: &Self::Value) -> String;
}

settings! {
    /// The program a new pane runs, and its arguments: words parted by
    /// blanks, none quoted, so `#` is a word like any other, and a quote
    /// (`'`, `"`) or a backslash is refused. A program or argument with a
    /// blank, quote or backslash in it is run through a wrapper script, or
    /// a symlink to it, whose path has none.
    shell "shell": Shell = default_shell()
        => "the server's `$SHELL`, else your login shell, else `/bin/sh`";
    /// The bytes of history each pane keeps: how far copy mode scrolls and
    /// `history capture` reads. Past it, the oldest rows go; a change
    /// applies to every pane.
    history_bytes "history-bytes": Number<0, { 1 << 30 }> = 8 << 20;
    /// How long a wait waits at most unless it says (`SECS timeout`).
    wait "wait-seconds": Limit = Limit::DEFAULT;
    /// Copies, fux's and programs', also reach your terminal's clipboard
    /// (OSC 52).
    clipboard "clipboard": bool = true;
    /// How many paste buffers are kept.
    buffers "buffers": Number<1, MAX_BUFFERS> = 16;
    /// A pane's bell rings your terminal and marks its tab.
    bell "bell": bool = true;
    /// Your terminal's title follows the focused pane.
    titles "titles": bool = false;
    /// Variables a new pane takes from whoever starts it (a request's
    /// caller, the client whose key it is) rather than from the server,
    /// which may have started long ago. One the starter lacks is unset,
    /// but for `FUX`, `FUX_PANE`, `FUX_SOCKET` and `FUX_SERVER`.
    update_environment "update-environment": Vec<String> = KEPT.map(str::to_owned).to_vec();
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

/// The most paste buffers kept.
pub const MAX_BUFFERS: usize = 1000;
/// The longest a wait waits: a day.
pub const MAX_WAIT: usize = 86_400;

/// How long a wait waits at most: up to `MAX_WAIT` seconds, so its
/// deadline is a time there is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit(u32);

impl Limit {
    /// By default: a build or a program's startup is waited through, while
    /// a wait that cannot be met still answers well inside what a tool
    /// running a command waits for it.
    pub const DEFAULT: Limit = Limit(30);

    /// `secs` seconds, if at most `MAX_WAIT`.
    pub fn secs(secs: u64) -> Option<Limit> {
        let within = |s: &u32| usize::try_from(*s).is_ok_and(|s| s <= MAX_WAIT);
        u32::try_from(secs).ok().filter(within).map(Limit)
    }

    /// `secs` seconds, or why not.
    pub fn read(secs: u64) -> Result<Limit, String> {
        Limit::secs(secs).ok_or_else(|| format!("{secs} s: a wait waits at most {MAX_WAIT} s"))
    }

    pub fn duration(self) -> Duration {
        Duration::from_secs(u64::from(self.0))
    }

    pub fn seconds(self) -> u32 {
        self.0
    }
}

/// The user's login shell from `/etc/passwd`, for when `SHELL` is unset
/// (a service, a sandbox, a cleared environment).
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

/// A whole number from `MIN` to `MAX`.
pub struct Number<const MIN: usize, const MAX: usize>;

impl<const MIN: usize, const MAX: usize> Reads for Number<MIN, MAX> {
    type Value = usize;
    fn takes() -> String {
        format!("a number, {MIN} to {MAX}")
    }
    fn read(text: &str) -> Result<usize, String> {
        let n: i128 = text.parse().map_err(|_| "not a number".to_owned())?;
        (usize::try_from(n).ok())
            .filter(|n| (MIN..=MAX).contains(n))
            .ok_or_else(|| format!("out of range, {MIN} to {MAX}"))
    }
    fn show(value: &usize) -> String {
        value.to_string()
    }
}

/// A wait's limit, in seconds.
impl Reads for Limit {
    type Value = Limit;
    fn takes() -> String {
        format!("seconds, 0 to {MAX_WAIT}")
    }
    fn read(text: &str) -> Result<Limit, String> {
        let n: i128 = text.parse().map_err(|_| "not a number".to_owned())?;
        let secs = u64::try_from(n).map_err(|_| format!("{n} s: a wait waits 0 s or more"))?;
        Limit::read(secs)
    }
    fn show(value: &Limit) -> String {
        value.seconds().to_string()
    }
}

/// `on` or `off` (or `true` or `false`).
impl Reads for bool {
    type Value = bool;
    fn takes() -> String {
        "on or off".to_owned()
    }
    fn read(text: &str) -> Result<bool, String> {
        match text {
            "on" | "true" => Ok(true),
            "off" | "false" => Ok(false),
            other => Err(format!("{other:?} is not on or off (true or false)")),
        }
    }
    fn show(value: &bool) -> String {
        if *value { "on" } else { "off" }.to_owned()
    }
}

/// A program and its arguments, separated by blanks.
impl Reads for Shell {
    type Value = Shell;
    fn takes() -> String {
        "a program and its arguments, separated by blanks, unquoted".to_owned()
    }
    fn read(text: &str) -> Result<Shell, String> {
        Shell::parse(text)
    }
    fn show(value: &Shell) -> String {
        value.argv().join(" ")
    }
}

/// Variables' names, separated by blanks.
impl Reads for Vec<String> {
    type Value = Vec<String>;
    fn takes() -> String {
        "variable names, separated by blanks".to_owned()
    }
    fn read(text: &str) -> Result<Vec<String>, String> {
        (text.split_whitespace())
            .map(|name| crate::command::var_name(name).map(|()| name.to_owned()))
            .collect::<Result<_, String>>()
    }
    fn show(value: &Vec<String>) -> String {
        value.join(" ")
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
    /// Words separated by blanks, the program first: no quoting, so no
    /// word holds a blank, and a quote or backslash, which would read as
    /// quoting it does not do, is refused.
    pub fn parse(value: &str) -> Result<Shell, String> {
        if let Some(quote) = value.chars().find(|c| matches!(c, '\'' | '"' | '\\')) {
            return Err(format!(
                "{quote} is refused: words are parted by blanks, not quoted or escaped; \
                 a program or argument with a blank is run through a wrapper script, or a \
                 symlink, whose path has none"
            ));
        }
        let mut words = value.split_whitespace().map(str::to_owned);
        let program = words.next().ok_or("a program is needed")?;
        Ok(Shell {
            program,
            args: words.collect(),
        })
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

    /// The shell's default is told by its rule, which the server follows
    /// where it runs, not by what the process asking for help would start.
    #[test]
    fn the_shells_default_is_told_by_its_rule() {
        let shell = table().into_iter().find(|[name, ..]| name == "shell");
        assert!(
            shell.is_some_and(|[_, _, default, _]| default.starts_with("the server's `$SHELL`"))
        );
    }

    #[test]
    fn settings_take_their_values_as_text() {
        let mut config = Config::default();
        assert_eq!(config.set("history-bytes", "5000"), Ok(()));
        assert_eq!(config.history_bytes, 5000);
        assert_eq!(config.set("bell", "false"), Ok(()));
        assert!(!config.bell);
        assert_eq!(config.set("shell", "/bin/zsh -l"), Ok(()));
        assert_eq!(config.shell.argv(), ["/bin/zsh", "-l"]);
        for quoted in ["'/bin/my shell' -l", "zsh -c \"ls\"", "my\\ shell"] {
            let refused = config.set("shell", quoted);
            assert!(
                refused.is_err_and(|e| e.contains("wrapper script")),
                "{quoted}"
            );
        }
        assert_eq!(config.shell.argv(), ["/bin/zsh", "-l"]);
        assert_eq!(config.set("shell", "/bin/zsh # -l"), Ok(()));
        assert_eq!(config.shell.argv(), ["/bin/zsh", "#", "-l"], "# is a word");
        assert!(config.set("history-bytes", "lots").is_err());
        let below = config.set("history-bytes", "-1");
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
