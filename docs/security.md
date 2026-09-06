# Security model

fux is a local multiplexer, not a sandbox. Pane commands run with the authority of the user who
started the session server, and that OS account is the authorization boundary.

## Trust boundaries

| Boundary | Enforcement | Limits |
|---|---|---|
| Attachment and control sockets | Private (0700) directories owned by the effective user, 0600 sockets, kernel peer-UID checks on both sides, a fixed preface before any command, bounded frames and deadlines | Any process running as the same user can control sessions. Root and a compromised account are outside the boundary |
| Server election and startup | `flock`-serialized manager election, inode-aware stale-socket recovery, a private nonce-named readiness channel, sanitized daemon environment | The daemon inherits the first viewer's environment minus credential-like keys |
| Pane output | vt100 emulation with bounded dimensions and history; control strings are filtered and truncated; titles and OSC 52 payloads are bounded | Programs can emit misleading text, titles, bells and clipboard writes |
| Remote access | Not part of fux. A koh gateway authenticates and authorizes peers before opening the local attachment socket and conveys the local user's authority | fux cannot distinguish a gateway from a local viewer |
| Observation | Not part of fux. zor reads `list`/`capture` over the control socket like any local client | Agent state is zor's presentation, never an authenticated claim |

There are no cryptographic identities, key files or network listeners. Descriptors contain a pid,
an instance nonce and socket paths only; the protocols carry no version numbers.

## Bounds

- Attachment: 64 KiB client frames, 4 KiB input chunks, 16 MiB server frames, five-second frame
  and write deadlines, 64 attachments per workspace, 64-message viewer outbox (slow viewers are
  disconnected, panes unaffected).
- Control: 1 MiB frames, two-second preface deadline, 64 connections per workspace, 128 KiB
  captures (text, or the total row text of a `rows` capture), 100 000 scrollback rows, 64 KiB
  `send-keys`, 128-byte labels, 32 event filters, 1024-event subscriber queues. `info` reports
  these and the configured session limits.
- Session: 64 workspaces, 32 tabs and 128 panes per workspace, 512×512 pane cells, 256 queued
  viewer requests during a creation barrier, per-step ingest budgets (64 pane chunks of at most
  64 KiB from a 256-deep channel and 256 ingress requests per collection, two collections per
  step when an output stream waits its 1 ms for more chunks) and signal polling between busy steps so a hot pane cannot starve input, timers,
  exit handling or shutdown.
- Configuration: 1 MiB file, 128 argv entries of at most 4 KiB, 16 KiB total per command.
- Names: workspace names and labels reject path separators, `.`/`..`, control characters and
  empty strings.

## Lifecycle safety

Closing a viewer never terminates panes. Confirmed close, `kill`, workspace kill and shutdown send
SIGHUP to the owned process group, SIGKILL after a one-second grace, and reap the leader; a
counted reap gate keeps the leader un-reaped (reaping is polled under the gate) until the group is
signalled, so a descendant ignoring SIGHUP cannot survive and a recycled group id is never hit.
A viewer attachment only sees and acts on its own workspace's panes; `workspace kill` over a
workspace connection is limited to that workspace. The server exits only after its adapters have joined every reader, writer and
spawn task. fux never kills an unrelated or older server on its own: a server that answers with an
unexpected frame or reply is reported as an error, and stopping it is the operator's explicit act
(`fux workspace kill`, or a signal to the pid in its descriptor).

## Terminal output and clipboard

`clipboard = "write-only"` lets copies and application OSC 52 writes reach the enclosing
terminal's clipboard, bounded to 1 MiB encoded and emitted once per copy; the default is
`disabled`. Titles and progress reports are presentation metadata. Automation should treat
captured text as untrusted.

When reporting an issue, include the fux revision, platform, terminal emulator, a minimal
reproduction and redacted diagnostics. Do not include captured terminal contents, environment
dumps or command histories.
