# Betamax headless terminal verification

The standalone Rust harness has an optional `betamax` feature. Setting
`FUX_BETAMAX_DIR` enables raster capture and makes the fux integration-test launcher
build that feature automatically. Requesting capture from a build without the
feature fails explicitly. Production fux/zor dependencies are unchanged.

The existing PTY drivers still launch the real binaries and send their keyboard,
mouse and resize events. Betamax consumes those same raw output bytes. Each
checkpoint saves styled terminal-state JSON, a label and a byte offset into the
raw VT stream, retained in size epochs. The `betamax-report` command replays those
exact offsets after the tests, verifies every replayed state against its live
snapshot, and renders PNGs. Capture waits for DEC synchronized-output frames to
finish; capture and replay reject checkpoints inside unfinished frames. Deferring raster work preserves interaction timing. The viewer scenario additionally compares
Ghostty and vt100 visible text at each successful checkpoint. Live text mismatches fail the scenario; replay mismatches and rendering errors
fail the report command. No vt100 screen reconstruction is used as
Betamax input.

Protocol-only scenarios also run in the suite below, but have no terminal viewport
to render. The fux viewer, cold-start/rejection/detach probes, shared PTY helper,
and zor dashboard are instrumented. Screenshots are checkpoints, not a recording
of every intermediate display frame. Use the existing timing harness without
capture enabled for performance measurements.

## Build and run

From the repository root, install the pinned native build tool into a disposable
environment. Betamax/libghostty requires Zig 0.15.2; the Rust harness requires 1.95
or newer.

```sh
python3 -m venv target/betamax-tools
target/betamax-tools/bin/pip install ziglang==0.15.2
export PATH="$(target/betamax-tools/bin/python -c \
  'import pathlib, ziglang; print(pathlib.Path(ziglang.__file__).parent)'):$PATH"
export FUX_BETAMAX_DIR="$PWD/target/betamax"
```

Select `Noto Sans Mono CJK SC`, which supplies monospace Latin and CJK glyphs.
On Ubuntu, install `fonts-noto-cjk`. On macOS, the following installs the pinned
font into your user font directory without replacing an existing file:

```sh
mkdir -p target/betamax-fonts "$HOME/Library/Fonts"
curl --fail --location \
  https://raw.githubusercontent.com/notofonts/noto-cjk/f8d157532fbfaeda587e826d4cd5b21a49186f7c/Sans/Mono/NotoSansMonoCJKsc-Regular.otf \
  --output target/betamax-fonts/NotoSansMonoCJKsc-Regular.otf
curl --fail --location \
  https://raw.githubusercontent.com/notofonts/noto-cjk/f8d157532fbfaeda587e826d4cd5b21a49186f7c/Sans/LICENSE \
  --output target/betamax-fonts/LICENSE
printf '%s  %s\n' \
  ec04cc376b34887cedbdf84074e2e226ed2761eeabdcb9173fc1dd7bfd153ef7 \
  target/betamax-fonts/NotoSansMonoCJKsc-Regular.otf | shasum -a 256 -c -
cp -n target/betamax-fonts/NotoSansMonoCJKsc-Regular.otf \
  "$HOME/Library/Fonts/fux-betamax-NotoSansMonoCJKsc-Regular.otf"
export FUX_BETAMAX_FONT="Noto Sans Mono CJK SC"
```

If the font variable is omitted, Betamax uses the system monospace fallback; that
may omit CJK glyphs. The raster regression deliberately fails when the chosen font
cannot paint the wide-character probe. The font request is recorded per session;
pixel output remains platform/font dependent and is not a portable golden baseline.

Run the complete standalone harness tests, then all fux local and zor integration
scenarios, with no scenario exclusions:

```sh
cargo +stable test --manifest-path tools/xtask/Cargo.toml \
  --features betamax --target-dir target/rust-harness --locked -- --test-threads=1

cargo +stable build --locked -p zor --no-default-features --features cli --bin zor
export ZOR_BIN="$PWD/target/debug/zor"
export FUX_REQUIRE_ZOR_BIN=1
# Matches the repository's macOS observation-deadline setting; omit on Linux.
export FUX_SCENARIO_DEADLINE_SCALE=3
cargo +stable test -p fux --locked --no-fail-fast --test local_cli \
  --test automation_integration -- --test-threads=1 --nocapture

target/rust-harness/debug/fux-xtask betamax-report "$FUX_BETAMAX_DIR"
```

Open `target/betamax/index.html` to review labeled PNGs at their original size.
Each `terminal-*` directory contains session metadata, `epoch-*.vt` recordings,
and numbered PNG/state/checkpoint files. Use a fresh output directory for each
acceptance run so that failed and successful runs cannot be confused.

For the viewer alone after building fux:

```sh
cargo +stable build --locked -p fux --bin fux
cargo +stable build --manifest-path tools/xtask/Cargo.toml --locked \
  --features betamax --target-dir target/rust-harness
target/rust-harness/debug/fux-xtask scenario viewer target/debug/fux
```

The CI Betamax job runs the harness and both real-process integration suites on
Linux and retains the images, states, raw VT and HTML index, including on failure.
A local run does not establish that the remote CI job has passed.

## Betamax version and local fixes

The dependency is pinned to `betamax-core` 0.1.11, with a local source patch under
`tools/xtask/vendor/betamax-core`. See [its patch and provenance record](../tools/xtask/vendor/betamax-core/FUX-PATCHES.md).
The copy preserves upstream licensing and third-party notices. It adds live
terminal/canvas resize, prevents wide-glyph erasure by later background painting,
and removes continuation-cell placeholder spaces from text snapshots. Focused
tests cover resize buffer preservation, styled text, PNG output and the second
half of a wide glyph.

## What visual approval means

Review nested borders, focus/zoom, drag previews, Unicode labels, menus, tiny-size
recovery, tab/workspace moves, and multiple viewers in the rendered evidence.
Passing the assertions alone is not approval of the images. Record the run,
reviewed checkpoints, findings and limitations in the acceptance ledger.

Betamax verifies its headless software rendering of the real output. It does not
verify a desktop terminal's own font engine, OS interception of Alt-mouse/Ctrl-A,
native clipboard contents, or subjective interaction latency. Those are the
remaining native-terminal checks in [the manual checklist](pane-layout-manual-acceptance.md).
