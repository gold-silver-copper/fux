# Security model

fux coordinates terminals; it is not a sandbox. Treat commands and pane processes with the same
authority as the user who launched fux.

## Trust boundaries

| Boundary | Guarantee | Explicit non-guarantee |
|---|---|---|
| Remote transport | iroh endpoint identity plus the workspace allowlist controls admission. | A workspace name, descriptor, or endpoint id alone is not authorization. |
| Local control | Owner-only runtime directories and socket permissions, strict schemas and bounded frames. Portable authorization relies on filesystem access; peer credentials are not checked on every supported Unix. | Another process running as the same authorized user may intentionally control the workspace. |
| Pane output | Escape sequences are parsed with bounded state and capture limits. | Pane titles, bells, OSC 52, OSC 7877 and displayed text are attacker-controlled. |
| Agent state | Reports are parsed strictly and adjacent duplicates may be suppressed. | OSC 7877 is not cryptographically attributable; `seq` is diagnostic, not an authorization token. |
| Persistent identity | Workspace endpoint keys are separate from public descriptors and require private permissions. | Backups, debuggers, root and same-user credential theft are outside the process boundary. |

## Defensive limits

Workspace dimensions, panes, tabs, popups, cell counts, cell text, metadata, clipboard data,
scrollback requests, control frames, argv/environment entries, captures, status segments and event
subscriber queues have explicit maxima. Malformed but decodable synchronized state renders through
safe fallback paths. Slow event consumers lose low-value pane-output notifications first and are
eventually disconnected rather than creating unbounded memory growth.

Runtime names accept a narrow ASCII set and reject empty names, separators, `.` and `..`. Operators
should keep `$XDG_RUNTIME_DIR` local and private, avoid placing descriptors or sockets in shared
directories, and verify permissions after copying identities between machines.

## Terminal-specific risks

Clipboard writes are sensitive: a compromised remote pane could replace a copied command. Keep
remote clipboard handling disabled unless the session needs it. Terminal emulators also differ in
unknown-OSC handling; OSC 7877 collision and passthrough behavior needs manual checking on each
supported emulator. OSC 21337 remains observation-only until its origin and schema are established.

`zor` improves state presentation but does not make agent reports trustworthy. Bare-pane fallback
removes detection while preserving shell availability; automation must tolerate unknown agent state.

## Reporting

Do not include endpoint secret keys, captured terminal secrets, environment dumps, socket contents,
or command histories in a public report. Provide the fux/zor revisions, platform, terminal emulator,
minimal reproduction, and redacted diagnostics.
