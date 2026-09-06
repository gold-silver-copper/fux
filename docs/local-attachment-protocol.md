# Local attachment protocol v5

A workspace attachment socket is `RUNTIME/fux/NAME.attach.sock` in the private fux runtime
directory. The server owns the path and removes only the inode it bound. Both peers check the
kernel peer UID against their own effective UID. fux does not authenticate remote users; a koh
gateway must do so before it opens this socket, and it carries these frames unchanged.

## Framing and limits

Each frame is a four-byte big-endian length followed by UTF-8 JSON. Empty frames are invalid.
Client frames are capped at 64 KiB and `input` payloads at 4096 bytes; server frames at 16 MiB.
Idle time before a frame's first byte is unlimited; once a frame starts it must finish within five
seconds, and writes have a five-second deadline. A partial frame may only be abandoned together
with its connection. At most 64 attachments (including handshakes) may occupy one workspace.

The client sends `hello` first and must receive the server `hello` before it enters raw mode; a
version mismatch is reported and the old server is never terminated to upgrade it.

## Client messages (`type` tag)

| Message | Meaning |
|---|---|
| `{"type":"hello","version":5,"rows":24,"columns":80}` | negotiate and declare the terminal size |
| `{"type":"input","bytes":[108,115,10]}` | byte-exact input for the viewer's focused pane |
| `{"type":"mouse","event":{"code":64,"column":12,"row":3,"release":false},"generation":7}` | an SGR mouse report for the application under the pointer; `generation` names the frame that was hit-tested. Stale generations are ignored |
| `{"type":"control","request":CONTROL_REQUEST}` | a control-protocol request executed in order with this viewer's input |
| `{"type":"view","request":9,"pane":1,"offset":40}` | private history read: the pane's viewport starting `offset` rows above the live screen |
| `{"type":"resize","rows":40,"columns":120}` | the viewer's new terminal size |
| `{"type":"detach"}` | release the viewport; nothing after it is applied |

## Server messages (external variant tag)

| Message | Meaning |
|---|---|
| `{"hello":{"version":5}}` | negotiation complete |
| `{"state":{"state":FRAME}}` | this viewer's frame (below) |
| `{"reply":{"reply":CONTROL_REPLY}}` | reply to one of this viewer's `control` requests |
| `{"view":{"reply":{"request":9,"pane":1,"view":PANE_VIEW,"history":812}}}` | history read; `view` is `null` when the pane is gone, `history` is the retained row count |
| `{"error":{"message":"…"}}` | a rejected message; the connection stays open where the error is recoverable |
| `{"exited":{"code":0}}` | the workspace retired; `code` may be `null`. The connection closes after it |

A frame is viewer-specific: `workspace`, `generation`, `active_tab`, `tabs` (id, label, focused
pane), `layout` (pane id and its content rectangle: the viewer's last row is the bar, siblings are
separated by one cell, and there is no frame around a pane; version 3 sent outer box rectangles
and version 4 put the bar on row 0, which is why the version changed), `panes` keyed by pane id
with `rows`, `columns`,
`cells` (`text`, `kind`, `style`), `cursor`, `modes`, `title`, `exit`, plus `focused`, `bindings`
and `message`. `state.state.panes.<id>.cells[].text` is the shape koh's gateway tests consume.
Frames are validated on both sides (dimensions, cell text length, total cell count).

## Ordering

- The server applies a connection's messages in arrival order. Several commands in one read
  observe their predecessors; input queued behind a split, new tab or new workspace is delivered
  to the newly focused pane only after the creation completed. A failed creation is reported as a
  `failed` reply and following input goes to the previous focus.
- After executing a control request the server sends the frame reflecting the applied state
  before its reply. Frames for one viewer coalesce (latest wins) but never overtake a reply that
  was queued after an earlier frame.
- The viewer keeps one control or view request outstanding at a time.
- `detach` applies everything queued before it and drops everything after it. Switching workspaces
  (`workspace select`) retargets the same connection; input after the switch reaches the
  destination's focused pane.
- A viewer whose outbox falls 64 messages behind is disconnected; its panes are unaffected.
- History reads never change focus, selection, viewport or another viewer's state, and only see
  panes of the attachment's current workspace (a foreign pane reads as `view: null`).

Workspace and manager commands use the separate [control protocol](local-control-protocol.md).
Versions 2 (with `pane-input`, `binding` and `copy-view` messages), 3 (box-rectangle layouts) and
4 (bar on row 0) are no longer served; a viewer meeting one reports the mismatch and, when interactive, offers to
stop the old server or run alongside it.
