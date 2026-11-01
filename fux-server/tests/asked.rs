//! A request's words declare the codes asking it fails with as the wire's
//! table marks them (`Code::asked`): every code the server's source, and
//! the wire's it answers with, names is one of them. A code named anew
//! there, unmarked, is seen here.
use fux_wire::code::Code;
use std::path::Path;

/// The `.rs` files under `dir`, read.
fn sources(dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            sources(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(std::fs::read_to_string(&path)?);
        }
    }
    Ok(())
}

/// `NoPane` as its code's name, `no_pane`.
fn snake(variant: &str) -> String {
    let mut name = String::new();
    for c in variant.chars() {
        if c.is_ascii_uppercase() && !name.is_empty() {
            name.push('_');
        }
        name.push(c.to_ascii_lowercase());
    }
    name
}

#[test]
fn every_code_the_server_answers_with_is_one_a_request_is_asked_with() -> std::io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut texts = Vec::new();
    sources(&root.join("src"), &mut texts)?;
    sources(&root.join("../fux-wire/src"), &mut texts)?;
    let mut unmarked = std::collections::BTreeSet::new();
    for text in &texts {
        for prefix in ["Raise::", "Code::"] {
            for (at, _) in text.match_indices(prefix) {
                let rest = text.get(at + prefix.len()..).unwrap_or_default();
                let variant: String = rest
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect();
                // What names no code: `Code::Lang`, `Code::from`.
                let Ok(code) = snake(&variant).parse::<Code>() else {
                    continue;
                };
                if !code.asked() && code != Code::Cancelled {
                    unmarked.insert(code.name());
                }
            }
        }
    }
    assert!(unmarked.is_empty(), "named, not [asked]: {unmarked:?}");
    Ok(())
}
