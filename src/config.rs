//! The running configuration: options and key bindings, changed by `set`,
//! `bind`, `unbind` and `unbind-all`, whether they come from the config file,
//! the CLI or the command prompt.
use crate::command::{self, Command, Usage};
use crate::keys::{Key, KeyPress};
use crate::words;
use std::path::{Path, PathBuf};

/// Keys typed after the prefix, bound to a command line. A binding of more
/// than one key makes each key before its last a layer: `t n` is `n` in the
/// layer `t`. A binding without the prefix (`bind -n`) is one key, kept in
/// [`Config::root`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub keys: Vec<KeyPress>,
    /// The command line as it was given, for `list-keys`, `describe` and
    /// labels.
    pub command: Vec<String>,
    /// The command it runs, parsed when the binding was made.
    pub parsed: Command,
    /// The command-column group; derived from the command when not given.
    /// A layer's title is the group of its first binding.
    pub group: Option<String>,
    /// After it runs, its layer stays active: its keys repeat without the
    /// prefix until Esc.
    pub repeat: bool,
}

/// Why `set`, `bind`, `unbind`, `unbind-all` or a config file is refused.
#[derive(Debug)]
pub enum Error {
    /// A word that names no key, in `command`.
    NotAKey {
        command: &'static str,
        word: String,
    },
    /// Escape after the prefix, which always closes the command column.
    EscapeBound {
        keys: Vec<KeyPress>,
    },
    /// `bind -n` given more than one key, or `-r`.
    RootUsage,
    /// `bind -n` given the prefix.
    RootPrefix {
        key: KeyPress,
    },
    /// `unbind -n` of a key bound to nothing without the prefix.
    RootNotBound {
        key: KeyPress,
    },
    /// `set prefix` given a key bound without the prefix.
    PrefixRootBound {
        key: KeyPress,
    },
    Empty,
    SetUsage,
    NoGroupName,
    BindUsage,
    NoCommand {
        keys: Vec<KeyPress>,
    },
    /// A binding whose command does not parse.
    Unparsed {
        keys: Vec<KeyPress>,
        usage: Usage,
    },
    /// Keys that are a layer, of the bindings `layer`.
    Layer {
        keys: Vec<KeyPress>,
        layer: Vec<Vec<KeyPress>>,
    },
    /// Keys that would start with a binding's keys, which run `command`.
    Runs {
        keys: Vec<KeyPress>,
        command: Vec<String>,
        new: Vec<KeyPress>,
    },
    UnbindUsage,
    NotBound {
        keys: Vec<KeyPress>,
    },
    UnbindAllUsage,
    /// A command other than `set`, `bind`, `unbind` and `unbind-all`.
    NotConfig {
        command: String,
    },
    OneValue {
        option: String,
    },
    NotANumber {
        option: String,
    },
    TooLarge {
        option: String,
        max: usize,
    },
    Key(crate::keys::Error),
    Words(words::Error),
    NoProgram,
    NotOnOff {
        option: String,
        value: String,
    },
    NoBuffers,
    UnknownOption {
        option: String,
    },
    /// A config file that could not be read.
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A line of a config file, and what is wrong with it.
    Line {
        path: PathBuf,
        line: usize,
        error: Box<Error>,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotAKey { command, word } => write!(
                f,
                "{command}: {word:?} is not a key; `fux list-keys` lists their names"
            ),
            Error::EscapeBound { keys } => write!(
                f,
                "bind {}: Escape cannot be bound after the prefix; it closes the column",
                keys_text(keys)
            ),
            Error::RootUsage => {
                f.write_str("bind -n takes one key and no -r: bind -n [-g GROUP] KEY COMMAND…")
            }
            Error::RootPrefix { key } => {
                write!(
                    f,
                    "bind -n {key}: {key} is the prefix, so it cannot be bound without it"
                )
            }
            Error::RootNotBound { key } => write!(f, "{key} is not bound without the prefix"),
            Error::PrefixRootBound { key } => write!(
                f,
                "set prefix {key}: {key} is bound without the prefix; `unbind -n {key}` first"
            ),
            Error::Empty => f.write_str("an empty command"),
            Error::SetUsage => f.write_str("usage: set OPTION VALUE"),
            Error::NoGroupName => f.write_str("bind -g needs a group name"),
            Error::BindUsage => f.write_str("usage: bind [-n] [-g GROUP] [-r] KEY… COMMAND…"),
            Error::NoCommand { keys } => {
                write!(f, "bind {}: no command given", keys_text(keys))
            }
            Error::Unparsed { keys, usage } => write!(f, "bind {}: {usage}", keys_text(keys)),
            Error::Layer { keys, layer } => {
                let layer: Vec<String> = layer.iter().map(|keys| keys_text(keys)).collect();
                write!(
                    f,
                    "{} is a layer ({}); unbind it first",
                    keys_text(keys),
                    layer.join(", ")
                )
            }
            Error::Runs { keys, command, new } => write!(
                f,
                "{} runs {}, so it cannot start {}; unbind it first",
                keys_text(keys),
                words::join(command),
                keys_text(new)
            ),
            Error::UnbindUsage => f.write_str("usage: unbind [-n] KEY…"),
            Error::NotBound { keys } => write!(f, "{} is not bound", keys_text(keys)),
            Error::UnbindAllUsage => f.write_str("usage: unbind-all"),
            Error::NotConfig { command } => write!(
                f,
                "{command} changes panes or layout, which a config file cannot do; \
                 only set, bind, unbind and unbind-all are allowed there"
            ),
            Error::OneValue { option } => write!(f, "set {option} takes one value"),
            Error::NotANumber { option } => write!(f, "set {option}: not a number"),
            Error::TooLarge { option, max } => write!(f, "set {option}: at most {max}"),
            Error::Key(error) => error.fmt(f),
            Error::Words(error) => error.fmt(f),
            Error::NoProgram => f.write_str("set shell needs a program"),
            Error::NotOnOff { option, value } => {
                write!(f, "set {option}: {value:?} is not on or off")
            }
            Error::NoBuffers => f.write_str("set buffers: at least 1"),
            Error::UnknownOption { option } => write!(
                f,
                "unknown option {option}; options are prefix, shell, history-lines, clipboard, \
                 buffers, bell, titles"
            ),
            Error::Read { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Line { path, line, error } => {
                write!(f, "{}:{line}: {error}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Unparsed { usage, .. } => Some(usage),
            Error::Key(error) => Some(error),
            Error::Words(error) => Some(error),
            Error::Read { source, .. } => Some(source),
            Error::Line { error, .. } => Some(error.as_ref()),
            Error::NotAKey { .. }
            | Error::EscapeBound { .. }
            | Error::RootUsage
            | Error::RootPrefix { .. }
            | Error::RootNotBound { .. }
            | Error::PrefixRootBound { .. }
            | Error::Empty
            | Error::SetUsage
            | Error::NoGroupName
            | Error::BindUsage
            | Error::NoCommand { .. }
            | Error::Layer { .. }
            | Error::Runs { .. }
            | Error::UnbindUsage
            | Error::NotBound { .. }
            | Error::UnbindAllUsage
            | Error::NotConfig { .. }
            | Error::OneValue { .. }
            | Error::NotANumber { .. }
            | Error::TooLarge { .. }
            | Error::NoProgram
            | Error::NotOnOff { .. }
            | Error::NoBuffers
            | Error::UnknownOption { .. } => None,
        }
    }
}

impl From<crate::keys::Error> for Error {
    fn from(error: crate::keys::Error) -> Error {
        Error::Key(error)
    }
}

impl From<words::Error> for Error {
    fn from(error: words::Error) -> Error {
        Error::Words(error)
    }
}

/// A key in `bind`, `unbind` or `set prefix`, by any name `fux list-keys`
/// gives or as any character. Case counts, as a key typed is matched as it
/// came: `V` is Shift-v, and `S-v` is read as `V`. A letter with Ctrl has
/// no case (`C-V` is `C-v`), as a key press keeps what a legacy terminal
/// sends.
fn key(command: &'static str, word: &str) -> Result<KeyPress, Error> {
    let press: KeyPress = word.parse().map_err(|_| Error::NotAKey {
        command,
        word: word.to_owned(),
    })?;
    // `KeyPress` drops Shift from a character, which carries its own: so
    // `S-` on a letter is its upper case.
    let mut rest = word;
    let mut shift = false;
    while let Some((modifier, tail)) = rest.split_at_checked(2) {
        if tail.is_empty() {
            break;
        }
        match modifier {
            "S-" | "s-" => shift = true,
            "C-" | "c-" | "M-" | "m-" => {}
            _ => break,
        }
        rest = tail;
    }
    if let Key::Char(c) = press.key
        && shift
        && c.is_ascii_lowercase()
    {
        return Ok(KeyPress::new(Key::Char(c.to_ascii_uppercase()), press.mods));
    }
    Ok(press)
}

/// Whether `press` is Escape as the command column takes it: with no Ctrl
/// or Alt.
fn is_escape(press: &KeyPress) -> bool {
    press.key == Key::Escape && !press.mods.ctrl && !press.mods.alt
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
    /// A pane's bell rings in the terminals showing its workspace.
    pub bell: bool,
    /// Each client's terminal title is its focused pane's.
    pub titles: bool,
    pub bindings: Vec<Binding>,
    /// Keys bound without the prefix (`bind -n`), one key each, in the order
    /// they were bound. None by default.
    pub root: Vec<Binding>,
}

/// The largest history a pane keeps.
pub const MAX_HISTORY: usize = 1_000_000;
pub const MAX_BUFFERS: usize = 1000;

/// The default bindings, as `bind` takes them, in command-column order.
/// After the prefix every default key is a lower-case letter: `r` and `m` are repeat
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
            prefix: KeyPress::new(crate::keys::Key::Char('b'), crate::keys::Modifiers::CTRL),
            shell: vec![shell],
            history_lines: 10_000,
            clipboard: true,
            buffers: 16,
            bell: true,
            titles: false,
            bindings: Vec::new(),
            root: Vec::new(),
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
    /// The keys after `path`, if the binding is in the layer at `path`:
    /// one or more of them.
    pub fn in_layer(&self, path: &[KeyPress]) -> Option<&[KeyPress]> {
        self.keys.strip_prefix(path).filter(|rest| !rest.is_empty())
    }
    /// The group this binding is listed under.
    pub fn group(&self) -> &str {
        self.group
            .as_deref()
            .unwrap_or_else(|| self.derived_group())
    }

    /// The group its command belongs to, whatever `-g` said.
    pub fn derived_group(&self) -> &'static str {
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
                | "terminate" | "choose-pane" | "send-keys" | "send-prefix" | "reorder",
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
    }
}

impl Config {
    /// The binding without the prefix (`bind -n`) for `press`, if one:
    /// matched as typed, as the prefix is.
    pub fn root_binding(&self, press: KeyPress) -> Option<&Binding> {
        self.root.iter().find(|b| b.keys == [press])
    }
    /// Applies `set`, `bind`, `unbind` or `unbind-all` given as words.
    pub fn apply(&mut self, argv: &[String]) -> Result<(), Error> {
        let (name, rest) = argv.split_first().ok_or(Error::Empty)?;
        match name.as_str() {
            "set" => {
                let (option, value) = rest.split_first().ok_or(Error::SetUsage)?;
                self.set(option, value)
            }
            "bind" => {
                let (mut group, mut repeat, mut root, mut rest) = (None, false, false, rest);
                loop {
                    match rest.split_first() {
                        Some((flag, after)) if flag == "-n" => {
                            root = true;
                            rest = after;
                        }
                        Some((flag, after)) if flag == "-g" => {
                            let (name, after) = after.split_first().ok_or(Error::NoGroupName)?;
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
                // The keys: the first word, and the words after it that name
                // keys; no command's name is a key's
                // (`no_command_is_named_as_a_key`).
                let (first, after) = rest.split_first().ok_or(Error::BindUsage)?;
                let more = after
                    .iter()
                    .take_while(|w| w.parse::<KeyPress>().is_ok())
                    .count();
                let (more, command) = after.split_at_checked(more).unwrap_or((after, &[]));
                let keys = std::iter::once(first)
                    .chain(more)
                    .map(|word| key("bind", word))
                    .collect::<Result<Vec<KeyPress>, Error>>()?;
                if root && (keys.len() > 1 || repeat) {
                    return Err(Error::RootUsage);
                }
                if command.is_empty() {
                    return Err(Error::NoCommand { keys });
                }
                if root && keys.first() == Some(&self.prefix) {
                    return Err(Error::RootPrefix { key: self.prefix });
                }
                if !root && keys.iter().any(is_escape) {
                    return Err(Error::EscapeBound { keys });
                }
                // Checked now, rather than each time its keys are typed.
                let parsed = match command::parse(command) {
                    Ok(parsed) => parsed,
                    Err(usage) => return Err(Error::Unparsed { keys, usage }),
                };
                let binding = Binding {
                    keys,
                    command: command.to_vec(),
                    parsed,
                    group,
                    repeat,
                };
                if root {
                    match self.root.iter_mut().find(|b| b.keys == binding.keys) {
                        Some(existing) => *existing = binding,
                        None => self.root.push(binding),
                    }
                    return Ok(());
                }
                self.bind(binding)
            }
            "unbind" => {
                if let Some((flag, rest)) = rest.split_first()
                    && flag == "-n"
                {
                    let [word] = rest else {
                        return Err(Error::UnbindUsage);
                    };
                    let key = key("unbind", word)?;
                    let before = self.root.len();
                    self.root.retain(|b| b.keys != [key]);
                    if self.root.len() == before {
                        return Err(Error::RootNotBound { key });
                    }
                    return Ok(());
                }
                if rest.is_empty() {
                    return Err(Error::UnbindUsage);
                }
                let keys = rest
                    .iter()
                    .map(|word| key("unbind", word))
                    .collect::<Result<Vec<KeyPress>, Error>>()?;
                // A binding, or a whole layer.
                let before = self.bindings.len();
                self.bindings.retain(|b| !b.keys.starts_with(&keys));
                if self.bindings.len() == before {
                    return Err(Error::NotBound { keys });
                }
                Ok(())
            }
            "unbind-all" => {
                if !rest.is_empty() {
                    return Err(Error::UnbindAllUsage);
                }
                self.unbind_all();
                Ok(())
            }
            other => Err(Error::NotConfig {
                command: other.to_owned(),
            }),
        }
    }

    /// Removes every binding, with the prefix and without it.
    pub fn unbind_all(&mut self) {
        self.bindings.clear();
        self.root.clear();
    }

    /// Adds a binding, replacing one of the same keys. Keys are a command
    /// or a layer, never both, so a binding that would make them both is
    /// refused.
    fn bind(&mut self, binding: Binding) -> Result<(), Error> {
        let keys = &binding.keys;
        let layer: Vec<Vec<KeyPress>> = self
            .bindings
            .iter()
            .filter(|b| b.keys.len() > keys.len() && b.keys.starts_with(keys))
            .map(|b| b.keys.clone())
            .collect();
        if !layer.is_empty() {
            return Err(Error::Layer {
                keys: binding.keys,
                layer,
            });
        }
        if let Some(command) = self
            .bindings
            .iter()
            .find(|b| b.keys.len() < keys.len() && keys.starts_with(&b.keys))
        {
            return Err(Error::Runs {
                keys: command.keys.clone(),
                command: command.command.clone(),
                new: binding.keys,
            });
        }
        match self.bindings.iter_mut().find(|b| b.keys == *keys) {
            Some(existing) => *existing = binding,
            None => self.bindings.push(binding),
        }
        Ok(())
    }

    fn set(&mut self, option: &str, value: &[String]) -> Result<(), Error> {
        let option_name = || option.to_owned();
        let one = || match value {
            [single] => Ok(single.as_str()),
            _ => Err(Error::OneValue {
                option: option_name(),
            }),
        };
        let number = |max: usize| -> Result<usize, Error> {
            let n: usize = one()?.parse().map_err(|_| Error::NotANumber {
                option: option_name(),
            })?;
            if n > max {
                return Err(Error::TooLarge {
                    option: option_name(),
                    max,
                });
            }
            Ok(n)
        };
        match option {
            "prefix" => {
                let prefix = one()?;
                // `C-b` and the like; an unknown name is the parser's error.
                prefix.parse::<KeyPress>()?;
                let prefix = key("set prefix", prefix)?;
                if self.root.iter().any(|b| b.keys == [prefix]) {
                    return Err(Error::PrefixRootBound { key: prefix });
                }
                self.prefix = prefix;
            }
            "shell" => {
                // `set shell /bin/zsh -l` and `set shell '/bin/zsh -l'` alike.
                let argv = if let [single] = value {
                    words::split(single)?
                } else {
                    value.to_vec()
                };
                if argv.first().is_none_or(|p| p.is_empty()) {
                    return Err(Error::NoProgram);
                }
                self.shell = argv;
            }
            "history-lines" => self.history_lines = number(MAX_HISTORY)?,
            "clipboard" => {
                self.clipboard = match one()? {
                    "on" | "write-only" => true,
                    "off" => false,
                    other => {
                        return Err(Error::NotOnOff {
                            option: option_name(),
                            value: other.to_owned(),
                        });
                    }
                }
            }
            "bell" | "titles" => {
                let on = match one()? {
                    "on" => true,
                    "off" => false,
                    other => {
                        return Err(Error::NotOnOff {
                            option: option_name(),
                            value: other.to_owned(),
                        });
                    }
                };
                if option == "bell" {
                    self.bell = on;
                } else {
                    self.titles = on;
                }
            }
            "buffers" => {
                let n = number(MAX_BUFFERS)?;
                if n == 0 {
                    return Err(Error::NoBuffers);
                }
                self.buffers = n;
            }
            other => {
                return Err(Error::UnknownOption {
                    option: other.to_owned(),
                });
            }
        }
        Ok(())
    }

    /// The configuration a file gives, applied line by line over the
    /// defaults. Any error names its file and line, and nothing applies.
    pub fn from_file(path: &Path) -> Result<Config, Error> {
        let mut config = Config::default();
        let text = match read_bounded(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(config),
            Err(source) => {
                return Err(Error::Read {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        for (number, line) in text.lines().enumerate() {
            let at = |error: Error| Error::Line {
                path: path.to_owned(),
                line: number.saturating_add(1),
                error: Box::new(error),
            };
            let argv = words::split(line).map_err(|e| at(e.into()))?;
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
            // One word, which `set shell` splits: a program whose path has
            // a space reads back as one.
            format!("set shell {}", words::quote(&words::join(&self.shell))),
            format!("set history-lines {}", self.history_lines),
            format!(
                "set clipboard {}",
                if self.clipboard { "on" } else { "off" }
            ),
            format!("set buffers {}", self.buffers),
            format!("set bell {}", if self.bell { "on" } else { "off" }),
            format!("set titles {}", if self.titles { "on" } else { "off" }),
        ];
        let root = self.root.iter().map(|b| (b, " -n"));
        for (binding, flag) in self.bindings.iter().map(|b| (b, "")).chain(root) {
            lines.push(format!(
                "bind{flag}{}{} {} {}",
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
    let set = |name: &str| std::env::var_os(name).filter(|d| !d.is_empty());
    let config = set("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("fux").join("fux.conf"))
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
        let argv = words::split(line).map_err(|e| e.to_string())?;
        config.apply(&argv).map_err(|e| e.to_string())
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
        assert_eq!(y.map(|b| b.group()), Some("Tools"));
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

    /// What `bind` says tells its failures apart: keys that are a layer,
    /// keys that are not letters, and a command that does not parse.
    #[test]
    fn bind_tells_a_layer_from_a_bad_key_and_a_bad_command() {
        let mut c = Config::default();
        let mut bind = |line: &str| c.apply(&words::split(line).unwrap_or_default());
        assert!(matches!(bind("bind t zoom"), Err(Error::Layer { .. })));
        assert!(matches!(bind("bind h u zoom"), Err(Error::Runs { .. })));
        assert!(matches!(
            bind("bind Nope zoom"),
            Err(Error::NotAKey {
                command: "bind",
                ..
            })
        ));
        assert!(matches!(
            bind("bind g nope"),
            Err(Error::Unparsed {
                usage: Usage::UnknownCommand(_),
                ..
            })
        ));
        assert!(matches!(bind("bind g"), Err(Error::NoCommand { .. })));
        assert!(bind("bind g zoom").is_ok());
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
        assert_eq!(
            apply(&mut c, "unbind"),
            Err("usage: unbind [-n] KEY…".into())
        );
    }

    /// A binding's command is parsed when the binding is made: one that
    /// does not parse is refused with the parser's message, and nothing is
    /// bound.
    #[test]
    fn a_binding_whose_command_does_not_parse_is_refused() -> Result<(), String> {
        let mut c = Config::default();
        let before = c.clone();
        for (line, error) in [
            (
                "bind g no-such-command --flag",
                "bind g: unknown command \"no-such-command\"; `fux help` lists commands",
            ),
            (
                "bind -r t z split",
                "bind t z: split needs -h (side by side) or -v (stacked)",
            ),
            (
                "bind g kill-pane -t 3",
                "bind g: \"3\" is not a pane; panes are %N",
            ),
        ] {
            assert_eq!(apply(&mut c, line), Err(error.to_owned()), "{line}");
        }
        assert_eq!(c, before);
        // Bound, the parsed command is what its words say.
        apply(&mut c, "bind g split -v -- htop")?;
        let g = c.bindings.iter().find(|b| keys_text(&b.keys) == "g");
        assert_eq!(
            g.map(|b| &b.parsed),
            Some(
                &crate::command::parse(
                    &words::split("split -v -- htop").map_err(|e| e.to_string())?
                )
                .map_err(|u| u.to_string())?
            )
        );
        // In a config file, such a line is an error naming its line.
        let dir = std::env::temp_dir().join(format!("fux-config-parse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("fux.conf");
        std::fs::write(&path, "set prefix C-a\nbind g nope\n").map_err(|e| e.to_string())?;
        let error = Config::from_file(&path)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            error.ends_with(":2: bind g: unknown command \"nope\"; `fux help` lists commands"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn keys_after_the_prefix_are_any_key_and_their_case_counts() {
        let mut c = Config::default();
        for line in [
            "bind C-Left resize-pane -L",
            "bind : command-prompt",
            "bind g ; zoom",
            "bind 1 zoom",
            "bind F5 zoom",
            "bind M-h select-pane -L",
            "bind t Up new-tab",
            "bind Space zoom",
        ] {
            assert_eq!(apply(&mut c, line), Ok(()), "{line}");
        }
        let keys =
            |c: &Config| -> Vec<String> { c.bindings.iter().map(|b| keys_text(&b.keys)).collect() };
        for bound in ["C-Left", ":", "g ;", "1", "F5", "M-h", "t Up", "Space"] {
            assert!(keys(&c).iter().any(|k| k == bound), "{bound}");
        }
        assert_eq!(
            apply(&mut c, "bind Nope zoom"),
            Err("bind: \"Nope\" is not a key; `fux list-keys` lists their names".into())
        );
        // Upper case is another key: Shift and the letter.
        assert!(apply(&mut c, "bind V split -v").is_ok());
        let v = |c: &Config, k: &str| {
            c.bindings
                .iter()
                .find(|b| keys_text(&b.keys) == k)
                .map(|b| b.command.clone())
        };
        assert_eq!(v(&c, "V"), Some(vec!["split".to_owned(), "-v".to_owned()]));
        assert_eq!(v(&c, "v"), Some(vec!["split".to_owned(), "-h".to_owned()]));
        // `S-v` is `V`, written back as `V`; with Ctrl a letter has no case.
        assert!(apply(&mut c, "bind S-v zoom").is_ok());
        assert_eq!(v(&c, "V"), Some(vec!["zoom".to_owned()]));
        assert!(apply(&mut c, "bind M-S-x zoom").is_ok());
        assert!(v(&c, "M-X").is_some());
        assert!(apply(&mut c, "bind C-S-y zoom").is_ok());
        assert!(v(&c, "C-y").is_some());
        // `T N` is not `t n`: unbinding it leaves `t n`.
        assert!(apply(&mut c, "unbind T N").is_err());
        assert!(v(&c, "t n").is_some());
        assert!(apply(&mut c, "unbind t n").is_ok());
        assert!(v(&c, "t n").is_none());
    }

    #[test]
    fn escape_after_the_prefix_cannot_be_bound() {
        let mut c = Config::default();
        for line in [
            "bind Escape zoom",
            "bind t Escape zoom",
            "bind Esc zoom",
            "bind S-Escape zoom",
        ] {
            assert!(
                matches!(
                    c.apply(&words::split(line).unwrap_or_default()),
                    Err(Error::EscapeBound { .. })
                ),
                "{line}"
            );
        }
        // With Ctrl or Alt it is another key, which the column does not take.
        assert_eq!(apply(&mut c, "bind M-Escape zoom"), Ok(()));
    }

    #[test]
    fn keys_bind_without_the_prefix_one_at_a_time() {
        let mut c = Config::default();
        assert_eq!(apply(&mut c, "bind -n M-h select-pane -L"), Ok(()));
        assert_eq!(apply(&mut c, "bind -n -g Tools F2 split -v"), Ok(()));
        // A plain letter is the user's choice.
        assert_eq!(apply(&mut c, "bind -n h zoom"), Ok(()));
        assert_eq!(c.root.len(), 3);
        // The keys after the prefix are untouched.
        assert_eq!(c.bindings.len(), Config::default().bindings.len());
        // Rebinding replaces.
        assert_eq!(apply(&mut c, "bind -n M-h select-pane -R"), Ok(()));
        assert_eq!(c.root.len(), 3);
        let refused = |c: &mut Config, line: &str| c.apply(&words::split(line).unwrap_or_default());
        assert!(matches!(
            refused(&mut c, "bind -n a b zoom"),
            Err(Error::RootUsage)
        ));
        assert!(matches!(
            refused(&mut c, "bind -n -r a zoom"),
            Err(Error::RootUsage)
        ));
        assert!(matches!(
            refused(&mut c, "bind -n C-b zoom"),
            Err(Error::RootPrefix { .. })
        ));
        assert!(matches!(
            refused(&mut c, "set prefix M-h"),
            Err(Error::PrefixRootBound { .. })
        ));
        assert!(matches!(
            refused(&mut c, "unbind -n M-l"),
            Err(Error::RootNotBound { .. })
        ));
        // Escape is no prefix key there, so it may be bound.
        assert_eq!(apply(&mut c, "bind -n Escape zoom"), Ok(()));
        // `describe` reads back as it was given.
        let mut again = Config::default();
        again.unbind_all();
        for line in c.describe().iter().filter(|l| l.starts_with("bind")) {
            assert_eq!(apply(&mut again, line), Ok(()), "{line}");
        }
        assert_eq!(again.root, c.root);
        assert_eq!(again.bindings, c.bindings);
        assert_eq!(apply(&mut c, "unbind -n M-h"), Ok(()));
        assert_eq!(c.root.len(), 3);
        assert_eq!(apply(&mut c, "unbind-all"), Ok(()));
        assert!(c.root.is_empty() && c.bindings.is_empty());
    }

    /// The words after `bind`'s first key that name keys are keys, so no
    /// command may be named as a key is.
    #[test]
    fn no_command_is_named_as_a_key() {
        let mut names = crate::keys::all_names();
        names.extend(["PgUp", "PPage", "PgDn", "NPage", "IC", "DC", "Esc"].map(String::from));
        let lower: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
        names.extend(lower);
        for name in names {
            assert!(
                matches!(
                    command::parse(std::slice::from_ref(&name)),
                    Err(Usage::UnknownCommand(_))
                ),
                "{name} is a command's name"
            );
        }
    }

    /// A config file is read up to 1 MiB, and a larger one refused rather
    /// than read whole: the form here of bevy-final finding 016, a file read
    /// without bound.
    #[test]
    fn a_config_file_over_a_mebibyte_is_refused() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("fux-config-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("fux.conf");
        // Comments only: read whole, it would apply as the defaults.
        let line = "# a comment, as long as a line may be\n";
        let fits: String = std::iter::repeat_n(line, (1 << 20) / line.len()).collect();
        std::fs::write(&path, &fits).map_err(|e| e.to_string())?;
        assert!(Config::from_file(&path).is_ok_and(|c| c == Config::default()));
        std::fs::write(&path, format!("{fits}{line}{line}")).map_err(|e| e.to_string())?;
        let error = Config::from_file(&path).err().map(|e| e.to_string());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            error
                .as_deref()
                .is_some_and(|e| e.ends_with("larger than 1 MiB")),
            "{error:?}"
        );
        Ok(())
    }

    #[test]
    fn a_file_applies_whole_or_names_its_bad_line() -> Result<(), Box<dyn std::error::Error>> {
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
        let error = Config::from_file(&path);
        assert!(matches!(
            &error,
            Err(Error::Line { line: 2, error, .. }) if matches!(error.as_ref(), Error::Key(_))
        ));
        let error = error.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(
            error.ends_with(":2: unknown key \"Nope\"; `fux list-keys` lists them"),
            "{error}"
        );
        assert!(Config::from_file(&dir.join("missing")).is_ok_and(|c| c == Config::default()));
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
    fn a_shell_is_described_so_that_it_reads_back() {
        for shell in [
            "/bin/sh",
            "/bin/zsh -l",
            "'/bin/zsh -l'",
            "\"'/My Shell/zsh'\"",
            "'/My Shell/zsh' -l",
            "/bin/sh -c 'it'\\''s'",
        ] {
            let mut c = Config::default();
            assert!(
                apply(&mut c, &format!("set shell {shell}")).is_ok(),
                "{shell}"
            );
            let back = read_back(&c);
            assert_eq!(back.as_ref().map(|b| &b.shell), Ok(&c.shell), "{shell}");
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
