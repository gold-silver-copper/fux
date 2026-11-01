//! How fast fuxth reads its library and runs: a few programs, each timed
//! over several runs.
//! `cargo run --release -p fuxth --example bench`
use fuxth::{AppValue, Done, Failure, Host, Machine, Outcome, STDLIB, Value, Vocabulary};
use std::time::Instant;

#[derive(Clone, Debug, PartialEq)]
struct Nothing;

impl AppValue for Nothing {
    fn kind(&self) -> &'static str {
        "nothing"
    }
}

/// One word, `beat` (-- 1), answered at once.
struct Ticks;

impl Host for Ticks {
    type Value = Nothing;
    type Request = ();
    fn call(
        &mut self,
        _word: usize,
        _args: fuxth::Args<'_, Nothing>,
        out: &mut Vec<Value<Nothing>>,
    ) -> Result<Done<()>, Failure> {
        out.push(Value::Int(1));
        Ok(Done::Now)
    }

    fn write(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}

const PROGRAMS: [(&str, &str); 8] = [
    ("count to a million", "0 begin 1 add dup 1000000 eq until"),
    (
        "recursive fib 25",
        "define fib dup 2 lt if else dup 1 sub recurse swap 2 sub recurse add then end 25 fib",
    ),
    (
        "9000 calls deep, 200 times",
        "define deep dup 0 eq if else 1 sub recurse 1 add then end
         0 begin 9000 deep drop 1 add dup 200 eq until",
    ),
    (
        "a million host words",
        "0 begin beat add dup 1000000 eq until",
    ),
    (
        "sort 20000 numbers",
        "list 0 20000 range each 7919 mul 10007 mod end end sort len",
    ),
    (
        "words of a 100k text",
        "str 'the quick brown fox ' 5000 copies fields len",
    ),
    (
        "upper and reverse 100k",
        "str abcdefghij 10000 copies upper reverse len",
    ),
    ("sum a list of 100k", "list 0 100000 range unpack end sum"),
];

/// Each run's own times, a line each (`program<TAB>run<TAB>library µs<TAB>run µs`),
/// so a comparison takes its own statistic; `FUXTH_BENCH_RUNS` runs (5).
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runs: u32 = (std::env::var("FUXTH_BENCH_RUNS").ok())
        .and_then(|n| n.parse().ok())
        .unwrap_or(5);
    for (name, text) in PROGRAMS {
        for run in 0..runs {
            let compiling = Instant::now();
            let mut words = Vocabulary::default();
            words.add("beat", "-- integer")?;
            let mut machine = Machine::new(words);
            machine.library("stdlib.fux", STDLIB, &mut Ticks)?;
            machine.read(fuxth::token::source("bench", text)?, None);
            let compiled = compiling.elapsed();
            let started = Instant::now();
            while machine.run(&mut Ticks, u64::MAX)? != Outcome::Finished {}
            let ran = started.elapsed();
            std::io::Write::write_fmt(
                &mut std::io::stdout(),
                format_args!(
                    "{name}\t{run}\t{}\t{}\n",
                    compiled.as_micros(),
                    ran.as_micros()
                ),
            )?;
        }
    }
    Ok(())
}
