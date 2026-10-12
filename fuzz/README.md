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

**`protocol`**: bytes from a peer on the server's socket (a client's
hello, attach, actions and requests, or the server's frames), into
`protocol::Decoder`, whole or in pieces of a size the first byte picks.
- The frames and first error are the same pushed whole, in pieces and byte
  by byte.
- After draining, `buffered()` is under `4 + MAX_FRAME`.
- Each frame round-trips through `Frame::encode`; `Frame::encode_into`
  after other bytes appends the same bytes; a paint, stdout, stderr or
  reply-part frame is what `ServerFrame::split_into` writes for its
  payload.

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

**`layout`**: `layout::place` on trees grown from one pane by up to 15
splits, then `resize`. After every placement:
- every pane is placed once, not empty, and at least `MIN` each way where
  the area has room;
- panes and separators lie in the area and never overlap;
- with room for 16 panes side by side every pane is shown, and with every
  pane shown they tile the area exactly; where it is too small, a split
  without room for its first child shows nothing, by design;
- `neighbor` names only another placed pane.

**`config`**: one client's keys, from the default keymap. Lines are
config statements (a leading `0xff`: `bind`, `bind-root`, `unbind`,
`unbind-root`, `unbind-all`, `prefix`, `label`, `group` or `repeat`, written
as fuxth, compiled with `script::compile` and run as a config is, against
the `Keymap`), keys typed (a leading `0xfe`: through `keymap::Held::press`,
as a client that types or only watches), or
any other text, as fuxth source (`fuxth::token::source`, `compile`, then
run as a config for a bounded number of steps up to its first request for
the server; words that read files are not run). After every line:
- a failing statement changes nothing;
- a small model of the statements, written from the README's rules (a
  sequence is a binding or a layer, never both; no Escape after the
  prefix; the prefix never bound without it; code compiled as it is
  bound), accepts the same ones and holds the same prefix, bindings,
  labels, groups and repeats;
- source's tokens, quoted as a saved session quotes words, split back to
  the same tokens; a program that does not compile fails with status 1
  and a code;
- after a change to the keymap: no empty layer, no Escape after the
  prefix, no binding of the prefix without it nor of one key twice, and
  every layer's column shows;
- a key runs only a program that is bound, and that compiles; with
  nothing held the prefix opens the column, a key bound without the prefix runs its program, and
  any other key goes to the pane; the column a client shows and its repeat
  mode are of layers that exist (unless the keymap changed under them).

**`session`**: clients typing into a whole `Session` with no processes
while fuxth programs arrive from the side. Each client reads its keys as
its client does: by the screen it has open (`screens::Screen`: a menu,
chooser, prompt, confirmation or copy mode), else through its bindings
(`keymap::Held`, one keymap the clients share). It sends the session its
actions, and runs its programs in-process as itself (a screen one opens,
built with what the session answers, opens), as it does the programs the
session hands it (`Outgoing::Run`) and the keys sent to it
(`Outgoing::Keys`); `Outgoing::Ended` ends its copy mode. Operations: bytes a
client types (whole or in pieces), the Escape deadline, resizes, one to
three clients attaching and detaching, a fixed list of programs (tabs,
splits, overlays for a client, `bind`/`unbind`, `prefix`, `set`, `detach`,
`paste-buffer`, `send-keys` to panes and clients and more), pane output, a
shell exiting. Waits are not waited on, and words that read files are not
run. After each, the target does what the server does (settles, acts on
the outbox, takes queued pane input), then checks:
- every pane is in exactly one tab's layout, every view's screen composes,
  and every attached client has keys;
- a client shown a repeat bar is in a repeat mode;
- every `Outgoing::Bytes` is OSC 52 with at most `MAX_CLIPBOARD` bytes of
  base64;
- a key typed in a repeat mode is not sent on to the pane, and reaches no
  pane unless the binding it runs there sends keys or pastes;
- a second session given every byte one at a time ends with the same `ls`,
  modes, notices, screens, configuration, paste buffers, keymap and
  clients' keys and screens.

## Corpus

`corpus/TARGET/fixture-*` are permanent seeds taken from the unit tests
that state each property (round-tripped frames and malformed ones, key
sequences, paint and layout cases, config lines and the default bindings,
the overlay tests' key sequences). `corpus/TARGET/regression-*` are
minimized inputs of fixed findings. Anything else in `corpus/` is local
and ignored by git.
