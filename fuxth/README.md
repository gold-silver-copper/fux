# fuxth

A small Forth for the shell, written for [fux](https://github.com/gold-silver-copper/fux)'s
command line and embeddable anywhere.

```text
fux 3 4 add                         # 7
fux str srv pane str 'make test' line
```

- **Words, not symbols.** `add sub mul div mod eq lt gt not dup drop swap over
  rot`: no word uses a character a shell treats specially, so a program can be
  a command's arguments, each argument one word, as well as a file.
- **A fixed, small language.** Numbers push themselves; `str` pushes the next
  word as a string; `define NAME … end` and `constant NAME` name things;
  `if … else … then` and `begin … until` are the control flow. Every other word
  takes and gives stack values, and never reads ahead.
- **A prelude in fuxth** ([`PRELUDE`]): `true false nip tuck -rot 2dup 2drop
  negate abs min max ne le ge`, for an application to compile first.
- **Typed values:** integers (checked: no overflow, no division by zero;
  division truncates toward zero), booleans (never integers), strings, and the
  application's own.
- **Compiled whole, and checked, before it runs.** An unknown word or an
  unclosed block is an error before anything happens; so is a step every run
  reaching it would fail: a word given fewer values than the stack surely
  holds, or a value of a kind it never takes. The checker follows the stack's
  depth as a range and its values' kinds as sets, through branches, loops,
  definitions and recursion, and through application words that give a value
  only sometimes; what it can't know is never an error.
- **Recursion in last place loops.** A call that is a definition's last step
  replaces its caller's frame, so a loop written as recursion never runs out of
  calls.
- **Pauses for its host.** An application word can suspend the run while the
  application does something (asks a server); the run goes on once it has the
  answer. All of a run's state is data: no threads, no blocking, no Rust
  recursion, and dropping it stops it. A budget of steps lets the host check for
  Ctrl-C in a loop that never pauses.
- **Errors** carry a stable code, a message, and the file, line and word.

Files are split as a shell splits a line: quotes, backslashes, and `#`
comments at a word's start.

## Embedding

```rust
use fuxth::{Compiler, Done, Failure, Host, Limits, Machine, Outcome, Value, Vocabulary, token};

let mut words = Vocabulary::default();
let ask = words.add("ask", 1, 1..=1);   // takes one value, gives one
let mut compiler = Compiler::new(words);
compiler.add(&token::source("prelude.fux", fuxth::PRELUDE)?)?;
compiler.add(&token::source("script.fux", "3 ask")?)?;
let mut machine = Machine::new(compiler.finish()?, Limits::default());
loop {
    match machine.run(&mut my_host, 10_000)? {
        Outcome::Finished => break,
        Outcome::Suspended(request) => { /* do it, then */ machine.push(answer)?; }
        Outcome::Paused => { /* check for Ctrl-C */ }
    }
}
```

The host implements [`Host`]: `call(word, args, out)` gets the word's values
(an iterator, the deepest first) and pushes its results, and answers
`Done::Now` or `Done::Later(request)`; a suspended word's results are what the
host pushes before running the machine again. A word gives as many values as
its vocabulary says (a range, for one that gives a value only sometimes): the
checker counts on it, and the machine fails a run with `contract` if a host
gives otherwise.

## Tested

- `tests/examples/`: programs (FizzBuzz, gcd, primes, Collatz, Ackermann …),
  each with what it leaves or the error it ends with.
- `tests/standard.rs`: cases from the Forth 2012 core tests and Exercism's Forth
  exercise, in fuxth's words, with where fuxth parts from them.
- `tests/differential.rs`: random programs run by fuxth and by a second, plain
  interpreter, which must agree; paused at every step or not, which must not
  matter; and rewritten in ways that mean the same (`dup drop`, inlining).
- `tests/gforth.rs`: random integer programs against gforth, when there is one
  (`FUXTH_GFORTH`).
- Fuzz targets in fux's `fuzz/`: `fuxth_tokens`, `fuxth_run`, `fuxth_pause`.

## License

MIT
