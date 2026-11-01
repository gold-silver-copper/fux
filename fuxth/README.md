# fuxth

A small Forth for the shell, written for [fux](https://github.com/gold-silver-copper/fux)'s
command line and embeddable anywhere: Forth's mechanisms, in English words.

```text
fux 3 4 add                                  # 7
fux str 'hello world' upper                  # HELLO WORLD
fux list 3 1 2 end sort str , join           # 1,2,3
fux define square dup mul end 7 square       # 49
fux code 3 4 add end execute                 # 7
```

A program is words, read and run one at a time, left to right. Each takes
values from the top of a stack and leaves values on it. What is left at the
end is printed, a value a line, the deepest first; `print` prints a value at
once.

- **Words, not symbols.** No word uses a character a shell treats specially,
  so a program can be a command's arguments, each argument one word, as well
  as a file. A string is what the shell, or the shell-style reader of files,
  quoted: `str 'two words'`.
- **An outer interpreter.** Each word is found as it is read and run then,
  or, while a definition is open, compiled into it. An unknown word fails as
  it is read, after the words before it acted.
- **A dictionary of versions.** `define NAME … end` makes a word, found from
  its `end` on; a word calls itself with `recurse`. Calls bind early: code
  keeps the version of each word it was compiled against, so a redefinition
  changes only what is compiled after it, and may call the version before
  it (`define down down down end`). Late binding is explicit: `find`,
  `execute`, a `let` name holding an xt, and a word `defer` made, which
  runs the xt `is` gave it last.
- **Immediate words** run even while a definition is compiled: every word
  that shapes a program is one (`define`, `str`, `let`, `if`, `list`, `try`
  …), and a program makes its own with `immediate`, `postpone` and
  `literal`:
  `define unless postpone not postpone if end immediate`.
- **A small core** (below). Everything else, even `dup swap over rot`, is
  the **standard library** ([`STDLIB`], `src/stdlib.fux`), written in fuxth.
- **Typed cells.** A stack item is one value: an integer (64-bit,
  two's-complement: `add sub mul` wrap around, as Forth's do; `div`
  truncates toward zero, and by zero it fails), a boolean, a string (its
  elements are its characters' codes: `str abc 0 at` is 97), a list (of any
  values, nested however deep), an execution token (an xt: `tick NAME`,
  `find`, `code … end`), an address, or the application's own. A flag is
  a boolean or an integer, true unless it is 0; comparisons give booleans,
  and arithmetic on one fails.
- **A data space of typed cells.** Forth's memory for variables and
  tables holds values, a cell each, and never host memory. An address is a
  kind of its own, made only by `next-address` and `create` and the words
  over them, never from an integer: an address plus n is the cell n on,
  two addresses subtract to how far apart they are, and every access is
  checked against the cells allotted (outside them, `invalid_address`,
  -9). A cell allotted but never stored fails `unset` when read. When the
  allocator refuses more cells, the word fails `memory` (-8).
- **Recursion in last place loops**: a call that is a definition's last step
  replaces its caller's frame.
- **Pauses for its host.** An application word can suspend the run while the
  application does something (asks a server); the run goes on once it has the
  answer, reading the next word. All of a run's state is data, the sources
  being read among it: no threads, no blocking, no Rust recursion in running
  or reading a program, and dropping it stops it. A budget of steps lets the
  host check for Ctrl-C in a loop that never pauses.
- **No limits of its own.** The stack, the calls nested, strings and lists
  grow as far as memory lets them: the user and the OS bound a run
  (Ctrl-C, `timeout`, `ulimit`). No walk over a value or a chain of code
  (comparing, writing, dropping) is a Rust recursion.
- **Errors** carry a stable code, a message, and the file, line and word;
  caught, a failure is a value, with its number in Forth's table where the
  standard has one (`stack_underflow` -4, `div_by_zero` -10, `fail` -2).

## The core

The words that shape a program run as they are read, even while a
definition is compiled:

| Immediate word | |
|---|---|
| `str WORD` | WORD as a string, whatever it looks like: `str 12`, `str 'two words'`, `str ''` |
| `define NAME … end` | a word of your own, found from its `end` on; at the top level only. Any name but a number, one the host reads as a value, or the core's |
| `X let NAME` | NAME gives X from then on: in a definition, a name of that call's; at the top level a value, which definitions read when they run. A later `let NAME` sets it again |
| `X to NAME` | sets the `let` name NAME, a top-level value from inside a definition too |
| `FLAG if … then`, `FLAG if … else … then` | one branch |
| `begin … FLAG until` | the body, until FLAG is true |
| `begin … FLAG while … end`, `begin … again` | while FLAG is true, the part after `while`, then from `begin` again; for ever (until `leave-definition` or a failure) |
| `LIMIT FIRST do … loop`, `do … N loop-by` | the body for each index from FIRST up to before LIMIT, none when the two are equal (Forth's `?do`); `loop-by` moves the index N, and ends as it crosses from just under LIMIT to LIMIT, either way. `i` gives the index, `j` the loop around's, `leave` ends the loop. Its index lives apart from the return stack |
| `X case V of … end … end` | the part after the first `V of` whose V equals X, X dropped; else the part before the last `end`, which drops the top value (X, unless that part moved it) |
| `ahead … then` | a jump past the words up to `then` (a mark, as `if`'s is, which `swap` and `roll` move as Forth's `cs-roll` does) |
| `list … end` | a list of the values its body leaves; the body starts with an empty stack (values from outside come in by name) |
| `SEQ each … end` | the body once per element of a string or list, the element pushed; the body sees the whole stack |
| `CODES try … catch … end` | the body, from an empty stack, its values left. If it fails with one of CODES (a string written just before `try`: `str 'timeout closed' try`; `str all try` for every code a running word raises), what it began is undone (its calls, sources being read, blocks, code being compiled and values) and the handler runs instead, on the stack as it was at `try` and one **failure**: its code, its number in Forth's table of `throw` codes, its message and place, the value an application's failure may carry (`Machine::raise`'s `carried`), and the steps whose words had taken effect (`Machine::acted`). `throw` raises it again; a failure of another code goes through untouched, as does one in the handler |
| `code … end` | the words between, compiled, as an xt: pushed at the top level, compiled as a literal in a definition. The xt keeps those words, as read (`Xt::words`), so an application can send it elsewhere and compile it again (`code` WORDS `end`, read there), so long as `code` and `end` themselves opened and closed it |
| `tick NAME` | NAME's xt, likewise |
| `XT is NAME`, `action-of NAME` | the word `defer` made, NAME, runs XT from then on; the xt it runs, likewise (Forth's `is` and `action-of`) |
| `postpone NAME` | in a definition: what NAME does when compiled, done when the definition runs |
| `X literal` | in a definition: X, pushed when it runs |
| `recurse`, `leave-definition` | in a definition: a call to itself; a return now, its loops left |
| `stop-compiling` | run the words read, until `start-compiling`, though a definition is open |
| `include PATH`, `require PATH` | the file's words read here, through the host, relative to the file naming it; `require` reads a file once, and a cycle is an error |

At the top level a construct (`if`, `begin`, `do`, `case`, `list`, `each`, `try`) is
compiled into code of its own, which runs as soon as its last construct
closes. While compiling, each open construct is a **mark** on the stack,
which only its own code's words resolve; `end` closes the innermost.

| Word | Stack | |
|---|---|---|
| `drop` | x -- | |
| `pick` | xn … x0 n -- xn … x0 xn | `0 pick` copies the top |
| `roll` | xn … x0 n -- xn-1 … x0 xn | `1 roll` swaps |
| `add sub mul div` | n m -- r | `add sub mul` wrap around; `add` and `sub` move an address by n cells, and `sub` gives two addresses' distance |
| `mul-div-mod` | n k d -- r q | n×k divided by d, the product exact (Forth's `*/mod`) |
| `and or xor` | a b -- x | logical on booleans, bitwise on integers (the library's `invert` too) |
| `shift-left shift-right` | n u -- n | 0s come in; past 63 places, 0 |
| `within` | x lo hi -- flag | lo ≤ x < hi, around the end of the integers when hi is below lo |
| `lt` | a b -- flag | two integers, two strings (by character codes) or two lists (element by element) |
| `eq` | a b -- flag | the same, of one kind; two xts, the same version of a word |
| `len` | seq -- n | |
| `at` | seq i -- x | from 0; of a string, a character's code |
| `slice` | seq from to -- seq | from `from` up to before `to` |
| `concat` | list -- seq | a list of strings into one string, a list of lists into one list |
| `push` | seq x -- seq | x at the end (to a string, x is a string) |
| `put` | seq i x -- seq | element i replaced |
| `char` | c -- s | the one-character string of a character's code |
| `depth` | -- n | how many values the stack has (in a block that starts it empty, above its start) |
| `to-return from-return copy-return` | x -- / -- x / -- x | the return stack: values only, never where a word returns to; a `try`'s body takes none from below where it began, so a caught failure restores it |
| `i j leave` | -- n / -- n / -- | in a `do` loop of the code running: its index, the outer loop's, end it now |
| `digits-begin hold holds digits-end` | -- / c -- / s -- / n -- s | a number's text, built from its end (Forth's `<# hold holds #>`) |
| `next-address allot store-next` | -- addr / n -- / x -- | where the data space goes on; n empty cells added (below 0, given back); x stored in a cell added (Forth's `here allot ,`) |
| `fetch store fill copy-cells` | addr -- x / x addr -- / addr u x -- / from to u -- | a cell read, written; x in u cells; u cells copied, overlapping or not (Forth's `@ ! fill move`) |
| `create NAME`, `does`, `body-of` | -- / -- / xt -- addr | a word that gives where the data space goes on; in a definition, the rest of it what the word `create` made last does, its address pushed first; the address a `create`d word gives (Forth's `>body`) |
| `variable NAME`, `constant NAME` | -- / x -- | a word that gives the address of a cell of its own; a word that gives x |
| `defer NAME` | -- | a word that runs the xt `is` gives it (none yet: it fails `unset`), so code compiled before runs the one given last |
| `base`, `number` | -- addr / s -- n | the address of the base `number` reads and `digit` writes in (10 to begin with); the integer s writes in it, or after Forth's prefixes `#`, `$` and `%` in base 10, 16 or 2 |
| `kind` | x -- name | `integer`, `boolean`, `string`, `list`, `address`, `xt`, or the host's |
| `string` | x -- s | as `print` shows it: a list's elements with spaces |
| `print`, `write`, `emit` | x -- / x -- / c -- | now: a line; text with no line end; a character |
| `fail` | x -- | stops the program, x the reason (Forth's `abort"`, -2) |
| `throw` | x -- | raises x, a failure, again; or the failure numbered x: the code with that number (`-10 throw` is `div_by_zero`), else `thrown`; `0 throw` raises none |
| `abort`, `quit` | -- | `-1 throw`, `-56 throw` |
| `execute` | xt -- | runs the word |
| `find` | s -- xt true / s false | the word named s now |
| `immediate-word` | xt -- flag | whether the word xt is runs while compiling (Forth's `find` gives 1 for one) |
| `next-word` | -- s | the next word of the input, for a word that reads one |
| `immediate` | -- | the word defined last runs while compiling: made again as a version of its own, so an xt taken before stays as it was |
| `see NAME`, `words`, `print-stack` | -- | write what made NAME (a comment saying where it was read, then its words as read, quoted to be read back; of a word in Rust, its effect); every word's name, a line each, oldest first; the values `depth` counts, a line each, deepest first, left as they are (Forth's `see words .s`) |
| `compiling`, `start-compiling` | -- flag / -- | whether words are compiled; compile again |
| `compile-call` | xt -- | a call to xt compiled into the definition open, though its words are run (Forth's `compile,`) |
| `evaluate` | s -- | reads s as a source file is read, here; only where the host enables it |

The standard library adds, among others: `dup swap over rot nip -rot
2dup 2drop tuck dup-nonzero 2swap 2over`, `true false not invert`, `gt le
ge ne min max`, `mod negate abs even pow div-mod mul-div`, `add-store
erase decimal hex`, `is-int
is-string is-list`, `empty cat glue first last pop shift remove range
reverse index-of contains count sum largest smallest merge sort pack
unpack`, `quote digit digits sign join split-on fields lines trim
starts-with is-space is-digit is-upper is-lower is-letter upper lower copies
pad`.

Files, stdin and `evaluate`'s strings are split as a shell splits a line:
quotes, backslashes, and `#` comments at a word's start. `quote (s -- s)`
builds one source token that reads back unchanged, including empty text and
embedded newlines.

## Embedding

```rust
use fuxth::{Code, Done, Failure, Host, Machine, Outcome, STDLIB, Value, Vocabulary, token};

let mut words = Vocabulary::default();
let ask = words.add("ask", "n -- ticket")?;       // takes one value, gives a ticket
words.raises(Code::Host("refused"))?;            // a failure of its words, for `try`
let mut machine = Machine::new(words);
machine.library("stdlib.fux", STDLIB)?;
machine.read(token::source("script.fux", "3 ask")?, None);
loop {
    match machine.run(&mut my_host, 10_000)? {
        Outcome::Finished => break,
        Outcome::Suspended(request) => { /* do it, then */ machine.push(answer); }
        // or, if doing it failed: machine.raise(failure, None)?;
        // or, carrying a value for a try's handler: machine.raise(failure, Some(value))?;
        Outcome::Paused => { /* check for Ctrl-C */ }
    }
}
```

A word's effect names what it takes before the `--` and what it gives
after: the machine hands the host as many values as there are names before
the `--`, the deepest first, after checking the stack has them
(`stack_underflow`). What kinds they are is the host's to check, as the
core's words check theirs (`type`). The application may say a kind's values
must be used (`must_use`, by its values' `AppValue::name`), may read words
of its own syntax as values (`literals`: fux reads `%3` as `str %3 pane`),
and may enable `evaluate`. Each name means one thing: a second word of a
name (the core's among them) or a code declared twice is refused
(`contract`), so a host adds each once and keeps the id `add` gave.

The host implements [`Host`]: `call(word, args, out)` gets the word's values
(an iterator, the deepest first) and pushes its results, and answers
`Done::Now` or `Done::Later(request)`; a suspended word's results are what the
host pushes before running the machine again, or its failure is what the
host raises there (`machine.raise(failure, carried)`), caught by a `try` as one at
once would be; `machine.at()` is the token of the word it stopped at. Once
what a word asked took effect, the host says so (`machine.acted(step)`):
every failure from then on lists that step, once (`Thrown::actions`, and
`machine.actions()` for one no `try` caught). The
codes of its words' failures are the vocabulary's (`raises`), for a `try` to
name. `write(text)` writes. `resolve` and `read` give `include` and
`require` their files: a host that reads none keeps the defaults, which
refuse.

A machine is a value: `Machine::clone` copies its dictionary, its `let`
names and what it runs, sharing the code. A host reads its libraries once
and clones the machine for each program; `library` reads one and gives each
definition's [`Doc`] (the comment on the line just above its `define`,
`# EFFECT : ABOUT`, and its code as written), refusing a definition without
it, and leaves the machine as it was if the library fails. `read` gives a
machine a source to read next; `clear` stops what it runs and empties its
stacks, its dictionary kept. A word that is one step (`dup` is `0 pick`) is
copied in where it is compiled, so a mistake in it is placed at the call.

For help, [`Builtin`] is the core's words: `ALL`, `name()`, `named(name)`,
`effect()`, `about()`, and whether each is `immediate()` or `hosted()`
(`evaluate`). A list's values are `List::items()`, read only. The
dictionary as it is now is `Machine::dictionary()`, each name's current
word once, in the order they were defined, and `Machine::definition(name)`
one: a [`Definition`] says its [`Origin`] (the core, the host, a library or
the program), whether it is immediate, its effect and help as the core,
the host or a library declare them, and the words that made it, as read
(`source()` quotes them to be read back), as `see` writes it.

## Tested

- `src/tests.rs`: the machine and reader, a property a test.
- `tests/examples/`: programs, each with what it leaves or the error it ends
  with.
- `tests/standard.rs`: cases from the Forth 2012 core tests (Annex F,
  Hayes's suite) and Exercism's Forth exercise, translated word for word,
  with where fuxth parts from them.
- `tests/random.rs`: random programs, which end the same paused at every
  step or not, and rewritten in ways that mean the same.
- `tests/corpus.rs`: any directory of programs (`FUXTH_CORPUS`) held to the
  same pausing check.
- Fuzz targets in fux's `fuzz/`: `fuxth_tokens`, `fuxth_run`, `fuxth_pause`.

## License

MIT
