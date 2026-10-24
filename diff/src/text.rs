//! Text, in the baseline and the current fux: lines split into the same
//! words or errors, words quote alike; key names read and print alike;
//! JSON strings and base64 are written the same. Config files are no longer
//! compared: the baseline's grammar is gone, and a config is a fuxth
//! program its clients run.
use crate::rng::Rng;
use crate::{Outcome, bump, same, times};

const PIECES: &[&str] = &[
    "a", "b", "-", "_", "/", "'", "\\", "\"", " ", "  ", "\t", "$", "`", "#", "\u{7}", "\n", "界",
    "é", "", ".", "=", "%", "@", "+", ":", ",", "*", "(", "!",
];

macro_rules! stack {
    ($name:ident, $fux:ident, $folded:expr) => {
        mod $name {
            use $fux::keys::KeyPress;
            use $fux::words;

            pub fn words(line: &str) -> String {
                let split = words::split(line).map_err(|e| e.to_string());
                let argv = split.clone().unwrap_or_default();
                format!(
                    "{split:?} {:?}",
                    argv.iter().map(|w| words::quote(w)).collect::<Vec<_>>()
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

// How each folds a key typed after the prefix, KeyPress::folded.
stack!(base, baseline, |p: KeyPress| p.folded());
stack!(cur, fux, |p: KeyPress| p.folded());

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
    Ok(format!(
        "{lines} lines split, quoted and written as JSON, {keys} key names"
    ))
}
