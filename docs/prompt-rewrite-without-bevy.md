# Rewrite fux without Bevy

## Objective

Build the fux specified in `docs/rewrite-design.md`: a terminal multiplexer
with no Bevy, no RPC and no async runtime. One server thread runs a `poll`
loop over PTYs and a Unix socket, and everything is controlled through the
`fux` CLI and keyboard overlays. Work on the branch `rewrite` (worktree
`/Users/kisaczka/Desktop/code/fux-rewrite`), milestone by milestone. End with
a PR from `rewrite` to `main` that the user can try by hand and merge.

**This first version is functionality only.** Build what a user sees and
uses:
- panes, tabs and workspaces;
- several clients with independent views;
- the command column, the choosers and the action menus;
- copy/select mode and paste buffers;
- the CLI and the config file.

Test each part with ordinary unit and integration tests. Do **not** build
fuzz targets, benchmarks, a black-box harness, lesson-by-lesson regression
tests or long CI jobs. The design lists those under "Later, not in the first
version".

The design document is the specification. Read it in full before starting,
and treat its tables and lists as requirements. If the code has to depart
from it, change the document in the same commit and say why in the commit
message. Never let the two disagree.

## State at the start (2026-09-24)

Re-check each point; this is context, not proof.

- `main` is `e2b114f`: the Bevy fux, 0.12.0, with PR #52 (the BRP policy
  guard) merged.
- `fux-vt` 0.1.1 is on crates.io. It has not changed since it was published.
- Branch `rewrite` (local; push it when you start) holds only the design
  commits on top of `f86dee7`. Rebase it onto `e2b114f` first. Only
  `docs/rewrite-design.md` and this prompt are new there, so it will not
  conflict.
- The user's decisions are recorded in the design's "Decisions" table:
  - detach and reattach;
  - several clients with independent views;
  - no layout saving;
  - **no mouse at all**;
  - a keyboard copy/select mode;
  - the full keyboard command column, choosers and action menus;
  - the config as a file of fux commands, with zero dependencies;
  - OSC 52 clipboard writes on by default;
  - functionality only in this first version.
- Two defaults are mine, not the user's. List them in the PR description as
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
tests are fine if they are small. Ask before adding anything heavier than
`tempfile`.

**Lints:**
- Keep today's clippy lints: forbid `unwrap`, `expect`, `panic!`,
  `unreachable!`, `todo!` and `unimplemented!`; warn on `indexing_slicing`.
- `cargo clippy --workspace --all-targets -D warnings` must pass on macOS and
  on Linux. Code behind `cfg(target_os)` is checked only on its own
  platform; CI's `ubuntu-24.04` job is the Linux check.
- `unsafe` only where the design allows it, each with a `SAFETY:` comment.

**Tests:**
- Every milestone lands with unit tests for its logic, and an integration
  test of its usable end state: a real server and PTYs, driven through the
  CLI and a scripted attach client.
- A fix to something that already worked needs a test that failed first.
- Never weaken a test to make it pass. Change one only when its assumption is
  proven wrong, with the evidence in the commit message.

**Commits:**
- Every commit builds and passes `cargo test --workspace --locked` on macOS.
- Messages say what changed and why, with the evidence.
- CI runs only on pull requests to `main` and on `main` itself. So open the
  PR as a draft at the end of milestone 1, and push `rewrite` at each
  milestone. Read CI's result before going on.

**Behaviour to carry over:** where the design says "as today", read the Bevy
code at the `bevy-final` tag (`git show bevy-final:src/…`). Port the
behaviour and its tests, not Bevy's structure.
- `encode.rs` has no Bevy at all. Port it and its tests nearly whole.
- `paste.rs`, `selection.rs`, `chrome.rs`, `actions.rs` and `interaction.rs`
  hold the rules for pastes, selection, the bar and the overlays.
- The `tests/design/` suites describe the overlays' expected behaviour key by
  key. Use them as a checklist, and drop only what involved the mouse.
- The design's process, input and socket rules carry what the Bevy version
  learned the hard way. The numbers in parentheses point into
  `bevy-final:fux-fuzz/BREAKS.md`. Implement them as written, even though
  this version does not add a dedicated test for each.

## Traps the previous runs fell into

- `$?` after a pipe is the last command's status. `grep -c` exits 1 when it
  counts zero.
- Something on this machine deletes `target/` during long runs. Copy
  binaries elsewhere before long runs, and check that a binary exists before
  trusting a result.
- A reboot wiped `/tmp` mid-run and lost every scratch file. Keep scratch in
  `~/.cache/fux-rewrite`.
- Python heredoc edits mangled byte escapes. Re-read a file after every
  scripted edit.
- In integration tests, a fork in one test can inherit another test's PTY
  slave before close-on-exec is set. That corrupted captures, and on Linux it
  made a dropped writer's newline echo back. Open PTYs and fork only while
  holding one test-wide lock.
- A test that snapshots state right after an action can race the action's
  asynchronous parts: a new process's pid, a PTY resize. Wait for the state
  first.
- macOS `waitid` reports stopped children even with only `WEXITED`, so check
  the status kind, not just the PID (020).
- rustix's `OpenptFlags::CLOEXEC` does not exist on macOS. Set `FD_CLOEXEC`
  with `fcntl_setfd`.
- A test that waits for a marker must print the marker after the state it
  confirms: the modes first, then `READY`.

## Work, in order

Follow the design's Plan.

- **0. Tag and archive:** tag `bevy-final` at `e2b114f` and push the tag.
  Rebase `rewrite` onto it.
- **1. Delete and set up:**
  - Delete `src/`, `tests/`, `fux-fuzz/`, `fux-agent-exercises/`,
    `verification/`, and the tracked `docs/prompt-pi-*.md` files (they drive
    the deleted agent exercises). Keep `docs/rewrite-design.md` and this
    prompt. The rest stays at `bevy-final`. Untracked files in the user's
    main checkout are theirs: leave them alone.
  - Set up the workspace (fux-vt plus the new `fux` at 0.13.0), a README that
    describes only what exists, and CI trimmed to the `verify` job, as the
    design says.
  - The skeleton: the command tokenizer, CLI dispatch, socket rules, protocol
    codec, and a server that answers `fux ls` and `fux kill-server`.
  - Open the draft PR from `rewrite` to `main`.

Then Plan steps 2–8, each a milestone that ends usable:

| Milestone | Usable end state |
| --- | --- |
| 2 | `fux` attaches to one shell; detach and reattach; resize works; `capture-pane` shows the screen |
| 3 | Splits, focus (next, previous, last, directional), resize by weight, zoom, the bar |
| 4 | Workspaces and tabs; two clients with independent views; PTY size is the minimum across viewers |
| 5 | The whole CLI table, `--json` on `ls` and `capture-pane`, targets and their errors |
| 6 | The config file, `set`, `bind`, `unbind`, `reload` with all-or-nothing errors, the prefix key |
| 7 | The command column, the choosers, action menus, the `:` prompt, rename, confirmations |
| 8 | Copy/select mode (motions, search, char/line/block selection), paste buffers, OSC 52 |

After milestone 8, use it once yourself on macOS: attach, split, open tabs
and workspaces, walk every overlay, copy something from history, detach and
reattach. Fix what is broken, with a test, or list it in the PR as known.
Then mark the PR ready. The PR asks the user to try it by hand before
merging.

## Done means

All of these, checked at the final head of `rewrite`:

- **Local gates on macOS:** `cargo fmt --all --check`, clippy with
  `-D warnings`, and `cargo test --workspace --locked`.
- **CI:** `verify` green on `ubuntu-24.04` and `macos-15` for the PR.
- **The design:** every row of `docs/rewrite-design.md`'s tables, apart from
  "Later, not in the first version", is implemented, or the document was
  changed with a reason. Audit it line by line at the end.
- **Dependencies:** `cargo tree -e normal --workspace` shows only the
  design's list and their own dependencies. No `bevy`, `nix`,
  `portable-pty`, `hyper`, `serde` or async crate appears.
- **fux-vt:** unchanged. If it changed, koh was verified as described under
  Rules.
- **Packaging:** `cargo publish --dry-run --workspace` packages both crates.
- **Docs:** the README describes installation, every command, every key, the
  config file, the `--json` shapes and the security model, and nothing that
  does not exist.
- **The PR:** open from `rewrite` to `main` and ready for review. Its
  description covers:
  - what was built;
  - what was dropped, and why;
  - what is deferred to a later version;
  - the two defaults above;
  - anything known and unfixed;
  - how it was verified.

## Cleanup

- Remove `~/.cache/fux-rewrite`.
- Only signal processes you started.
- Keep the `fux-rewrite` worktree while the PR is open.

## Report

Short, covering:
- each milestone and its evidence;
- dependency and line counts, against the design's estimate;
- anything the design changed, and why;
- the PR link, and what is left for the user to decide.
