//! Text, in the baseline and the current fux: lines split into the same
//! words or errors, words quote and join alike, command lines for a shell
//! are the same with and without fish; key names read and print alike;
//! config lines apply alike, one after another, to the same configuration,
//! described the same way, and config files read alike; JSON strings and
//! base64 are written the same.
use crate::rng::Rng;
use crate::{Outcome, bump, same, times};

const PIECES: &[&str] = &[
    "a", "b", "-", "_", "/", "'", "\\", "\"", " ", "  ", "\t", "$", "`", "#", "\u{7}", "\n", "界",
    "é", "", ".", "=", "%", "@", "+", ":", ",", "*", "(", "!",
];

const CONFIG: &[&str] = &[
    "set prefix C-a",
    "set prefix M-x",
    "set prefix nonsense",
    "set shell /bin/zsh -l",
    "set shell '/bin/bash --norc'",
    "set shell ''",
    "set history-lines 99",
    "set history-lines 1000001",
    "set history-lines x",
    "set clipboard off",
    "set clipboard write-only",
    "set clipboard maybe",
    "set buffers 0",
    "set buffers 3",
    "set buffers 1 2",
    "set nothing 1",
    "set",
    "bind x zoom",
    "bind -g Mine x zoom",
    "bind -r r h resize-pane -L",
    "bind t n new-tab",
    "bind t select-tab --next",
    "bind t n m zoom",
    "bind 1 zoom",
    "bind X",
    "bind -g",
    "bind q bogus-command",
    "unbind x",
    "unbind t",
    "unbind t n",
    "unbind",
    "unbind Z",
    "unbind-all",
    "unbind-all now",
    "split -h",
    "# a comment",
    "",
    "bind 'unterminated",
];

macro_rules! stack {
    ($name:ident, $fux:ident, $folded:expr) => {
        mod $name {
            use $fux::config::Config;
            use $fux::keys::KeyPress;
            use $fux::words;

            pub fn words(line: &str) -> String {
                let split = words::split(line).map_err(|e| e.to_string());
                let argv = split.clone().unwrap_or_default();
                format!(
                    "{split:?} {:?} {} {:?} {:?}",
                    argv.iter().map(|w| words::quote(w)).collect::<Vec<_>>(),
                    words::join(&argv),
                    words::shell_line(&argv, false).map_err(|e| e.to_string()),
                    words::shell_line(&argv, true).map_err(|e| e.to_string())
                )
            }

            pub fn key(name: &str) -> String {
                let press = name.parse::<KeyPress>();
                format!(
                    "{press:?} {:?} {:?}",
                    press
                        .as_ref()
                        .map(ToString::to_string)
                        .map_err(ToString::to_string),
                    press.as_ref().map(|p| ($folded)(*p).to_string())
                )
            }

            pub fn names() -> Vec<String> {
                $fux::keys::all_names()
            }

            /// The configuration each line leaves, and what it said.
            pub fn config(lines: &[&str]) -> Vec<String> {
                let mut config = Config::default();
                let mut out = Vec::new();
                for line in lines {
                    let result = words::split(line)
                        .map_err(|e| e.to_string())
                        .and_then(|argv| config.apply(&argv).map_err(|e| e.to_string()));
                    out.push(format!("{line:?}: {result:?}"));
                }
                out.extend(config.describe());
                out.extend(config.bindings.iter().map(|b| {
                    format!(
                        "{:?} {} {} {:?}",
                        b.keys,
                        b.group(),
                        b.derived_group(),
                        b.parsed
                    )
                }));
                out.push(format!(
                    "{:?} {:?} {} {} {}",
                    config.prefix,
                    config.shell,
                    config.history_lines,
                    config.clipboard,
                    config.buffers
                ));
                out
            }

            /// A config file read whole.
            pub fn file(path: &std::path::Path) -> String {
                match Config::from_file(path) {
                    Ok(config) => config.describe().join("\n"),
                    Err(error) => format!("error: {error}"),
                }
            }

            pub fn json(text: &str) -> String {
                let json = $fux::json::Json::Array(vec![
                    $fux::json::Json::str(text),
                    $fux::json::Json::Number(-3),
                    $fux::json::Json::Null,
                    $fux::json::Json::Bool(true),
                    $fux::json::Json::Object(vec![("key", $fux::json::Json::str(text))]),
                ]);
                format!("{} {}", json.render(), $fux::json::base64(text.as_bytes()))
            }
        }
    };
}

// How each folds a key typed after the prefix: 0.17 and before with
// config::folded, the current fux with KeyPress::folded.
stack!(base, baseline, baseline::config::folded);
stack!(cur, fux, |p: fux::keys::KeyPress| p.folded());

pub fn run(r: &mut Rng, scale: usize) -> Outcome {
    let mut lines = 0u64;
    for _ in 0..times(50_000, scale) {
        let line: String = (0..r.below(10))
            .map(|_| r.pick(PIECES).copied().unwrap_or_default())
            .collect();
        same(
            &format!("the line {line:?}"),
            base::words(&line),
            cur::words(&line),
        )?;
        same(
            &format!("the text {line:?}"),
            base::json(&line),
            cur::json(&line),
        )?;
        bump(&mut lines);
    }
    same("every key name", base::names(), cur::names())?;
    let mut keys = 0u64;
    let names = base::names();
    for _ in 0..times(20_000, scale) {
        let mut name = String::new();
        for prefix in ["C-", "M-", "S-", "c-", "m-", "s-"] {
            if r.chance(15) {
                name.push_str(prefix);
            }
        }
        if r.chance(70) {
            name.push_str(r.pick(&names).map_or("a", String::as_str));
        } else {
            name.push_str(r.pick(PIECES).copied().unwrap_or_default());
        }
        same(
            &format!("the key {name:?}"),
            base::key(&name),
            cur::key(&name),
        )?;
        bump(&mut keys);
    }
    let mut configs = 0u64;
    let dir = std::env::temp_dir().join(format!("fux-diff-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for case in 0..times(2_000, scale) {
        let config: Vec<&str> = (0..r.below(12))
            .map(|_| r.pick(CONFIG).copied().unwrap_or_default())
            .collect();
        same(
            &format!("config {case}: {config:?}"),
            base::config(&config),
            cur::config(&config),
        )?;
        let path = dir.join("fux.conf");
        std::fs::write(&path, config.join("\n")).map_err(|e| format!("{}: {e}", path.display()))?;
        same(
            &format!("config file {case}: {config:?}"),
            base::file(&path),
            cur::file(&path),
        )?;
        bump(&mut configs);
    }
    let missing = dir.join("none.conf");
    same(
        "a missing config file",
        base::file(&missing),
        cur::file(&missing),
    )?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(format!(
        "{lines} lines split, quoted and written as JSON, {keys} key names, {configs} configurations and files"
    ))
}
