# Tighten the fux/zor boundary

Follow-up to the boundary audit (raw transcripts under
`/Users/kisaczka/Desktop/code/fux-final/.verification/boundary-audit/`). Intended split: fux is
a generic terminal multiplexer; zor owns agent detection, state, tasks, checks and artifacts;
koh is transport and is out of scope here. The audit found nothing in fux that must move to
zor, and several things in zor that duplicate fux or depend on it too tightly. Do the five
items below as **one pull request** against `main` of
`https://github.com/gold-silver-copper/fux`, branch `boundary/tighten`, from a fresh worktree
of `main`. Commit the items in order as separate, reviewable commits (each commit must build
and pass the workspace's fast suites on its own), push once the whole branch passes the
verification below, open the PR, wait for hosted CI and report it. Do not merge. Keep the
runtime of `/Users/kisaczka/Desktop/code/fux` unchanged.

**No compatibility shims.** Old forms are removed in the same PR that introduces their
replacement, and every in-repo consumer (fux CLI, viewer, zor, fixtures, docs, scenarios) is
moved in that same PR. No deprecated aliases, no dual code paths, no feature flags, no
"transitional" wrappers, no `zor <command>` exec shim: if something is replaced, the old thing
is gone. Backwards compatibility with released fux 0.7.0 / zor 0.2.0 is not a goal; the next
release is a breaking one and the changelog says so.

Rules for every item: no single build, test run or CI wait over five minutes (prefer under
two); no batch campaigns; do not run the full 45-command headless gate (leave its command in
the report); protocol changes are allowed only where the item says so, and the old form is deleted in
the same commit series (no additive-then-remove staging); no behavior change outside the
item's stated scope; per-crate lints stay as they are; the agent-boundary
inventory (`crates/fux/tests/fixtures/multiplexer-boundary.json`) is regenerated only after you
confirm every new fux declaration is generic. Use one independent subagent to review
the complete diff for scope creep, semantic drift and any leftover compatibility path, and
fix confirmed findings before pushing.

## 1. zor tolerates unknown fux event kinds

`crates/zor/src/watch/events.rs` hard-codes the closed set `pane.opened|closed|title|output`,
`tab.opened|closed`, `client.attached|detached`, `workspace.changed` and treats any other kind
as a fatal stream error, so adding an event to fux breaks zor's subscription. Change zor to
ignore unknown kinds while still enforcing cursor continuity (stream stability and
`sequence == previous + 1`) on every frame including ignored ones. Keep the strict rejection
of malformed envelopes (missing or duplicate cursor, unknown top-level fields) exactly as is.
Add a deterministic test feeding a subscription with an unknown kind between known ones and
asserting the known ones are processed and the cursor advances; add a real-fux scenario check
only if an existing scenario can be extended in under a minute of runtime. Update
`crates/zor/OBSERVATION-CONTRACT.md` to say unknown kinds are ignored. Verify with zor's lib
tests and the `zor-events` scenario.

## 2. Separate zor's standalone PTY wrapper

zor's `src/pty.rs`, the wrapper parts of `src/platform/{macos,linux}.rs`, `src/emit/title.rs`,
the nested-wrapper detection (`ZOR_PID`, `run_transparent`) and the OSC 7877 emission into a
passthrough stream implement a second pane runtime that exists only for standalone
`zor <command>` use without fux. Default decision (the user may override): keep the product,
move it out of the composed crate. Create `crates/zor-wrap` (binary `zor-wrap`, its own
`[lints]` copied from zor, member of the workspace) containing the wrapper runtime and the code
it needs; the `zor` crate keeps observation over fux's control protocol, the service, tasks,
checks, artifacts, rules and OSC 7877 as a wire format (move the shared OSC types to whichever
crate both can depend on without a cycle; a small `zor-osc` library crate is acceptable if
that is the only way). `zor <command>` is removed from the `zor` binary; the wrapper is invoked only as
`zor-wrap <command>`. Update every doc, README example and scenario that used the old form. Every test
that exercised the wrapper moves with it and still passes; the README of each crate says what
it is. Zor's `--no-default-features` and `--all-features` checks, layering greps, and the
`zor-*` scenarios must pass unchanged; CI's `crates/zor/tools/xtask` checks may need the new
crate path. Record line counts moved and removed.

## 3. Structured capture in fux; zor stops re-emulating

zor's `src/screen.rs` re-parses fux's text captures with its own vt100 parser to evaluate
rules, so bytes are emulated twice and three vt100 parsers exist across the projects. Add a
capture form to fux's control protocol: `format:"cells"` returning, for the requested
rows, per-cell text and the existing attribute model (`CellStyle`, cell kind), plus cursor,
size, title, progress, revision, input sequence and `truncated`, sharing the same coherence
guarantees as the text form (one revision, one grid sequence). Reuse the retained grid
(`Grid`/`GridCell` in `crates/fux/src/terminal.rs`); do not add a second rendering model.
Golden-fixture it under `crates/fux/tests/verify/fixtures/` like the other frames. Then make
zor's observe/watch paths consume it directly through `rules::view::ScreenView` and delete the
re-emulation path for fux captures and its vt100 dependency from the `zor` crate; `screen.rs`
lives on only inside `zor-wrap` (item 2), if at all.
Keep zor's `if_revision`/unchanged handling and `max_bytes`/`truncated` semantics. Measure
before and after with `headless-performance` (one repetition, fux and zor CPU per phase) and
report the numbers; a regression in fux CPU above noise must be explained or reverted. The
text form stays for the CLI and for koh (which forwards opaque bytes and never parses it);
`format:"rows"`/`since` are removed here (see item 5), not kept alongside.

## 4. Shared local-socket crate

fux (`crates/fux/src/proto/socket.rs`, `daemon/`) and zor (`src/service.rs`, `src/fux.rs`)
each implement the same discipline: private runtime directory with 0700 checks, 0600 socket,
same-user peer authentication, instance nonce, bounded newline-framed JSON with size caps,
client caps and deadlines. Extract the parts that are byte-for-byte the same discipline into
`crates/local-ipc` (library, `forbid(unsafe_code)` unless nix wrappers are already used
without unsafe, lints at least as strict as fux's), and make both crates use it. Do not change
any wire byte, path, permission, limit or error text; the protocol fixtures and every socket
test must pass unchanged, and `cargo test -p fux --test structure` must keep proving fux does
not depend on zor. Keep the crate free of any fux or zor type. If a piece cannot be shared
without changing behavior in one crate, leave it in place and say so; do not keep two copies
of anything that was shared. Report the lines removed from each
crate.

## 5. Trim fux's unconsumed surface

- `PaneSummary.progress` and OSC 9;4 progress parsing: item 3's cells capture carries
  `progress`, so zor consumes it from fux and drops its own OSC 9;4 parser; fux keeps exactly
  one parser and exposes progress only through capture (drop the `list` field if nothing reads
  it). Update fixtures and the boundary inventory.
- Remove `capture format:"rows"` and `since` (`CaptureRow`, `since_applied`, the CLI
  `--rows/--since` flags); the cells form supersedes them.
- `wait`: keep `exit` and `seq` conditions; remove `pattern` and `quiet` together with their
  limits and their share of the Waits phase, since no consumer other than `fux run` uses them
  and they are expect-style automation policy. Rewrite `fux run` to wait on `exit` only and
  keep its documented behavior (final screen, exit status, timeout). Keep `MAX_PENDING_WAITS`
  and per-pane limits for the remaining conditions.
- Update `docs/design.md`'s ownership table to list what fux actually promises automation
  consumers (coherent capture, input receipts, event log with gaps, final records) and what
  zor actually consumes (not only `list`/`capture`).
- Verify: fux lib/ECS/fixtures/structure/boundary suites, all local CLI scenarios, all
  `zor-*` scenarios, protocol docs regenerated where they enumerate commands.

## Reporting

Bump versions for the breaking release (fux 0.8.0, zor 0.3.0, zor-wrap 0.1.0, local-ipc
0.1.0) with changelog entries that name every removed command, field and CLI flag. Write
`.verification/boundary-tightening/REPORT.md` in the worktree with: the PR URL and hosted CI
result; per item, the commits, lines added/removed per crate, and any deliberate deviation
from this prompt and why; the reviewer's disposition; the measurement numbers for item 3; and
a grep-backed statement that no removed identifier, command name or flag remains anywhere in
the workspace outside changelogs and archived evidence. End
with the remaining boundary concerns not addressed here (dashboard TUI, worktree management,
koh's two composition stories) and the exact gate command:

```sh
cargo run --locked --manifest-path tools/xtask/Cargo.toml -- dependencies verify --build --headless
```
