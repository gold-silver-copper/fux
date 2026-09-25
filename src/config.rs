//! The running configuration: options and key bindings, changed by `set`,
//! `bind`, `unbind` and `unbind-all`, whether they come from the config file,
//! the CLI or the `:` prompt.
use crate::keys::KeyPress;
use crate::words;
use std::path::{Path, PathBuf};

/// A key bound, after the prefix, to a command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub key: KeyPress,
    pub command: Vec<String>,
    /// The command-column group; derived from the command when not given.
    pub group: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub prefix: KeyPress,
    pub shell: Vec<String>,
    pub history_lines: usize,
    /// Copies are also sent to the client's terminal as OSC 52.
    pub clipboard: bool,
    /// How many paste buffers are kept.
    pub buffers: usize,
    pub bindings: Vec<Binding>,
}

/// The largest history a pane keeps.
pub const MAX_HISTORY: usize = 1_000_000;
pub const MAX_BUFFERS: usize = 1000;

/// The default bindings, in command-column order.
const DEFAULT_BINDINGS: &[(&str, &str)] = &[
    ("h", "split -h"),
    ("v", "split -v"),
    ("x", "confirm-close pane"),
    ("z", "zoom"),
    ("r", "rename-prompt pane"),
    ("p", "menu pane"),
    ("c", "copy-mode"),
    ("P", "paste-buffer"),
    ("C-Left", "resize-pane -L"),
    ("C-Right", "resize-pane -R"),
    ("C-Up", "resize-pane -U"),
    ("C-Down", "resize-pane -D"),
    ("S-Left", "move-pane -L"),
    ("S-Right", "move-pane -R"),
    ("S-Up", "move-pane -U"),
    ("S-Down", "move-pane -D"),
    ("Tab", "select-pane --next"),
    ("BTab", "select-pane --previous"),
    ("BSpace", "select-pane --last"),
    ("M-Left", "select-pane -L"),
    ("M-Right", "select-pane -R"),
    ("M-Up", "select-pane -U"),
    ("M-Down", "select-pane -D"),
    ("t", "new-tab"),
    ("]", "select-tab --next"),
    ("[", "select-tab --previous"),
    ("T", "choose-tab"),
    ("s", "menu tab"),
    ("w", "new-workspace"),
    ("}", "select-workspace --next"),
    ("{", "select-workspace --previous"),
    ("W", "choose-workspace"),
    ("S", "menu workspace"),
    (":", "command-prompt"),
    ("d", "detach"),
];

impl Default for Config {
    fn default() -> Self {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/sh".into());
        let bindings = DEFAULT_BINDINGS
            .iter()
            .filter_map(|(key, command)| {
                Some(Binding {
                    key: key.parse().ok()?,
                    command: words::split(command).ok()?,
                    group: None,
                })
            })
            .collect();
        Self {
            prefix: KeyPress::new(
                crate::keys::Key::Char('b'),
                crate::keys::Modifiers {
                    ctrl: true,
                    ..crate::keys::Modifiers::NONE
                },
            ),
            shell: vec![shell],
            history_lines: 10_000,
            clipboard: true,
            buffers: 16,
            bindings,
        }
    }
}

/// The groups of the command column, in order; custom groups follow them,
/// and `Other` is last.
pub const GROUPS: &[&str] = &["Panes", "Focus", "Tabs", "Workspaces", "Session"];

impl Binding {
    /// The group this binding is listed under.
    pub fn group(&self) -> String {
        if let Some(group) = &self.group {
            return group.clone();
        }
        let name = self.command.first().map(String::as_str).unwrap_or("");
        let second = self.command.get(1).map(String::as_str);
        match (name, second) {
            ("select-pane", _) => "Focus",
            ("menu", Some("tab"))
            | ("rename-prompt", Some("tab"))
            | ("confirm-close", Some("tab")) => "Tabs",
            ("menu", Some("workspace"))
            | ("rename-prompt", Some("workspace"))
            | ("confirm-close", Some("workspace")) => "Workspaces",
            (
                "split" | "kill-pane" | "zoom" | "resize-pane" | "swap-pane" | "move-pane"
                | "copy-mode" | "paste-buffer" | "menu" | "rename-prompt" | "confirm-close"
                | "terminate" | "choose-pane" | "send-keys",
                _,
            ) => "Panes",
            ("new-tab" | "select-tab" | "choose-tab" | "kill-tab", _) => "Tabs",
            ("new-workspace" | "select-workspace" | "choose-workspace" | "kill-workspace", _) => {
                "Workspaces"
            }
            ("detach" | "command-prompt" | "command-column" | "reload" | "kill-server", _) => {
                "Session"
            }
            _ => "Other",
        }
        .to_owned()
    }
}

impl Config {
    /// Applies `set`, `bind`, `unbind` or `unbind-all` given as words.
    pub fn apply(&mut self, argv: &[String]) -> Result<(), String> {
        let (name, rest) = argv.split_first().ok_or("an empty command")?;
        match name.as_str() {
            "set" => {
                let (option, value) = rest.split_first().ok_or("usage: set OPTION VALUE")?;
                self.set(option, value)
            }
            "bind" => {
                let (group, rest) = match rest.split_first() {
                    Some((flag, rest)) if flag == "-g" => {
                        let (group, rest) =
                            rest.split_first().ok_or("bind -g needs a group name")?;
                        (Some(group.clone()), rest)
                    }
                    _ => (None, rest),
                };
                let (key, command) = rest
                    .split_first()
                    .ok_or("usage: bind [-g GROUP] KEY COMMAND…")?;
                if command.is_empty() {
                    return Err(format!("bind {key}: no command given"));
                }
                let key: KeyPress = key.parse()?;
                let binding = Binding {
                    key,
                    command: command.to_vec(),
                    group,
                };
                match self.bindings.iter_mut().find(|b| b.key == key) {
                    Some(existing) => *existing = binding,
                    None => self.bindings.push(binding),
                }
                Ok(())
            }
            "unbind" => {
                let [key] = rest else {
                    return Err("usage: unbind KEY".into());
                };
                let key: KeyPress = key.parse()?;
                let before = self.bindings.len();
                self.bindings.retain(|b| b.key != key);
                if self.bindings.len() == before {
                    return Err(format!("{key} is not bound"));
                }
                Ok(())
            }
            "unbind-all" => {
                if !rest.is_empty() {
                    return Err("usage: unbind-all".into());
                }
                self.bindings.clear();
                Ok(())
            }
            other => Err(format!(
                "{other} changes panes or layout, which a config file cannot do; \
                 only set, bind, unbind and unbind-all are allowed there"
            )),
        }
    }

    fn set(&mut self, option: &str, value: &[String]) -> Result<(), String> {
        let one = || match value {
            [single] => Ok(single.as_str()),
            _ => Err(format!("set {option} takes one value")),
        };
        let number = |max: usize| -> Result<usize, String> {
            let n: usize = one()?
                .parse()
                .map_err(|_| format!("set {option}: not a number"))?;
            if n > max {
                return Err(format!("set {option}: at most {max}"));
            }
            Ok(n)
        };
        match option {
            "prefix" => self.prefix = one()?.parse()?,
            "shell" => {
                // `set shell /bin/zsh -l` and `set shell '/bin/zsh -l'` alike.
                let argv = if let [single] = value {
                    words::split(single)?
                } else {
                    value.to_vec()
                };
                if argv.first().is_none_or(|p| p.is_empty()) {
                    return Err("set shell needs a program".into());
                }
                self.shell = argv;
            }
            "history-lines" => self.history_lines = number(MAX_HISTORY)?,
            "clipboard" => {
                self.clipboard = match one()? {
                    "on" | "write-only" => true,
                    "off" => false,
                    other => return Err(format!("set clipboard: {other:?} is not on or off")),
                }
            }
            "buffers" => {
                let n = number(MAX_BUFFERS)?;
                if n == 0 {
                    return Err("set buffers: at least 1".into());
                }
                self.buffers = n;
            }
            other => {
                return Err(format!(
                    "unknown option {other}; options are prefix, shell, history-lines, clipboard, buffers"
                ));
            }
        }
        Ok(())
    }

    /// The configuration a file gives, applied line by line over the
    /// defaults. Any error names its file and line, and nothing applies.
    pub fn from_file(path: &Path) -> Result<Config, String> {
        let mut config = Config::default();
        let text = match read_bounded(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(config),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        for (number, line) in text.lines().enumerate() {
            let line_number = number.saturating_add(1);
            let at = |e: String| format!("{}:{}: {e}", path.display(), line_number);
            let argv = words::split(line).map_err(at)?;
            if argv.is_empty() {
                continue;
            }
            config.apply(&argv).map_err(at)?;
        }
        Ok(config)
    }

    /// Settings as `fux` commands, for display.
    pub fn describe(&self) -> Vec<String> {
        let mut lines = vec![
            format!("set prefix {}", self.prefix),
            format!("set shell {}", words::join(&self.shell)),
            format!("set history-lines {}", self.history_lines),
            format!(
                "set clipboard {}",
                if self.clipboard { "on" } else { "off" }
            ),
            format!("set buffers {}", self.buffers),
        ];
        for binding in &self.bindings {
            lines.push(format!(
                "bind{} {} {}",
                binding
                    .group
                    .as_ref()
                    .map(|g| format!(" -g {}", words::quote(g)))
                    .unwrap_or_default(),
                words::quote(&binding.key.to_string()),
                words::join(&binding.command)
            ));
        }
        lines
    }
}

/// The config file: `--config`, else `$XDG_CONFIG_HOME/fux/fux.conf`, else
/// `~/.config/fux/fux.conf`.
pub fn default_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir).join("fux").join("fux.conf"));
    }
    std::env::var_os("HOME")
        .filter(|d| !d.is_empty())
        .map(|home| {
            PathBuf::from(home)
                .join(".config")
                .join("fux")
                .join("fux.conf")
        })
}

/// A config file is read whole but bounded, so a mistaken path cannot make
/// the server read without end.
fn read_bounded(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    const LIMIT: u64 = 1 << 20;
    let mut text = String::new();
    std::fs::File::open(path)?
        .take(LIMIT + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > LIMIT {
        return Err(std::io::Error::other("larger than 1 MiB"));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(config: &mut Config, line: &str) -> Result<(), String> {
        config.apply(&words::split(line)?)
    }

    #[test]
    fn set_bind_and_unbind_change_the_configuration() {
        let mut c = Config::default();
        assert!(apply(&mut c, "set prefix C-a").is_ok());
        assert_eq!(c.prefix.to_string(), "C-a");
        assert!(apply(&mut c, "set shell /bin/zsh -l").is_ok());
        assert_eq!(c.shell, ["/bin/zsh", "-l"]);
        assert!(apply(&mut c, "set shell '/bin/bash --norc'").is_ok());
        assert_eq!(c.shell, ["/bin/bash", "--norc"]);
        assert!(apply(&mut c, "set clipboard off").is_ok());
        assert!(!c.clipboard);
        assert!(apply(&mut c, "set history-lines 99").is_ok());
        assert_eq!(c.history_lines, 99);
        assert!(apply(&mut c, "set buffers 0").is_err());
        assert!(apply(&mut c, "set nope 1").is_err());
        assert!(apply(&mut c, "set history-lines lots").is_err());
        assert!(apply(&mut c, "bind -g Tools y split -h -- htop").is_ok());
        let y = c.bindings.iter().find(|b| b.key.to_string() == "y");
        assert_eq!(y.map(|b| b.group()), Some("Tools".into()));
        assert_eq!(y.map(|b| b.command.len()), Some(4));
        assert!(apply(&mut c, "bind h kill-pane").is_ok());
        assert_eq!(
            c.bindings
                .iter()
                .filter(|b| b.key.to_string() == "h")
                .count(),
            1
        );
        assert!(apply(&mut c, "unbind h").is_ok());
        assert!(apply(&mut c, "unbind h").is_err());
        assert!(apply(&mut c, "unbind-all").is_ok());
        assert!(c.bindings.is_empty());
        assert!(
            apply(&mut c, "split -h").is_err(),
            "layout commands are refused"
        );
        assert!(apply(&mut c, "bind x").is_err());
    }

    #[test]
    fn a_file_applies_whole_or_names_its_bad_line() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("fux-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("fux.conf");
        std::fs::write(
            &path,
            "# comment\nset prefix C-a\n\nbind q detach # trailing\n",
        )
        .map_err(|e| e.to_string())?;
        let config = Config::from_file(&path)?;
        assert_eq!(config.prefix.to_string(), "C-a");
        assert!(config.bindings.iter().any(|b| b.key.to_string() == "q"));
        std::fs::write(&path, "set prefix C-a\nset prefix Nope\n").map_err(|e| e.to_string())?;
        let error = Config::from_file(&path).err().unwrap_or_default();
        assert!(
            error.ends_with(":2: unknown key \"Nope\"; `fux list-keys` lists them"),
            "{error}"
        );
        assert_eq!(
            Config::from_file(&dir.join("missing")),
            Ok(Config::default())
        );
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn default_bindings_all_parse_and_are_grouped() {
        let c = Config::default();
        assert_eq!(c.bindings.len(), DEFAULT_BINDINGS.len());
        for b in &c.bindings {
            assert_ne!(b.group(), "Other", "{:?}", b.command);
        }
        for line in c.describe() {
            let mut fresh = Config::default();
            assert!(apply(&mut fresh, &line).is_ok(), "{line}");
        }
    }
}
