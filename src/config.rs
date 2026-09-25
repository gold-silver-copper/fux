//! The running configuration: options and key bindings, changed by `set`,
//! `bind`, `unbind` and `unbind-all`, whether they come from the config file,
//! the CLI or the command prompt.
use crate::keys::KeyPress;
use crate::words;
use std::path::{Path, PathBuf};

/// Keys typed after the prefix, bound to a command line. A binding of more
/// than one key makes each key before its last a layer: `t n` is `n` in the
/// layer `t`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub keys: Vec<KeyPress>,
    pub command: Vec<String>,
    /// The command-column group; derived from the command when not given.
    /// A layer's title is the group of its first binding.
    pub group: Option<String>,
    /// After it runs, its layer stays active: its keys repeat without the
    /// prefix until Esc.
    pub repeat: bool,
}

/// A key after the prefix: one letter, either case, stored in lower case,
/// as a key typed after the prefix is matched in lower case.
fn letter(command: &str, word: &str) -> Result<KeyPress, String> {
    match word.chars().collect::<Vec<_>>().as_slice() {
        [c] if c.is_ascii_alphabetic() => Ok(KeyPress::char(c.to_ascii_lowercase())),
        [_] | [] | [_, _, ..] => Err(format!(
            "{command}: {word:?} is not a letter: keys after the prefix are a–z, without modifiers"
        )),
    }
}

/// A key typed after the prefix, as bindings are matched: a letter in lower
/// case. Anything else, and a letter with Ctrl or Alt, is left as it is and
/// matches no binding.
pub fn folded(press: KeyPress) -> KeyPress {
    if let crate::keys::Key::Char(c) = press.key
        && c.is_ascii_alphabetic()
        && !press.mods.ctrl
        && !press.mods.alt
    {
        return KeyPress::char(c.to_ascii_lowercase());
    }
    press
}

/// Keys as they are written: `t n`.
pub fn keys_text(keys: &[KeyPress]) -> String {
    keys.iter()
        .map(KeyPress::to_string)
        .collect::<Vec<_>>()
        .join(" ")
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

/// The default bindings, as `bind` takes them, in command-column order.
/// After the prefix every key is a plain letter: `r` and `m` are repeat
/// modes, `t` and `w` layers sharing their verbs.
const DEFAULT_BINDINGS: &[&str] = &[
    "h select-pane -L",
    "j select-pane -D",
    "k select-pane -U",
    "l select-pane -R",
    "o select-pane --next",
    "q select-pane --last",
    "v split -h",
    "s split -v",
    "x confirm-close pane",
    "z zoom",
    "a menu pane",
    "c copy-mode",
    "p paste-buffer",
    "-g Resize -r r h resize-pane -L",
    "-g Resize -r r j resize-pane -D",
    "-g Resize -r r k resize-pane -U",
    "-g Resize -r r l resize-pane -R",
    "-g Move -r m h move-pane -L",
    "-g Move -r m j move-pane -D",
    "-g Move -r m k move-pane -U",
    "-g Move -r m l move-pane -R",
    "n select-tab --next",
    "b select-tab --previous",
    "t n new-tab",
    "t h select-tab --previous",
    "t l select-tab --next",
    "t g choose-tab",
    "t r rename-prompt tab",
    "t x confirm-close tab",
    "t a menu tab",
    "-g Reorder -r t m h reorder tab --previous",
    "-g Reorder -r t m l reorder tab --next",
    "w n new-workspace",
    "w h select-workspace --previous",
    "w l select-workspace --next",
    "w g choose-workspace",
    "w r rename-prompt workspace",
    "w x confirm-close workspace",
    "w a menu workspace",
    "-g Reorder -r w m h reorder workspace --previous",
    "-g Reorder -r w m l reorder workspace --next",
    "e command-prompt",
    "d detach",
];

impl Default for Config {
    fn default() -> Self {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/sh".into());
        let mut config = Self {
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
            bindings: Vec::new(),
        };
        // Each is checked by `default_bindings_all_parse_and_are_grouped`.
        for line in DEFAULT_BINDINGS {
            let bind =
                std::iter::once("bind".to_owned()).chain(words::split(line).unwrap_or_default());
            let _ = config.apply(&bind.collect::<Vec<_>>());
        }
        config
    }
}

/// The groups of the command column, in order; custom groups follow them,
/// and `Other` is last.
pub const GROUPS: &[&str] = &["Panes", "Focus", "Tabs", "Workspaces", "Session"];

impl Binding {
    /// The group this binding is listed under.
    pub fn group(&self) -> String {
        match &self.group {
            Some(group) => group.clone(),
            None => self.derived_group(),
        }
    }

    /// The group its command belongs to, whatever `-g` said.
    pub fn derived_group(&self) -> String {
        let name = self.command.first().map(String::as_str).unwrap_or("");
        let second = self.command.get(1).map(String::as_str);
        match (name, second) {
            ("select-pane", _) => "Focus",
            ("menu", Some("tab"))
            | ("rename-prompt", Some("tab"))
            | ("confirm-close", Some("tab"))
            | ("reorder", Some("tab")) => "Tabs",
            ("menu", Some("workspace"))
            | ("rename-prompt", Some("workspace"))
            | ("confirm-close", Some("workspace"))
            | ("reorder", Some("workspace")) => "Workspaces",
            (
                "split" | "kill-pane" | "zoom" | "resize-pane" | "swap-pane" | "move-pane"
                | "copy-mode" | "paste-buffer" | "menu" | "rename-prompt" | "confirm-close"
                | "terminate" | "choose-pane" | "send-keys" | "reorder",
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
                let (mut group, mut repeat, mut rest) = (None, false, rest);
                loop {
                    match rest.split_first() {
                        Some((flag, after)) if flag == "-g" => {
                            let (name, after) =
                                after.split_first().ok_or("bind -g needs a group name")?;
                            group = Some(name.clone());
                            rest = after;
                        }
                        Some((flag, after)) if flag == "-r" => {
                            repeat = true;
                            rest = after;
                        }
                        _ => break,
                    }
                }
                // The keys: the first word, and the one-character words after
                // it; no command's name is one character.
                let (first, after) = rest
                    .split_first()
                    .ok_or("usage: bind [-g GROUP] [-r] KEY… COMMAND…")?;
                let more = after.iter().take_while(|w| w.chars().count() == 1).count();
                let (more, command) = after.split_at_checked(more).unwrap_or((after, &[]));
                let keys = std::iter::once(first)
                    .chain(more)
                    .map(|key| letter("bind", key))
                    .collect::<Result<Vec<KeyPress>, String>>()?;
                if command.is_empty() {
                    return Err(format!("bind {}: no command given", keys_text(&keys)));
                }
                self.bind(Binding {
                    keys,
                    command: command.to_vec(),
                    group,
                    repeat,
                })
            }
            "unbind" => {
                if rest.is_empty() {
                    return Err("usage: unbind KEY…".into());
                }
                let keys = rest
                    .iter()
                    .map(|key| letter("unbind", key))
                    .collect::<Result<Vec<KeyPress>, String>>()?;
                // A binding, or a whole layer.
                let before = self.bindings.len();
                self.bindings.retain(|b| !b.keys.starts_with(&keys));
                if self.bindings.len() == before {
                    return Err(format!("{} is not bound", keys_text(&keys)));
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

    /// Adds a binding, replacing one of the same keys. Keys are a command
    /// or a layer, never both, so a binding that would make them both is
    /// refused.
    fn bind(&mut self, binding: Binding) -> Result<(), String> {
        let keys = &binding.keys;
        let layer: Vec<String> = self
            .bindings
            .iter()
            .filter(|b| b.keys.len() > keys.len() && b.keys.starts_with(keys))
            .map(|b| keys_text(&b.keys))
            .collect();
        if !layer.is_empty() {
            return Err(format!(
                "{} is a layer ({}); unbind it first",
                keys_text(keys),
                layer.join(", ")
            ));
        }
        if let Some(command) = self
            .bindings
            .iter()
            .find(|b| b.keys.len() < keys.len() && keys.starts_with(&b.keys))
        {
            return Err(format!(
                "{} runs {}, so it cannot start {}; unbind it first",
                keys_text(&command.keys),
                words::join(&command.command),
                keys_text(keys)
            ));
        }
        match self.bindings.iter_mut().find(|b| b.keys == *keys) {
            Some(existing) => *existing = binding,
            None => self.bindings.push(binding),
        }
        Ok(())
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
            format!("set prefix {}", words::quote(&self.prefix.to_string())),
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
                "bind{}{} {} {}",
                binding
                    .group
                    .as_ref()
                    .map(|g| format!(" -g {}", words::quote(g)))
                    .unwrap_or_default(),
                if binding.repeat { " -r" } else { "" },
                binding
                    .keys
                    .iter()
                    .map(|key| words::quote(&key.to_string()))
                    .collect::<Vec<_>>()
                    .join(" "),
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
        let y = c.bindings.iter().find(|b| keys_text(&b.keys) == "y");
        assert_eq!(y.map(|b| b.group()), Some("Tools".into()));
        assert_eq!(y.map(|b| b.command.len()), Some(4));
        assert!(apply(&mut c, "bind h kill-pane").is_ok());
        assert_eq!(
            c.bindings
                .iter()
                .filter(|b| keys_text(&b.keys) == "h")
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
    fn keys_are_a_command_or_a_layer_never_both() {
        let mut c = Config::default();
        assert!(apply(&mut c, "bind g n new-tab").is_ok());
        assert!(apply(&mut c, "bind -g Grow -r g m h reorder tab --previous").is_ok());
        assert_eq!(
            apply(&mut c, "bind g new-tab"),
            Err("g is a layer (g n, g m h); unbind it first".into())
        );
        assert_eq!(
            apply(&mut c, "bind h u zoom"),
            Err("h runs select-pane -L, so it cannot start h u; unbind it first".into())
        );
        assert_eq!(
            apply(&mut c, "bind g n m zoom"),
            Err("g n runs new-tab, so it cannot start g n m; unbind it first".into())
        );
        // Rebinding the same keys replaces the binding.
        assert!(apply(&mut c, "bind g n zoom").is_ok());
        let g: Vec<_> = c
            .bindings
            .iter()
            .filter(|b| keys_text(&b.keys).starts_with('g'))
            .collect();
        assert_eq!(g.len(), 2);
        assert!(g.iter().any(|b| b.command == ["zoom"] && !b.repeat));
        assert!(g.iter().any(|b| b.repeat && b.group() == "Grow"));
        // The repeat flag and the keys survive `describe`.
        let mut fresh = Config::default();
        for line in c.describe() {
            assert!(apply(&mut fresh, &line).is_ok(), "{line}");
        }
        assert_eq!(fresh, c);
        // `unbind` takes one binding, or a whole layer.
        assert!(apply(&mut c, "unbind g m h").is_ok());
        assert!(apply(&mut c, "unbind g m").is_err());
        assert!(apply(&mut c, "bind g m l zoom").is_ok());
        assert!(apply(&mut c, "unbind g").is_ok());
        assert!(
            !c.bindings
                .iter()
                .any(|b| keys_text(&b.keys).starts_with('g'))
        );
        assert_eq!(apply(&mut c, "unbind"), Err("usage: unbind KEY…".into()));
    }

    #[test]
    fn keys_after_the_prefix_are_letters_stored_in_lower_case() {
        let mut c = Config::default();
        for (line, word) in [
            ("bind C-Left resize-pane -L", "C-Left"),
            ("bind : command-prompt", ":"),
            ("bind g ; zoom", ";"),
            ("bind 1 zoom", "1"),
        ] {
            assert_eq!(
                apply(&mut c, line),
                Err(format!(
                    "bind: {word:?} is not a letter: keys after the prefix are a–z, without modifiers"
                )),
                "{line}"
            );
        }
        assert!(apply(&mut c, "unbind S-Left").is_err());
        // Upper case is the same key as lower case.
        assert!(apply(&mut c, "bind T N zoom").is_ok());
        let t_n: Vec<_> = c
            .bindings
            .iter()
            .filter(|b| keys_text(&b.keys) == "t n")
            .collect();
        assert_eq!(t_n.len(), 1);
        assert!(t_n.iter().all(|b| b.command == ["zoom"]));
        assert!(apply(&mut c, "unbind T N").is_ok());
        assert!(!c.bindings.iter().any(|b| keys_text(&b.keys) == "t n"));
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
        assert!(config.bindings.iter().any(|b| keys_text(&b.keys) == "q"));
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

    /// `describe()`'s lines, read back, give the configuration they describe.
    fn read_back(c: &Config) -> Result<Config, String> {
        let mut fresh = Config::default();
        apply(&mut fresh, "unbind-all")?;
        for line in c.describe() {
            apply(&mut fresh, &line).map_err(|e| format!("{line:?}: {e}"))?;
        }
        Ok(fresh)
    }

    #[test]
    fn a_prefix_that_needs_quoting_is_described_quoted() {
        for prefix in ["'#'", "\\'", "'\\'", "'\"'", "'\u{3000}'"] {
            let mut c = Config::default();
            assert!(
                apply(&mut c, &format!("set prefix {prefix}")).is_ok(),
                "{prefix}"
            );
            let back = read_back(&c);
            assert_eq!(back.as_ref().map(|b| b.prefix), Ok(c.prefix), "{prefix}");
            assert!(back == Ok(c), "{prefix}");
        }
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
