# Review the multiplexer boundary

Fux owns terminal processes, panes, layouts, rendering and generic control primitives. Zor owns
agent interpretation, tasks, worktrees, checks, results, recovery policy and its dashboard. Koh
owns transport. A fux primitive must also be useful to a controller of a shell, build, editor or
debugger without knowing what an agent is.

`cargo test --locked --test agent_boundary --test structure` runs complementary checks:

- AST inventory of production Rust structs, enums and type aliases: fields, types, variants,
  serialization/configuration attributes and CLI declarations must match the reviewed fixture.
  A new generic `metadata` field therefore requires review even with no agent-specific name.
- Macro definitions and invocations are also inventoried. The scanner does not expand macros;
  changed tokens require review so a generic generated state type cannot bypass the inventory.
- AST identifiers and decoded string/byte/numeric literals trip on known agent/transport imports,
  git invocation literals and agent OSC selectors. Macro token trees are inspected too. Direct
  `cfg(test)` modules/functions/types and doc attributes are excluded; other conditional source
  is conservatively included without pretending to evaluate every build configuration.
- Real terminal emulation receives byte-split OSC 7877 reports. Ordinary title, progress, captured
  text and host replies must retain their generic behavior, with no agent-specific interpretation.
- Existing structural checks cover dependency aliases, standalone builds, spawn ownership, bounded
  queues, ECS purity and removal of old agent protocol variants.

The fixture is `tests/fixtures/multiplexer-boundary.json`. Its initial inventory was reviewed for
terminal-only ownership, including generic input receipts, event cursors, final output records,
creation cwd/argv and ordinary terminal progress. These are not task records or success signals.
The parsers are dev-dependencies only; the runtime multiplexer does not import them or zor/koh.

For an intentional generic API/model change, generate a candidate inventory with:

```sh
FUX_UPDATE_BOUNDARY=1 cargo test --locked --test agent_boundary
git diff -- tests/fixtures/multiplexer-boundary.json
cargo test --locked --test agent_boundary --test structure
```

Retain the update only after reviewing the code and inventory together against the ownership
rule. Explain the generic consumer and test the contract with shell/build-like fixtures. Do not
accept a snapshot regeneration as evidence that a new field or command belongs in fux. CI does
not set the update variable; normal tests compare without updating the fixture.

This gate makes structural drift reviewable; it is not a proof of all possible program semantics.
Disguised policy inside existing function bodies, generated code outside the scanned source, or
changes to the check itself still require full-diff review. An absence of known keywords is not
evidence of architectural correctness. Agent-specific parsing, launch/resume templates, git policy,
task state and dashboard behavior belong in zor even if they can be hidden behind generic names.
