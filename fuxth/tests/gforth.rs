//! fuxth held to gforth on random integer programs. A program that ends
//! with only integers and booleans in fuxth must leave the same in gforth,
//! a boolean as gforth's flag (-1 or 0). `div` and `mod` become gforth's
//! symmetric division (`sm/rem`), which fuxth's is: gforth's own `/`
//! floors. Forth runs `if` and `begin` only in a definition, so each
//! program is one. Skipped when there's no gforth: `FUXTH_GFORTH` names one, else
//! `gforth` on the path.
mod common;
use common::{Ended, Gen, Rng, V, fuxth};
use std::process::Command;

const PROGRAMS: u64 = 25_000;

/// Words gforth spells otherwise, or lacks.
const HELPERS: &str = "\
: fx-div >r s>d r> sm/rem nip ;
: fx-mod >r s>d r> sm/rem drop ;
: ne <> ; : le > 0= ; : ge < 0= ;
100 maxdepth-.s !
";

fn forth(word: &str) -> &str {
    match word {
        "add" => "+",
        "sub" => "-",
        "mul" => "*",
        "div" => "fx-div",
        "mod" => "fx-mod",
        "eq" => "=",
        "lt" => "<",
        "gt" => ">",
        "not" => "0=",
        other => other,
    }
}

fn shown(left: &[V]) -> Option<String> {
    let values: Option<Vec<String>> = left
        .iter()
        .map(|v| match v {
            V::Int(n) => Some(n.to_string()),
            V::Bool(b) => Some(if *b { "-1" } else { "0" }.to_owned()),
            V::Str(_) | V::List(_) => None,
        })
        .collect();
    let values = values?;
    Some(format!("<{}> {}", values.len(), values.join(" ")))
}

#[test]
fn fuxth_leaves_what_gforth_leaves() -> std::io::Result<()> {
    let gforth = std::env::var("FUXTH_GFORTH").unwrap_or_else(|_| "gforth".to_owned());
    if Command::new(&gforth).arg("-e").arg("bye").output().is_err() {
        return Ok(());
    }
    let mut source = String::from(HELPERS);
    let mut expected = Vec::new();
    for seed in 0..PROGRAMS {
        let mut rng = Rng::new(seed.wrapping_add(3 << 32));
        let text = Gen::new(&mut rng).numeric_program();
        if text.is_empty() {
            continue;
        }
        if text.lines().any(|l| matches!(l, "str" | "ask" | "maybe")) {
            continue;
        }
        let Ended::Left(printed, left) = fuxth(&text, u64::MAX) else {
            continue;
        };
        if !printed.is_empty() {
            continue;
        }
        let Some(shown) = shown(&left) else {
            continue;
        };
        let words: Vec<&str> = text.split_whitespace().map(forth).collect();
        // Forth runs control words only inside a definition.
        source.push_str(&format!(
            ": p{seed} {} ;\np{seed} .s cr clearstack\n",
            words.join(" ")
        ));
        expected.push((seed, text, shown));
    }
    source.push_str("bye\n");
    let file = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("gforth-programs.fs");
    std::fs::write(&file, source)?;
    let output = Command::new(&gforth).arg(&file).output()?;
    let printed = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = printed.lines().map(str::trim).collect();
    assert!(expected.len() > 1000, "{} programs", expected.len());
    assert_eq!(
        lines.len(),
        expected.len(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for ((seed, text, shown), line) in expected.iter().zip(&lines) {
        let shown = shown.trim();
        assert_eq!(line, &shown, "seed {seed}\n{text}");
    }
    Ok(())
}
