# Hand off verified worker results

```sh
zor task handoff lead --operation collect-workers \
  --from worker-a --from worker-b \
  --text 'Review both retained outputs and identify integration changes.' \
  --timeout-ms 60000
zor task submit collect-workers
zor task reconcile collect-workers
zor task wait collect-workers --follow --timeout-ms 60000
```

Handoff prepares one durable prompt for an existing receiving task. It requires one to eight
distinct predecessors with [sealed verification](RESULTS.md). Failed, cancelled or unverified
tasks cannot satisfy this dependency. The recipient must be open, able to accept a prompt,
and free of competing prompt coordination on its pane. Self-handoff is refused. Preparation
does not create workers, run checks, merge files or send input.

The single-line JSON prompt includes the instruction, destination and each predecessor's
task/attempt/source, selected check/artifact IDs and verification generation. A structured
`evidence_cli_prefix` identifies the canonical local journal directory, including custom
`--state-directory` configurations. Append `source-inspect`, `source-file`, `check-inspect`
or `artifact-inspect` arguments to retrieve retained evidence. Contents and report tokens are
not embedded. The recipient needs local journal access and a zor binary. This is not a remote
grant or automatic transfer to another host. Journal relocation does not rewrite a prepared prompt.

One transaction records pinned generations, journal path, instruction and exact prompt text.
Journal loading validates that text against the selected evidence. Reordering `--from` arguments
is the same intent. Exact retries return current coordination without resetting the deadline or
sending again. Changed predecessors, instruction, journal path or timeout conflict. Ordinary
`task prepare` cannot claim a handoff operation even with identical text.

Submit, reconcile, report, wait, abandon and cancellation use existing prompt semantics.
Delivery establishes PTY delivery only. A response does not verify the destination task;
the recipient needs its own declared checks and explicit verification. Stopping verified workers
and removing their owned worktrees preserve the references and retained bytes.

Instructions are 1..4096 UTF-8 bytes satisfying normal literal single-line input rules. The
journal path is absolute UTF-8 without control characters, at most 4096 bytes. The complete
prompt must fit the existing 65536-byte input limit; a large selection may be refused even
within the eight-predecessor bound. Existing prompt count, four-MiB journal, writer exclusion
and receipt limits apply. Result prompt summaries expose predecessor generations without instructions.

Run the disposable two-worker scenario with independently built binaries:

```sh
python3 tests/verify/zor_workflow.py target/debug/fux zor/target/debug/zor
```

Both scripted workers receive work before results are collected. One fails a required check
and repairs its output under new prompt/source/check identities. The lead receives a handoff
only after both are verified. The scenario covers CLI/service retries, custom-journal evidence
access, malformed reference rejection, retained results after cleanup and dirty-tree refusal.
It is required by the combined real-zor integration test. Fan-out is bounded to two workers in
this composition of existing APIs; a durable group scheduler remains unimplemented. Scripted
reports do not establish real-agent adapter guarantees or comparative superiority over herdr.
