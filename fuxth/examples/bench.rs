//! How fast fuxth runs: a few programs, each timed over several runs.
//! `cargo run --release -p fuxth --example bench`
use fuxth::{AppValue, Compiler, Done, Failure, Host, Limits, Machine, Outcome, Value, Vocabulary};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
struct Nothing;

impl AppValue for Nothing {
    fn kind(&self) -> &'static str {
        "nothing"
    }
}

/// One word, `tick` (-- 1), answered at once.
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

    fn print(&mut self, _line: &str) -> Result<(), Failure> {
        Ok(())
    }
}

const PROGRAMS: [(&str, &str); 4] = [
    ("count to a million", "0 begin 1 add dup 1000000 eq until"),
    (
        "recursive fib 25",
        "define fib dup 2 lt if else dup 1 sub fib swap 2 sub fib add then end 25 fib",
    ),
    (
        "9000 calls deep, 200 times",
        "define deep dup 0 eq if else 1 sub deep 1 add then end
         0 begin 9000 deep drop 1 add dup 200 eq until",
    ),
    (
        "a million host words",
        "0 begin tick add dup 1000000 eq until",
    ),
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for (name, text) in PROGRAMS {
        let mut best = Duration::MAX;
        for _ in 0..5 {
            let mut words = Vocabulary::default();
            words.add("tick", 0, 1..=1);
            let mut compiler = Compiler::new(words);
            compiler.add(&fuxth::token::source("bench", text)?)?;
            let mut machine = Machine::new(compiler.finish()?, Limits::default());
            let started = Instant::now();
            while machine.run(&mut Ticks, u64::MAX)? != Outcome::Finished {}
            best = best.min(started.elapsed());
        }
        let ms = best.as_secs_f64() * 1000.0;
        std::io::Write::write_fmt(
            &mut std::io::stdout(),
            format_args!("{name:28} {ms:8.1} ms\n"),
        )?;
    }
    Ok(())
}
