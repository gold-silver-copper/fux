//! The corpus: what real programs wrote to a terminal, as `fux-vt/compare`
//! recorded it (`fux-vt/compare/corpus/NAME.bin`, and `NAME.json` with the
//! size and the steps: the keys typed, escaped, and where the output after
//! them ends).
use std::path::{Path, PathBuf};

/// Where the recordings are in a checkout whose root is `root`.
pub fn dir(root: &Path) -> PathBuf {
    root.join("fux-vt/compare/corpus")
}

/// A recording.
pub struct Recording {
    pub name: String,
    pub rows: u16,
    pub cols: u16,
    /// Each step's keys, as a legacy terminal sends them, and the program's
    /// output after them.
    pub steps: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Recording {
    /// Every byte the program wrote, in order.
    pub fn bytes(&self) -> Vec<u8> {
        self.steps
            .iter()
            .flat_map(|(_, bytes)| bytes.iter().copied())
            .collect()
    }
}

fn field<'a>(
    value: &'a serde_json::Value,
    name: &str,
    path: &Path,
) -> Result<&'a serde_json::Value, String> {
    value
        .get(name)
        .ok_or(format!("{}: no {name:?}", path.display()))
}

fn load(path: &Path) -> Result<Recording, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let bin = path.with_extension("bin");
    let bytes = std::fs::read(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    let size = field(&json, "size", path)?;
    let dimension = |i: usize| -> Result<u16, String> {
        size.get(i)
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u16::try_from(n).ok())
            .ok_or(format!("{}: a bad size", path.display()))
    };
    let mut steps = Vec::new();
    let mut from = 0usize;
    for step in field(&json, "steps", path)?
        .as_array()
        .ok_or(format!("{}: steps is not a list", path.display()))?
    {
        let keys = unescape(field(step, "keys", path)?.as_str().unwrap_or_default())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let end = field(step, "end", path)?
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(format!("{}: a step's end is not a number", path.display()))?;
        let output = bytes
            .get(from..end)
            .ok_or(format!("{}: a step ends past the bytes", path.display()))?;
        steps.push((keys, output.to_vec()));
        from = end;
    }
    if from != bytes.len() {
        return Err(format!("{}: bytes after the last step", path.display()));
    }
    Ok(Recording {
        name: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        rows: dimension(0)?,
        cols: dimension(1)?,
        steps,
    })
}

/// Every recording in `dir`, by name.
pub fn recordings(dir: &Path) -> Result<Vec<Recording>, String> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    paths.iter().map(|p| load(p)).collect()
}

/// The recordings' keys back to bytes, as `fux-vt/compare` escapes them
/// (`src/escape.rs`): `\e`, `\r`, `\n`, `\t`, `\\`, `\'`, `\xNN` and
/// `\u{N}`.
pub fn unescape(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut chars = text.chars();
    let hex =
        |digits: &str| u32::from_str_radix(digits, 16).map_err(|e| format!("{digits:?}: {e}"));
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('e') => out.push(0x1b),
            Some('r') => out.push(b'\r'),
            Some('n') => out.push(b'\n'),
            Some('t') => out.push(b'\t'),
            Some('\\') => out.push(b'\\'),
            Some('\'') => out.push(b'\''),
            Some('x') => {
                let digits: String = chars.by_ref().take(2).collect();
                out.push(u8::try_from(hex(&digits)?).map_err(|e| e.to_string())?);
            }
            Some('u') => {
                if chars.next() != Some('{') {
                    return Err("\\u must be followed by {".into());
                }
                let digits: String = chars.by_ref().take_while(|&c| c != '}').collect();
                let c = char::from_u32(hex(&digits)?)
                    .ok_or(format!("\\u{{{digits}}} is not a character"))?;
                let mut buf = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            other => return Err(format!("unknown escape \\{}", other.unwrap_or(' '))),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn keys_unescape_as_compare_escapes_them() {
        let text = "a\\e[1m\\r\\n\\t\\\\\\'\\x07\\x7f\\xff\\u{754c}\\xc3";
        let bytes = b"a\x1b[1m\r\n\t\\'\x07\x7f\xff\xe7\x95\x8c\xc3";
        assert_eq!(super::unescape(text), Ok(bytes.to_vec()));
        assert!(super::unescape("\\q").is_err());
    }

    /// Every recording in the checkout loads, with its steps covering its
    /// bytes.
    #[test]
    fn the_corpus_loads() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let all = super::recordings(&super::dir(&root)).unwrap_or_default();
        assert!(!all.is_empty());
        for r in &all {
            assert!(r.rows > 0 && r.cols > 0, "{}", r.name);
            assert!(!r.bytes().is_empty(), "{}", r.name);
        }
    }
}
