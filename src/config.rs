//! Legacy server policy and checked value readers. Local `set` uses
//! `client_state`; remaining server consumers migrate with their requests.
use crate::script::Fux;
use crate::wire::{Error, Reader, Wire};
use fuxth::Value;
use std::path::PathBuf;
use std::time::Duration;

/// Temporary server policy and its typed transport. Local settings have
/// their own table, without native preference defaults or wire values.
macro_rules! settings {
    ($($(#[doc = $about:literal])* $field:ident $variant:ident: $reads:ty
        = $default:expr;)*) => {
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct Config {
            $($(#[doc = $about])* pub $field: $reads,)*
        }

        impl Default for Config {
            fn default() -> Config {
                Config {
                    $($field: $default,)*
                }
            }
        }

        /// Remaining typed server settings, pending consumer migration.
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub enum Setting {
            $($variant($reads),)*
        }
        $crate::wire::wire_enum!(Setting { $($variant(value)),* });

        impl Config {
            /// `setting` in force.
            pub fn apply(&mut self, setting: Setting) {
                match setting {
                    $(Setting::$variant(value) => self.$field = value,)*
                }
            }
        }
    };
}

/// A local setting's type, read once as `set` is given it.
pub trait Reads: Sized {
    fn takes() -> String;
    fn read(value: &Value<Fux>) -> Result<Self, String>;
}

settings! {
    /// The program a pane given none runs (not one `run` or `split` gives
    /// its own), then its arguments: a list of strings, each one word whatever
    /// it holds, as `list str /bin/zsh str -l end`.
    shell Shell: Shell = default_shell();
    /// The bytes of history each pane keeps: how far copy mode scrolls and
    /// `history capture` reads. Past it, the oldest rows go; a change
    /// applies to every pane.
    history_bytes HistoryBytes: Number<0, { 1 << 30 }> = Number(8 << 20);
    /// How long a wait waits for a change before it answers `timeout`,
    /// unless it says (`SECS timeout`).
    wait Wait: Limit = Limit::DEFAULT;
    /// Copies, fux's and programs', also reach your terminal's clipboard
    /// (OSC 52); set false, a program's copy copies nothing, to the buffers
    /// either.
    clipboard Clipboard: bool = true;
    /// How many paste buffers are kept.
    buffers Buffers: Number<1, MAX_BUFFERS> = Number(16);
    /// A pane's bell rings your terminal and marks its tab.
    bell Bell: bool = true;
    /// Your terminal's title follows the focused pane.
    titles Titles: bool = false;
    /// Variables a new pane takes from whoever starts it (a request's
    /// caller, the client whose key it is) rather than from the server,
    /// which may have started long ago. One the starter lacks is unset,
    /// but `FUX`, `FUX_PANE`, `FUX_SOCKET` and `FUX_SERVER` are reserved:
    /// requesting them here or in `env` is refused.
    update_environment UpdateEnvironment: Vec<EnvName> =
        Vec::new();
}

/// The most paste buffers kept.
pub const MAX_BUFFERS: usize = 1000;
/// The longest limit a wait may be given: a day.
pub const MAX_WAIT: usize = 86_400;

/// How long a wait waits before it times out: up to `MAX_WAIT` seconds, so its
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
        Limit::secs(secs).ok_or_else(|| format!("{secs} s: a wait's limit is at most {MAX_WAIT} s"))
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
    let alone = |program| Shell::new(program, Vec::new()).ok();
    (std::env::var("SHELL").ok().and_then(alone))
        .or_else(|| login_shell().and_then(alone))
        .unwrap_or_else(|| Shell {
            program: "/bin/sh".to_owned(),
            args: Vec::new(),
        })
}

/// Why `value` is not what a setting takes.
fn wrong(takes: &str, value: &Value<Fux>) -> String {
    format!("takes {takes}, not {}", value.kind())
}

/// An integer.
fn int(value: &Value<Fux>) -> Result<i64, String> {
    let Value::Int(n) = value else {
        return Err(wrong("a number", value));
    };
    Ok(*n)
}

/// A list of strings, each as it is.
fn strings(value: &Value<Fux>) -> Result<Vec<String>, String> {
    let takes = "a list of strings";
    let Value::List(list) = value else {
        return Err(wrong(takes, value));
    };
    (list.items().iter())
        .map(|item| {
            let Value::Str(s) = item else {
                return Err(wrong(takes, item));
            };
            Ok(s.to_string())
        })
        .collect()
}

/// A whole number from `MIN` to `MAX`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Number<const MIN: usize, const MAX: usize>(usize);

impl<const MIN: usize, const MAX: usize> Number<MIN, MAX> {
    /// `n`, if from `MIN` to `MAX`.
    pub fn new(n: usize) -> Option<Self> {
        (MIN..=MAX).contains(&n).then_some(Number(n))
    }

    pub fn get(self) -> usize {
        self.0
    }
}

impl<const MIN: usize, const MAX: usize> Reads for Number<MIN, MAX> {
    fn takes() -> String {
        format!("a number, {MIN} to {MAX}")
    }
    fn read(value: &Value<Fux>) -> Result<Self, String> {
        let n = int(value)?;
        (usize::try_from(n).ok().and_then(Number::new))
            .ok_or_else(|| format!("{n} is out of range, {MIN} to {MAX}"))
    }
}

impl<const MIN: usize, const MAX: usize> Wire for Number<MIN, MAX> {
    fn put(&self, out: &mut Vec<u8>) {
        self.0.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Number::new(usize::get(r)?).ok_or(Error::Bad("setting"))
    }
}

/// A wait's limit, in seconds.
impl Reads for Limit {
    fn takes() -> String {
        format!("seconds, 0 to {MAX_WAIT}")
    }
    fn read(value: &Value<Fux>) -> Result<Limit, String> {
        let n = int(value)?;
        let secs = u64::try_from(n).map_err(|_| format!("{n} s: a wait waits 0 s or more"))?;
        Limit::read(secs)
    }
}

/// `true` or `false`.
impl Reads for bool {
    fn takes() -> String {
        "true or false".to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<bool, String> {
        let Value::Bool(b) = value else {
            return Err(wrong("true or false", value));
        };
        Ok(*b)
    }
}

/// The shell fux starts in a pane: a program, never empty, then its
/// arguments, each a word as it is, blanks, quotes and all, but for NUL,
/// which no word a program is given holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell {
    program: String,
    args: Vec<String>,
}

impl Shell {
    /// `program` and `args`, or why they are no shell.
    pub fn new(program: String, args: Vec<String>) -> Result<Shell, String> {
        if program.is_empty() {
            return Err("a program is needed".to_owned());
        }
        if let Some(word) = std::iter::once(&program)
            .chain(&args)
            .find(|w| w.contains('\0'))
        {
            return Err(format!("{word:?}: a program's word holds no NUL"));
        }
        Ok(Shell { program, args })
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

/// The program, then its arguments: a list of strings, not empty.
impl Reads for Shell {
    fn takes() -> String {
        "a list of strings: a program, then its arguments".to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<Shell, String> {
        let mut words = strings(value)?.into_iter();
        let program = words.next().ok_or("a program is needed")?;
        Shell::new(program, words.collect())
    }
}

/// Read back only if it is a shell.
impl Wire for Shell {
    fn put(&self, out: &mut Vec<u8>) {
        self.program.put(out);
        self.args.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let (program, args) = (String::get(r)?, Vec::get(r)?);
        Shell::new(program, args).map_err(|_| Error::Bad("shell"))
    }
}

/// A shell variable a caller may pass to a pane: letters, digits and `_`,
/// not first a digit, excluding the pane's reserved identity names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvName(String);

impl EnvName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for EnvName {
    type Err = String;
    fn from_str(name: &str) -> Result<EnvName, String> {
        crate::command::pane_var(name).map(|()| EnvName(name.to_owned()))
    }
}

impl Wire for EnvName {
    fn put(&self, out: &mut Vec<u8>) {
        self.0.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        String::get(r)?
            .parse()
            .map_err(|_| Error::Bad("variable name"))
    }
}

/// Variables' names: a list of strings.
impl Reads for Vec<EnvName> {
    fn takes() -> String {
        "a list of variables' names".to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<Vec<EnvName>, String> {
        strings(value)?.iter().map(|name| name.parse()).collect()
    }
}

/// The config file, a fuxth program a typing terminal runs as it attaches:
/// `$XDG_CONFIG_HOME/fux/config.fux`, else `~/.config/fux/config.fux`.
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
    fn legacy_settings_keep_their_checked_wire_values() {
        let d = Config::default();
        for setting in [
            Setting::Shell(d.shell),
            Setting::HistoryBytes(d.history_bytes),
            Setting::Wait(d.wait),
            Setting::Clipboard(d.clipboard),
            Setting::Buffers(d.buffers),
            Setting::Bell(d.bell),
            Setting::Titles(d.titles),
            Setting::UpdateEnvironment(d.update_environment),
        ] {
            let bytes = crate::wire::encode(&setting);
            assert_eq!(Reader::new(&bytes).whole::<Setting>(), Ok(setting));
        }
        let mut zero = crate::wire::encode(&Setting::Buffers(Number(1)));
        zero.pop();
        zero.push(0);
        assert!(Reader::new(&zero).whole::<Setting>().is_err());
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
