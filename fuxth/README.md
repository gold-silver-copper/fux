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
- **A small core.** 21 words and 13 parsing words (below). Everything else,
  even `dup swap over rot`, is the **standard library** ([`STDLIB`],
  `src/stdlib.fux`), written in fuxth.
- **Values:** integers (64-bit, checked: no overflow, no division by zero;
  `div` truncates toward zero), booleans (never integers), strings (their
  elements are one-character strings), lists (of any values, lists included,
  nested at most 64 deep), and the application's own.
- **Checked before it runs.** The whole program is compiled first. An unknown
  word or an unclosed block is an error before anything happens (told with
  a word in scope spelt nearly so, or `str WORD` for text); so is a step
  that every run reaching it would fail: a word given fewer values than the
  stack surely holds, or a value of a kind it never takes (`str a 1 add`, or
  a kind the application's word doesn't take, as its vocabulary says), and
  a value the application says must be used left at the end (`unused`). A
  number written just before `pick` or `roll` is part of the step. The
  checker follows depths as ranges and kinds as sets through branches,
  loops, `list`, `each` and definitions (each summed up once by what it
  uses and leaves, whatever kinds it is given); not values (`1 0 div` fails
  as it runs), list elements' kinds, top-level names read inside a
  definition, or kinds deeper than the top 32. What it can't know is never
  an error, and steps no run reaches aren't checked.
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
| `define NAME … end` | a word of your own; it may call itself, and words defined after it in the same source; it may take a library's or the application's word's name, from there on (not the core's or a parsing word's, nor one the source defines or a top-level `let` names already); at the top level only |
| `VALUE let NAME` | NAME pushes VALUE from there on in the text: inside a definition, for that call; at the top level, for the rest of the program (definitions written after it may read it). A later `let NAME` sets it again. |
| `FLAG if … then`, `FLAG if … else … then` | one branch |
| `begin … FLAG until` | the body, until FLAG is true |
| `list … end` | a list of the values its body leaves; the body starts with an empty stack (values from outside come in by name) |
| `SEQ each … end` | the body once per element of a string or list, the element pushed; the body sees the whole stack |
| `CODES try … catch … end` | the body, from an empty stack (values from outside come in by name), its values left. If it fails with one of CODES (a string written just before `try`: `str 'timeout closed' try`), what it began is undone (its calls, blocks and values) and the handler runs instead, on the stack as it was at `try` and the failure's message and code, both strings, the code on top; a failure of a code the application declares with `carries` may carry a value instead of the message (`Machine::raise_with`). A carried value must match the explicit declaration for its registered code; an undeclared or wrong payload is an uncatchable `contract` failure before recovery. Duplicate `carries` declarations are refused. The message is the failure's own, as its word or `fail` gave it (`Error::message`), with no place, word or host's telling added: only a failure that ends the program is said where, by its host (`Error::located`); one longer than a string may be fails with `too_long` instead, for a `try` further out, not cut. A failure of another code goes through untouched, as does one in the handler. A code no running word raises (a typo, `unknown_word`) is refused before the program runs, unless the application's words raise it as they run; one found compiling is never caught. Checked as `if … else … then`: the two ways join after `end` |

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

The standard library adds, among others: `dup swap over rot nip -rot
2dup 2drop`, `true false not and or`, `gt le ge ne min max`, `mod negate abs
even pow`, `is-int is-string is-list`, `empty cat glue first last pop
shift remove range reverse index-of contains count sum largest smallest
merge sort pack unpack`, `number join split-on words lines trim starts-with
is-space is-digit is-upper is-lower is-letter upper lower copies pad`.

Files are split as a shell splits a line: quotes, backslashes, and `#`
comments at a word's start.

## Embedding

```rust
use fuxth::{Code, Compiler, Done, Failure, Host, Limits, Machine, Outcome, STDLIB, Value, Vocabulary, token};

let mut words = Vocabulary::default();
words.kind("ticket", "a ticket")?;               // a kind of the application's values
words.name("n", "integer", "an integer")?;       // a name effects use, for kinds
let ask = words.add("ask", "n -- ticket")?;       // takes an integer, gives a ticket
words.raises(Code::Host("refused"))?;            // a failure of its words, for `try`
words.carries(Code::Host("refused"), "ticket")?; // which a handler may get as a ticket
let mut compiler = Compiler::new(words);
compiler.add(&token::source("stdlib.fux", STDLIB)?)?;
compiler.add(&token::source("script.fux", "3 ask")?)?;
let mut machine = Machine::new(compiler.finish()?, Limits::default());
loop {
    match machine.run(&mut my_host, 10_000)? {
        Outcome::Finished => break,
        Outcome::Suspended(request) => { /* do it, then */ machine.push(answer)?; }
        // or, if doing it failed: machine.raise(failure)?;
        // or, carrying a value for a try's handler: machine.raise_with(failure, value)?;
        Outcome::Paused => { /* check for Ctrl-C */ }
    }
}
```

The application names its kinds (`kind`: as its values' `AppValue::name`
says them, and as an error does), may say a kind's values must be used, and
names sets of kinds for its effects (`name`: `"pane answer"`; `from>to` for
a kind that becomes another when passed through, `x secs -- x`). Each word's
effect is read strictly, in the names of kinds (`integer`, `boolean`,
`string`, `list`, `any`, the application's, and its names): a name before
the `--` is what a place takes, after it what one gives, and a name on both
sides passes the kind it was given through. `taken_as("pane", "answer")`,
said before any word, takes and gives a kind's values where a word's
effect names another (a name of effects reads each kind as itself).
Each name means one thing: a second kind or name of one spelling, a
second word of a name (the core's and the parsing words' among them) or
a code declared twice is refused (`contract`), so a host adds each once
and keeps the id `add` gave. The checker follows those kinds, as it does
the core's, and refuses only what surely fails.

The host implements [`Host`]: `call(word, args, out)` gets the word's values
(an iterator, the deepest first) and pushes its results, and answers
`Done::Now` or `Done::Later(request)`; a suspended word's results are what the
host pushes before running the machine again, or its failure is what the
host raises there (`machine.raise(failure)`), caught by a `try` as one at
once would be; `machine.at()` is the token of the word it stopped at. The
codes of its words' failures are the vocabulary's (`raises`), for a `try` to
name: its own (`Code::Host`), and any of the language's found only compiling
or at a program's end that its words raise as they run (`Code::Unused`).
A `Failure` may name a word of its own, one in a program the word was given. `print(line)` prints. A word
gives as many values as its vocabulary says, of the kinds it says: the checker counts on it, and the machine fails a run
with `contract` if a host gives otherwise, when it returns or resumes. A word that is one step (`dup`
is `0 pick`) is copied in where it is called, so a mistake in it is placed
at the call. A failed `add` leaves the compiler as it was.
`Compiler::definitions` is a compiler for a program of its own that sees
those definitions, not their source's top level or `let` names (a key
binding of a config's, say); a definition reading one may not be called
there (`unset`).

For help, [`Builtin`] and [`Parsing`] are the core's words and the parsing
words: each has `ALL`, `name()` and `named(name)`, a built-in its `effect()`
and `about()`, a parsing word its `syntax()` and what it `reads()` of the
token after it (`str` text, `define` and `let` a name). `Compiler::library`
compiles a library as `add` does and gives each definition's [`Doc`]: the
comment on the line just above its `define`, `# EFFECT : ABOUT`, and its
code as written; a definition without that comment is refused, and so is
a block open as the library begins or ends. A list's values are
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
