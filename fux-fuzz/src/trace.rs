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
    Startup { config: Config },
    Resize { sizes: Vec<[u16; 4]>, token: String },
    Shutdown { mode: Shutdown },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Config {
    Valid,
    Missing,
    Malformed,
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
            matches!(scenario, "all" | "startup" | "resize" | "shutdown"),
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
            version: 1,
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
        ensure(self.version == 1, "unsupported trace version")?;
        ensure(
            !self.actions.is_empty() && self.actions.len() <= 700,
            "invalid scenario count",
        )?;
        for action in &self.actions {
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
        plan.version = 2;
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
