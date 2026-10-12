# fuxth

A small Forth for the shell, written for [fux](https://github.com/gold-silver-copper/fux)'s
command line and embeddable anywhere.

```text
fux 3 4 add                         # 7
fux str srv pane str 'make test' line
```

- **Words, not symbols.** `add sub mul div mod eq lt gt not dup drop swap over`:
  no word uses a character a shell treats specially, so a program can be a
  command's arguments, each argument one word, as well as a file.
- **A fixed, small language.** Numbers push themselves; `str` pushes the next
  word as a string; `define NAME … end` and `constant NAME` name things;
  `if … else … then` and `begin … until` are the control flow. Every other word
  takes and gives stack values, and never reads ahead.
- **Typed values:** integers (checked: no overflow, no division by zero),
  booleans, strings, and the application's own.
- **Compiled whole before it runs:** an unknown word or an unclosed block is an
  error before anything happens.
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
let ask = words.add("ask", 1);          // takes one value
let mut compiler = Compiler::new(words);
compiler.add(&token::source("script.fx", "3 ask")?)?;
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
and pushes its results, and answers `Done::Now` or `Done::Later(request)`.

## License

MIT
