use crate::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::Write,
    path::Path,
    time::Instant,
};

// Versioned scenario recipes, not a general scenario language. All varying
// choices are stored here; replay never calls the generator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub version: u32,
    pub seed: u64,
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scenario", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Startup {
        config: Config,
    },
    Resize {
        sizes: Vec<[u16; 4]>,
        token: String,
    },
    Shutdown {
        mode: Shutdown,
    },
    Paste {
        text: String,
        repeats: usize,
        bracketed: bool,
        chunk_bytes: usize,
    },
    /// A graceful termination signal delivered to an attached frontend.
    Signal {
        signal: FrontendSignal,
    },
    /// Raw outer-terminal key encodings that must reach the pane byte-exact.
    Keys {
        sequences: Vec<String>,
    },
    /// Copy-mode selections whose OSC 52 payload must decode to the selected text.
    Copy {
        /// Start with the clipboard disabled and enable it by creating the
        /// configuration file while the server runs.
        reload: bool,
    },
    /// History scrolling by command, wheel and copy-mode paging, plus copying
    /// from history and the selection-invalidation rule.
    History,
    /// Viewer-local zoom with two viewers sharing two panes.
    Zoom,
    /// Pane moves, tab/workspace closes, shared references and confirmations.
    Layout,
    /// Stock-spawned Launch recipes, reflected dimension edits, termination
    /// paths, failed launches, natural exit and history_lines.
    Process,
    /// Directional focus ranking, focus cycling and swaps.
    Nav,
    /// Layout save and load, including failures that must not replace it.
    Scene,
    /// Hot reload of prefix and bindings.
    Config,
    /// Prompts, confirmations, choosers and unavailable actions.
    Overlay,
    /// Outer-terminal mouse events against a pane that requested a protocol.
    Mouse {
        /// The DECSET the child requests: 1000, 1002 or 1003.
        mode: u16,
        /// Whether the child also requests SGR (1006) encoding.
        sgr: bool,
        /// Two side-by-side panes instead of one full-width pane.
        split: bool,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendSignal {
    Interrupt,
    Terminate,
    Hangup,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Config {
    Valid,
    Missing,
    Malformed,
    /// No shell override; only `clipboard: write-only`.
    Clipboard,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shutdown {
    Initializing,
    Lifecycle,
    Output,
}

impl Plan {
    pub fn generate(scenario: &str, seed: u64, iterations: usize, count: usize) -> Result<Self> {
        ensure(
            matches!(
                scenario,
                "all"
                    | "startup"
                    | "resize"
                    | "shutdown"
                    | "paste"
                    | "signal"
                    | "keys"
                    | "mouse"
                    | "copy"
                    | "history"
                    | "zoom"
                    | "layout"
                    | "process"
                    | "nav"
                    | "scene"
                    | "config"
                    | "overlay"
            ),
            "unknown scenario",
        )?;
        ensure(
            (1..=100).contains(&iterations) && (1..=200).contains(&count),
            "iterations must be 1..100 and actions 1..200",
        )?;
        let mut state = seed;
        let mut actions = Vec::new();
        for _ in 0..iterations {
            if matches!(scenario, "all" | "startup") {
                for config in [Config::Missing, Config::Malformed, Config::Valid] {
                    actions.push(Action::Startup { config });
                }
            }
            if matches!(scenario, "all" | "resize") {
                let mut sizes = Vec::new();
                // Each fixed tiny case ends a burst, so it gets a convergence
                // assertion rather than being only an intermediate ioctl.
                for size in [[1, 1, 2, 2], [2, 1, 1, 80], [24, 80, 12, 40]] {
                    sizes.extend([size; 3]);
                }
                for _ in 0..count {
                    let mut next = || {
                        // SplitMix64: zero is a useful seed too.
                        state = state.wrapping_add(0x9e3779b97f4a7c15);
                        let mut x = state;
                        x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                        x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
                        x ^ (x >> 31)
                    };
                    sizes.push([
                        1 + (next() % 40) as u16,
                        1 + (next() % 120) as u16,
                        1 + (next() % 40) as u16,
                        1 + (next() % 120) as u16,
                    ]);
                }
                sizes.push([24, 80, 18, 60]);
                actions.push(Action::Resize {
                    sizes,
                    token: format!("INPUT-{state:016x}"),
                });
            }
            if matches!(scenario, "all" | "paste") {
                // Payload limit is in UTF-8 bytes, exclusive of terminal framing.
                // The boundary controls distinguish envelope overhead from size
                // rejection, Unicode decoding, fragmentation, or a blocked PTY.
                for (text, repeats, bracketed, chunk_bytes) in [
                    ("x", 0, false, 1),
                    ("line\n界é", 3, true, 1),
                    ("x", 65_524, true, 1024),
                    ("x", 65_525, true, 1024),
                    ("x", 65_536, false, 1024),
                    ("x", 65_536, true, 1024),
                    ("界", 21_845, false, 1024),
                    ("界", 21_845, true, 1024),
                    ("x", 65_537, false, 1024),
                    ("x", 65_537, true, 1024),
                ] {
                    actions.push(Action::Paste {
                        text: text.into(),
                        repeats,
                        bracketed,
                        chunk_bytes,
                    });
                }
            }
            if matches!(scenario, "all" | "keys") {
                let canonical = canonical_keys();
                actions.push(Action::Keys {
                    sequences: canonical.clone(),
                });
                // A seeded order finds escape-timing interactions between
                // neighbours that the canonical order happens to avoid.
                let mut shuffled = canonical;
                for i in (1..shuffled.len()).rev() {
                    state = state.wrapping_add(0x9e3779b97f4a7c15);
                    let mut x = state;
                    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
                    let j = ((x ^ (x >> 31)) % (i as u64 + 1)) as usize;
                    shuffled.swap(i, j);
                }
                actions.push(Action::Keys {
                    sequences: shuffled,
                });
            }
            if matches!(scenario, "all" | "copy") {
                for reload in [false, true] {
                    actions.push(Action::Copy { reload });
                }
            }
            if matches!(scenario, "all" | "history") {
                actions.push(Action::History);
            }
            if matches!(scenario, "all" | "zoom") {
                actions.push(Action::Zoom);
            }
            if matches!(scenario, "all" | "layout") {
                actions.push(Action::Layout);
            }
            if matches!(scenario, "all" | "process") {
                actions.push(Action::Process);
            }
            if matches!(scenario, "all" | "nav") {
                actions.push(Action::Nav);
            }
            if matches!(scenario, "all" | "scene") {
                actions.push(Action::Scene);
            }
            if matches!(scenario, "all" | "config") {
                actions.push(Action::Config);
            }
            if matches!(scenario, "all" | "overlay") {
                actions.push(Action::Overlay);
            }
            if matches!(scenario, "all" | "mouse") {
                for (mode, sgr, split) in [
                    (1002, true, false),
                    (1000, false, false),
                    (1003, true, false),
                    (1002, true, true),
                    (1000, false, true),
                ] {
                    actions.push(Action::Mouse { mode, sgr, split });
                }
            }
            if matches!(scenario, "all" | "signal") {
                for signal in [
                    FrontendSignal::Interrupt,
                    FrontendSignal::Terminate,
                    FrontendSignal::Hangup,
                ] {
                    actions.push(Action::Signal { signal });
                }
            }
            if matches!(scenario, "all" | "shutdown") {
                for mode in [
                    Shutdown::Initializing,
                    Shutdown::Lifecycle,
                    Shutdown::Output,
                ] {
                    actions.push(Action::Shutdown { mode });
                }
            }
        }
        Ok(Self {
            version: 3,
            seed,
            actions,
        })
    }
    pub fn read(path: &Path) -> Result<Self> {
        ensure(
            fs::metadata(path)?.len() <= 4 * 1024 * 1024,
            "trace exceeds 4 MiB",
        )?;
        let plan: Self = serde_json::from_slice(&fs::read(path)?)?;
        plan.validate()?;
        Ok(plan)
    }
    pub fn validate(&self) -> Result<()> {
        ensure(matches!(self.version, 1..=3), "unsupported trace version")?;
        ensure(
            !self.actions.is_empty() && self.actions.len() <= 1700,
            "invalid scenario count",
        )?;
        for action in &self.actions {
            if let Action::Paste {
                text,
                repeats,
                chunk_bytes,
                ..
            } = action
            {
                ensure(self.version >= 2, "paste actions require trace version 2")?;
                let bytes = text
                    .len()
                    .checked_mul(*repeats)
                    .ok_or("paste size overflow")?;
                ensure(
                    !text.is_empty()
                        && text.len() <= 64
                        && text.chars().all(|ch| !ch.is_control() || ch == '\n'),
                    "invalid paste text",
                )?;
                ensure(
                    bytes <= 65_537 && (1..=4096).contains(chunk_bytes),
                    "paste bounds exceeded",
                )?;
                ensure(bytes.div_ceil(*chunk_bytes) <= 256, "too many paste chunks")?;
            }
            if let Action::Signal { .. } = action {
                ensure(self.version >= 3, "signal actions require trace version 3")?;
            }
            if let Action::Copy { .. }
            | Action::History
            | Action::Zoom
            | Action::Layout
            | Action::Process = action
            {
                ensure(self.version >= 3, "this action requires trace version 3")?;
            }
            if let Action::Startup { config } = action {
                ensure(
                    *config != Config::Clipboard,
                    "startup recipes use shell configurations",
                )?;
            }
            if let Action::Mouse { mode, .. } = action {
                ensure(self.version >= 3, "mouse actions require trace version 3")?;
                ensure(
                    matches!(mode, 1000 | 1002 | 1003),
                    "unsupported mouse protocol mode",
                )?;
            }
            if let Action::Keys { sequences } = action {
                ensure(self.version >= 3, "keys actions require trace version 3")?;
                ensure(
                    !sequences.is_empty()
                        && sequences.len() <= 256
                        && sequences.iter().all(|k| !k.is_empty() && k.len() <= 16),
                    "invalid key sequences",
                )?;
            }
            if let Action::Resize { sizes, token } = action {
                ensure(
                    !sizes.is_empty() && sizes.len() <= 210,
                    "invalid resize count",
                )?;
                ensure(
                    sizes.iter().flatten().all(|n| (1..=160).contains(n)),
                    "invalid PTY size",
                )?;
                ensure(
                    !token.is_empty()
                        && token.len() <= 64
                        && token
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                    "invalid input token",
                )?;
            }
        }
        Ok(())
    }
}

/// Every xterm encoding the frontend decodes and fux re-encodes with the same
/// bytes: an ordinary application must see what the user typed. The prefix
/// itself (Ctrl-B) is excluded because fux owns it by design.
pub fn canonical_keys() -> Vec<String> {
    let mut keys: Vec<String> = ["a", "Z", " ", "~", "界", "é"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    // Control characters: NUL, Ctrl-A..Ctrl-Z except the prefix, then the four
    // C0 controls above Ctrl-Z (Ctrl-\, Ctrl-], Ctrl-^, Ctrl-_).
    keys.push("\0".into());
    keys.extend(
        (1u8..=0x1a)
            .filter(|c| *c != 0x02 && *c != 0x1b)
            .map(|c| char::from(c).to_string()),
    );
    keys.extend((0x1cu8..=0x1f).map(|c| char::from(c).to_string()));
    keys.extend(
        [
            "\x7f",
            "\r",
            "\t",
            "\x1b[Z",
            "\x1b",
            "\x1bx",
            "\x1b[A",
            "\x1b[B",
            "\x1b[C",
            "\x1b[D",
            "\x1b[H",
            "\x1b[F",
            "\x1b[2~",
            "\x1b[3~",
            "\x1b[5~",
            "\x1b[6~",
            "\x1bOP",
            "\x1bOQ",
            "\x1bOR",
            "\x1bOS",
            "\x1b[15~",
            "\x1b[17~",
            "\x1b[24~",
            "\x1b[1;5D",
            "\x1b[1;2A",
            "\x1b[1;3C",
            "\x1b[3;5~",
            "\x1b[1;8P",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    keys
}

pub struct Journal {
    file: File,
    start: Instant,
    bytes: usize,
}
impl Journal {
    pub fn new(path: &Path) -> Result<Self> {
        Ok(Self {
            file: File::create(path)?,
            start: Instant::now(),
            bytes: 0,
        })
    }
    pub fn record(&mut self, kind: &str, value: Value) -> Result<()> {
        let mut line = serde_json::to_vec(
            &json!({"ms": self.start.elapsed().as_millis(), "kind": kind, "value": value}),
        )?;
        line.push(b'\n');
        ensure(
            self.bytes + line.len() <= 4 * 1024 * 1024,
            "event journal exceeded 4 MiB",
        )?;
        self.file.write_all(&line)?;
        self.file.flush()?;
        self.bytes += line.len();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seed_and_serialized_replay_preserve_concrete_actions() -> Result<()> {
        let plan = Plan::generate("all", 42, 2, 10)?;
        assert_eq!(plan, Plan::generate("all", 42, 2, 10)?);
        assert_ne!(plan, Plan::generate("all", 43, 2, 10)?);
        let mut replay: Plan = serde_json::from_slice(&serde_json::to_vec(&plan)?)?;
        replay.seed = 999; // informational: replay still uses the saved sizes/token
        assert_eq!(replay.actions, plan.actions);
        replay.validate()?;
        Ok(())
    }
    #[test]
    fn rejects_unbounded_or_unknown_recipes() -> Result<()> {
        let mut plan = Plan::generate("resize", 0, 1, 1)?;
        plan.version = 99;
        assert!(plan.validate().is_err());
        assert!(Plan::generate("all", 0, 101, 1).is_err());
        assert!(
            serde_json::from_str::<Action>(
                r#"{"scenario":"startup","config":"missing","extra":1}"#
            )
            .is_err()
        );
        Ok(())
    }
}
