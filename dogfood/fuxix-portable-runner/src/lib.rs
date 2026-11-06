//! A task runner: runs a list of commands one after another, each with a
//! timeout, on pipes or in a PTY, ends a task's whole process tree when it
//! runs out of time (and whatever it leaves running when it ends), and
//! reports how each ended. Everything the OS does goes through [`os`].
pub mod os;

use std::time::{Duration, Instant};

/// One line of a task file: `NAME TIMEOUT MODE ARGV...`, e.g.
/// `build 30s pipe cargo build` or `ui 2.5s pty top -n 1`. Arguments split
/// at blanks; '…' and "…" quote. `#` starts a comment line.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub name: String,
    pub timeout: Duration,
    pub pty: bool,
    pub argv: Vec<String>,
}

/// "30s", "2.5s", "500ms", "2m".
fn duration(text: &str) -> Option<Duration> {
    let (number, unit) = text.split_at(text.find(|c: char| c.is_ascii_alphabetic())?);
    let n: f64 = number.parse().ok().filter(|n: &f64| n.is_finite() && *n >= 0.0)?;
    let seconds = match unit {
        "ms" => n / 1000.0,
        "s" => n,
        "m" => n * 60.0,
        _ => return None,
    };
    Duration::try_from_secs_f64(seconds).ok()
}

/// Splits `line` into words at blanks, with '…' and "…" quoting.
fn words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.get_or_insert_default().push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                word.get_or_insert_default();
            }
            (None, c) if c.is_whitespace() => words.extend(word.take()),
            (None, c) => word.get_or_insert_default().push(c),
        }
    }
    if quote.is_some() {
        return Err("an unclosed quote".into());
    }
    words.extend(word);
    Ok(words)
}

pub fn parse(text: &str) -> Result<Vec<Spec>, String> {
    let mut specs = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |e: &str| format!("line {}: {e}", index + 1);
        let mut parts = words(line).map_err(|e| at(&e))?.into_iter();
        let (Some(name), Some(timeout), Some(mode)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(at("want NAME TIMEOUT MODE COMMAND..."));
        };
        let timeout = duration(&timeout).ok_or_else(|| at(&format!("bad timeout {timeout}")))?;
        let pty = match mode.as_str() {
            "pty" => true,
            "pipe" => false,
            _ => return Err(at(&format!("mode {mode}: not pipe or pty"))),
        };
        let argv: Vec<String> = parts.collect();
        if argv.is_empty() {
            return Err(at("no command"));
        }
        specs.push(Spec {
            name,
            timeout,
            pty,
            argv,
        });
    }
    Ok(specs)
}

/// How a task ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    Ended(os::Status),
    /// Out of time: its tree, this many processes, was ended.
    TimedOut { killed: usize },
    /// It could not be started.
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub name: String,
    pub end: End,
    pub took: Duration,
    pub output: Vec<u8>,
    /// Processes it left running after it ended, which were ended.
    pub leftovers: usize,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.end == End::Ended(os::Status::Exited(0))
    }
}

/// How long a task told to end (TERM) has before it is killed.
pub const GRACE: Duration = Duration::from_millis(500);

/// Runs `spec`, passing its output to `sink` as it comes.
pub fn run(os: &mut os::Os, spec: &Spec, sink: &mut dyn FnMut(&[u8])) -> Report {
    let start = Instant::now();
    let mut output = Vec::new();
    let mut tee = |bytes: &[u8]| {
        output.extend_from_slice(bytes);
        sink(bytes);
    };
    let end = match os.spawn(&spec.argv, spec.pty.then_some((24, 80))) {
        Err(error) => End::Failed(error.to_string()),
        Ok(mut task) => {
            let deadline = start + spec.timeout;
            let mut end = None;
            while end.is_none() {
                let left = deadline.saturating_duration_since(Instant::now());
                match os.wait(&mut task, left, &mut tee) {
                    Ok(Some(status)) => end = Some(End::Ended(status)),
                    Ok(None) if left.is_zero() => {
                        let killed = os.kill_tree(&task, GRACE);
                        // Reap the root, and read what it wrote as it died.
                        let reaped = Instant::now() + Duration::from_secs(2);
                        while Instant::now() < reaped {
                            if let Ok(Some(_)) = os.wait(&mut task, Duration::from_millis(50), &mut tee) {
                                break;
                            }
                        }
                        end = Some(End::TimedOut { killed });
                    }
                    Ok(None) => {}
                    Err(error) => end = Some(End::Failed(error.to_string())),
                }
            }
            let leftovers = os.leftovers(&task);
            if leftovers > 0 {
                os.kill_tree(&task, GRACE);
            }
            return Report {
                name: spec.name.clone(),
                end: end.unwrap_or(End::Failed("no end".into())),
                took: start.elapsed(),
                output,
                leftovers,
            };
        }
    };
    Report {
        name: spec.name.clone(),
        end,
        took: start.elapsed(),
        output,
        leftovers: 0,
    }
}

/// A report's line in the summary.
pub fn summary(report: &Report) -> String {
    let end = match &report.end {
        End::Ended(os::Status::Exited(code)) => format!("exit {code}"),
        End::Ended(os::Status::Killed(signal)) => format!("killed by {signal}"),
        End::TimedOut { killed } => format!("timed out, {killed} processes ended"),
        End::Failed(error) => format!("failed to run: {error}"),
    };
    let left = match report.leftovers {
        0 => String::new(),
        n => format!(", left {n} running (ended)"),
    };
    format!(
        "{:<4} {:<12} {:>8.2}s {:>7}B  {end}{left}",
        if report.ok() { "ok" } else { "FAIL" },
        report.name,
        report.took.as_secs_f64(),
        report.output.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_file_is_read() -> Result<(), String> {
        let specs = parse("# c\nbuild 1.5s pipe sh -c 'echo \"a b\"'\nui 200ms pty top\n")?;
        assert_eq!(
            specs.first(),
            Some(&Spec {
                name: "build".into(),
                timeout: Duration::from_millis(1500),
                pty: false,
                argv: vec!["sh".into(), "-c".into(), "echo \"a b\"".into()],
            })
        );
        assert_eq!(specs.get(1).map(|s| (s.pty, s.timeout)), Some((true, Duration::from_millis(200))));
        assert!(parse("x 1s tty ls").is_err());
        assert!(parse("x 1s pipe 'ls").is_err());
        Ok(())
    }

    /// A task that times out has its whole tree ended; statuses and output
    /// come back on a console.
    #[test]
    fn a_timed_out_tree_is_ended_and_statuses_are_reported() -> Result<(), String> {
        let mut os = os::Os::new().map_err(|e| e.to_string())?;
        let spec = |name: &str, timeout: u64, pty: bool, argv: &[&str]| Spec {
            name: name.into(),
            timeout: Duration::from_millis(timeout),
            pty,
            argv: argv.iter().map(|a| (*a).to_owned()).collect(),
        };
        let script = "start /b ping -n 3141 127.0.0.1 > NUL & echo started& ping -n 3143 127.0.0.1 > NUL";
        let hung = run(&mut os, &spec("hung", 1500, false, &["cmd.exe", "/c", script]), &mut |_| {});
        assert!(matches!(hung.end, End::TimedOut { killed } if killed >= 2), "{hung:?}");
        assert!(String::from_utf8_lossy(&hung.output).contains("started"));
        let failed = run(&mut os, &spec("exit3", 5000, false, &["cmd.exe", "/c", "echo out& exit 3"]), &mut |_| {});
        assert_eq!(failed.end, End::Ended(os::Status::Exited(3)));
        assert!(String::from_utf8_lossy(&failed.output).contains("out"));
        let missing = run(&mut os, &spec("none", 5000, false, &["C:\\no\\such\\program.exe"]), &mut |_| {});
        assert!(matches!(missing.end, End::Failed(_)));
        Ok(())
    }
}
