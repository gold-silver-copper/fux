# fuxth

A small stack language for the shell, written for [fux](https://github.com/gold-silver-copper/fux)'s
command line and embeddable anywhere.

```text
fux 3 4 add                                  # 7
fux str 'hello world' upper                  # HELLO WORLD
fux list 3 1 2 end sort str , join           # 1,2,3
fux str srv pane str 'make test' line
```

A program is words, run left to right. Each takes values from the top of a
stack and leaves values on it. What is left at the end is printed, a value a
line, the deepest first; `print` prints a value at once.

- **Words, not symbols.** No word uses a character a shell treats specially,
  so a program can be a command's arguments, each argument one word, as well
  as a file.
- **A small core.** 21 words and 11 parsing words (below). Everything else,
  even `dup swap over rot`, is the **standard library** ([`STDLIB`],
  `src/stdlib.fux`), written in fuxth.
- **Values:** integers (64-bit, checked: no overflow, no division by zero;
  `div` truncates toward zero), booleans (never integers), strings (their
  elements are one-character strings), lists (of any values, lists included,
  nested at most 64 deep), and the application's own.
- **Checked before it runs.** The whole program is compiled first. An unknown
  word or an unclosed block is an error before anything happens; so is a step
  that every run reaching it would fail: a word given fewer values than the
  stack surely holds, or a value of a kind it never takes (`str a 1 add`,
  `str a not`: for a word of the library or your own, kinds on which every
  way through it fails, checked again at each call with the kinds it is
  given; a way ending at `fail`, or at a call to a word that only fails, counts as
  failing, unless every way does).
  A number written just before `pick` or `roll` is part of the step, and
  checked as a depth. It follows depths and kinds, not values (`1 0 div`
  fails as it runs), nor list elements' kinds, nor top-level names read
  inside a definition, nor whether a word's call to itself fails (taken
  as one that may succeed, leaving what its other ways leave), nor kinds
  under a call leaving a varying number of values, or deeper than the top
  32. It follows
  depths as ranges and kinds as sets through branches, loops, `list` and
  `each`, and definitions (each summed up by what it uses and leaves, and
  checked again at each call with the kinds of its top 8 values); what it
  can't know is never an error. Steps no run reaches (after `fail`, or a
  call that never returns) aren't checked.
- **Recursion in last place loops**: a call that is a definition's last step
  replaces its caller's frame.
- **Pauses for its host.** An application word can suspend the run while the
  application does something (asks a server); the run goes on once it has the
  answer. All of a run's state is data: no threads, no blocking, no Rust
  recursion in running a program, and dropping it stops it. A budget of steps
  lets the host check for Ctrl-C in a loop that never pauses, and
  `Machine::steps` counts every step taken, through every pause, for a host
  that caps a run.
- **Bounded.** 1,000,000 values on the stack, 10,000 nested calls; a string or
  list built holds at most 1,000,000 elements, a run builds at most
  16,000,000 in all, and a list holds at most 16,000,000 in all, counting
  through the lists and strings in it; long steps cost a step per 64
  elements; `push` and `put` grow a string or list in place when nothing
  else holds it. String literals are not built, so they are exempt.
- **Errors** carry a stable code, a message, and the file, line and word.

## The core

| Parsing word | |
|---|---|
| `str WORD` | WORD as a string, whatever it looks like: `str 12`, `str 'two words'`, `str ''` |
| `define NAME … end` | a word of your own; it may call itself; at the top level only |
| `VALUE let NAME` | NAME pushes VALUE from there on in the text: inside a definition, for that call; at the top level, for the rest of the program (definitions written after it may read it). A later `let NAME` sets it again. |
| `FLAG if … then`, `FLAG if … else … then` | one branch |
| `begin … FLAG until` | the body, until FLAG is true |
| `list … end` | a list of the values its body leaves; the body starts with an empty stack (values from outside come in by name) |
| `SEQ each … end` | the body once per element of a string or list, the element pushed; the body sees the whole stack |

| Word | Stack | |
|---|---|---|
| `drop` | x -- | |
| `pick` | xn … x0 n -- xn … x0 xn | `0 pick` copies the top |
| `roll` | xn … x0 n -- xn-1 … x0 xn | `1 roll` swaps |
| `add sub mul div` | n m -- r | checked |
| `lt` | a b -- flag | two integers, two strings (by character codes) or two lists (element by element) |
| `eq` | a b -- flag | the same, of one kind |
| `len` | seq -- n | |
| `at` | seq i -- x | from 0 |
| `slice` | seq from to -- seq | from `from` up to before `to` |
| `concat` | list -- seq | a list of strings into one string, a list of lists into one list |
| `push` | seq x -- seq | x at the end (to a string, x is a string) |
| `put` | seq i x -- seq | element i replaced |
| `code` / `char` | c -- n / n -- c | a character's code, and back |
| `kind` | x -- name | `integer`, `boolean`, `string`, `list`, or the host's |
| `string` | x -- s | as `print` shows it: a list's elements with spaces |
| `print` | x -- | now, a line |
| `fail` | x -- | stops the program, x the reason |

The standard library adds, among others: `dup swap over rot nip tuck -rot
2dup 2drop`, `true false not and or`, `gt le ge ne min max`, `mod negate abs
even pow`, `is-int is-bool is-string is-list`, `empty cat glue first last pop
shift remove chars range reverse index-of contains count sum largest smallest
merge sort pack unpack`, `number join split-on words lines trim starts-with
is-space is-digit is-upper is-lower is-letter upper lower copies pad`.

Files are split as a shell splits a line: quotes, backslashes, and `#`
comments at a word's start.

## Embedding

```rust
use fuxth::{Compiler, Done, Failure, Host, Limits, Machine, Outcome, STDLIB, Value, Vocabulary, token};

let mut words = Vocabulary::default();
let ask = words.add("ask", 1, 1);   // takes one value, gives one
let mut compiler = Compiler::new(words);
compiler.add(&token::source("stdlib.fux", STDLIB)?)?;
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
host pushes before running the machine again; `machine.at()` is the token
of the word it stopped at, and `machine.next()` the one it takes next, if its
frame has one. `print(line)` prints. A word
gives as many values as its vocabulary says: the checker counts on it, and the machine fails a run
with `contract` if a host gives otherwise. A word that is one step (`dup`
is `0 pick`) is copied in where it is called, so a mistake in it is placed
at the call.

For help, [`Builtin`] and [`Parsing`] are the core's words and the parsing
words: each has `ALL`, `name()` and `named(name)`, a built-in its `effect()`
and `about()`, a parsing word its `syntax()`. A list's values are
`List::items()`, read only, so its nesting and size limits hold.

## Tested

- `tests/examples/`: programs, each with what it leaves or the error it ends
  with.
- `tests/standard.rs`: cases from the Forth 2012 core tests and Exercism's
  Forth exercise, in fuxth's words, with where fuxth parts from them.
- `tests/differential.rs`: random programs run by fuxth and by a second, plain
  interpreter, which must agree; paused at every step or not, which must not
  matter; and rewritten in ways that mean the same.
- `tests/gforth.rs`: random integer programs against gforth, when there is one
  (`FUXTH_GFORTH`).
- `tests/corpus.rs`: any directory of programs (`FUXTH_CORPUS`) held to the
  same checks.
- Fuzz targets in fux's `fuzz/`: `fuxth_tokens`, `fuxth_run`, `fuxth_pause`.

## License

MIT
