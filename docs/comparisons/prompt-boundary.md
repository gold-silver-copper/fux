# Prompt boundaries: zor and herdr 0.8.2

This comparison demonstrates one narrow benefit of zor's prompt-scoped response reports: a fresh
response can resolve its wait without an observable working phase. It does not establish real-agent
coverage, task correctness, overall superiority, or a performance advantage. All agent semantics
remain in zor; fux is used through its existing generic API.

The [harness](../../tools/comparisons/prompt_boundary.py) runs the same compiled scripted worker in
private real herdr and fux sessions. Herdr receives explicit state reports; zor receives an explicit
response report scoped to its prompt token and input receipt. These are different integration
contracts expressing analogous logical events. No actual Claude process, credentials or personal
sessions are used. The worker's executable name is `claude` solely to satisfy herdr's foreground
process admission; its output is synthetic and does not test detection manifests.

## Recorded results

The [JSON evidence](prompt-boundary.json) contains three repetitions of each applicable case,
binary hashes, versions, platform, reference commit, harness/worker source hashes, raw responses,
herdr report sequences, observed input, and successful server/worker cleanup. These are functional
outcome counts under a 1,500 ms prompt budget, not timing benchmarks. Every accepted prompt reached
the worker exactly once during these fault-free runs; this does not test reconnect deduplication.

| Scenario | herdr 0.8.2 | zor over fux | Interpretation |
|---|---|---|---|
| Old evidence, silent new prompt | 3/3 timed out with existing global idle | 3/3 timed out with retained response to a prior prompt | Neither accepted the old evidence tested here |
| Response followed by fresh idle, no working transition | 3/3 timed out | 3/3 response-observed | Zor's explicit prompt-scoped report can resolve without working evidence |
| Response with working then idle | 3/3 agent_prompted | 3/3 response-observed | Herdr's activity gate has a passing positive control; zor uses its same response-report path |
| Already blocked before submission | 3/3 agent_blocked, zero input | Not compared | Herdr explicitly refuses this prompt rather than satisfying a wait from old blocked state |

For the old-evidence case, zor first submits a seed prompt, records its response, then submits a
new silent prompt while retaining the old report. Herdr starts with a global idle report. These
are not identical input evidence types. The immediate-response and working-response cases wait
for a worker-authored marker after terminal response output before issuing integration reports.
The harness confirms herdr accepted the working report before sending idle. Zor has no working
report in its prompt-result protocol; both response cases therefore exercise the same path.

All zor task outcomes remain `open`. Neither an `agent_prompted` response nor `response-observed`
means independently verified task success. The harness checks worker PID disappearance after its
own server exits; it never signals an unrelated process or connects to a personal runtime.

## Source explanation and provenance

Herdr's `src/app/api/agents.rs::queue_agent_prompt` refuses Blocked before writing. In
`src/api/wait.rs::prompt_agent`, an initially idle prompt must observe Working or Blocked before
settling to its requested status. A fresh idle-only report does not satisfy that gate. The default
activity budget is five seconds, but this fixture's shorter total budget produces `timeout`.
Herdr's working-to-idle control succeeds. Zor's `src/tasks/wait.rs` can return ResponseObserved
from the current prompt's retained response once delivery and target/input correlation are valid;
it does not require a working screen.

The installed herdr was 0.8.0 and was excluded. The tested 0.8.2 was built from a clean `git archive`
of the reference commit in [build provenance](prompt-boundary-build.json), with Zig 0.15.2 and
locked Cargo dependencies. After compilation, all 2,452 tracked files in the build copy were
compared with the reference: regular-file bytes and symlink targets matched. The source manifest
hash records ordered tracked paths, a NUL separator, and each file's SHA-256 digest. The build
provenance also pins the resulting binary hash to the hash in the runtime evidence. The harness
itself records a supplied binary and reference HEAD; that alone is not proof of their relationship.

Zig's archive was downloaded from the official [download index](https://ziglang.org/download/index.json)
and its length and SHA-256 checked before extraction. A first build failed because generated-helper
paths mixed `/tmp` with `/private/tmp`; rebuilding with canonical paths/private caches passed.
No source changes were made in the reference or its tracked build-copy files.

## Reproduction

Use Zig 0.15.2 for the host architecture. On the tested macOS host, create a disposable build copy
using canonical paths (set `ZIG` below to the absolute toolchain executable):

```sh
comparison_root="$(mktemp -d /tmp/herdr-comparison.XXXXXX)"
comparison_root="$(cd "$comparison_root" && pwd -P)"
git -C references/herdr archive HEAD > "$comparison_root/source.tar"
mkdir "$comparison_root/herdr"
tar -xf "$comparison_root/source.tar" -C "$comparison_root/herdr"
(
  cd "$comparison_root/herdr"
  CARGO_HOME="$comparison_root/cargo" \
  CARGO_TARGET_DIR="$comparison_root/target" \
  ZIG_GLOBAL_CACHE_DIR="$comparison_root/zig-global" \
  ZIG_LOCAL_CACHE_DIR="$comparison_root/zig-local" \
  ZIG=/absolute/path/to/zig-0.15.2/zig cargo build --locked
)
cargo build --locked
cargo build --manifest-path zor/Cargo.toml --locked
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- capture-prompt-boundary \
  --herdr "$comparison_root/target/debug/herdr" \
  --fux target/debug/fux --zor zor/target/debug/zor \
  --herdr-reference references/herdr --repetitions 3 \
  --output "$comparison_root/prompt-boundary.json"
```

The fixture requires Python 3, `/usr/bin/clang`, Unix sockets, and permission to run private PTYs.
Output must be a new file. An outcome change fails the pinned baseline assertions and requires
inspection, not relabeling it as a regression in the compared product. Reference build/benchmarking
is separate from standalone fux gates. The broader seven-scenario comparison remains unfinished.

The first-party capture is now Rust. The historical artifact above is unchanged;
exact Python source is retained as a nonexecutable archive. A seven-case migration
run passed at `/tmp/fux-rust-prompt-capture.json` and records its actual Rust source
and binary hashes. The synthetic worker and baseline assertions are unchanged;
this remains a contract comparison, not a real-agent correctness claim.
