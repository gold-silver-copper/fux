# Audit of the start: literal-sol (3637b9fa) on today's master

Read whole against master cb4ca30d8 and v1/windows-client 8951c4039. file:line is at this commit.

## Good, keep
- Every run's guarantee is simple: a fresh interpreter (`src/script/bridge.rs:540`) can't leak.
- Scalar transport: bytes, exact float bits, symbol, absent, empty list (`fux-wire/src/callback.rs:23`).
- Server-run checks: `same_run`, `scope` (`src/script/mod.rs:150-175`), four promise tests kept.
- One failure shape: `Failed` with its effect list (`mod.rs:470`), D-25 told by `notice` and `taken`.
- `fuxth`, `fuxth-run`, `fuxed` already under `archived/`, outside the workspace.

## Must change (the brief's Avoid 1, and the speed gap)
1. Handles, requests and answers pass through grift as `(native-value #bytes …)` from a hand-written
   codec, even between local calls (`bridge.rs:742-746, 769-773, 810-888`, `Wire for Opts` at `:890`).
2. A run re-defines every word as text and evaluates it (`bridge.rs:551-563`), then the whole library
   and sessions (`:565-574`), then the config (`:610-627`).
3. Word dispatch: one `native-request` by name, `Word::named` + `position` per call (`bridge.rs:204,321`).
4. `bridge.rs:203-240` special-cases `kind`, `string`; `bridge.rs:300-318` two overlapping id readers.

## Found, not in the brief
5. `fux-wire/src/code.rs`: every doc comment and the `Raise` enum went; `Raise = Code`, so `fail` can
   name a run-loop code. master's distinction is lost, and `Raise::X.into()` lint failures follow.
6. README.md cut from 450 to 67 lines; `docs/json.md`'s content (fields and codes) has no replacement.
7. `src/help.rs`: the overview and topics lost their guide; must be checked against every word.
8. `src/script/words.rs` test module: tests for wrong operands (`a_wrong_operand_fails…`,
   `a_refusal_names_all…`, `a_reader_given_another_kind…`) were cut or moved; each needs accounting.
9. `.github/workflows/ci.yml` still runs `fuxed/test.sh` and names fuxth crates (lines 93-96, 150,
   352-367). `bench/`, `walk/`, `fuzz/` depend on `../fuxth`, which moved.
10. Clippy: 17 errors in `src/script` (arithmetic, wildcard, useless conversions).

## The kept interpreter: what isolation needs
grift's pairs and strings are immutable and a context has no parent accessor, so a run in a child of the
config's context can't reach its parents through `define!`. It still reaches any context the config
holds: `(define! store (context))` in a config, then `(bind store …)` from a key, persists; so does a
record, as a record is a context. Nothing in grift can freeze a context or tell a host one changed, and
no grift change is allowed here. So a reused interpreter can't promise isolation; a rebuilt one can.

**Decision:** a client's worker runs each job in an interpreter nothing has run in, and builds the next
one after the job ends, off the key's path. A key pays nothing for building; a run's definitions, a failed
or stopped run's leftovers and held handles die with its interpreter by construction. Tension: the
brief's "one interpreter kept per client" against "any failure of isolation is a must-fix": isolation
wins, as the speed comes from the same place. A desk measurement can overturn it.
