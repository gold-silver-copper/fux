> Historical record from before the 0.3.0 bevy_ecs rewrite (2026-09-05). The host, popup panes, sidecar supervision, protocol v2/`FUXCTL1` and verification results described here no longer exist. Current architecture: [design.md](design.md); current evidence: [ecs-acceptance.md](ecs-acceptance.md).

# Prompt: build the deterministic fux verification corpus

Implement a deterministic, automated verification system for **fux** and **zor** that exercises
the same product behavior through independent test interpreters, including real binaries and PTYs.
The objective is not merely to increase line coverage. Build an executable behavioral
specification that detects divergence between the pure workspace model, the in-process production
stack, and installed binaries.

This work is inspired by the behavioral corpus, record/replay, independent interpreters,
timeout-guarded lifecycle proofs, and structural guards in 0xPlaygrounds/rig PR #2443. Adapt those
ideas to terminal multiplexing; do not copy Rig source or fixtures.

## Read first

Before editing:

1. Read every governing `AGENTS.md`.
2. Read `docs/build-prompt.md`, `docs/design.md`, `docs/wrapper-design.md`,
   `docs/implementation-notes.md`, `docs/security.md`, and `docs/release-readiness.md` in full.
3. Inventory all existing tests, fixture helpers, injected clocks, process owners, daemon seams,
   PTY adapters, Koh loopback helpers, and CI jobs. Reuse sound infrastructure instead of building
   a parallel testing framework.
4. Record the initial branches, revisions, and `git status --short` for fux, zor, and the pinned Koh
   checkout. Preserve unrelated and pre-existing changes.
5. Read the current public APIs rather than assuming the designs still match the implementation.

Do not edit Koh unless a production seam needed for truthful verification is absent. If a Koh
change is unavoidable, keep it additive, give it direct tests, and report it separately. Do not
weaken a requirement to fit an existing API.

## Required outcome

Create a verification package under `tests/verify/`, or a dedicated non-published
`fux-verify` workspace crate if that produces a cleaner dependency boundary. It must contain:

```text
tests/verify/
  corpus/          # scenario definitions grouped by behavior
  fixtures/        # reviewed canonical transcripts and terminal cassettes
  interpreters/    # model, in-process, and binary drivers
  oracle/          # intentionally small independent reference models
  fixture-child/   # deterministic PTY child executable or scripts
  loom/            # small concurrency/ownership models where valuable
```

Names may follow repository conventions, but preserve these conceptual boundaries.

The suite must run offline, without credentials, external network access, a graphical session, or
ambient user configuration. Every test creates a private temporary home/runtime/config directory.
Tests must not read or modify the developer's real configuration, sockets, keys, clipboard, or
notification services.

## 1. Define scenarios as data

Define a serializable scenario vocabulary. It must express at least:

- daemon start, attach, detach, reconnect, and shutdown;
- workspace creation, selection, switching, and deletion;
- client viewport resize and disconnect;
- child output, expected input, terminal query/reply, signal, and exit;
- prefix commands, ordinary input, bracketed paste, copy-mode input, and mouse input;
- control requests and subscriptions;
- clock advancement and deterministic timeout expiry;
- transport loss, duplication, reordering, and reconnect boundaries where the existing Koh test
  seams permit them;
- expected snapshots, control events, PTY writes, terminal output, exit statuses, and final
  resource ownership.

Prefer strongly typed Rust scenario data. Serialized fixture files must have a schema version and
strict decoding; reject unknown fields. Bound every string, byte payload, collection, dimension,
and step count before allocation or execution.

An illustrative shape is:

```rust,ignore
Scenario {
    name: "split_resize_copy_close",
    initial_size: (24, 80),
    steps: vec![
        Step::Attach { client: "alice" },
        Step::Input { client: "alice", bytes: prefix("|") },
        Step::ChildOutput { pane: 2, bytes: b"hello\r\n".to_vec() },
        Step::Resize { client: "alice", rows: 40, columns: 120 },
        Step::Input { client: "alice", bytes: prefix("[") },
        Step::Input { client: "alice", bytes: b"ggvGy".to_vec() },
        Step::KillPane { pane: 2 },
        Step::Detach { client: "alice" },
    ],
    expected: Expected { /* semantic assertions */ },
}
```

Do not make fixture files encode implementation-private locks, task identities, or concrete thread
schedules. They describe public behavior and observable ownership.

## 2. Produce one canonical transcript

Every interpreter emits the same canonical semantic transcript. It includes:

- authoritative workspace snapshots or stable hashes plus meaningful decoded fields;
- exact dotted control-event names, subscription/request correlation, and event order;
- logical client, workspace, tab, pane, popup, and process identities;
- PTY input writes, resizes, signals, child exits, and retained final-frame state;
- terminal frames or semantic cells, cursor, modes, status, selection, and prediction target;
- lifecycle transitions and cleanup results.

Normalize only inherently nondeterministic values such as PIDs, temporary paths, endpoint IDs,
socket names, cryptographic keys, and monotonic timestamps. Map them to stable first-observed
logical identifiers. Never normalize away ordering, counts, exit statuses, error categories,
payload bytes, dimensions, or resource leaks.

Canonical serialization must have stable field ordering and newline termination. Fixture updates
must be explicit; tests never rewrite goldens unless a separately invoked record command is used.
Record mode must scrub secrets and reject suspicious credentials, bearer tokens, real home paths,
cookies, and private keys before publishing a fixture.

## 3. Implement three independent interpreters

### A. Pure model interpreter

Exercise workspace state, layout, diffs, router, copy mode, control protocol, event model,
compositor, and resource accounting with scripted process/PTY interfaces and an injected clock.
This interpreter must not call the production implementation for its expected results.

### B. In-process production interpreter

Exercise the real `WorkspaceHost`, control server, daemon/manager logic, Koh state synchronization,
loopback Iroh transport, terminal emulator, and task/process owners. Use deterministic fixture
children, private runtime directories, and bounded deadlines.

### C. Binary interpreter

Launch the actual built `fux` and `zor` binaries. Drive fux through a real pseudo-terminal as a
user would. Exercise the real manager socket, control socket, daemon subprocess, Zor wrapper, and
terminal-mode lifecycle. Never substitute an in-process call for a user-visible binary boundary in
this interpreter.

Each applicable scenario must produce equivalent semantic outcomes through all three interpreters.
If a scenario applies only to a production boundary, state that in its metadata and still compare
the in-process and binary interpreters where possible.

## 4. Deterministic fixture child

Build a tiny fixture executable, preferably in Rust, with a bounded line or framed protocol. It
must support:

- announcing readiness through an explicit pipe/socket, never a timing sleep;
- writing exact bytes and split control sequences to its PTY;
- waiting for and recording exact input bytes;
- answering or withholding terminal queries;
- reporting its terminal dimensions;
- changing title, progress, bell, clipboard, and OSC 7877 state;
- forking a child that exits, ignores HUP, holds the slave PTY, or waits for a signal;
- filling stdout or refusing stdin to exercise backpressure;
- exiting with a chosen status;
- reporting cleanup through an out-of-band private channel.

The protocol and implementation must be size bounded and have a hard execution deadline. The
fixture must never depend on locale, shell startup files, terminal theme, process scheduler speed,
or utilities whose behavior differs across macOS, Linux, and Termux.

## 5. Required behavior matrices

Implement explicit, reviewable matrix rows. Do not claim coverage merely because a property test
could happen to generate a case.

### Input framing

- Every default prefix command at every byte boundary.
- Multiple commands in one transport chunk.
- Partial prefix and mouse sequences across timer expiry.
- Bracketed paste containing prefix bytes.
- Copy-mode batched keys and every split of CSI/SS3 navigation sequences.
- Unknown sequences remain byte exact outside modal input and are consumed under documented modal
  policy.

### Pane and process lifecycle

- Bare and Zor-wrapped panes.
- Normal exit, nonzero exit, HUP, TERM, INT, hard kill, and wrapper death.
- Leader exits while a descendant ignores HUP and holds the PTY.
- Exit before first attach, during attach, during output backpressure, and during shutdown.
- Explicit close versus natural exit races publish one close event and the real status.
- The final frame and final OSC state reach the client deterministically before workspace retirement.
- No zombie, descendant, reader thread, writer thread, or worker handle remains.

### Rendering

- ASCII, combining grapheme clusters, CJK width-two cells, emoji/flags, and replacements of wide
  cells with narrow cells and vice versa.
- Hostile peer cells containing controls, multiple graphemes, invalid width/kind combinations,
  oversized strings, and malformed continuation cells render safely or are rejected.
- Initial paint and incremental Buffer diff produce correct bytes without erasing wide cells.
- Tiny terminal sizes, multiple resizes, popups, copy selection, prediction overlays, status
  truncation, agent state, and visibly highlighted blocked panes.
- Synchronized output always closes on success, write failure, cancellation, and Drop.

### Mouse

- X10/default, UTF-8, and SGR encodings.
- Press, release, drag, button-motion, any-motion, and wheel modes.
- Border clicks, content corners, popup coordinates, and clipped small clients.
- Host re-encoding matches the application-requested mode and never forwards border coordinates.

### Workspace and daemon lifecycle

- Zero, one, and multiple named workspaces.
- Bare fux opens the picker when multiple workspaces exist.
- Prefix-s lists and switches real named workspaces by clean detach/reconnect.
- Two simultaneous first clients elect one manager/workspace and both attach successfully.
- Stale, symlinked, oversized, wrong-owner, wrong-mode, wrong-name, and previous-instance
  descriptors are rejected or recovered safely.
- Failure after endpoint creation but before control-server readiness rolls back every artifact.
- SIGINT, SIGTERM, control kill, manager loss, and last-pane natural exit use the same complete
  teardown path.

### Control protocol and events

- Every request succeeds and fails through its documented schema.
- Empty/default commands and every optional CLI alias field reach the wire unchanged.
- Capture and List either fit the frame or return a structured size error; the socket remains usable.
- Exact dotted event names and request/subscription IDs are asserted on raw JSONL bytes.
- Keyboard, mouse, control, and natural-child mutations publish equivalent authoritative events.
- Slow subscribers shed pane output before state events and are eventually disconnected.

### Resource pressure

- Input and output channel saturation.
- Maximum pane, tab, popup, dimension, visible-cell, scrollback, capture, event, notification,
  connection, and subscriber limits.
- Repeated pane/binding/notification churn proves completed handles and identity maps are reaped.
- Resource units conservatively cover allocation size, including cell structs, text capacities,
  topology, metadata, and terminal history.
- Reconfiguring below current usage fails transactionally.
- A valid maximum request never triggers a larger unbounded intermediate allocation.

### Platform policy

- Linux XDG paths, macOS private HOME fallback, Android `/system/bin/sh`, and Termux `PREFIX/bin`
  lookup.
- Bare executable names search a controlled PATH; explicit paths remain explicit.
- Tests use synthetic environments and do not require the host OS to have Termux paths.

## 6. Terminal cassettes

Add bounded, reviewable terminal-session cassettes where they improve regression coverage. Record:

- child-to-host PTY chunks with original chunk boundaries;
- host-to-child input and terminal replies;
- resizes and signals;
- OSC callbacks and parsed state;
- process exit;
- resulting semantic frames and selected exact terminal bytes.

Replay every control string at all byte boundaries. For most tests compare semantic cells and
events. Compare exact output bytes for synchronized output, terminal-mode enter/restore, OSC 52,
mouse encoding, wide-cell painting, and suspend/resume.

Do not record volatile ANSI output blindly and bless it. Every golden diff must be understandable
to a reviewer.

## 7. Independent oracles and properties

Create deliberately small reference implementations for:

- recursive layout geometry and directional focus;
- prefix/input streaming and ambiguity timeout;
- copy selection and wrapped-line extraction;
- workspace event transitions;
- conservative resource accounting;
- selected terminal grid operations.

Production code must not be called from an oracle. Compare production results to the oracle using
bounded proptest generators. Shrunk failures must print the scenario seed/steps in a directly
replayable form.

Retain deterministic chaos tests for state-sync loss, duplication, reordering, and divergent
bases. Fixed regression rows remain mandatory for previously found bugs.

## 8. Concurrency proofs

Use Loom only for compact ownership/state machines, not PTYs or network implementations. Model
where practical:

- manager election and the first-client race;
- snapshot-versus-notification ordering;
- natural exit versus explicit close;
- shutdown versus a queued control mutation;
- subscriber close versus event publication;
- notification child completion versus shutdown;
- process signal/reap ownership without stale PID/PGID use.

Every non-Loom hang-sensitive test has an outer deadline and a shorter cleanup deadline. A timeout
is a failure, never an ignored test or success fallback.

## 9. Structural architecture guards

Add automated guards, using syntax-aware inspection where reasonable, for these invariants:

- no detached process or task spawn outside approved ownership wrappers;
- no unbounded channels or append-only worker/process registries;
- every spawned child belongs to an owner that signals, reaps, and joins it on every exit path;
- no signaling through informational or already-reaped PIDs/PGIDs;
- no hard-coded `/bin/sh` or `/usr/bin/env` in portable production paths;
- no real-time sleeps in pure tests;
- no ignored tests, placeholder success, `todo!`, or `unimplemented!`;
- Zor without default features exports only the OSC contract and excludes CLI dependencies;
- exact allowed dependency direction between fux, zor, and Koh;
- every control event has its documented dotted wire spelling;
- CI executes all intended packages, targets, features, doctests, and verification suites.

Guards must fail with an actionable message naming the invariant and remediation.

## 10. Ten mandatory end-to-end golden paths

At minimum, implement these through the production interpreters:

1. Start daemon, create workspace, attach, type, render, detach, and reattach to the same state.
2. Two simultaneous first clients elect one daemon and both converge.
3. Split two real PTYs, resize, and verify exact inner geometry and application terminal size.
4. Zor reports working, blocked, and idle; workspace state, status line, notifications, and dotted
   events agree without duplicates.
5. An interactive popup receives keyboard/mouse input and prediction in its own coordinates, then
   exits and is removed.
6. Copy mode over wrapped, wide, and combining content yanks exact text through OSC 52 without
   writing modal keys to the child.
7. Natural last-pane exit delivers final bytes, final agent state, and real status before clean
   workspace retirement.
8. Control kill tears down a shell whose descendant ignores HUP and reports the real exit status.
9. SIGTERM during each startup phase leaves no child, task, socket, descriptor, endpoint, or lock.
10. A remote viewer reconnecting after deterministic loss converges to the exact authoritative
    workspace state without duplicate lifecycle events.

Every path asserts final ownership, not only visible output:

```text
zero owned child processes
zero live worker/task handles
zero registered subscribers
zero private sockets/descriptors for retired workspaces
zero open synchronized-output frames
terminal modes, cursor, title, and screen restored
```

## 11. CI and packaged-artifact verification

Add layered jobs:

### Every pull request

- formatting and strict clippy;
- unit tests and doctests;
- pure corpus and independent-oracle properties;
- protocol/golden tests;
- structural guards;
- bounded Loom models;
- no-default-feature Zor surface test.

### Linux and macOS pull requests

- in-process corpus with real PTYs;
- loopback Koh transport;
- selected full-binary golden paths;
- platform path/runtime policy tests.

### Nightly

- full binary corpus;
- deterministic scheduling/fault matrix;
- extended churn and resource-pressure runs;
- applicable Miri, sanitizer, or model-checking subsets.

### Release gate

1. Check out dependencies exactly as a clean user would.
2. Build and verify the Zor package artifact.
3. Build and verify the Fux package artifact against publishable dependency versions, with path
   overrides removed as Cargo would remove them.
4. Install from the packaged artifacts into an empty temporary Cargo home.
5. Run the binary smoke corpus against those installed binaries.
6. Fail if a required dependency revision is unpublished or a clean checkout lacks its source.

Do not let the dirty development workspace stand in for packaged-artifact verification.

## Implementation order

Proceed in reviewable phases:

1. Scenario schema, canonical transcript, normalizer, fixture child, and one small pure scenario.
2. Pure interpreter plus independent layout/input/event/resource oracles.
3. In-process interpreter and the first three golden paths.
4. Binary interpreter and private environment/process ownership harness.
5. Required matrices and all ten golden paths.
6. Terminal cassettes and replay tooling.
7. Loom models and structural guards.
8. CI layering and packaged-artifact gate.
9. Documentation, fixture safety audit, full review, and final verification.

At each phase, run targeted checks first and then every affected broad gate. Do not commit generated
runtime state, secrets, keys, sockets, build output, or unreviewed recordings.

## Completion gate

Do not declare completion until all of the following are true:

- all ten golden paths pass deterministically;
- applicable scenarios agree across every interpreter;
- every process/task/socket/endpoint/tempdir is owned and cleaned on success, error, cancellation,
  timeout, panic, and Drop;
- tests pass under normal parallel execution and repeated execution;
- no test relies on an unexplained wall-clock sleep or ambient machine state;
- fixtures are bounded, versioned, reviewed, and free of secrets;
- all prior regression tests remain enabled;
- formatting, strict clippy, tests, doctests, Android/no-default checks, docs, package verification,
  structural guards, and relevant Loom models pass;
- a fresh independent reviewer inspects the complete diff and confirms no unresolved P0/P1 issue;
- CI required checks are green;
- clean packaged binaries pass the release smoke corpus.

If an external prerequisite prevents a gate, complete everything independent of it and report the
exact command, error, missing artifact/revision, and remaining work. Never turn an unavailable
prerequisite into a skipped or silently passing test.

## Final report

Report:

- the corpus and interpreter architecture;
- every matrix row and golden path implemented;
- independent oracles and concurrency models added;
- structural guards added;
- fixture recording/replay and secret-safety policy;
- exact verification commands and results;
- packaged-artifact and CI state;
- independent-review findings and how each was resolved;
- any remaining external blocker or risk.
