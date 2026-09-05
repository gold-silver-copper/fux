# Security model

Fux is a local multiplexer, not a process sandbox. Pane commands run with the authority of the
user who launched the session server. The local OS account is the authorization boundary.

## Trust boundaries

| Boundary | Enforcement | Limits |
|---|---|---|
| Local attachment and control | Private owned directories and sockets, kernel peer UID checks, bounded messages, and protocol validation. | Other processes with the same UID may control sessions. Root and a compromised user account are outside this boundary. |
| Optional remote access | Koh authenticates transport identities and checks its allowlist before connecting to the fixed local service. | Fux does not authenticate remote identities or manage keys. A permitted gateway conveys the local user's authority. |
| Pane output | Terminal emulation and capture use bounded state. | Commands can emit titles, bells, clipboard requests, OSC 7877 reports, and misleading text. |
| Observation | Fux parses bounded metadata and supervises the optional observer process. | Agent status is not an authenticated claim. Sequence numbers do not establish reporter identity. |

Default fux has no network transport or cryptographic identities. Its descriptors contain local
socket paths and protocol versions. Existing identity files from older versions are left untouched.
Network policy, key storage, encryption, discovery, and relay configuration belong to koh.

## Defensive limits and lifecycle

Local attachment uses bounded length-prefixed frames, a version handshake, frame completion and
write deadlines, bounded client counts, and bounded producer queues. Client detachment removes its
viewport but leaves pane processes alive. Endpoint shutdown cancels and reaps its connection tasks.
See [the attachment contract](local-attachment-protocol.md) for the wire limits.

Control requests, dimensions, panes, tabs, popups, cell text, metadata, clipboard data, scrollback,
argv/environment entries, captures, and event queues have explicit bounds. Slow control subscribers
are disconnected instead of accumulating unlimited output. Runtime names reject path separators,
empty names, `.` and `..`. Unsafe ownership, permissions, symlinks, and non-socket collisions are
rejected. Stale socket cleanup must identify a refused connection and preserve replacement nodes.

Fux starts commands directly. Optional zor sidecars cannot hold the pane's PTY open by owning the
command. Fux terminates and reaps failed observers; observer failures clear stale status without
terminating the observed pane. Detection and sampling logic remain in zor.

## Terminal output and reporting

Clipboard writes can replace text a user intends to paste; enable them only for sessions where
that behavior is wanted. Agent reports and titles must be treated as presentation metadata by
any automation. Terminal-emulator handling of unknown OSC sequences still needs manual coverage;
OSC 21337 is observation-only.

When reporting an issue, provide revisions, platform, terminal emulator, a minimal reproduction,
and redacted diagnostics. Do not include captured terminal secrets, keys, environment dumps, or
private command histories.
