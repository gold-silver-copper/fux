//! Instructions retired by a child process, which, unlike time, other work
//! on the machine does not change:
//! - macOS: `/usr/bin/time -l`, which prints the child's `instructions
//!   retired` (from its resource usage);
//! - Linux: `perf stat -e instructions:u` where perf may count, else
//!   valgrind's cachegrind (`I refs`), which is slower by fifty times or so
//!   but counts the same anywhere.
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Counter {
    Time,
    Perf,
    Cachegrind,
}

impl Counter {
    pub fn name(self) -> &'static str {
        match self {
            Counter::Time => "/usr/bin/time -l (instructions retired)",
            Counter::Perf => "perf stat -e instructions:u",
            Counter::Cachegrind => "valgrind --tool=cachegrind (I refs)",
        }
    }

    /// The first counter that counts `true` here.
    pub fn detect() -> Result<Counter, String> {
        for counter in [Counter::Time, Counter::Perf, Counter::Cachegrind] {
            if counter
                .count(Path::new("/bin/sh"), &["-c", ":"])
                .is_ok_and(|n| n.0 > 0)
            {
                return Ok(counter);
            }
        }
        Err(
            "nothing here counts instructions: on macOS /usr/bin/time -l, on Linux \
             perf (with kernel.perf_event_paranoid at 2 or less) or valgrind"
                .into(),
        )
    }

    /// Runs `program` with `args`; the instructions it retired, and what it
    /// wrote to stdout.
    pub fn count(self, program: &Path, args: &[&str]) -> Result<(u64, String), String> {
        let mut command = match self {
            Counter::Time => {
                let mut c = Command::new("/usr/bin/time");
                c.arg("-l").arg(program);
                c
            }
            Counter::Perf => {
                let mut c = Command::new("perf");
                c.args(["stat", "-x", ",", "-e", "instructions:u", "--"])
                    .arg(program);
                c
            }
            Counter::Cachegrind => {
                let mut c = Command::new("valgrind");
                c.args([
                    "--tool=cachegrind",
                    "--cache-sim=no",
                    "--cachegrind-out-file=/dev/null",
                ])
                .arg(program);
                c
            }
        };
        let out = command
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("{}: {e}", self.name()))?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            return Err(format!(
                "{} {args:?} failed ({}): {}",
                program.display(),
                out.status,
                stderr.trim()
            ));
        }
        let n = self.parse(&stderr).ok_or(format!(
            "{}: no count in {:?}",
            self.name(),
            stderr.trim()
        ))?;
        Ok((n, String::from_utf8_lossy(&out.stdout).into_owned()))
    }

    fn parse(self, stderr: &str) -> Option<u64> {
        let digits = |text: &str| -> Option<u64> {
            let kept: String = text.chars().filter(char::is_ascii_digit).collect();
            kept.parse().ok()
        };
        stderr.lines().find_map(|line| match self {
            // "     10558014  instructions retired"
            Counter::Time => line
                .trim()
                .strip_suffix("instructions retired")
                .and_then(digits),
            // "10558014,,instructions:u,1000000,100.00,,"
            Counter::Perf => line
                .split(',')
                .nth(2)
                .filter(|event| event.starts_with("instructions"))
                .and_then(|_| line.split(',').next())
                .and_then(digits),
            // "==123== I   refs:      10,558,014"
            Counter::Cachegrind => line
                .split_once("I   refs:")
                .or_else(|| line.split_once("I refs:"))
                .and_then(|(_, n)| digits(n)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Counter;

    #[test]
    fn each_counter_reads_its_tool() {
        let time = "        0  voluntary context switches\n     10558014  instructions retired\n     10045345  cycles elapsed\n";
        assert_eq!(Counter::Time.parse(time), Some(10_558_014));
        let perf = "10558014,,instructions:u,1000000,100.00,,\n";
        assert_eq!(Counter::Perf.parse(perf), Some(10_558_014));
        assert_eq!(
            Counter::Perf.parse("<not supported>,,instructions:u,0,0,,\n"),
            None
        );
        let grind = "==123== Cachegrind, a cache and branch-prediction profiler\n==123== I   refs:      10,558,014\n";
        assert_eq!(Counter::Cachegrind.parse(grind), Some(10_558_014));
    }
}
