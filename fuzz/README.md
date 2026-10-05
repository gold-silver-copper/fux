# fux's fuzz targets

An excluded workspace with its own lockfile, depending only on
libfuzzer-sys and fux and fux-vt by path. fux-vt's own targets are in
`fux-vt/fuzz`, and the fux-vt oracle's in `diff/fuzz`.

```sh
cargo +nightly fuzz build --fuzz-dir fuzz
cargo +nightly fuzz run TARGET --fuzz-dir fuzz -- -max_total_time=600 -max_len=4096 -rss_limit_mb=1024 -timeout=25
ASAN_OPTIONS=quarantine_size_mb=16 cargo +nightly fuzz run session --fuzz-dir fuzz -- \
  -dict=fuzz/session.dict -max_total_time=600 -max_len=4096 -rss_limit_mb=1024 -timeout=25
```

- `TARGET` is `protocol`, `paint`, `layout`, `config` or `session`. The
  terminal-input decoder's target, `keys`, is fux-vt's (`fux-vt/fuzz`).
- `-timeout=25` makes a hang fail within the run (libFuzzer's default is
  1200 s).
- `config` and `session` take a dictionary: `-dict=fuzz/config.dict`,
  `-dict=fuzz/session.dict`.
- `session` needs `ASAN_OPTIONS=quarantine_size_mb=16`: it frees millions
  of small allocations, and with the default 256 MB quarantine RSS passes
  1 GB within minutes. A smaller quarantine only shortens how long a freed
  allocation is watched, which safe Rust makes moot.

Where they run:

- **CI** (`.github/workflows/fuzz.yml`, nightly and on request, cargo-fuzz
  0.13.2) replays each target's stored corpus (`-runs=0`); `cargo test`
  does not. CI's "Excluded packages" step runs fmt, clippy and `cargo test`
  on this workspace on every push.
- **`fux-vt/compare/run.sh fuzz`** (and `run.sh deep`) fuzzes every target
  here, in fux-vt and the oracle for a share of the time each, and lists
  any crash.

## Targets

Every target checks that nothing panics. Each target's input format is
documented at the top of its file in `fuzz_targets/`.

**`protocol`**: bytes from a peer on the server's socket, into
`protocol::Decoder`, whole or in pieces of a size the first byte picks.
- The frames and first error are the same pushed whole, in pieces and byte
  by byte.
- After draining, `buffered()` is under `4 + MAX_FRAME`.
- Each frame round-trips through `Frame::encode`; `Frame::encode_into`
  after other bytes appends the same bytes; a paint, stdout or stderr frame
  is what `Stream::encode_into` writes for its payload.

**`paint`**: `render::paint` diffs applied to a fux-vt terminal.
- Input: a size (1–40 by 1–120, or one row of `u16::MAX` columns), a flags
  byte (old and new cursors; a new size of its own, forcing a full repaint;
  underline styles drawn by the terminal, and by the old paint), the
  cursors, then four-byte cell writes. Glyphs include `é`, a combining
  accent and `界`; no attribute set holds both bold and dim.
- After painting `old` from nothing and then the diff to `new`, every cell
  equals `new`'s in text, width and attributes (a blank reads as a space; a
  wide glyph in the last column is painted blank), and the cursor is
  `new`'s or hidden.

**`layout`**: `layout::place` on normalized trees (up to 16 panes, four
deep, weights 0 to `u32::MAX`), then `resize`. After every placement:
- every pane is placed once, not empty, and at least `MIN` each way where
  the area has room;
- panes and separators lie in the area and never overlap;
- when the tree fits (its minimum by `min_len`'s rule), they tile the area
  exactly; where it does not, a split without room for its first child
  shows nothing, by design;
- `neighbor` names only another placed pane.

**`config`**: `set`, `bind`, `unbind` and `unbind-all` lines into
`Config::apply`, from the defaults; lines are text, or structured (a
leading `0xff`) to reach every command and word. After every line:
- a failing line changes nothing;
- a small model of `bind`, `unbind` and `unbind-all`, written from the
  README's rules, accepts the same lines and holds the same bindings in
  the same order;
- `split(&join(&words))` gives the words back.

After a line that changes the configuration: every binding has keys and a
command; keys are lower-case letters without modifiers; no binding's keys
equal or start another's; and `describe()` applied after `unbind-all` gives
the same configuration back.

**`session`**: clients typing into a whole `Session` with no processes
while commands arrive from the side. Operations: bytes a client types
(whole or in pieces), the Escape deadline, resizes, one to three clients
attaching and detaching, a fixed list of commands (tabs, splits, overlays
with `-c`, `bind`/`unbind`, `set prefix`, `reload`, `detach`,
`paste-buffer`, `send-keys` and more), pane output, a shell exiting. After
each, the target does what the server does (settles, acts on the outbox,
takes queued pane input), then checks:
- every pane is in exactly one tab's layout, and every view's workspace,
  tab and focus exist and its screen composes;
- each overlay's state is valid (a column's layer exists, a repeat mode's
  layer repeats, a prompt's cursor is in its text of at most 4096 bytes, a
  list selects an item);
- every `Outgoing::Bytes` is OSC 52 with at most `MAX_CLIPBOARD` bytes of
  base64;
- a key typed in a repeat mode reaches no pane, unless bound there to
  `send-keys` or `paste-buffer`;
- a second session given every byte one at a time ends with the same `ls`,
  modes, notices, screens, configuration and paste buffers.

## Corpus

`corpus/TARGET/fixture-*` are permanent seeds taken from the unit tests
that state each property (round-tripped frames and malformed ones, key
sequences, paint and layout cases, config lines and the default bindings,
the overlay tests' key sequences). `corpus/TARGET/regression-*` are
minimized inputs of fixed findings. Anything else in `corpus/` is local
and ignored by git.
