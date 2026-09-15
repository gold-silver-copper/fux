# Herdr versus fux + zor + koh: capability audit

**Updated 2026-09-15; original audit 2026-09-12.** The filename is retained for existing references.

The stack has distinctive durable delivery, native correlation and source/check/artifact verification contracts. It is not a whole-product Herdr replacement. Since the original audit, fux has shipped substantial pane/layout interaction and zor has shipped saved-machine supervision, exact remote attachment and guarded remote recovery. Calling either subsystem absent is now incorrect. Representative-repository verification capacity, provider breadth, generic restart restoration, terminal-controller leases, transcript harvesting, graphics, plugins and native Windows remain important gaps. No evidence establishes universal performance or reliability superiority.

## Scope and evidence rules

This update recovered the full deleted audit with `git show aa7bf71^:docs/capability-audit-2026-09-12.md`, then reassessed its categories against the local source checkouts. No reference checkout was fetched, updated or edited.

| Component | Inspected revision | Manifest version / role |
|---|---|---|
| fux workspace | `d8b64bd2508501f9a8fd181420e4b3ccbedd3725` | fux **0.11.0**, zor **0.6.0**, local-ipc **0.3.0** |
| koh | `da712875e4f527b718abe44e9d68f94048e916c7` | **0.12.1**; local reference HEAD equals the committed companion pin |
| Herdr reference | `d184b41fa36923c132629af725ff98bb02aa1b61` | Manifest **0.9.0**; comparison source includes post-release master work |
| Archived stack acceptance records | `483d8cab7f5f4ea87c75c68cc74ed267561e828d` | Historical reports and their raw evidence, not the current product revision |

Versions come from the [fux][fux-manifest], [zor][zor-manifest], [local-ipc][ipc-manifest], [koh][koh-manifest] and [Herdr][herdr-manifest] manifests; the transport dependency is selected by [companions.json][companions]. Herdr's [latest-release metadata](https://api.github.com/repos/herdrdev/herdr/releases/latest), checked on 2026-09-15, identifies stable [v0.9.0](https://github.com/herdrdev/herdr/releases/tag/v0.9.0), published 2026-09-07. Release metadata is not runtime acceptance, and this source comparison must not silently attribute every master feature to the stable binary.

Four evidence levels remain separate:

- **Current implementation:** inspected source, command/API contracts and scenario assertions. A test's existence is not a newly observed passing result.
- **Recorded verification:** retained fixtures, dated reports and prior process/visual runs, with their original revisions and caveats.
- **Real-provider acceptance:** actual provider behavior for a stated version, configuration and scenario. A real TUI using a synthetic local provider is not cloud-model acceptance.
- **Comparative acceptance:** equivalent workflows exercised in both products. It is not established by this audit.

The new executable evidence here is a read-only inventory of committed Git objects. No build, test suite, formatter, linter, live provider, CI rerun, physical-host/WAN experiment or new UI/performance acceptance was performed. “No equivalent found” means no equivalent in the inspected core surfaces; scripts or third-party plugins may supply workflows outside this comparison.

## Capability matrix

“Ahead” refers only to the named built-in contract, not overall quality or universal correctness.

| Area | Herdr reference capability | Stack at the inspected revisions | Assessment |
|---|---|---|---|
| Persistent terminals | Workspaces, tabs, panes, detach/reattach and independent clients | Same essentials, bounded history, retained grids and independent viewer focus | Core subset covered; detach persistence is not restart restoration |
| Interactive layout | Zoom/swap/move, ordering, export/apply, contextual mouse UI | Zoom/swap/move/resize/ratio, live tab/workspace transfers, labels/order, layout export/apply and workspace archives; keyboard/mouse modes | Old missing-layout claim removed; archive application targets existing resources, not resurrection |
| Agent discovery | 21 agents in the principal table, plus less-tested Gemini and Cline | Versioned passive rules for Codex, Claude and OpenCode; Pi startup fixture does not add a built-in rule | Herdr retains a large breadth advantage |
| Prompt delivery | Foreground-occupant targeting, ordered text/paste/Enter, prompt-and-wait | Stable operations, reservation/receipt reconciliation and pinned target identity | Complementary safeguards; generic zor submission remains single-line |
| Prompt completion | Lifecycle waits expressly do not identify an individual turn | Codex thread/turn/user-item correlation; OpenCode session/message ancestry | Stack has stronger explicit correlation contracts, with provider acceptance limits |
| Verified task results | No equivalent built-in retained-source/check/artifact seal found | Immutable verification selection over required checks and captured artifacts | Distinct advantage, still blocked for a normal full fux source tree by enforced bounds |
| Scheduling | Terminal/lifecycle primitives usable by agents, scripts and plugins | Durable dependency groups, admission concurrency, explicit/automatic advancement, pause/cancel | Built-in policy advantage; not a worker launcher, merge engine or automatic verifier |
| Worktrees | Integrated create/open/remove and workspace grouping | Owned creation/removal, durable intent, checkout identity, task linkage and conservative process/check guards | Herdr convenience versus zor ownership/recovery evidence |
| Direct control and observation | Writable per-terminal session with one controller/takeover; separate read-only ANSI observers | Ordinary workspace viewers plus exact-process attachments, capture and event subscription | Exact targeting now exists; no equivalent controller lease or read-only terminal observer authority found |
| Agent transcript retrieval | Conditional active scrolling of recognized idle alternate-screen agents | Host screen/history capture and bounded native response summaries | Application-driven transcript harvesting remains a gap |
| Agent attention | Integrated sidebar, machine aggregation, badges and notifications | zor aggregate/machine-scoped dashboard, task/observed rows, freshness, notifications and exact attach/return | Old absence of cross-host attention removed; broader Herdr operator workflows remain unmatched |
| Multiple machines | Saved SSH profiles, setup, combined navigation, independent reconnect; master CLI routing | Saved koh-backed profiles, `--machine` routing for supported operations, independent readers, catalog reload and aggregate dashboard | Implemented product layer, not just raw proxy sockets; setup/routing breadth and real-host acceptance remain gaps |
| Network transport | SSH/configuration/remote setup ecosystem | Authenticated peer-ID QUIC/iroh opaque gateways; retained byte-stream reconnect | Different transport choices, not a universal winner; published pin lacks structured connection-status reporting |
| Server/machine restart | Session shape/cwd/focus restore, optional screen replay, official agent resume | No general fux restart restore; durable zor lifecycle records, startup reconciliation and explicit eligible provider resume | Partial recovery is implemented; generic session restoration remains missing |
| Live server replacement | Experimental opt-in PTY/process handoff on supported paths | No equivalent fux live handoff | Separate gap; do not present Herdr's experiment as universal support |
| Extension ecosystem | Packaged plugins, actions/events/panes/link handlers, installers and marketplace | Rules, OSC library, hooks, scripts and JSON APIs | No equivalent packaged plugin host/catalog found |
| Terminal richness | Kitty graphics/image API, richer configuration and IME-related options | Text/cells/history, copy/OSC52 policy, contextual mouse controls and application mouse forwarding | Mouse/layout gap narrowed; graphics and wider terminal parity remain unproven |
| Platforms/distribution | Stable Linux/macOS/Windows binaries and install/update paths | Unix-oriented fux/zor/koh; Android compile checks and koh Termux documentation, no native Windows support | Native desktop-platform gap remains; Android compile evidence is not full-stack runtime acceptance |
| Long-running operations | Not certified here as unlimited or failure-free | Finite journals, receipts and intent logs; recovery guards deliberately preserve uncertainty | Needs sustained lifecycle/archival work; bounded helper cleanup is already implemented |
| Performance | Current rendering/input implementation | Retained-grid/delta architecture and historical internal measurements | No matched current-Herdr superiority evidence |

Stack matrix evidence: [layout CLI][layout-cli], [command registry][commands], [machine catalog][catalog], [supervision][supervision], [dashboard][dashboard], [adapter capabilities][capabilities], [verification][verify], [groups][groups]. Herdr matrix evidence: [core API][herdr-api], [CLI][herdr-cli], [agents][herdr-agents], [restore][herdr-restore], [machines][herdr-machines], [plugins][herdr-plugins], [configuration][herdr-config] and [installation][herdr-install].

## Verification capacity: the original blocker still exists

[Source collection][source] reads the **entire committed Git tree** of an open managed task's owned, ready worktree. It is not patch collection or subtree selection. Dirty/untracked/ignored working files are excluded; Git LFS pointers remain pointer bytes. Only regular file modes are accepted; symlinks, submodules and unsupported paths are rejected. File, tree and commit identities are checked before publication.

The fresh inventory at `d8b64bd2508501f9a8fd181420e4b3ccbedd3725` is:

| Committed scope | Blob files | Total blob bytes | Files over 65,536 bytes | Path bytes |
|---|---:|---:|---:|---:|
| Whole repository | 1,112 | 9,050,985 | 14 | 64,241 |
| `crates/fux/` | 142 | 1,736,580 | 3 | 6,224 |
| `crates/zor/` | 221 | 2,735,545 | 3 | 8,845 |

All whole-tree entries have regular modes `100644` or `100755`; no symlink/gitlink is present in this measured tree. Its largest blob is 250,067 bytes. The whole repository exceeds the file-count, per-file, aggregate-byte **and path-byte** limits. Both crate rows exceed per-file and total-byte limits and are diagnostic scopes only, not supported collection modes.

**Conclusion from measurement plus source, not a new failing runtime experiment:** the current retained-source path cannot collect this full fux revision. An unbound live-directory check cannot replace the common retained source required by the verification seal. The table excludes uncommitted document edits, reference checkouts and generated artifacts.

Reproduce the measurement read-only from the fux checkout:

```python
import subprocess

revision = "d8b64bd2508501f9a8fd181420e4b3ccbedd3725"
entries = []
raw = subprocess.check_output(
    ["git", "ls-tree", "-r", "-l", "-z", "--full-tree", revision]
)
for entry in raw.split(b"\0"):
    if not entry:
        continue
    metadata, path = entry.split(b"\t", 1)
    mode, kind, oid, size = metadata.split()
    entries.append((mode, kind, None if size == b"-" else int(size), path))
for prefix in (b"", b"crates/fux/", b"crates/zor/"):
    blobs = [row for row in entries if row[1] == b"blob" and row[3].startswith(prefix)]
    print(prefix or b"whole", len(blobs), sum(row[2] for row in blobs),
          sum(row[2] > 65536 for row in blobs), sum(len(row[3]) for row in blobs))
print("unsupported modes", [(row[0], row[3]) for row in entries
                             if row[0] not in (b"100644", b"100755")])
```

Source-bound checks materialize initial input into fresh **writable** directories without `.git`. They inherit programs/environment and are not OS/network sandboxes or dependency locks. Supporting Git-dependent checks needs an explicit solution. [Verification selection][verify] requires the latest passing required checks on the same source/attempt, no capture failures, and latest required artifacts captured by selected checks. It proves declared evidence and retained bytes, not semantic correctness. [Check execution][check] and [artifact capture][check-artifacts] keep exit, capture and verification outcomes distinct.

### Current resource ceilings

These are separate ceilings, not a promise that all can be reached simultaneously. Serialized journal bytes, pending reservations and native-worker/turn reservations share storage admission.

| Resource / scope | Enforced bound or behavior | Consequence |
|---|---|---|
| One source | 256 files; 64 KiB/file; 256 KiB aggregate file bytes; 32 KiB path bytes; 64 KiB retained commit object | Representative repositories remain too large |
| Sources per journal | 128 records, 512 KiB aggregate file bytes | Finite history; no source compaction API found |
| Checks per journal | 128 records; each submitted check reserves 65,536 bytes **plus 300,000 bytes per requested artifact** in serialized capacity | The old 64-KiB-only reservation description was incomplete |
| Check execution | CLI default 30 seconds; maximum 300,000 ms; 256 KiB captured output per stream; at most 4,096 UTF-8 bytes retained per stream | Not a general long-build/full-log runner |
| Service check admission | Two workers, four queued requests; direct CLI checks bypass this lane | Not a global process concurrency limit |
| Service task reply | Normal three-second client budget extended by twelve seconds for task dispatch; started work can outlive that reply | Inspect the original execution ID; missing reply is not permission to replay |
| Check cancellation/cleanup | No check-cancel API or automatic materialization cleanup found; task cancellation/stop are separate | Time/output failures use bounded owned process-group cleanup, but escaped descendants are not proven dead |
| Artifacts per journal | 64 KiB/file; 128 records including pending captures; 512 KiB bytes including pending 64-KiB reservations | Small evidence reports, not unrestricted build products |
| Task journal | 4 MiB serialized bound; 128 tasks, 512 attempts, 1,024 prompts, 32 groups | Managed history cannot be recycled indefinitely |
| fux input receipts | 128 globally/server, requested lifetime at most 600,000 ms; no eviction of unexpired receipts just to admit input | Backpressure and a bounded reconciliation window |
| fux final records | 128 globally/server, requested lifetime at most 14,400,000 ms; capacity eviction can occur sooner | Four hours is not a guaranteed observation window |
| fux event replay | Per workspace: 1,024 events or 512 KiB; volatile | Handle replay gaps and server incarnation changes |
| Machine catalog | 32 saved machines, 64 attachment bindings per machine, 256 KiB file | Local plus at most 32 independent supervision workers |
| Controller resume intents | 256 records, 512 KiB file; no supported archival command found | Fail-closed evidence, not infinite operation history or replay authority |
| koh gateway | 30-second detached-session retention, 64 clients, 1–1,024 authorized peers | Byte-stream reconnection is bounded and not application restart recovery |

Limits were inspected in [source][source], [journal model][model], [store][store], [check][check], [process runner][process], [service task lanes][service-tasks], [service deadlines][service], [fux resources][resources], [control protocol][control], [events][events], [catalog][catalog], [intent store][intents] and [koh gateway/session code][koh-gateway]. Zor journal durability does not extend fux's volatile receipt or event windows.

## Provider depth and delivery authority

| Provider | Implemented zor contract | Retained evidence and remaining gap |
|---|---|---|
| Codex | Managed app-server stdio; persistent thread; correlated native submit/response/history reconciliation; structured blockers; native interrupt; native recreation | Fixtures and real-fux process scenarios cover adapter paths. Recreation requires a live owned wrapper and materialized native storage; lost-wrapper restart remains unsupported. Approval policy is `never`; recording an input request does not provide an approval-answer API. No retained live-model turn/materialized real-provider resume acceptance for this **native adapter** is claimed |
| OpenCode | Managed plugin, exact armed-input/message ancestry, response/needs-input evidence, producer freshness, explicit native-session resume | Real OpenCode 1.18.29 captures include tool continuation, unanswered permission/question, reload and zor resume across an owned fux restart, using a **synthetic local provider**. No native interrupt. Resume requires retained storage/session metadata, proven original-process absence and eligibility guards |
| Claude Code | Generic managed launch, passive lifecycle rules and headless noninteractive launch policy | Real authenticated CLI viewport/exit captures exist; no managed native correlation, native interrupt or session recreation adapter |

Sources: [implemented capabilities][capabilities], [Codex session policy][codex-session], [OpenCode resume][resume], [integration evidence][integrations] and [versioned fixtures][fixtures]. Capability inspection is static and deliberately says availability is not probed. This audit did not run those commands again.

The retained fixtures now include authenticated **Codex and Claude CLI** working/response/exit traces and a Codex unanswered approval trace. These must not be erased by saying “no real-provider evidence exists.” Conversely, their viewport observations do not prove native adapter correlation or verified tasks. Narrow idle/working/blocker rules remain version/layout-specific; copied screen content can spoof passive evidence. OpenCode's real TUI with synthetic responses establishes application workflow/correlation, not paid-provider acceptance. A Pi startup capture establishes neither a rule nor general support.

Herdr's [21-agent table][herdr-agents] and [17 official resume paths][herdr-restore] are substantially broader. Its Claude/Codex integrations chiefly contribute session identity; their lifecycle authority is still the screen manifest. Other integrations can supply authoritative lifecycle state. Breadth does not imply native per-turn correlation for every agent.

Herdr's [prompt-and-wait][herdr-automation] pins the current agent occupant, refuses already-blocked input, orders prompt/Enter and requires activity from a settled start. It does not identify a particular turn; an already-working turn can satisfy the lifecycle wait. Zor's [generic submission][submit] revalidates root PID/target identity and reconciles the original durable operation, but does not establish which foreground program consumes the input. Its literal generic prompt rejects control characters, including newlines; native Codex is a separate path. Durable receipts, foreground safety, provider correlation and exclusive human control are different guarantees. None implies exactly-once semantic execution of arbitrary work.

## Recovery, scheduling and cleanup

Fux persistence still means keeping its server and pane processes alive across viewer detach. Layout archives apply geometry/labels/order to existing live resources; they do **not** restore deleted containers or processes after server/machine restart. Herdr's [restore implementation][herdr-restore-code] starts new shells in saved cwd and uses supported [native resume plans][herdr-resume-code]; arbitrary old processes do not survive ordinary restart there either. Live PTY handoff is a separate experimental feature.

Zor now has a bounded [startup lifecycle sweep][recovery] that reconciles retained launches without submitting prepared launches or inventing replay authority. Explicit recovery can continue previously authorized managed stop intent. Remote control supports guarded actions and eligible resume, not just inspection. These are meaningful implemented recovery paths, but do not close the following gaps:

- [OpenCode eligibility][resume] rejects membership in **any retained group**, because membership pins the previous attempt. It also rejects submitted/uncertain checks and requires an open task without stop intent. A replacement fux incarnation and prepared destination workspace may be needed; durable records do not create the whole environment.
- [Groups][groups] schedule prepared prompts for existing managed tasks and can gate on explicit verification. They do not launch replacement workers, generate checks, verify output automatically or merge work. Pausing/cancelling coordination is not retracting already queued input or killing workers.
- [Check recovery][recovery] changes abandoned submitted executions to `Uncertain` only when no cooperating runner can publish. It explicitly does not prove that children stopped. [Worktree removal][worktree] retains the unresolved check/launch guard.
- [Task forgetting][tasks] refuses managed launch history and delivered/uncertain prompt history. No general managed-history compaction, group deletion/rebinding or uncertain-check resolution API was found.
- [Owned process cleanup][process] is implemented for controller-owned helpers/check subprocesses, including bounded waiting and avoiding later signaling of reaped identities. It must not be described as absent; equally, it is not authority to terminate adopted panes or proof of escaped-descendant cleanup.
- [Controller resume intents][intents] persist original route/task/attempt intent before dispatch and can be inspected after controller restart. Retained or absent operation evidence never authorizes automatic replay; archival and broader mutation-intent coverage remain unfinished.

## Remote operation and human intervention

The old audit's “no machine catalog/unified cross-host view” is obsolete. [Catalog code][catalog] stores stable machine IDs, optional control binding and explicit per-workspace attachment bindings. Unknown machine selection must not become Local. [Independent supervision][supervision] uses one bounded worker per machine, a six-second read budget, one-second polling interval and five-second freshness limit; failed reads preserve old data without renewing its authority. Selection carries machine, service, task/attempt/session or process identity, not a display label alone. The [dashboard][dashboard] supports aggregate/machine-scoped views and exact attachment/return.

Zor owns koh helper processes and their private proxy directories, not the remote fux/zor owners. Control and attachment require distinct service endpoints; koh owns key loading/authentication and transports opaque bytes. Removing a profile or exiting a controller should retire owned helpers, not stop the remote workspace. A viewer authorized to type into a shell has that account's shell privileges: gateway endpoint separation is not a same-user sandbox or a read-only viewer grant.

**Published-pin limitation:** [koh's actual CLI][koh-cli] has no `--status-file`. [Zor's helper][connection] probes help, uses the socket announcement for local readiness and degrades to generic transport failure with diagnostics. It cannot authoritatively distinguish unauthorized, expired and offline from local EOF. The separately retained development-koh status extension is not shipped by this pin. Local helper readiness is not remote admission. Koh's [30-second reconnect][koh-sessions] and retained byte stream do not restore terminated application processes, and the gateway path does not inherit standalone predictive local echo.

The stack now covers saved supervision and narrow guarded remote task operations; it does not replace Herdr's SSH setup/install/update ecosystem or route every local task/check/group command remotely. Source/worktree paths and task ownership stay at the remote owner. Restarts, live network loss, service identity changes and lack of native provider storage remain distinct failure modes.

Fux exact attachments pin server incarnation, workspace/stream, pane and PID, isolating input to that selected process and closing when its route/identity becomes invalid. They are not Herdr's [single-controller/takeover and read-only observer API][herdr-cli]. No equivalent per-terminal lease/observer authority was found in the current fux control surface. Likewise, capture/history/native summaries are not Herdr's [conditional transcript harvesting][herdr-automation]: Herdr actively scrolls recognized idle alternate-screen agents at the bottom with suitable mouse reporting, restores the bottom, and can reject explicit large reads for working/blocked/unknown agents with `agent_not_idle`.

Herdr's Windows scope needs qualification. Its [Windows page][herdr-windows] advertises native desktop support and saved SSH/standalone remote connections to Linux/macOS/Windows targets, while its connecting-machines documentation still contains narrower language. [Remote preparation code][herdr-remote-code] supports Windows targets requiring a compatible preinstalled package on `PATH`; attach does not install/update Windows packages. Direct terminal attach and live handoff do not gain Windows parity merely because native desktop binaries exist. Neither product's Windows/remote behavior was exercised in this update.

## Recorded verification versus new acceptance

[The current verification index][verification-index] points to archived evidence rather than current-tree report paths. Historical records are checkpoint narratives: their final sections supersede earlier “pending” rows, but do not certify later revisions.

- [Multi-machine checkpoint 27][machine-evidence] records passing Local-plus-two-isolated-stacks composition through the clean published koh pin, guarded remote CLI resume and successful dashboard resume. It records reviewed Betamax frames, exact binary/source provenance and owned-helper cleanup. The stacks are processes on one host, not two physical/WAN hosts. Current [multi-machine scenario assertions][machine-scenario] retain exact-target input, independent authorization failure, catalog reload, viewer SIGKILL/restoration and remote-owner survival coverage.
- That report records **three of four** published-pin lost-reply runs passing; the timing-sensitive lost-reply scenario was not placed in the ordinary composition gate. Earlier development-koh lost-reply evidence and unpublished structured transport-status evidence must remain separately labelled. A configured CI job is not a newly observed successful CI run.
- [Pane/layout records][layout-evidence] include historical implementation and manual/visual acceptance material. [Layout performance][layout-performance] compares changed fux with earlier fux `1792223`, under substantial shared-host contention. It is not a head-to-head Herdr benchmark or rendered-latency certification.
- [Provider fixtures][fixtures] retain binary/harness/version/configuration provenance and explicit negative/untested scopes. Offline validators preserve prior evidence; they do not contact a provider or refresh acceptance.
- The former audit's [CI run 34709567214](https://github.com/gold-silver-copper/fux/actions/runs/34709567214) belongs to the earlier September 12 baseline. Its passing counts and skipped optional koh job are not current-revision results. This audit did not rerun or recertify CI. The current verification index still says a complete final Linux gate, Android runtime, emulator-specific mouse/clipboard, relay/NAT, live remote and paid-provider zor acceptance are not established.

Some living docs conflict with implementation: [service ownership][ownership] still calls the remote controller future despite the machine/dashboard code; README's abbreviated interaction guidance does not enumerate the shipped layout surface. Those omissions do not negate implemented code. This update is scoped to this audit and the [parity prompt](prompts/herdr-parity-prompt.md); the other documents and archived checkpoint ledgers remain unchanged.

## Recommended order and unpassed acceptance gates

Protect existing layouts, machine navigation, exact attachment, durable receipts, native correlation and verification semantics while closing remaining work. Do not rebuild shipped subsystems merely because the original audit called them absent.

| Priority / scenario | Remaining work | Required proof — not passed by this audit |
|---|---|---|
| 1. Representative-repository verification | Scalable source/artifact storage, suitable time/output policy, supported file forms/Git-dependent checks and sustainable retention | Full fux plus representative external projects complete retained source → required checks → captured artifacts → explicit seal; stale/mixed-source evidence rejects. Raising one constant without aggregate/reservation policy is insufficient |
| 2. Supported-provider vertical slices | Native Claude support; deliberate permission/question responses, generic multiline policy, remaining Codex/OpenCode recovery/interrupt gaps | Versioned actual-provider launch/input/correlation/blocker/interrupt/lost-reply/process-death/resume acceptance for every advertised flow; preserve narrow fixture claims |
| 3. Unattended lifecycle | Explicit uncertain-check/launch resolution, managed journal and controller-intent archival, grouped-worker replacement/rebinding | Multi-day soak exceeding current retained limits and 1,000 task/check cycles; injected controller/worker/server failures, no duplicate input, no manual journal replacement or ambiguous cleanup |
| 4. Remote/operator completion | Finish transport-state evidence upstream in koh, broader deliberate workflows and setup; retain working machine catalog/dashboard/attachment | Same product scenarios on at least two physical hosts; WAN loss/NAT/relay/suspend, independent reconnect, stale-state rejection and deterministic lost-reply acceptance on the published pin |
| 5. Generic restart and direct intervention | Session shape/cwd/focus restoration, supported provider resume policy; per-terminal controller lease/read-only observer and return to automation | Restart restores declared state without confusing replay with process survival; takeover/observation avoids misdirected or duplicate input. Advertised live handoff needs separate PTY-survival proof |
| 6. Product breadth | Broader agent integrations, conditional transcript reads, graphics/terminal behavior, plugin packaging and native Windows/install/update | Versioned platform/provider/runtime acceptance and matched Herdr operator workflows; existing layout archives are not session restart support |
| 7. Comparative performance/reliability | Matched releases, enabled features, build profiles, hardware and workload | Retain paired raw startup/idle/active CPU, memory, many-pane/viewer, sustained-output, agent observation, reconnect/restart and terminal-correctness results |

For performance, count **all participating processes**, including zor and both koh endpoints. Measure actual rendered input-to-visible p50/p95/p99 latency, not command acceptance or screen-hash timing. Include warm/cold starts, varied pane/viewer counts and adverse network conditions. Internal architecture benchmarks cannot establish whole-product superiority.

Ownership stays explicit: **fux** owns generic terminal/layout/attachment and any future generic persistence; **zor** owns provider/task/check/artifact/recovery policy and the composed operator UI/catalog; **koh** owns authenticated transport, credentials and reconnect. Any required koh change belongs upstream in its own repository, followed by verification/publication and an ordinary companion-pin update—not an edited reference checkout or hidden dependency patch.

The stack is substantially closer in interactive layout and multi-machine supervision than the September 12 audit said. Its distinctive orchestration contracts remain bounded and unevenly accepted. A whole-product superset claim, parity percentage or delivery date is not defensible from the evidence here.

[fux-manifest]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/fux/Cargo.toml
[zor-manifest]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/Cargo.toml
[ipc-manifest]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/local-ipc/Cargo.toml
[koh-manifest]: https://github.com/gold-silver-copper/koh/blob/da712875e4f527b718abe44e9d68f94048e916c7/Cargo.toml
[herdr-manifest]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/Cargo.toml
[companions]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/tools/xtask/companions.json
[layout-cli]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/fux/src/layout_cli.rs
[commands]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/fux/src/commands.rs
[catalog]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/machines/catalog.rs
[supervision]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/machines/supervision.rs
[dashboard]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/dashboard/multi.rs
[capabilities]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/tasks/capabilities.rs
[source]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/tasks/source.rs
[verify]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/tasks/verify.rs
[groups]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/tasks/group.rs
[model]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/tasks/model.rs
[store]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/tasks/store.rs
[check]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/tasks/check.rs
[check-artifacts]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/tasks/check_artifacts.rs
[process]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/platform/process.rs
[service-tasks]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/service_tasks.rs
[service]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/service.rs
[resources]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/fux/src/ecs/resources.rs
[control]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/fux/src/proto/control.rs
[events]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/fux/src/ecs/events.rs
[intents]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/machines/intents.rs
[codex-session]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/tasks/codex/session.rs
[resume]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/tasks/resume.rs
[integrations]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/INTEGRATIONS.md
[fixtures]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/tests/fixtures/agents/README.md
[submit]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/tasks/submit.rs
[recovery]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/tasks/recovery.rs
[worktree]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/tasks/worktree.rs
[tasks]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/tasks/mod.rs
[connection]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/crates/zor/src/machines/connection.rs
[koh-cli]: https://github.com/gold-silver-copper/koh/blob/da712875e4f527b718abe44e9d68f94048e916c7/src/gateway/cli.rs
[koh-gateway]: https://github.com/gold-silver-copper/koh/blob/da712875e4f527b718abe44e9d68f94048e916c7/src/gateway/mod.rs
[koh-sessions]: https://github.com/gold-silver-copper/koh/blob/da712875e4f527b718abe44e9d68f94048e916c7/src/gateway/sessions.rs
[verification-index]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/docs/verification.md
[ownership]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/docs/service-ownership-contract.md
[machine-scenario]: https://github.com/gold-silver-copper/fux/blob/d8b64bd2508501f9a8fd181420e4b3ccbedd3725/tools/xtask/src/scenarios/zor_multi_machine.rs
[machine-evidence]: https://github.com/gold-silver-copper/fux/blob/483d8cab7f5f4ea87c75c68cc74ed267561e828d/docs/multi-machine-supervision-implementation.md#twenty-seventh-checkpoint-published-pin-composition-in-ordinary-ci
[layout-evidence]: https://github.com/gold-silver-copper/fux/blob/483d8cab7f5f4ea87c75c68cc74ed267561e828d/docs/pane-layout-implementation.md
[layout-performance]: https://github.com/gold-silver-copper/fux/blob/483d8cab7f5f4ea87c75c68cc74ed267561e828d/docs/pane-layout-performance-2026-09-12.md
[herdr-api]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/docs/next/website/src/content/docs/socket-api.mdx
[herdr-cli]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/docs/next/website/src/content/docs/cli-reference.mdx
[herdr-agents]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/docs/next/website/src/content/docs/agents.mdx
[herdr-restore]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/docs/next/website/src/content/docs/session-state.mdx
[herdr-machines]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/docs/next/website/src/content/docs/connecting-machines.mdx
[herdr-plugins]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/docs/next/website/src/content/docs/plugins.mdx
[herdr-config]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/docs/next/website/src/content/docs/configuration.mdx
[herdr-install]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/docs/next/website/src/content/docs/install.mdx
[herdr-automation]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/docs/next/website/src/content/docs/agent-automation.mdx
[herdr-restore-code]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/src/persist/restore.rs
[herdr-resume-code]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/src/agent_resume.rs
[herdr-windows]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/docs/next/website/src/content/docs/windows-beta.mdx
[herdr-remote-code]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/src/remote/attach.rs
