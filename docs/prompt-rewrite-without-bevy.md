# Rewrite fux without Bevy

## Objective

Build the fux specified in `docs/rewrite-design.md`: a terminal multiplexer
with no Bevy, no RPC and no async runtime. One server thread runs a `poll`
loop over PTYs and a Unix socket, and everything is controlled through the
`fux` CLI and keyboard overlays. Work on the branch `rewrite` (worktree
`/Users/kisaczka/Desktop/code/fux-rewrite`), milestone by milestone. End with
an open PR from `rewrite` to `main` that the user can merge.

The design document is the specification. Read it in full before starting,
and treat its tables and lists as requirements. If the code has to depart
from it, change the document in the same commit and say why in the commit
message. Never let the two disagree.

## State at the start (2026-09-23)

Re-check each point; this is context, not proof.

- `main` is `f86dee7`: the Bevy fux, 0.12.0, with PR #51 merged.
- `fux-vt` 0.1.1 is on crates.io, published from `f86dee7`.
- Branch `rewrite` (local; push it when you start) holds only the design
  commits on top of `f86dee7`.
- The user's decisions are recorded in the design's "Decisions" table:
  - detach and reattach;
  - several clients with independent views;
  - no layout saving;
  - **no mouse at all**;
  - a keyboard copy/select mode;
  - the full keyboard command column, choosers and action menus;
  - the config as a file of fux commands, with zero dependencies;
  - OSC 52 clipboard writes on by default.
- Two defaults are mine, not the user's; list them in the PR description as
  open to change:
  - `remain-on-exit off`: a pane closes when its program exits;
  - the server exits when its last pane closes.

## Rules

**Things you do not touch:**
- koh, and its checkout. The branch `feat/fux-vt-for-koh` is never deleted:
  koh depends on it by git.
- `main`: never push to it and never merge the PR. The user merges.
- crates.io: publish nothing. Close no issue or PR.
- The `bevyengine` organization: never post there.

**fux-vt:**
- Use its public API as it is.
- A fux-vt change must be a bug fix with a test that failed first.
- It must keep everything koh uses, with the same meaning.
- Check koh after it: in a throwaway clone of `~/Desktop/code/koh`, branch
  `feat/fux-vt`, run
  `cargo test --config 'patch."https://github.com/gold-silver-copper/fux".fux-vt.path="…/fux-vt"'`
  without `--locked`. The lockfile diff must be only fux-vt's source line.
- Bump fux-vt to 0.1.2 only if its public API grows.

**Dependencies are exactly the design's list:** `fux-vt`, `rustix`,
`signal-hook`, `unicode-width`, and `libc` on macOS only. Adding any other
runtime dependency needs the user's approval first. Dev-dependencies for
tests are fine if small. Ask before adding anything heavier than
`tempfile`-sized.

**Lints:**
- Keep today's clippy lints: forbid `unwrap`, `expect`, `panic!`,
  `unreachable!`, `todo!`, `unimplemented!`; warn on `indexing_slicing`.
- `cargo clippy --workspace --all-targets -D warnings` must pass on macOS and
  on Linux, because code behind `cfg(target_os)` is checked only on its
  platform.
- `unsafe` only where the design allows it, each with a `SAFETY:` comment.

**Tests:**
- Every milestone lands with its tests.
- Every lesson test from the design's Testing section must be shown to catch
  its bug: break the code on purpose, run the test, see it fail, restore the
  code. Record that in the commit message.
- A fix to something that already worked needs a test that failed first.
- Never weaken a test to make it pass. Change one only when its assumption is
  proven wrong, with the evidence in the commit message.

**Commits:**
- Every commit builds and passes `cargo test --workspace --locked` on macOS.
- Messages say what changed and why, with the evidence.
- Push `rewrite` at each milestone, so CI runs.

**Behaviour to carry over:** where the design says "as today", read the Bevy
code at the `bevy-final` tag (`git show bevy-final:src/…`) and port the
behaviour and its tests, not Bevy's structure.
- `encode.rs` has no Bevy at all. Port it and its property tests nearly
  whole.
- `paste.rs`, `selection.rs`, `chrome.rs`, `actions.rs` and `interaction.rs`
  hold the rules for pastes, selection, the bar and the overlays.
- The `tests/design/` suites describe the overlays' expected behaviour key by
  key. Use them as a checklist, and drop only what involved the mouse.

## Traps the previous runs fell into

- `$?` after a pipe is the last command's status. `grep -c` exits 1 when it
  counts zero.
- Something on this machine deletes `target/` during long runs. Copy
  binaries elsewhere before long runs, and check a binary exists before
  trusting a result.
- A reboot wiped `/tmp` mid-run and lost every scratch file. Keep scratch in
  `~/.cache/fux-rewrite`.
- Python heredoc edits mangled byte escapes. Re-read a file after every
  scripted edit.
- In integration tests, a fork in one test can inherit another test's PTY
  slave before close-on-exec is set. That corrupted captures, and on Linux
  it made a dropped writer's newline echo back. Open PTYs and fork only while
  holding one test-wide lock.
- macOS `waitid` reports stopped children even with only `WEXITED`, so check
  the status kind, not just the PID (020).
- rustix's `OpenptFlags::CLOEXEC` does not exist on macOS. Set `FD_CLOEXEC`
  with `fcntl_setfd`.
- CPU-limited containers (`docker run --cpus`) exaggerate timing failures on
  a loaded Mac. Before calling a timing failure a regression, compare against
  a baseline under the same conditions.
- A test that waits for a marker must print the marker after the state it
  confirms: the modes first, then `READY`.

## Linux

Linux must work as well as macOS.
- Docker needs OrbStack; start it with `open -a OrbStack`.
- `fux-fuzz/linux/` is deleted with the rest. Keep a slimmed copy as
  `ci/linux/`: the Dockerfile without ALSA, and `run.sh` with its
  `FUX_LINUX_TMP=ext4` option (014 needs ext4).
- Use it for arm64 native, arm64 with ext4, and amd64 emulated.
- Under emulation, trust gates and unit tests. Integration timing under
  Rosetta is not evidence; the CI `ubuntu-24.04` runner is the x86_64
  evidence.

## Work, in order

Follow the design's Plan. Push `rewrite` at each milestone and read CI's
result before going on. Plan steps 0 and 1:

- **0. Tag and archive:** tag `bevy-final` at `f86dee7` and push the tag.
- **1. Delete and set up:**
  - Delete `src/`, `tests/`, `fux-fuzz/`, `fux-agent-exercises/`,
    `verification/`, and the four tracked `docs/prompt-pi-*.md` files (they
    drive the deleted agent exercises). Keep only `docs/rewrite-design.md`,
    this prompt and the new `docs/lessons.md`. The rest stays at
    `bevy-final`. Untracked files in the user's main checkout are theirs:
    leave them alone.
  - Write `docs/lessons.md` from `bevy-final:fux-fuzz/BREAKS.md`: one line per
    finding 001–021 saying whether it still applies and, when it does, which
    test covers it (fill that in as the tests land).
  - Set up the workspace (fux-vt plus the new `fux` at 0.13.0), a README that
    describes only what exists, and CI trimmed as the design says.
  - The skeleton: the command tokenizer, CLI dispatch, socket rules, protocol
    codec, and a server that answers `fux ls` and `fux kill-server`.

Then Plan steps 2–9, each a milestone that ends usable:

| Milestone | Usable end state |
| --- | --- |
| 2 | `fux` attaches to one shell; detach and reattach; resize works; `capture-pane` shows the screen |
| 3 | Splits, focus (next, previous, last, directional), resize by weight, zoom, the bar |
| 4 | Workspaces and tabs; two clients with independent views; PTY size is the minimum across viewers |
| 5 | The whole CLI table, `--json` on `ls` and `capture-pane`, targets and their errors |
| 6 | The config file, `set`, `bind`, `unbind`, `reload` with all-or-nothing errors, the prefix key |
| 7 | The command column, the choosers, action menus, the `:` prompt, rename, confirmations |
| 8 | Copy/select mode (motions, search, char/line/block selection), paste buffers, OSC 52 |
| 9 | Every lesson test, each shown to catch its bug; `cargo-fuzz` targets for the decoder, the protocol codec and the tokenizer, each run for at least 10 minutes |

After milestone 9, exercise it the way a user would, on macOS and Linux,
through real attach clients on real PTYs driven by a script:
- `bash`, `zsh` and `dash` shells;
- `vim`, `less` and `htop`;
- a long build that writes a lot of output;
- Unicode and wide glyphs;
- detach and reattach mid-output;
- two clients at different sizes on the same tab;
- every overlay, and copy mode over history.

Check what each client shows against `capture-pane`. Every problem found is
fixed with a test, or listed in the PR description as known. The PR asks the
user to try it by hand before merging; an agent's run is not a substitute
for that.

## Done means

All of these, checked at the final head of `rewrite`:

- **Local gates:** on macOS, Linux arm64 native, and Linux arm64 with ext4
  `/tmp`: `cargo fmt --all --check`, clippy with `-D warnings`, and
  `cargo test --workspace --locked`. On Linux amd64 emulated, the gates and
  unit tests.
- **CI:** green on `ubuntu-24.04` and `macos-15` for the PR, and a
  `workflow_dispatch` run of the long jobs (fuzz targets) green.
- **The lessons:** every lesson test passes, and each was shown to fail
  against the bug it guards; `docs/lessons.md` is complete.
- **The design:** every row of `docs/rewrite-design.md`'s tables is
  implemented, or the document was changed with a reason. Audit it line by
  line at the end.
- **Dependencies:** `cargo tree -e normal --workspace` shows only the
  design's list and their own dependencies. No `bevy`, `nix`, `portable-pty`,
  `hyper`, `serde` or async crate appears.
- **fux-vt:** unchanged. If it changed, koh was verified as described under
  Rules.
- **Packaging:** `cargo publish --dry-run --workspace` packages both crates.
- **Docs:** the README describes installation, every command, every key, the
  config file, the `--json` shapes and the security model, and nothing that
  does not exist.
- **The PR:** open from `rewrite` to `main`. Its description covers what was
  built, what was dropped and why, the two defaults above, anything known and
  unfixed, and how it was verified.

## Cleanup

- Remove containers, the Linux volumes and images this run created, and
  `~/.cache/fux-rewrite`.
- Stop OrbStack.
- Only signal processes you started.
- Keep the `fux-rewrite` worktree while the PR is open.

## Report

Short, covering:
- each milestone and its evidence;
- the lesson tests and how each was shown to catch its bug;
- dependency and line counts, against the design's estimate;
- anything the design changed, and why;
- the PR link, and what is left for the user to decide.
