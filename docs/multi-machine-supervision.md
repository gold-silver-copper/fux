# Multi-machine supervision

Implementation status: the select → inspect → attach → return → reconcile workflow works
across Local and two isolated remote stacks and is exercised in ordinary CI against the clean
published koh pin. Machine-routed CLI reads, guarded cancel/stop/reconcile, guarded application
resume (CLI and dashboard), the aggregate and machine-scoped dashboard, exact viewer handoff
(see [the attachment protocol](local-attachment-protocol.md)), catalog reload, machine-aware
notifications, and the transport-loss/expiry/restart taxonomy are implemented and verified on
loopback. The [two-host manual checklist](multi-machine-manual-acceptance.md) provides setup,
navigation, failure checks and cleanup commands; actual physical-host, WAN and Herdr-parity
acceptance is not claimed, and distinct koh transport states require the optional development
koh status extension.

## Build and transport prerequisite

Build the current workspace's fux and zor binaries with the repository's stable toolchain.
Remote commands work against the clean published koh companion pin. The controller probes
`koh gateway connect --help` once per koh executable: when koh advertises `--status-file`
it consumes koh's structured transport transitions, so unauthorized, expired, ended and
offline peers are named distinctly. The published pin does not yet expose `--status-file`;
against it the controller falls back to koh's own socket announcement for readiness, and a
failed connection is reported as one generic transport failure with koh's diagnostic rather
than an inferred authorization verdict. The optional development status extension is retained
in `verification/multi-machine/checkpoint-12/koh-development.patch`, based on the published
revision listed beside that patch; publishing it is a separate authorized step. Use
`--koh-binary /absolute/path/to/koh` to select the executable. Do not modify the pinned
reference checkout to make a composition test pass.

The controller starts only owned local `koh gateway connect` helpers. The remote fux and zor
services must already be running. The aggregate dashboard also requires an existing Local zor
service; it displays an unavailable Local service without auto-starting it.

## Save and select a machine

Given an existing authorized zor control endpoint and an existing client key file:

```sh
zor machine add builder --endpoint ENDPOINT_ID --key-file /absolute/path/client.key --direct IP:PORT
zor machine list
zor machine inspect builder
zor machine rename builder renamed-builder
zor --machine renamed-builder --koh-binary /absolute/path/koh status
zor --machine renamed-builder --koh-binary /absolute/path/koh dashboard --once
```

The endpoint is koh's authenticated service identity, not a friendly alias. `--direct` and
`--relay-url` are mutually exclusive; omit both to use koh's default connection settings.
Actual relay/WAN behavior is not established by the loopback verification below.

Machine IDs remain stable across rename. The catalog defaults to
`$XDG_CONFIG_HOME/zor/machines.json` or `$HOME/.config/zor/machines.json`; override with the
global `--machines-file /absolute/path/machines.json`. It contains version 1, a `machines`
array, stable `id`, editable `name`, optional `control` binding and `attachments` keyed by
workspace. Bindings reference `endpoint`, `key_file`, optional `direct` and `relay_url`.
They contain no secret key material. The catalog and parent directory must be private to
the current user. Limits: 32 remote machines, 64 attachment bindings per machine, 256 KiB.

Koh retains responsibility for unlocking and validating credentials. Controller helpers have
no interactive stdin; configure koh's existing noninteractive credential mechanism before
starting supervision. A missing credential or unsupported helper fails explicitly.

Control and attachment grants are independent. `machine control NAME --clear` removes the
control binding. `machine bind NAME --workspace WORKSPACE --endpoint ENDPOINT_ID --key-file
/absolute/path/key` configures a separate attachment binding; it does not grant control.
`machine remove NAME` removes configuration only. Press uppercase R in the integrated dashboard to reload catalog changes.
Unknown machine selectors never fall back to Local.

## Inspect and act on tasks

```sh
zor --machine renamed-builder --koh-binary /absolute/path/koh task list
zor --machine renamed-builder --koh-binary /absolute/path/koh task inspect TASK_ID
zor --machine renamed-builder --koh-binary /absolute/path/koh task result TASK_ID
zor --machine renamed-builder --koh-binary /absolute/path/koh task cancel TASK_ID
zor --machine renamed-builder --koh-binary /absolute/path/koh task stop TASK_ID
zor --machine renamed-builder --koh-binary /absolute/path/koh task launch-reconcile TASK_ID
```

The remote CLI also routes the existing OpenCode resume policy:

```sh
zor --machine renamed-builder --koh-binary /absolute/path/koh task resume TASK_ID --operation UNIQUE_OPERATION_ID --instance FUX_INSTANCE
zor --machine renamed-builder --koh-binary /absolute/path/koh task resume-status TASK_ID --operation UNIQUE_OPERATION_ID
```

This requires `task-resume-v1`, a managed eligible OpenCode task, retained native-session
evidence and an explicit fux incarnation. A stable operation ID records the resume intent;
connectivity never invokes this command automatically. A real koh composition fixture verifies
successful guarded orchestration with synthetic OpenCode events; actual provider restoration
is not established by that fixture.

`resume-status` requires `task-resume-status-v1`. It reads the retained operation's launch
phase, fux incarnation, previous attempt, session and pane without submitting or reconciling
a launch. A null `record` means no matching resume evidence is retained; it does not prove
that an earlier request was never sent and does not authorize replay. Inspect the task as
well before deciding what to do with an unknown outcome.

Remote CLI resume and dashboard resume save a controller intent before dispatch. Run `zor machine resume-intents`
after a lost reply or controller restart to recover the original machine ID, control endpoint,
service incarnation, task/attempt/process selection, operation ID and requested fux incarnation.
These records live beside the machine catalog in `machines.json.resume-intents.json` (or
beside the explicit `--machines-file`), under private file permissions. They remain intent
evidence even after a successful reply; use remote status/inspection to establish outcome.
Reusing an operation cannot change its task, control endpoint or requested fux incarnation.
Local dashboard intents identify the local service socket instead of a koh endpoint. Cancelling
the input form writes no intent. Cancelling during preparation may leave an intent record even
when no mutation was sent, so a retained intent must never be interpreted as completion.
The log currently retains up to 256 records and refuses additional operations at capacity;
archival controls are unfinished.

In the multi-machine dashboard, select a task and press `u`. Enter
`OPERATION_ID FUX_INSTANCE`, then press Enter to submit once. Escape cancels the form;
Backspace edits it. Use an explicitly chosen fux incarnation and retain the operation ID
for reconciliation. The service checks the selected attempt/process and provider eligibility.
Press `e` from the dashboard to open the full current status/error, then use `j/k` to
scroll and Escape to return. Narrow dashboards show a hint when a status is available.
The form does not select replacement runtimes or replay prompts. Its operation ID appears
in failure messages. To inspect it in the dashboard, select the current task row and press
`U` (uppercase), enter the operation ID, and press Enter. This reads remote evidence and
shows the saved controller intent; `j/k` scroll and Escape returns. It refuses a saved
operation for another task or control endpoint. The action requires a fresh task/service
selection. When a machine is unavailable, use `zor machine resume-intents` to inspect local
intent evidence without contacting it. Successful dashboard resume acceptance remains unfinished.

Use the current profile name (or stable ID) in each command. Replies include machine identity
and service incarnation. Paths and retained task evidence come from the selected host; remote
selection cannot use a local `--state-directory`. Delivery receipts, provider claims and
verified task outcomes retain their original distinctions.

Cancel retires task coordination while preserving pane processes. Stop requires a managed
launch and refuses adopted panes. The client first reads the selected task, then sends a
single action guarded by its attempt, session and process identity. Zor validates that guard
under the exclusive task-store lock, and fux receives only generic exact-process operations.
A changed service or task/attempt is rejected. A failed/lost response can follow a committed
action: the client does not replay it. Inspect the same task and attempt before deciding on
an explicit retry. For a managed launch, `task launch-reconcile` refreshes retained lifecycle evidence without
sending another kill or creating a replacement pane. This is not an exactly-once or durable
operation-receipt guarantee.

## Aggregate dashboard

```sh
zor --koh-binary /absolute/path/koh --fux-binary /absolute/path/fux dashboard --all-machines
zor --koh-binary /absolute/path/koh dashboard --all-machines --once
zor --machine renamed-builder --koh-binary /absolute/path/koh --fux-binary /absolute/path/fux dashboard
zor --machine local --koh-binary /absolute/path/koh --fux-binary /absolute/path/fux dashboard
```

An interactive `--machine` command opens the integrated dashboard on that machine's scope;
Tab still visits the other saved machines and All machines. A saved stable ID also works.
Explicit machine dashboards observe running services and do not auto-start Local. They reject
`--state-directory`; remote scopes also reject a local `--directory` override. A missing
default Local runtime location leaves Local unavailable while remote supervision continues.
The existing one-shot `--machine NAME dashboard --once` command retains its machine/view JSON
envelope. Add `--bell` for terminal alerts and `--notify` for desktop notifications.
`--notification-command /absolute/path/program` overrides desktop delivery only when
`--notify` is present; the program receives title and body as two arguments.

- ? opens the controls reference; j/k scrolls it and Escape returns to the same selection.
  Narrow terminals use compact rows and wrapped status messages. Inspection text wraps, so
  vertical scrolling can reveal long values without horizontal truncation.
- Tab cycles All machines, Local and saved machine scopes.
- R reloads the saved catalog in the background. Renames preserve selection and connections;
  changed control bindings invalidate cached observations. Removed machines release owned
  helpers and leave their former selection unavailable until you select another row.
  Invalid catalogs retain the currently loaded profiles. Finish or cancel a pending action
  before reloading; actions wait until reload finishes. Repeated edits may require a short
  wait for retiring connections to close. Profile removal never stops remote owners.
- j/k selects a row; selections are remembered per scope.
- Enter or i inspects a task or a freshly observed agent; r fetches a task result. In the detail view, j/k scrolls
  and Escape or q returns to the dashboard.
- a resolves the selected task or observed agent's live pane route and starts the fux viewer using that workspace's
  explicit attachment binding. `--fux-binary` defaults to `fux` on PATH.
- c cancels coordination, s stops a managed launch, and l reconciles retained launch evidence.
  Unsupported actions are refused before dispatch. Result, cancel, stop and reconcile require a task row. Observed agents can be inspected
  and attached without creating task records.
- Escape cancels pending preparation. An action already sent may still complete; inspect
  the original task before retrying. A second action is not sent while one is pending.
- Detach using fux's configured binding (default Ctrl-A, then d) to return to the selected
  machine and row. q in the dashboard or Ctrl-C exits and restores terminal settings.

Observed-agent attachment requires the service to hold fresh, identified observation evidence
for the exact pane process. The service resolves its route using its own runtime directory;
clients cannot supply a filesystem path. A replaced or expired observation is refused.

Each machine has an independent bounded reader. Reads have a six-second overall budget and
poll at one-second intervals. Freshness expires after five seconds, including retained row
age. Failures preserve cached evidence as stale without renewing its timestamp. Unconfigured
machines remain visible as unavailable. The one-shot output includes each machine's view,
problem, freshness and observation age. Notifications cover all loaded machines even in a machine scope. New fresh attention is
coalesced over a five-second cooldown using machine, service, task/attempt and process identity.
Names and task details are omitted from desktop messages. Stale evidence never alerts; a
transport recovery or rename does not repeat unchanged evidence. A new service incarnation
is a new source of evidence. Notification delivery is bounded to two seconds, reports failures
in the dashboard, and is stopped before viewer handoff. Controller restarts begin a new
notification history. Selection includes machine, service incarnation, task attempt/session and
exact process identity. A replacement is not silently selected. Attachment preparation runs
outside the input loop, revalidates the route after opening its proxy, and refuses a missing
workspace binding. Viewer focus does not change shared workspace defaults.

The aggregate reader uses `supervision-v1`: one bounded service response containing current
capabilities, service incarnation, passive observations and task overview. It validates all
parts before publishing a view. This avoids creating three retained koh sessions per poll;
koh's retention and capacity limits are unchanged. An incompatible response is shown as a
failed observation, with old evidence retained only as stale.

Dashboard actions borrow the machine observer's existing control gateway. They retain an
ownership lease until their bounded request finishes, so retiring a connection cannot redirect
an in-flight action to another helper. Attachment opens a separate gateway for its explicit
workspace binding. Closing supervision retires each published control connection and cleans
up helpers independently per host; none of these lifetimes owns the remote service or pane.

## Current verification and remaining acceptance

Three real-process composition scenarios run in ordinary CI (the `Pinned koh composition`
job) against the exact built fux and zor binaries and the clean published koh pin, with no
optional prerequisite and no skip:

- `zor-multi-machine` — Local plus two isolated remote stacks under a controlling PTY: saved
  profile lifecycle, an aggregate view with distinct same-named tasks, an unauthorized and an
  unreachable host failing independently without falling back to Local, exact attachment to a
  non-default workspace pane, input isolation, detach and return, a missing binding, an
  unauthorized attachment binding applied by live reload, viewer SIGKILL recovery, cancelled
  preparation, profile removal, terminal restoration, owned-helper cleanup and remote survival.
- `zor-remote-resume` — a guarded OpenCode resume through a real koh gateway and the public
  remote CLI: one new finished attempt, no prompt replay, preserved archived evidence, and
  stale/duplicate reconciliation guards.
- `zor-remote-resume-dashboard` — the interactive dashboard `u` resume control driving the
  same guarded resume: machine-scoped entry, the typed operation/incarnation form, one
  dispatch, the resumed-task detail and a durable pre-dispatch intent.

The deliberately-lost-reply composition (`zor-remote-resume-lost-reply`) proves unknown-outcome
handling with a test-only reply gate; its transport timing is sensitive on the published pin,
so it is retained development-koh evidence (checkpoints 23–24), not part of the always-green
gate. Run any scenario locally with
`fux-xtask scenario NAME FUX_BIN ZOR_BIN KOH_BIN` using absolute binary paths.

The fixture topology is loopback, not independent physical hosts. The [two-host manual
checklist](multi-machine-manual-acceptance.md) is available. Actual physical-host acceptance
and the deliberately-lost-reply gate on the published pin remain pending. Checkpoint 12
retains the real QUIC reconnect/expiry workflow and its reviewed Betamax frames. Do not use
these checkpoints as evidence of universal provider, restart, WAN, SSH/bootstrap or Herdr
parity.
