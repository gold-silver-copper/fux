# View task attention and agent evidence

```sh
zor dashboard
zor dashboard --bell
zor dashboard --notify
zor dashboard --notify --notification-command /path/to/notifier
zor dashboard --once
```

Dashboard starts zor's local service when absent and uses the running service's configuration
when present. `--directory` selects the service endpoint; global `--state-directory`, `--rules`
and `--agent` configure a newly started service. Existing configuration wins. Task rows come
from the service's actual journal, including custom state directories. Merely viewing an absent
journal does not create it. Closing the dashboard leaves zor, fux and workers running.

The ordinary terminal application displays task outcomes, failed required checks/captures,
prompt attention, launch/worktree problems and agent observations across workspaces. Task
and observation rows remain separate: idle, blocked, command exit and a verified task outcome
are different kinds of evidence. Select a row to see its evidence detail. Verified rows identify
the retained source and selected check/artifact counts. Unclassified panes remain explicit.

Group rows show retained coordination separately from tasks and agent observations. They
include scheduling intent (`manual`, `automatic`, or `paused`), admitted count, member count,
concurrency and bounded scheduling failure details. An incomplete paused group or a group
needing attention enters the attention filter and optional bell's normal transition handling.
Unfinished unsent-arm retirement also needs attention, even when the group's canonical status
is `cancelled` or `complete`. For a cancelled group, retry `group-cancel` to advance retirement;
for other groups, inspect the retained operations before choosing recovery. Cleanly cancelled
and completed groups are quiet. Group rows have no pane target or task outcome: automatic
intent does not establish service health, and a group does not authorize focus or cleanup.
Use `task group-inspect`, `group-run`, `group-pause` and the existing recovery commands for
details and actions; the dashboard is read-only except for explicit pane focus.

For managed integrations, fresh native claims take precedence over passive rules when
the pinned pane/process identity and input sequence agree. Missing, expired or mismatched
integration evidence becomes unknown and needs attention; passive idle cannot conceal an
unavailable producer. Unconfigured panes use passive detection. JSON observation rows expose
`evidence`: source, producer, heartbeat age/sequence, claim, freshness, correlation and secondary
passive state. Producer freshness is bounded to six seconds; the dashboard also applies its
five-second evidence limit. Task rows retain coordination history separately, so a recorded
NeedsInput report can coexist with a later idle observation without implying task completion.
See [integration evidence and limits](INTEGRATIONS.md).

Use `j`/`k` to select, `a` to toggle attention-only filtering, Enter to focus a live target,
and `q` or Ctrl-C to exit. Focus revalidates the recorded fux identity before using its generic
pane-focus API. This focuses the pane in its workspace; it does not move a viewer attached to
another workspace. Use normal fux workspace navigation to view that workspace. Closed,
missing or stale targets cannot silently resolve to another pane.

`--bell` emits a terminal bell when a fresh attention entry appears, once per transition into
the attention set. `--notify` additionally requests desktop notifications while this dashboard
is running. It conflicts with `--once`. Existing attention on startup counts as a new entry.
Notification text contains only an aggregate count and an invitation to open the dashboard;
task titles, prompts, paths, output and failure details are not sent to the desktop backend.

Delivery uses `/usr/bin/osascript` with a fixed script and separate title/body arguments on
macOS, or `notify-send` in a Linux desktop session. `--notification-command` selects a custom
executable instead and requires `--notify`. It receives exactly two arguments: title and body.
Zor uses no shell expansion. The command inherits the dashboard environment, with null stdin,
stdout and stderr; its output cannot enter the terminal. Custom commands must stay in the
foreground until finished. They are trusted programs, not a process sandbox: descendants
that daemonize or outlive an exited command are not supervised.

At most one command runs at a time, and attempts are separated by at least five seconds.
During cooldown or delivery, fresh new attention keys coalesce within the bounded snapshot;
resolved, aged or unavailable entries are removed before a later attempt. A service error
clears pending delivery. Each command has a two-second deadline checked by the dashboard
loop. On timeout or quit, an unreaped command's process group is killed and its direct child
reaped; notifier cleanup precedes the snapshot-thread join. Native filesystem stalls and a
busy terminal/focus operation are outside a strict wall-clock guarantee.
If child status cannot establish that the process is still owned and unreaped, cleanup does
not signal its PID or process group. A polling error is reported instead of risking PID reuse.

Spawn failures, nonzero exits and timeouts appear in the dashboard without ending it. A failed
attempt is not automatically replayed: the next new attention transition can trigger another
attempt. Closing/restarting the dashboard discards its notification history and pending keys.
Successful command exit means backend handoff only; OS permissions, focus settings and desktop
availability can prevent visible delivery. The fixtures use a disposable custom notifier;
they do not establish actual OS popup rendering on either platform.

Rendering replaces non-ASCII and
control characters with `?` to keep text from changing display width or injecting terminal
commands. JSON `--once` preserves original text. The UI uses alternate-screen/raw mode and
restores cursor, terminal attributes and descriptor flags on normal exit, handled signals and
errors. SIGKILL cannot run restoration. Extremely stalled terminal output fails under a bounded
write deadline; restoration output is best effort in that case.

The dashboard fetches through one bounded worker/queue about once per second. Observation
and task snapshots have separate sequence/generation markers and are not an atomic combined
transaction. Their service incarnation must agree. Observation age includes acquisition time,
queue delay and elapsed display time. Stale observations render as unknown; aged or stale
evidence cannot trigger bells or focus. Focus checks freshness again for each input action.
Service errors clear the displayed rows and show an unavailable/stale state until a new read
succeeds. Already-retained task outcomes are not inferred from screen activity.

`--once` returns service identity, observation sequence, task generation, actual state directory,
staleness, rows and problems as JSON. The service `overview` task action exposes at most288
task/launch/group rows (including at most32 groups) and up to128 integration summaries without
prompt text, report tokens or artifact/source bytes. Group evidence includes journal generation,
canonical group state, scheduling intent, counts, failure diagnostic, pending retirement count
and up to eight pending operation IDs. The dashboard can add at most128 observation rows.
The raw service snapshot, `zor status` and `zor watch` remain passive; the dashboard performs
the merge with the separately fetched overview. Existing service response
limits still apply; oversized views fail explicitly. The terminal displays a scrollable selection
within at most 160 columns and 60 rows. Synchronous focus and in-flight fetch cleanup retain
their RPC deadlines; native filesystem stalls remain outside strict latency guarantees.

`tests/verify/zor_dashboard.py` tests custom service state, failed-check and blocked attention,
read-only views, live focus, resizing, optional bells, quit/signal restoration and service loss
through real binaries and a disposable PTY. Unit tests cover freshness expiry, safe labels and
tiny terminal rendering. These are UI/protocol fixtures, not expanded real-agent detection
coverage or a measured comparison against herdr.
The manual/automatic group fixtures cover group failure attention, pause/resume, restart,
counts, completion and cancellation. The binding fixture covers cancelled groups retaining
attention after a lost retirement reply, read-only viewing, and clearing attention after retry.
The dashboard fixture also covers notification arguments, hidden command output, unchanged
attention suppression, missing executables, nonzero exit, timeout, child reaping and quit
cleanup. Policy tests cover cooldown coalescing and removal of resolved/stale/lost entries.
