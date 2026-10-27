//! Instructions retired by a child process, which, unlike time, other work
//! on the machine does not change:
//! - macOS: `/usr/bin/time -l`, which prints the child's `instructions
//!   retired` (from its resource usage);
//! - Linux: `bench/perf.sh`, which runs `perf stat -e instructions:u`
//!   pinned to one core type and keeps perf's output whole; else
//!   valgrind's cachegrind (`I refs`), which is slower by fifty times or so
//!   but counts the same anywhere.
//!
//! A perf count is taken only when it is whole: every PMU line that ran
//! ran for all the time it was enabled ([`perf`]). A share under 100% means
//! perf multiplexed the counter, or the child moved between core types,
//! and perf would have scaled a part up to a guess.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Counter {
    Time,
    Perf,
    Cachegrind,
}

/// One counted run.
pub struct Run {
    pub instructions: u64,
    /// What the child wrote to stdout.
    pub stdout: String,
    /// The counter's own report, unedited: for perf, `perf.sh`'s lines
    /// (the CPU set, the PMUs, the CPUs) and perf's output.
    pub raw: String,
}

/// The wrapper the perf counter runs, which the crate that builds this
/// file names (`PERF_SH`): fux-bench's own, and fux-vt-compare's, which
/// builds this very file (`#[path]`), so the two count alike.
fn wrapper() -> PathBuf {
    Path::new(crate::PERF_SH).to_path_buf()
}

impl Counter {
    pub fn name(self) -> &'static str {
        match self {
            Counter::Time => "/usr/bin/time -l (instructions retired)",
            Counter::Perf => "bench/perf.sh (perf stat -e instructions:u, pinned, unscaled)",
            Counter::Cachegrind => "valgrind --tool=cachegrind (I refs)",
        }
    }

    /// The first counter that counts `true` here, and its report of that
    /// run: for perf, the CPU set and topology it counts on.
    pub fn detect() -> Result<(Counter, String), String> {
        let mut why = Vec::new();
        for counter in [Counter::Time, Counter::Perf, Counter::Cachegrind] {
            match counter.count(Path::new("/bin/sh"), &["-c", ":"]) {
                Ok(run) if run.instructions > 0 => return Ok((counter, setup(&run.raw))),
                Ok(_) => why.push(format!("{}: counted 0", counter.name())),
                Err(e) => why.push(e),
            }
        }
        Err(format!(
            "nothing here counts instructions: on macOS /usr/bin/time -l, on Linux \
             perf (FUX_PERF, or perf on PATH, with kernel.perf_event_paranoid at 2 or \
             less) or valgrind:\n  {}",
            why.join("\n  ")
        ))
    }

    /// Runs `program` with `args`, counted.
    pub fn count(self, program: &Path, args: &[&str]) -> Result<Run, String> {
        let mut command = match self {
            Counter::Time => {
                let mut c = Command::new("/usr/bin/time");
                c.arg("-l").arg(program);
                c
            }
            Counter::Perf => {
                let mut c = Command::new(wrapper());
                c.arg(program);
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
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        if !out.status.success() {
            return Err(format!(
                "{} {args:?} failed ({}): {}",
                program.display(),
                out.status,
                stderr.trim()
            ));
        }
        let instructions = self.parse(&stderr)?;
        Ok(Run {
            instructions,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            raw: stderr,
        })
    }

    pub(crate) fn parse(self, stderr: &str) -> Result<u64, String> {
        let digits = |text: &str| -> Option<u64> {
            let kept: String = text.chars().filter(char::is_ascii_digit).collect();
            kept.parse().ok()
        };
        let found = match self {
            Counter::Perf => return perf(stderr),
            // "     10558014  instructions retired"
            Counter::Time => stderr.lines().find_map(|line| {
                line.trim()
                    .strip_suffix("instructions retired")
                    .and_then(digits)
            }),
            // "==123== I   refs:      10,558,014"
            Counter::Cachegrind => stderr.lines().find_map(|line| {
                line.split_once("I   refs:")
                    .or_else(|| line.split_once("I refs:"))
                    .and_then(|(_, n)| digits(n))
            }),
        };
        found.ok_or(format!("{}: no count in {:?}", self.name(), stderr.trim()))
    }
}

/// The `fux-perf:` lines of a report but its markers: the CPU set, the
/// PMUs and the CPUs counted on. Empty for the other counters.
fn setup(raw: &str) -> String {
    raw.lines()
        .filter_map(|l| l.strip_prefix("fux-perf: "))
        .filter(|l| *l != "begin" && *l != "end")
        .collect::<Vec<_>>()
        .join("\n")
}

/// The instructions in `perf.sh`'s report: perf's lines between its last
/// `fux-perf: begin` and `fux-perf: end`, from `perf stat -v -x ,
/// --no-scale -e instructions:u`.
///
/// perf prints one CSV line per PMU (`VALUE,,EVENT,RUNNING,SHARE,,`), and
/// for each PMU it opened, in the same order, a verbose line `EVENT: VALUE
/// ENABLED RUNNING`. A PMU that never ran (the other core type's) has
/// RUNNING 0 and counts nothing. Every PMU that ran must have run for all
/// of ENABLED, and its VALUE must be the same in both lines (unscaled);
/// the count is their sum. Anything else is refused: an unsupported event,
/// a share under 100%, lines that don't pair, or no PMU that ran.
fn perf(stderr: &str) -> Result<u64, String> {
    // The event as perf names it: plain, or a PMU's (`apple_avalanche_pmu/
    // instructions/u` in CSV, `cpu_core/instructions/u` on Intel's hybrids,
    // which name their verbose lines so too).
    let instructions =
        |event: &str| event.starts_with("instructions") || event.contains("/instructions/");
    let block = stderr
        .rsplit_once("fux-perf: begin\n")
        .and_then(|(_, rest)| rest.split_once("fux-perf: end"))
        .map(|(block, _)| block)
        .ok_or("perf: no report from bench/perf.sh (its `fux-perf: begin`…`end` block)")?;
    let mut csv = Vec::new();
    let mut verbose = Vec::new();
    for line in block.lines() {
        let fields: Vec<&str> = line.split(',').collect();
        if let [value, _unit, event, _running, _share, ..] = fields.as_slice() {
            if !instructions(event) {
                return Err(format!("perf: an event not asked for: {line:?}"));
            }
            match *value {
                "<not supported>" => {
                    return Err(format!("perf: instructions not supported here: {line:?}"));
                }
                "<not counted>" => csv.push((None, *event)),
                n => csv.push((
                    Some(n.parse::<u64>().map_err(|_| format!("perf: {line:?}"))?),
                    *event,
                )),
            }
        } else if let Some((event, numbers)) = line.split_once(": ")
            && instructions(event)
        {
            let n: Vec<u64> = numbers
                .split(' ')
                .map(str::parse)
                .collect::<Result<_, _>>()
                .map_err(|_| format!("perf: {line:?}"))?;
            let [value, enabled, running] = n.as_slice() else {
                return Err(format!("perf: {line:?}"));
            };
            verbose.push((*value, *enabled, *running));
        } else if !(line.is_empty() || line.starts_with('#')) {
            return Err(format!(
                "perf: a line it doesn't print, malformed: {line:?}"
            ));
        }
    }
    if csv.len() != verbose.len() || csv.is_empty() {
        return Err(format!(
            "perf: {} PMU lines and {} with their times: malformed",
            csv.len(),
            verbose.len()
        ));
    }
    let mut total = 0u64;
    let mut ran = 0usize;
    for ((value, event), (counted, enabled, running)) in csv.into_iter().zip(verbose) {
        if running == 0 {
            if value.unwrap_or(0) != 0 || counted != 0 {
                return Err(format!("perf: {event} counted without running"));
            }
            continue;
        }
        if running != enabled {
            return Err(format!(
                "perf: {event} ran {running} of the {enabled} ns it was enabled ({:.2}%): \
                 multiplexed, or the child moved between core types; not a whole count",
                (running as f64) * 100.0 / (enabled as f64)
            ));
        }
        if value != Some(counted) {
            return Err(format!(
                "perf: {event} counted {value:?} and {counted}: scaled or malformed"
            ));
        }
        total = total.saturating_add(counted);
        ran = ran.saturating_add(1);
    }
    if ran == 0 {
        return Err("perf: no PMU ran".into());
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::Counter;

    #[test]
    fn each_counter_reads_its_tool() {
        let time = "        0  voluntary context switches\n     10558014  instructions retired\n     10045345  cycles elapsed\n";
        assert_eq!(Counter::Time.parse(time), Ok(10_558_014));
        let grind = "==123== Cachegrind, a cache and branch-prediction profiler\n==123== I   refs:      10,558,014\n";
        assert_eq!(Counter::Cachegrind.parse(grind), Ok(10_558_014));
    }

    /// perf's real output, kept in `bench/perf-fixtures/` (`unsupported`
    /// and `malformed` are edited from it; `old-wrapper` is the line the
    /// baseline's wrapper made up, with its fixed 100.00).
    fn fixture(name: &str) -> Result<u64, String> {
        let text = match name {
            "pinned" => include_str!("../perf-fixtures/pinned.txt"),
            "single-pmu" => include_str!("../perf-fixtures/single-pmu.txt"),
            "migrated" => include_str!("../perf-fixtures/migrated.txt"),
            "multiplexed" => include_str!("../perf-fixtures/multiplexed.txt"),
            "unsupported" => include_str!("../perf-fixtures/unsupported.txt"),
            "malformed" => include_str!("../perf-fixtures/malformed.txt"),
            "old-wrapper" => include_str!("../perf-fixtures/old-wrapper.txt"),
            _ => "",
        };
        Counter::Perf.parse(text)
    }

    #[test]
    fn perf_takes_a_whole_count() {
        // The P-cores' PMU ran all the time; the E-cores' never did.
        assert_eq!(fixture("pinned"), Ok(10_040_388));
        assert_eq!(fixture("single-pmu"), Ok(10_558_014));
    }

    #[test]
    fn perf_refuses_a_count_that_is_not_whole() {
        for (name, says) in [
            ("migrated", "75.57%"),
            ("multiplexed", "1.26%"),
            ("unsupported", "not supported"),
            ("malformed", "malformed"),
            ("old-wrapper", "no report"),
        ] {
            let got = fixture(name);
            assert!(
                got.as_ref().is_err_and(|e| e.contains(says)),
                "{name}: {got:?}"
            );
        }
    }

    /// Intel's hybrid cores name each PMU's verbose line as its CSV line
    /// (`cpu_core/instructions/u: …`): read as Apple's are, not refused as
    /// malformed. Written in the shape of the `pinned` fixture, the PMUs
    /// renamed (no hybrid Intel machine is here to record one).
    #[test]
    fn perf_reads_a_hybrid_intel_report() {
        let report = "fux-perf: begin\n\ncpu_atom/instructions/u: 0 1000 0\n\
                      cpu_core/instructions/u: 10040388 1000 1000\n\
                      <not counted>,,cpu_atom/instructions/u,0,0.00,,\n\
                      10040388,,cpu_core/instructions/u,1000,100.00,,\nfux-perf: end\n";
        assert_eq!(Counter::Perf.parse(report), Ok(10_040_388));
    }

    #[test]
    fn the_setup_is_kept_from_the_report() {
        let setup = super::setup(include_str!("../perf-fixtures/pinned.txt"));
        assert!(setup.contains("cpus 4-11"), "{setup}");
        assert!(setup.contains("pmu apple_blizzard_pmu 0-3"), "{setup}");
        assert!(!setup.contains("begin"), "{setup}");
    }
}
