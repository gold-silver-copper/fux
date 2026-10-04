//! `footprint`: the memory each engine in this process holds, with the
//! same content in every engine.
//!
//! Each measure runs in a child process of its own (this binary again,
//! [`CHILD`]), so that no engine's allocations, freed or kept, mix with
//! another's. The child makes the content's bytes first, then reads its
//! memory (`memory::read`), makes the engine and feeds it, 4 KiB at a time
//! as `bench` does, and reads its memory again, with the bytes still held:
//! the difference is the engine's. Two counts:
//!
//! - the footprint: the dirty memory of the process, resident or
//!   compressed (macOS `phys_footprint`; Linux `RssAnon`), which sees
//!   every allocator, and memory a process maps itself (Ghostty's pages);
//! - malloc: the bytes malloc has handed out and not taken back (macOS
//!   only), exact, but blind to memory mapped directly.
//!
//! The measures, at 80 and 200 columns, 50 rows:
//!
//! - `empty`: a new screen;
//! - `dense` and `medium`: a screen full of SGR-styled text, `bench`'s
//!   `dense-cells` (every cell its own 256-colour foreground and
//!   background) and `medium-cells` (an SGR change every few cells), made
//!   for the screen's size;
//! - `history:NAME`: 10,000 rows of history from a workload: `bench`'s
//!   synthetic ones from their generators, and the corpus, every recording
//!   in turn at this size. The workload is cut into pieces; a piece of one
//!   that does not scroll the screen into history by itself (all but
//!   `ascii`, `scrolling` and `unicode`) is followed by a scroll-out:
//!   back to the main screen, margins and SGR reset, and a newline for
//!   every row from the bottom one, so its screen goes into history. Every
//!   engine gets the same pieces: as many as fux-vt, keeping 10,000 rows,
//!   needs to fill its history (found here, before the children run).
//!
//! The history limits ([`HISTORY`] rows): fux-vt, alacritty, libvterm (the
//! shim's history, which keeps each pushed row's cells), avt, wezterm and
//! vt100 are each made to keep 10,000 rows, so each holds the same
//! 10,000 rows. Ghostty's limit is in bytes, and prunes whole pages: it is
//! given [`GHOSTTY_SCROLLBACK`], more than any measure here fills, so it
//! keeps every row it is fed, a few more than 10,000 where the last piece
//! pushes several. Each engine's figure per row is divided by the rows it
//! says it holds.
use crate::bench::{self, CHUNK, WORKLOADS};
use crate::engine::{ENGINES, Engine, SUBJECT, Setup};
use crate::engines;
use crate::memory::{self, Reading};
use crate::rng::Rng;
use std::fmt::Write as _;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// The hidden subcommand a child runs: `CHILD ENGINE MEASURE COLS PIECES`.
pub const CHILD: &str = "--footprint-child";

/// The rows of history every engine with a row limit keeps.
pub const HISTORY: usize = 10_000;
/// Ghostty's scrollback limit, in bytes: more than 10,000 rows of any
/// content here take (dense cells at 200 columns take under 40 MiB).
const GHOSTTY_SCROLLBACK: usize = 1 << 30;
/// The screen's rows.
const ROWS: u16 = 50;
/// The screen widths measured.
const WIDTHS: [u16; 2] = [80, 200];
/// A synthetic workload's piece: as much as `bench` feeds at once.
const PIECE: usize = CHUNK;
/// The workloads whose output scrolls the screen into history by itself.
const SCROLLS: [&str; 3] = ["ascii", "scrolling", "unicode"];
/// The most a history measure feeds before giving up on filling it.
const MOST: usize = 256 << 20;

/// The engines measured: fux-vt, and each in-process engine named.
const IN_PROCESS: [&str; 7] = [
    "fux-vt",
    "ghostty",
    "alacritty",
    "libvterm",
    "avt",
    "wezterm",
    "vt100",
];

/// How each engine is configured here, for the report.
const CONFIGURED: [(&str, &str); 7] = [
    (
        "fux-vt",
        "10,000 rows of history, set up as fux sets up a pane",
    ),
    (
        "ghostty",
        "max_scrollback 1 GiB (its limit is in bytes): keeps every row fed",
    ),
    ("alacritty", "scrolling_history 10,000 rows"),
    (
        "libvterm",
        "the shim keeps 10,000 pushed rows (libvterm keeps none itself)",
    ),
    ("avt", "scrollback_limit 10,000 rows"),
    ("wezterm", "scrollback_size 10,000 rows"),
    ("vt100", "scrollback_len 10,000 rows"),
];

/// Makes the named engine with its history limited as above.
fn make(name: &str, setup: &Setup) -> Result<Box<dyn Engine>, String> {
    let limited = Setup {
        history: HISTORY,
        ..*setup
    };
    match name {
        "fux-vt" => (SUBJECT.make)(&limited),
        "ghostty" => engines::ghostty::with_scrollback(setup, GHOSTTY_SCROLLBACK),
        "alacritty" => engines::alacritty::with_history(setup, HISTORY),
        "libvterm" => engines::libvterm::with_history(setup, HISTORY),
        "avt" => engines::avt::with_history(setup, HISTORY),
        "wezterm" => engines::wezterm::with_history(setup, HISTORY),
        "vt100" => engines::vt100::with_history(setup, HISTORY),
        other => Err(format!("footprint: no in-process engine {other:?}")),
    }
}

/// What fills the screen or its history.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Measure {
    Empty,
    Dense,
    Medium,
    /// History from the named workload (a synthetic one, or `corpus`).
    History(String),
}

impl Measure {
    fn parse(text: &str) -> Result<Measure, String> {
        match text {
            "empty" => Ok(Measure::Empty),
            "dense" => Ok(Measure::Dense),
            "medium" => Ok(Measure::Medium),
            other => other
                .strip_prefix("history:")
                .filter(|name| *name == "corpus" || WORKLOADS.iter().any(|(n, _, _)| n == name))
                .map(|name| Measure::History(name.to_owned()))
                .ok_or(format!("footprint: no measure {other:?}")),
        }
    }

    fn name(&self) -> String {
        match self {
            Measure::Empty => "empty".into(),
            Measure::Dense => "dense".into(),
            Measure::Medium => "medium".into(),
            Measure::History(name) => format!("history:{name}"),
        }
    }
}

/// The scroll-out: back to the main screen, no margins or origin mode,
/// the empty rendition, and a newline for each row from the bottom one.
fn scroll_out(rows: u16) -> Vec<u8> {
    let mut out = format!("\x1b[?1049l\x1b[?6l\x1b[r\x1b[m\x1b[{rows}H").into_bytes();
    out.extend(std::iter::repeat_n(b'\n', usize::from(rows)));
    out
}

/// A workload's pieces, one after another, the same every time.
struct Pieces {
    rng: Rng,
    make: Option<fn(&mut Rng, usize) -> Vec<u8>>,
    scrolls: bool,
    recordings: Vec<Vec<u8>>,
    next: usize,
    tail: Vec<u8>,
}

impl Pieces {
    fn new(name: &str, rows: u16) -> Result<Pieces, String> {
        let tail = scroll_out(rows);
        if name == "corpus" {
            let recordings: Vec<Vec<u8>> = crate::corpus::recordings(&[])?
                .iter()
                .map(crate::corpus::Recording::bytes)
                .collect();
            if recordings.is_empty() {
                return Err("footprint: the corpus is empty".into());
            }
            return Ok(Pieces {
                rng: Rng::new(1),
                make: None,
                scrolls: false,
                recordings,
                next: 0,
                tail,
            });
        }
        let make = WORKLOADS
            .iter()
            .find(|(n, _, _)| *n == name)
            .map(|(_, _, make)| *make)
            .ok_or(format!("footprint: no workload {name:?}"))?;
        Ok(Pieces {
            rng: Rng::new(1),
            make: Some(make),
            scrolls: SCROLLS.contains(&name),
            recordings: Vec::new(),
            next: 0,
            tail,
        })
    }

    /// The next piece: a synthetic workload's next [`PIECE`] bytes (or a
    /// little more: its generator ends where a line or screen does), or
    /// the corpus's next recording; with the scroll-out after it where the
    /// workload does not scroll by itself.
    fn next(&mut self) -> Vec<u8> {
        let mut piece = match self.make {
            Some(make) => make(&mut self.rng, PIECE),
            None => {
                let count = self.recordings.len().max(1);
                let i = self.next.checked_rem(count).unwrap_or(0);
                self.next = self.next.wrapping_add(1);
                self.recordings.get(i).cloned().unwrap_or_default()
            }
        };
        if !self.scrolls {
            piece.extend_from_slice(&self.tail);
        }
        piece
    }
}

/// How many pieces of `name` fill fux-vt's history at `cols` columns.
fn pieces_to_fill(name: &str, cols: u16) -> Result<usize, String> {
    let setup = Setup {
        rows: ROWS,
        cols,
        history: HISTORY,
        reflow: true,
    };
    let mut vt = make("fux-vt", &setup)?;
    let mut pieces = Pieces::new(name, ROWS)?;
    let (mut count, mut fed) = (0usize, 0usize);
    while vt.history_len().unwrap_or(0) < HISTORY {
        if fed > MOST {
            return Err(format!(
                "footprint: {name} at {cols} columns fills no history in {} MiB",
                MOST >> 20
            ));
        }
        let piece = pieces.next();
        vt.feed(&piece, CHUNK)?;
        fed = fed.saturating_add(piece.len());
        count = count.saturating_add(1);
    }
    Ok(count)
}

/// What a child reports.
#[derive(Clone, Debug, Default)]
struct Measured {
    before: Reading,
    after: Reading,
    history: Option<usize>,
    bytes: usize,
    seconds: f64,
}

fn grown(after: u64, before: u64) -> i64 {
    i64::try_from(after)
        .unwrap_or(i64::MAX)
        .saturating_sub(i64::try_from(before).unwrap_or(i64::MAX))
}

impl Measured {
    fn footprint(&self) -> i64 {
        grown(self.after.footprint, self.before.footprint)
    }
    fn resident(&self) -> i64 {
        grown(self.after.resident, self.before.resident)
    }
    fn malloc(&self) -> Option<i64> {
        Some(grown(self.after.malloc?, self.before.malloc?))
    }
}

/// The child: `ENGINE MEASURE COLS PIECES`. Prints what it measured as
/// one line of JSON.
pub fn child(args: &[String]) -> Result<(), String> {
    let [engine, measure, cols, pieces] = args else {
        return Err(format!("{CHILD} ENGINE MEASURE COLS PIECES"));
    };
    let measure = Measure::parse(measure)?;
    let cols: u16 = cols.parse().map_err(|e| format!("columns: {e}"))?;
    let count: usize = pieces.parse().map_err(|e| format!("pieces: {e}"))?;
    let setup = Setup {
        rows: ROWS,
        cols,
        history: HISTORY,
        reflow: true,
    };
    // The bytes, made before the first reading and held past the second.
    let mut rng = Rng::new(1);
    let bytes: Vec<Vec<u8>> = match &measure {
        Measure::Empty => Vec::new(),
        Measure::Dense => {
            let mut out = Vec::new();
            bench::dense_screen(&mut rng, &mut out, ROWS, cols);
            vec![out]
        }
        Measure::Medium => {
            let mut out = Vec::new();
            bench::medium_screen(&mut rng, &mut out, ROWS, cols);
            vec![out]
        }
        Measure::History(name) => {
            let mut source = Pieces::new(name, ROWS)?;
            (0..count).map(|_| source.next()).collect()
        }
    };
    let before = memory::read()?;
    let started = Instant::now();
    let mut vt = make(engine, &setup)?;
    for piece in &bytes {
        vt.feed(piece, CHUNK)?;
    }
    let seconds = started.elapsed().as_secs_f64();
    let after = memory::read()?;
    let history = vt.history_len();
    let json = serde_json::json!({
        "before": reading_json(&before),
        "after": reading_json(&after),
        "history": history,
        "bytes": bytes.iter().map(Vec::len).sum::<usize>(),
        "seconds": seconds,
    });
    println!("{json}");
    drop(vt);
    Ok(())
}

fn reading_json(r: &Reading) -> serde_json::Value {
    serde_json::json!({
        "footprint": r.footprint,
        "resident": r.resident,
        "malloc": r.malloc,
    })
}

fn reading_of(v: Option<&serde_json::Value>) -> Result<Reading, String> {
    let field = |name: &str| {
        v.and_then(|v| v.get(name))
            .and_then(serde_json::Value::as_u64)
    };
    Ok(Reading {
        footprint: field("footprint").ok_or("footprint child: no footprint")?,
        resident: field("resident").ok_or("footprint child: no resident size")?,
        malloc: field("malloc"),
    })
}

/// One child to run.
#[derive(Clone, Debug)]
struct Job {
    engine: &'static str,
    measure: Measure,
    cols: u16,
    pieces: usize,
}

fn run_child(job: &Job) -> Result<Measured, String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = Command::new(me)
        .arg(CHILD)
        .arg(job.engine)
        .arg(job.measure.name())
        .arg(job.cols.to_string())
        .arg(job.pieces.to_string())
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("footprint child: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{} {} at {}: {} {}",
            job.engine,
            job.measure.name(),
            job.cols,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(text.trim())
        .map_err(|e| format!("footprint child said {text:?}: {e}"))?;
    let count = |name: &str| {
        v.get(name)
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
    };
    Ok(Measured {
        before: reading_of(v.get("before"))?,
        after: reading_of(v.get("after"))?,
        history: count("history"),
        bytes: count("bytes").unwrap_or(0),
        seconds: v
            .get("seconds")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
    })
}

/// Runs every job on `jobs` threads; each job's result, in order.
fn run_all(all: &[Job], jobs: usize) -> Vec<Result<Measured, String>> {
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(job) = all.get(i) else {
                        break;
                    };
                    let result = run_child(job);
                    results
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((i, result));
                }
            });
        }
    });
    let mut results = results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    results.sort_by_key(|(i, _)| *i);
    results.into_iter().map(|(_, r)| r).collect()
}

/// `a / b`, rounded, for a table.
fn per(a: i64, b: usize) -> String {
    let b = u32::try_from(b).unwrap_or(u32::MAX);
    if b == 0 {
        return "-".into();
    }
    format!("{:.0}", (a as f64) / f64::from(b))
}

fn mib(n: i64) -> String {
    format!("{:.1}", (n as f64) / f64::from(1u32 << 20))
}

/// Measures fux-vt and the in-process engines among `engines` (indices
/// into `ENGINES`); prints the tables, and writes them as JSON to `json`
/// if given.
pub fn run(engines: &[usize], json: Option<&str>, jobs: usize) -> Result<bool, String> {
    let started = Instant::now();
    let names: Vec<&'static str> = std::iter::once(SUBJECT.name)
        .chain(
            engines
                .iter()
                .filter_map(|&i| ENGINES.get(i))
                .filter(|k| k.in_process)
                .map(|k| k.name),
        )
        .filter(|n| IN_PROCESS.contains(n))
        .collect();
    let sources: Vec<String> = WORKLOADS
        .iter()
        .map(|(n, _, _)| (*n).to_owned())
        .chain(std::iter::once("corpus".to_owned()))
        .collect();
    let screens = [Measure::Empty, Measure::Dense, Measure::Medium];
    // How many pieces fill fux-vt's history, per workload and width.
    let mut fill = Vec::new();
    for source in &sources {
        for cols in WIDTHS {
            fill.push((source.clone(), cols, pieces_to_fill(source, cols)?));
        }
    }
    let mut all = Vec::new();
    for &engine in &names {
        for cols in WIDTHS {
            for measure in &screens {
                all.push(Job {
                    engine,
                    measure: measure.clone(),
                    cols,
                    pieces: 0,
                });
            }
        }
        for (source, cols, pieces) in &fill {
            all.push(Job {
                engine,
                measure: Measure::History(source.clone()),
                cols: *cols,
                pieces: *pieces,
            });
        }
    }
    eprintln!(
        "footprint: {} children for {} engines, {jobs} at a time",
        all.len(),
        names.len()
    );
    let results = run_all(&all, jobs);
    let mut failed = false;
    let found = |engine: &str, measure: &Measure, cols: u16| -> Option<&Measured> {
        all.iter()
            .zip(&results)
            .find(|(j, _)| j.engine == engine && j.measure == *measure && j.cols == cols)
            .and_then(|(_, r)| r.as_ref().ok())
    };
    for (job, result) in all.iter().zip(&results) {
        if let Err(e) = result {
            failed = true;
            eprintln!(
                "footprint: {} {} at {}: {e}",
                job.engine,
                job.measure.name(),
                job.cols
            );
        }
    }

    let mut text = String::new();
    let _ = writeln!(
        text,
        "Memory each engine holds, measured in a child process of its own: the footprint \
         ({}) grown by making the engine and feeding it; malloc: bytes in use grown.\n",
        if cfg!(target_os = "macos") {
            "task_info phys_footprint"
        } else {
            "RssAnon"
        }
    );
    let header = |text: &mut String, title: &str, columns: &[String]| {
        let _ = writeln!(text, "{title}");
        let _ = write!(text, "{:<10}", "");
        for c in columns {
            let _ = write!(text, " {c:>10}");
        }
        let _ = writeln!(text);
    };
    let screen_columns: Vec<String> = WIDTHS
        .iter()
        .flat_map(|cols| screens.iter().map(move |m| format!("{} {cols}", m.name())))
        .collect();
    for (title, count, by_row) in [
        (
            format!("A screen of {ROWS} rows: footprint, bytes per cell"),
            Count::Footprint,
            false,
        ),
        (
            format!("A screen of {ROWS} rows: footprint, bytes per row"),
            Count::Footprint,
            true,
        ),
        (
            format!("A screen of {ROWS} rows: malloc, bytes per cell"),
            Count::Malloc,
            false,
        ),
    ] {
        header(&mut text, &title, &screen_columns);
        for &engine in &names {
            let _ = write!(text, "{engine:<10}");
            for cols in WIDTHS {
                let units = if by_row {
                    usize::from(ROWS)
                } else {
                    usize::from(ROWS).saturating_mul(usize::from(cols))
                };
                for m in &screens {
                    let cell = found(engine, m, cols)
                        .and_then(|r| count.of(r))
                        .map_or("-".into(), |n| per(n, units));
                    let _ = write!(text, " {cell:>10}");
                }
            }
            let _ = writeln!(text);
        }
        let _ = writeln!(text);
    }
    let short = |s: &str| -> String {
        match s {
            "dense-cells" => "dense".into(),
            "medium-cells" => "medium".into(),
            "cursor-motion" => "cursor".into(),
            "scroll-region" => "region".into(),
            other => other.into(),
        }
    };
    let source_columns: Vec<String> = sources.iter().map(|s| short(s)).collect();
    for cols in WIDTHS {
        for (title, count) in [
            (
                format!(
                    "{HISTORY} rows of history at {cols} columns: footprint beyond an empty \
                     screen's, bytes per history row held"
                ),
                Count::Footprint,
            ),
            (
                format!(
                    "{HISTORY} rows of history at {cols} columns: malloc beyond an empty \
                     screen's, bytes per history row held"
                ),
                Count::Malloc,
            ),
            (
                format!("{HISTORY} rows of history at {cols} columns: footprint in all, MiB"),
                Count::Total,
            ),
        ] {
            header(&mut text, &title, &source_columns);
            for &engine in &names {
                let _ = write!(text, "{engine:<10}");
                let empty = found(engine, &Measure::Empty, cols);
                for source in &sources {
                    let m = Measure::History(source.clone());
                    let cell = found(engine, &m, cols).map_or("-".into(), |r| {
                        let held = r.history.unwrap_or(0);
                        match count {
                            Count::Total => mib(r.footprint()),
                            Count::Footprint | Count::Malloc => {
                                match (count.of(r), empty.and_then(|e| count.of(e))) {
                                    (Some(all), Some(screen)) => {
                                        per(all.saturating_sub(screen), held)
                                    }
                                    _ => "-".into(),
                                }
                            }
                        }
                    });
                    let _ = write!(text, " {cell:>10}");
                }
                let _ = writeln!(text);
            }
            let _ = writeln!(text);
        }
    }
    // Rows held where they are not the 10,000 asked for.
    let mut odd = Vec::new();
    for &engine in &names {
        for (source, cols, _) in &fill {
            let m = Measure::History(source.clone());
            if let Some(r) = found(engine, &m, *cols)
                && r.history != Some(HISTORY)
            {
                odd.push(format!(
                    "{engine} {} at {cols}: {}",
                    short(source),
                    r.history.map_or("?".into(), |n| n.to_string())
                ));
            }
        }
    }
    let _ = writeln!(
        text,
        "rows of history held, where not {HISTORY}: {}",
        if odd.is_empty() {
            "none".to_owned()
        } else {
            odd.join("; ")
        }
    );
    let _ = writeln!(text, "\nhow each engine is configured:");
    for (engine, how) in CONFIGURED {
        if names.contains(&engine) {
            let _ = writeln!(text, "  {engine:<10} {how}");
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    let _ = writeln!(
        text,
        "\n{} children, {jobs} at a time, in {seconds:.0} s",
        all.len()
    );
    print!("{text}");

    if let Some(path) = json {
        let measures: Vec<serde_json::Value> = all
            .iter()
            .zip(&results)
            .filter_map(|(job, result)| {
                let r = result.as_ref().ok()?;
                let cells = usize::from(ROWS).saturating_mul(usize::from(job.cols));
                // History beyond an empty screen's, per history row held.
                let empty = found(job.engine, &Measure::Empty, job.cols);
                let beyond = |count: Count| -> Option<f64> {
                    let held = r.history.filter(|n| *n > 0)?;
                    let grown = count.of(r)?.saturating_sub(count.of(empty?)?);
                    Some((grown as f64) / (held as f64))
                };
                let per_history_row = matches!(job.measure, Measure::History(_)).then(|| {
                    serde_json::json!({
                        "footprint": beyond(Count::Footprint),
                        "malloc": beyond(Count::Malloc),
                    })
                });
                Some(serde_json::json!({
                    "engine": job.engine,
                    "measure": job.measure.name(),
                    "rows": ROWS,
                    "cols": job.cols,
                    "pieces": job.pieces,
                    "bytes_fed": r.bytes,
                    "seconds": r.seconds,
                    "history_rows": r.history,
                    "footprint": r.footprint(),
                    "resident": r.resident(),
                    "malloc": r.malloc(),
                    "footprint_per_cell": (r.footprint() as f64) / (cells.max(1) as f64),
                    "footprint_per_row": (r.footprint() as f64) / f64::from(ROWS),
                    "per_history_row": per_history_row,
                    "before": reading_json(&r.before),
                    "after": reading_json(&r.after),
                }))
            })
            .collect();
        let configured: serde_json::Map<String, serde_json::Value> = CONFIGURED
            .iter()
            .filter(|(engine, _)| names.contains(engine))
            .map(|(engine, how)| ((*engine).to_owned(), serde_json::Value::from(*how)))
            .collect();
        let doc = serde_json::json!({
            "kind": "fux-vt-compare footprint",
            "history_rows": HISTORY,
            "screen_rows": ROWS,
            "widths": WIDTHS,
            "counter": if cfg!(target_os = "macos") {
                "task_info phys_footprint; malloc_zone_statistics size_in_use"
            } else {
                "/proc/self/status RssAnon"
            },
            "engines": names,
            "configured": configured,
            "seconds": seconds,
            "measures": measures,
        });
        let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))?;
        eprintln!("wrote {path}");
    }
    Ok(!failed)
}

/// Which count a table shows.
#[derive(Clone, Copy)]
enum Count {
    Footprint,
    Malloc,
    Total,
}

impl Count {
    fn of(self, r: &Measured) -> Option<i64> {
        match self {
            Count::Footprint | Count::Total => Some(r.footprint()),
            Count::Malloc => r.malloc(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Measure, Pieces, ROWS};

    #[test]
    fn measures_are_named_as_they_are_read() -> Result<(), String> {
        for m in [
            Measure::Empty,
            Measure::Dense,
            Measure::Medium,
            Measure::History("ascii".into()),
            Measure::History("corpus".into()),
        ] {
            assert_eq!(Measure::parse(&m.name())?, m);
        }
        assert!(Measure::parse("history:nothing").is_err());
        Ok(())
    }

    /// The pieces are the same every time; a workload that does not
    /// scroll by itself has its screen scrolled out after each.
    #[test]
    fn pieces_are_reproducible_and_scroll_out() -> Result<(), String> {
        let take = |name: &str| -> Result<Vec<Vec<u8>>, String> {
            let mut p = Pieces::new(name, ROWS)?;
            Ok((0..3).map(|_| p.next()).collect())
        };
        assert_eq!(take("dense-cells")?, take("dense-cells")?);
        let tail = super::scroll_out(ROWS);
        assert!(take("dense-cells")?.iter().all(|p| p.ends_with(&tail)));
        assert!(!take("ascii")?.iter().any(|p| p.ends_with(&tail)));
        Ok(())
    }
}
