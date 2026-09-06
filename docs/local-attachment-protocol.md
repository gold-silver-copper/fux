# Local attachment protocol v6

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
| `{"type":"hello","version":6,"rows":24,"columns":80}` | negotiate and declare the terminal size |
| `{"type":"input","bytes":[108,115,10]}` | byte-exact input for the viewer's focused pane |
| `{"type":"mouse","event":{"code":64,"column":12,"row":3,"release":false},"generation":7}` | an SGR mouse report for the application under the pointer; `generation` names the frame that was hit-tested. Stale generations are ignored |
| `{"type":"control","request":CONTROL_REQUEST}` | a control-protocol request executed in order with this viewer's input |
| `{"type":"view","request":9,"pane":1,"offset":40}` | private history read: the pane's viewport starting `offset` rows above the live screen |
| `{"type":"resize","rows":40,"columns":120}` | the viewer's new terminal size |
| `{"type":"detach"}` | release the viewport; nothing after it is applied |

## Server messages (external variant tag)

| Message | Meaning |
|---|---|
| `{"hello":{"version":6}}` | negotiation complete |
| `{"bindings":{"bindings":{"prefix":1,"bindings":{"124":"split-side",…}}}}` | the prefix and bindings, once after the hello (they do not change while a server runs) |
| `{"state":{"state":UPDATE}}` | this viewer's frame update (below) |
| `{"reply":{"reply":CONTROL_REPLY}}` | reply to one of this viewer's `control` requests |
| `{"view":{"reply":{"request":9,"pane":1,"view":PANE_UPDATE,"history":812}}}` | history read; `view` is a full pane update, or `null` when the pane is gone; `history` is the retained row count |
| `{"error":{"message":"…"}}` | a rejected message; the connection stays open where the error is recoverable |
| `{"exited":{"code":0}}` | the workspace retired; `code` may be `null`. The connection closes after it |

## Frame updates

A `state` message carries an update to the viewer's frame; the viewer keeps the frame and applies
updates in order. Every update carries the viewer's metadata in full: `workspace`, `generation`
(increases with every update; `mouse` reports echo it), `tabs` (id, label), `active_tab`,
`focused`, `layout` (pane id and content rectangle: the viewer's last row is the bar, siblings are
separated by one cell, there is no frame around a pane), `exit_code` and `message`. `panes` holds
only the panes that changed since the viewer's previous update, keyed by pane id; a pane listed
in `layout` but absent from `panes` is unchanged, and a pane the viewer holds that is no longer
in `layout` is dropped. `full: true` means the viewer holds nothing yet (attach, workspace
switch): every visible pane is carried in full and the viewer discards whatever it held.

A pane update carries `rows`, `columns`, `cursor`, `modes`, `title`, `offset`, `exit` and the
carried rows: `lines` lists `{row, wrapped, len}` in row order and `cells` holds the rows' cells
back to back, `len` wire cells per line. With `full: true` every row is carried exactly once and
the viewer builds the pane from scratch; otherwise the pane's `rows` and `columns` must match
what the viewer holds and the carried rows replace those rows. A pane whose size changed is
always sent in full.

A wire cell is `{"text":"a"}` for text (`kind` present only for `wide-leading`), `{}` for one
blank default cell, `{"run":40}` for a run of blank cells, `{"kind":"wide-continuation"}` for the
cell after a wide character, with `style` present only when it is not the default
(`{"foreground":"Default","background":"Default",…}`). A run never crosses a row. Each line must
expand to exactly `columns` cells; text cells hold one grapheme of width one (two for
`wide-leading`) and no control characters; titles are at most 1,024 bytes; a frame carries at most
262,144 cells in total. A viewer rejects an update that breaks these rules or names a pane it does
not hold as a delta, and closes the connection.

`state.state.panes.<id>.cells[].text` is the shape koh's gateway tests consume: the text of the
carried rows, which is where an echoed input lands.

## Ordering

- The server applies a connection's messages in arrival order. Several commands in one read
  observe their predecessors; input queued behind a split, new tab or new workspace is delivered
  to the newly focused pane only after the creation completed. A failed creation is reported as a
  `failed` reply and following input goes to the previous focus.
- After executing a control request the server sends the frame update reflecting the applied
  state before its reply. Updates queued for one viewer with no reply between them are merged
  (later rows replace earlier rows, later metadata wins, untouched panes stay), so a slow viewer
  receives one update whose effect equals applying every merged update in order; an update never
  overtakes a reply that was queued after an earlier update.
- The viewer keeps one control or view request outstanding at a time.
- `detach` applies everything queued before it and drops everything after it. Switching workspaces
  (`workspace select`) retargets the same connection; input after the switch reaches the
  destination's focused pane.
- A viewer whose outbox falls 64 messages behind is disconnected; its panes are unaffected.
- History reads never change focus, selection, viewport or another viewer's state, and only see
  panes of the attachment's current workspace (a foreign pane reads as `view: null`).

Workspace and manager commands use the separate [control protocol](local-control-protocol.md).
Versions 2 (with `pane-input`, `binding` and `copy-view` messages), 3 (box-rectangle layouts),
4 (bar on row 0) and 5 (every frame carried every cell of every pane as `{text, kind, style}`
objects plus the bindings) are no longer served; a viewer meeting one reports the mismatch and,
when interactive, offers to stop the old server or run alongside it.
