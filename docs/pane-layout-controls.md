# Pane and layout controls

Implementation is in progress; remaining pane/layout controls and acceptance checks are tracked in [the implementation ledger](pane-layout-implementation.md).
The controls below are implemented. None depends on hypertile or a reference checkout.

## Keyboard and focus

With the default Ctrl-A prefix, `z` zooms/restores the focused pane, `o` focuses the next pane
and `u` focuses the previous pane. Traversal follows the split tree's first-child-before-second
order and wraps, including a harmless return to the same pane when only one exists. Keyboard
navigation remains available while zoomed; selecting another pane clears zoom to reveal it.
Existing h/j/k/l directional focus and split/resize controls remain available.
Commands and contextual availability are listed in the command popup and `fux bindings`.

Pane, tab and workspace Close dialogs offer **Confirm close** and **Cancel** rows. Click a row,
press `y` to confirm, or press `n`/Escape to cancel. Clicking outside the dialog also cancels;
the explanatory warning row performs no action. Mouse presses and releases remain captured by
the dialog. If its target disappears before confirmation, no close request is sent.

Prefix `!` (also normalized as `1`) returns to the last focused pane. Repeating it toggles
between the two most recent distinct panes, including tabs and workspaces on the same server.
History belongs to the attached viewer; new viewers do not inherit another viewer's history.
Deleted targets fail safely without selecting a replacement. Cross-workspace navigation respects
the destination viewer limit. Selecting a pane hidden by zoom clears zoom to reveal it.
History is live navigation state and is not serialized into layout archives.

`fux WORKSPACE focus last` and control `{"command":"focus","id":1,"target":"last"}`
use the workspace's default-selection history on a workspace control connection; they cannot
navigate into another workspace or change an attached viewer's private selection. The same control
target sent by an attached viewer uses that viewer's history.

Prefix `=` or **Rename workspace** in the workspace menu edits a shared display label.
Enter saves, Escape cancels, Ctrl-U clears and Backspace deletes. Empty restores the routing
name; labels allow up to 128 UTF-8 bytes without control characters. Pasted newlines do not
submit. The editor captures server, workspace lifetime and viewer identity and cancels when
they change. Workspace choosers show `label (routing-name)` so duplicate labels remain distinct.

Use `fux workspace rename NAME LABEL --instance INSTANCE --stream STREAM` for the same mutation;
pass `""` as LABEL to clear. Obtain the lifetime from the workspace catalog or listing's event
cursor. The control request is
`{"command":"workspace","id":1,"instance":"INSTANCE","stream":42,"action":{"rename":{"label":"Build team"}}}`.
Renaming preserves the routing name, sockets, workspace lifetime, pane IDs, PTYs and running
tasks. Listings/catalogs expose `label` separately from `name`. Archives save and restore labels
atomically; their expected-state check catches concurrent renames. Bare layout imports preserve
workspace labels. An unchanged label emits no change event.

Prefix `;` opens **Rename pane** for the pane focused when the prompt opened. Enter saves,
Escape cancels, Ctrl-U clears and Backspace deletes. An empty name removes the manual label
and restores the application's current title in the status bar. Application title updates do
not overwrite manual labels. Names allow at most 128 UTF-8 bytes without control characters.
The label belongs to the pane, is shared by viewers and follows it through layout and container
moves. Renaming neither writes to the PTY nor changes its process, geometry or terminal contents.
Changing focus keeps the original rename target; target disappearance, a workspace switch or
a changed server instance cancels the prompt. Pasted newlines cannot submit it.

Use `fux rename-pane PANE NAME --instance INSTANCE` for the same operation, with the instance
from `fux info` or `fux list`; pass `""` as NAME to clear it. The control request is
`{"command":"rename-pane","id":1,"instance":"INSTANCE","pane":1,"name":"build"}`.
`fux list` reports the manual `label` separately from the application `title`. A successful
rename advances the tab's layout revision; an unchanged name is a no-op. Bare tree imports leave
labels intact; complete exports and workspace archives restore them as described below.

Prefix `r` enters directional resize, `v` enters directional swap, and `m` enters directional
move. Arrows or h/j/k/l apply that direction to the original target pane; Enter and
Escape finish, retaining already committed edits. Resize grows toward that boundary; uppercase H/J/K/L shrinks. Pasted text never
executes these actions. A moved or closed target cancels the mode instead of changing its target.

Each directional resize key changes the nearest matching ancestor split by 250 ratio units
(2.5 percentage points), clamped to 500–9500. This is a proportion of that split's usable area,
not a fixed number of cells. Uppercase shrinks toward the same boundary. If there is no ancestor
boundary on that side, the operation fails without changing the layout. Small ratio changes can
round to the same cell geometry; only effective dimension changes resize a PTY.

When the terminal is too small, the split tree and its ratios stay intact. Each split reserves
one separator cell when its extent is nonzero, rounds the first child's share down, and gives
the remainder to the second child. A pane with zero width or height has no visible content;
it remains in the pane list and next/previous traversal. PTY/emulator dimensions are clamped
to at least 2×2 even when the visible rectangle is smaller. No process is closed to make room.

Use prefix `o`/`u` to select a hidden pane and prefix `z` to give it the available content area.
Unzoom and enlarge the terminal to recover the original arrangement. A one-row terminal has
no content rows because the status bar occupies that row; keyboard navigation still works, but
displaying pane content requires enlarging it. Zoom is shared by viewers of that tab, and the
smallest attached viewer sets its area. Mouse targets require visible cells.

Prefix `.` opens **Swap with another pane**. Choose a destination by pane ID and manual label
or application title, then press Enter or click its row. Arrows/j/k and the wheel navigate;
Escape or an outside click cancels. The source is fixed when the chooser opens, even if focus
changes afterward. The request swaps those two pane positions directly, including nonadjacent
panes, with no intermediate moves. Changed layout, workspace, server/viewer identity or missing
panes cancel the chooser. This action is also available in the pane context menu and uses the
existing guarded `layout swap` control API. Unzoom first when only one pane is visible.

## Initial split ratio and focus

`fux WORKSPACE split horizontal --ratio 7000 --no-focus` creates a pane on the right while
keeping the current focus. `vertical` creates below; `new` accepts the same options. Ratios
are integers from 500 through 9500 on a 10000 scale, representing the existing pane's share
of the usable extent after the separator. Thus 7000 means 70% existing / 30% new. The default
is 5000 (50/50). Invalid ratios fail before reserving or launching a process. Rounding follows
the split tree's integer geometry rules; tiny dimensions still use the existing PTY minimum.
Explicit `--rows`/`--columns` override the estimated initial PTY size for headless panes.

The default is `--focus`. A focused split selects the new pane and clears shared zoom so it
is visible. On an attached viewer, other viewers keep their private selections. A workspace
control split selects its target tab and new pane in the workspace default selection, including
when `--target` names a pane in a hidden tab; existing viewers retain their private tabs.
`--no-focus` preserves both selection and shared zoom. Input queued behind the creation barrier
then reaches the preserved pane instead of the new one. On the wire, `split` accepts `ratio`
(default 5000) and `focus` (default true). Options after `--` belong to the child command.

## Mouse gestures

Prefix `*` (also `8`) or **Cycle right-click policy** in the pane menu cycles
ordinary right-click ownership. The menu title shows the current policy:

- `auto`: open the menu unless the application has enabled mouse reporting.
- `fux`: always open the pane menu, including over applications that report mouse input.
- `pane`: forward ordinary right-clicks through the terminal's negotiated mouse protocol;
  applications without mouse reporting receive no synthesized mouse bytes.

Alt-right-click opens the menu in every policy. Captured menu presses retain their releases;
changing policy does not leak a release into the application. Other mouse buttons and layout
gesture modifiers retain their existing behavior.

Use `fux WORKSPACE pane-input PANE --right-click auto|fux|pane --instance INSTANCE` to set
an exact policy, or `fux WORKSPACE split horizontal --right-click pane -- COMMAND` to select
it at creation (`new` accepts the same option). Child arguments after `--` are untouched.
Policies belong to the live pane, are shared by viewers, and follow it across tabs/workspaces.
Changing policy preserves the process, terminal contents and geometry; a changed value advances
the tab layout revision to invalidate stale menus/gestures. Setting the current value is a no-op.
Layout exports/archives do not serialize this policy; applying them preserves each live pane's
current policy. Listings omit the `right_click` field for `auto` and report `fux` or `pane` otherwise.


In the default `auto` policy, right-click pane content to open its action menu when the application
has not enabled mouse reporting. When it has, ordinary right-clicks reach the application; Alt-right-click explicitly
opens fux's menu. Right-click a painted tab label or workspace name for its menu. Menus use the
same actions, labels and availability rules as the command registry. Unavailable actions are
dimmed with a reason. Arrow keys or j/k select a row, Enter or a left-click activates it, the
wheel scrolls selection, and Escape or an outside left-click dismisses the menu. Captured click
releases are consumed even if the menu closes before release.

Keyboard equivalents are prefix `?` (or `/`) for the focused pane menu, prefix `'` for the
selected tab menu and prefix backtick for the current workspace menu. A pane menu targets the
clicked pane even when another pane is focused; opening it does not change focus. Tab menus
can rename, reorder or close an inactive tab without selecting it first. A changed server,
workspace, viewer identity, tab catalog or captured pane layout cancels the menu safely.
Pane menus expose rename, split, zoom, explicit or directional swap, move/resize, moves to tabs/workspaces
and close. Workspace menus expose choose, new, rename, reorder and close.

Prefix `q` or the workspace menu's **Close workspace** opens a confirmation. `y` closes all
panes in that workspace and detaches its viewers; `n` or Escape cancels. Other workspaces keep
running. The confirmation captures the workspace name, server instance, viewer identity and
workspace lifetime. Switching workspaces or replacing that lifetime cancels it; the server
independently rejects stale lifetimes. This operates through the current workspace attachment
and does not require manager access. Pasted confirmation text is ignored.

The CLI equivalent is `fux workspace close NAME --instance INSTANCE --stream STREAM`, with
identity values from `fux workspace catalog` or `fux NAME list` (the workspace event cursor's
`stream`). It uses the same guarded control request as the viewer:
`{"command":"workspace","id":1,"instance":"INSTANCE","stream":42,"action":{"kill":{"name":"NAME"}}}`.
Both forms scope the operation to the connected workspace; a stale request cannot close a
replacement workspace that reused its name.

Left-drag an internal separator to resize it. Alt-left-drag pane content onto another pane to
move it beside that target; the nearest target edge selects the side. The selected destination
half is highlighted in cyan, and a hint names the target and side. This marks the insertion side;
the server determines the resulting geometry. The preview preserves text and highlights both
cells of wide characters crossing its boundary. Release applies one atomic edit; Escape cancels
and removes the preview. Both gestures retain their starting
layout revision and cancel if the tab, workspace, pane membership or geometry changes. Output
frames alone do not cancel them. The rest of a cancelled gesture is swallowed until release;
a fresh press also recovers if the terminal lost that release.
Only the captured left button can complete a drag; wheel and other-button reports are ignored
during capture, except for the tab-destination wheel selection described below.
Unrelated releases and motion do not clear cancellation suppression; a fresh press starts a
new gesture. Releasing Alt before the left button
still completes a pane drag. A changed server or viewer identity also cancels the gesture.
Unmodified content clicks and application mouse input retain their existing routing; Shift
retains its history/copy override. Layout dragging is disabled while zoomed.

Gestures do not continuously resize PTYs while the pointer moves: the release commits the
change. Server-side hit testing verifies that the original separator still exists. Border
positions round toward the requested terminal cell, subject to ratio bounds.

Alt-left-drag a pane onto a visible tab label to transfer it into that tab, beside its first pane.
The destination label highlights in cyan. Drop regions come from the rendered labels, including
truncation, rather than estimated character positions. The gesture retains both layout revisions;
destination edits or changes to tab-bar positions cancel it. To reach tabs that do not fit, keep
the pointer over a visible tab label and turn the wheel: down selects the next destination, up
the previous, wrapping in tab order and skipping the source. The hint names the selected tab;
release without moving the pointer to transfer there. Moving the pointer resumes direct pane
or tab targeting. This uses the destination list captured at drag start. Empty-source handling matches
the keyboard transfer described below.

Alt-left-drag onto the workspace name at the left of the bar and release to choose a destination
workspace. Click its row to move the pane into a new tab there and follow it. The wheel changes
the selected row and scrolls longer lists; Escape cancels. Click targets use the painted chooser
rows, including when the list scrolls. The dragged pane remains the source even if another pane
was focused. This requires a manager connection and uses the same lifetime and routing guards as
the keyboard chooser. The click's release is consumed so it cannot reach the destination shell.

Zoom is shared by the tab. It retains the underlying split tree, each viewer's saved focus and
the hidden terminals' last size. All viewers see and type into the zoomed pane. Unzoom restores
private focus; explicitly focusing another pane clears zoom. Closing the zoomed pane also
clears zoom. Visible geometry uses the smallest attached viewer dimensions, as before.
Moving a pane without changing its terminal dimensions publishes its new position without
resizing the emulator or sending a PTY resize request to the child process.

## CLI and concurrent edits

`fux locate-pane PANE --instance INSTANCE` asks the manager for the current location of an
existing pane process. It works after the pane's original workspace has retired. The reply's
`result.result.value.location` contains the server instance, pane ID, PID, current `workspace`
and lifetime `stream`, current `tab` and `layout_generation`, plus immutable `origin_workspace`
and `origin_stream`, and `accepts_input`. The latter is false after PTY EOF: the process may
still be running and its route remains available for cleanup. Input/liveness consumers must require
`accepts_input: true`; EOF does not prove process exit. Display-label changes affect neither route. Wrong server instances fail
with `conflict`; missing, closed or retiring targets fail with `not-found`. This read-only
operation never creates a workspace, starts a process or changes focus.

The manager request is `{"request":"pane-location","instance":"INSTANCE","pane":1}`.
The successful reply is tagged `"reply":"pane-location"` and wraps a completed control reply
whose result kind is also `pane-location`. Manager access is required: a workspace-only
attachment cannot use this operation to discover panes in another workspace. The location is
a snapshot, not a reservation; mutations still require their normal identity/revision guards.
Lookup preserves workspace-scoped submission authority; zor's remaining managed-launch pins
are described below.

`fux layout TAB inspect PANE` reads a coherent pane geometry snapshot without changing focus,
zoom, layout or process state. It needs no generation or instance argument; an optional instance
still rejects a replacement server. As with export, a supplied generation does not constrain a
read. The control request is
`{"command":"layout","id":1,"tab":1,"action":{"operation":"inspect","pane":2}}`.
The reply's result kind is `pane-geometry`, with the snapshot under `result.value.geometry`:

- `instance`, `tab`, `generation`, `pane` and canonical `document` identify the observed layout.
- `area` is the tab's current content area; `rect` is the pane's underlying split rectangle,
  including when another pane is zoomed. Separator cells lie outside pane content rectangles.
- `visible_rect` is the rectangle used when displaying this tab: full `area` for its zoomed pane,
  null for panes hidden by zoom or without any content cells, otherwise `rect`. It does not mean
  a viewer is currently showing the tab; unshown tabs retain their last area.
- `neighbors` contains `left`, `right`, `up`, `down`, each a pane ID or null. These use the same
  underlying-tree overlap, distance and tree-order rules as directional focus. A hidden neighbor
  is valid: explicitly focusing it clears zoom. `navigation_area` exposes the geometry used for
  this computation. When either area dimension is zero, directional focus and inspection both
  use the existing 1000-by-1000 virtual area; it is not a PTY resize or a visible rectangle.
- `edges` contains four booleans with the same directional names, indicating contact with the
  outer edges of `area`, not with internal separators. All are false for an empty pane rectangle.
  Edge flags refer to the underlying rectangle, even while that pane is zoomed.

Missing or foreign tab/pane identities fail with `not-found`. A viewer resize awaiting layout
resolution returns `conflict`; retry the read after the next frame. Deriving geometry and neighbors
from one tree/area snapshot avoids combining separately timed listing and export reads. Inspection
is workspace-scoped and never discovers panes in another workspace.

`fux layout TAB export` returns a normal JSON reply whose `result.value` contains `instance`,
`tab`, `generation`, `zoomed`, `labels` and `document`. Changes require the observed instance and
layout generation, for example:

```sh
fux layout 1 export > layout.json
fux layout 1 --instance INSTANCE --generation GENERATION swap 1 2
fux layout 1 --instance INSTANCE --generation GENERATION move 1 2 down
fux layout 1 --instance INSTANCE --generation GENERATION resize 1 right 500
fux layout 1 --instance INSTANCE --generation GENERATION ratio 0 6000
fux layout 1 --instance INSTANCE --generation GENERATION zoom 1
fux layout 1 --instance INSTANCE --generation GENERATION zoom
fux layout 1 --instance INSTANCE --generation GENERATION apply layout.json
```

Replace INSTANCE and GENERATION with values from a fresh export. Each successful mutation
returns another export. A stale generation or server incarnation is rejected; inspect before
retrying. A viewer control request is already bound to its server connection, while a control
socket mutation must explicitly supply the server instance.

Swap exchanges pane positions. Move removes/reinserts a pane beside a target within the same
tab. Neither starts or stops a process. Directional resize selects the nearest enclosing
boundary on that side: positive delta grows the branch toward it, negative shrinks it. Ratios
are integer units of 1/10,000, restricted to 500..9500. `ratio` uses the split index in the
exported document, not an internal arena ID.

`apply` accepts either a bare document or the complete successful export reply. A bare document
applies tree geometry and preserves zoom and manual labels; a complete export restores the tree,
saved zoom state (including unzoomed) and labels. Both preserve viewer-private focus. All pane IDs must map to
exactly the existing destination tab's pane set. Use workspace layout archives for the complete existing-container envelope described below.
To target different existing panes, repeat --map SOURCE=DESTINATION for every source pane
on the apply command. A nonempty mapping must cover every source ID exactly once and map
onto exactly the existing destination pane set; partial, duplicate and foreign mappings fail
before any layout change. Import never launches commands.

The control API's apply action accepts `zoom: {"mode":"preserve"}` (the default) or
`zoom: {"mode":"set","pane":PANE_ID}`; use `pane: null` to restore an unzoomed layout.
An explicit zoom target belongs to the source document and follows the same pane remapping.
Unknown zoom targets fail atomically alongside malformed trees or stale revisions. Reapplying
an unchanged tree, zoom and labels does not advance the revision or resize processes.

`labels` is a list of `[PANE_ID, NAME]` pairs. Exports sort it by pane ID and omit unlabelled panes.
For the control API's apply action, omitted/null `labels` preserves current labels; a supplied
list replaces all labels in the imported tree, clearing panes absent from that list. Thus `[]`
clears every label. Label IDs use the source document and follow `--map` exactly as zoom does.
Duplicate IDs, foreign panes, names over 128 UTF-8 bytes and control characters reject the entire
edit before geometry, labels or focus change. Empty names normalize to cleared labels. Labels
restore atomically with the tree and zoom; a concurrent rename makes an older revision stale.

## Moving between tabs and ordering

Automation transfers (`layout TAB to-tab`, `layout TAB new-tab`, and `transfer-pane`) accept
mutually exclusive `--focus` and `--no-focus`; the default is no-focus. No-focus preserves
existing selections and destination zoom. When the moved pane leaves the selected source,
that selection falls back to a remaining pane/tab; it cannot keep an invalid source target.
A newly created workspace necessarily selects its only pane.

`--focus` selects the moved pane in the destination workspace default and clears destination
zoom to reveal it. Existing attached viewers retain their private selections. The same
workspace `transfer` API with `focus:true` sent by an attached viewer also selects the pane
for that requester. This is one navigation outcome: its internal source fallback does not enter
last-focus history. A manager `follow` request moves its named attached viewer and implies
focused transfer, even if `focus` is omitted/false. It checks destination viewer capacity before
mutating either workspace. Other viewers keep private selections, subject to source removal
and the shared zoom change. Both transfer APIs default their `focus` field to false.


`fux workspace export-layout` reads a version-1 archive through the global manager socket.
Its `archive` contains the server instance and an ordered `workspaces` list. Each workspace
contains its name/lifetime stream, default selected tab and ordered `tabs`. Each tab includes
its ID, tab label, layout generation, default focused pane beneath zoom, zoomed pane, pane
`labels` list and tree document. Every tab's labels refer to panes in its saved tree. Archive
apply restores all labels, including clearing omitted entries, and validates all tabs before
committing any change. Its expected archive comparison detects concurrent renames.
This is one coherent read of committed open containers; it does not create workspaces or processes.
Attached viewers' private selections are excluded. Archives exceeding the manager frame limit
fail explicitly.

`fux workspace apply-layout DESIRED_FILE --against EXPECTED_FILE` restores the archive atomically.
Both files accept the bare archive or a complete export reply. Export a fresh expected state before
applying an edited or previously saved desired state. If the current state differs from the expected
archive, the operation fails; desired generation fields are replaced with server-owned revisions.
The archive must contain exactly the existing workspaces and tabs, with matching workspace lifetime
streams, and every existing pane exactly once within its current workspace. Live panes may move
between those tabs. It restores workspace/tab order, tab labels, default selected tabs and pane focus,
trees and zoom without launching processes. Existing viewers keep their selected tab and valid private
focus; focus whose pane moves away falls back to the restored tree's first leaf, independent of
the serialized node-array order. Pending pane creation or
unsettled viewer sizes reject the operation. Malformed trees, foreign/duplicate/missing panes and invalid
focus/zoom targets fail before any change. The combined request must fit the manager frame limit.
Creating or removing containers and cross-workspace routing changes use the separate lifecycle and
transfer operations; archive apply does not perform those transitions.

Prefix `b` moves the focused live pane into a new tab. Prefix `e` opens a destination tab
chooser: arrows or j/k select, Enter inserts the pane to the right of that tab's first pane,
and Escape cancels. The chooser keeps the original source pane and both layout revisions;
concurrent changes are cancelled locally or rejected by the server. Paste never submits a
choice. If the source still contains panes, the viewer stays there; an emptied source closes
and its viewers select a surviving tab.

Prefix `g` reorders the active tab. Choose another tab and press Enter to place the active tab
before it, or press `$` to place it last. Escape cancels. This preserves the current tab/focus.

Tab and workspace choosers also accept wheel navigation and a left-click on a painted entry
to select that destination. An outside left-click cancels. The captured click's release never
reaches the underlying terminal application. Context-menu reorder actions can therefore be
completed entirely with the mouse.

`fux layout SOURCE_TAB --instance INSTANCE --generation GENERATION new-tab PANE [LABEL]`
moves that existing process into a new tab. `to-tab PANE DESTINATION_TAB DESTINATION_GENERATION
TARGET_PANE SIDE` moves beside an existing pane in another tab of the same workspace. Both
source and destination revisions are checked. Transfers are refused while a pane is starting
in either affected tab. The empty source tab is removed and its viewers select a surviving tab;
no replacement shell is launched. Input receipts stay valid because the workspace route and
pane identity have not changed. Cross-workspace transfer uses the manager endpoint described below.

`fux tab reorder TAB [BEFORE_TAB]` places an existing tab before another tab, or last when no
reference is supplied. IDs, focus and processes are unchanged. Missing reference IDs are
rejected without partially changing the order.

Workspace order is set with `fux workspace reorder NAME [BEFORE_NAME]`. Omitting the
reference moves the workspace last. The manager catalog and workspace chooser use this order;
unlisted newly created workspaces follow in name order. Missing names leave the existing order
unchanged. This operation is available through the global manager socket, not a workspace
control endpoint.

Prefix `f` opens the workspace reorder chooser. Select another workspace and press Enter to
place the current workspace before it, or `$` to place it last; Escape cancels. Prefix `y`
names a new workspace and moves the focused pane there when Enter confirms the name. The
viewer follows the pane and remains attached even when the source workspace becomes empty.
Prefix `i` opens the existing-workspace destination chooser. Arrows or j/k select a workspace;
Enter moves the pane into a new tab there and follows it. Escape cancels, including while the
catalog is loading. The chooser retains the original source revision and destination workspace
lifetime: deleting and recreating a workspace with the same name cannot silently redirect the move.
The read-only `fux workspace catalog` command returns the server instance and ordered workspace
names with their lifetime streams, without creating workspaces.

These workspace commands require a local manager connection and are disabled for explicit socket attachments.
Pasted Enter never confirms a workspace command. A source layout/tab change cancels a pending
move; confirmed manager operations complete or report their failure before queued input resumes.

## Cross-workspace transfer and route constraints

`fux transfer-pane PANE --instance INSTANCE --source-tab TAB --generation GENERATION
--workspace NAME --stream STREAM` moves a live pane into a new tab of an existing workspace.
Get the source revision from layout export and the destination stream from its listing or
manager descriptor. Add `--destination-tab TAB --destination-generation GENERATION --target
PANE --side left|right|up|down` to insert beside a pane in an existing destination tab.
`--label NAME` labels a new destination tab. All IDs belong to the same server instance.

For an existing-tab destination, `--ratio 7000` gives the existing target 70% and the moved
pane 30% of their usable split extent. The option is also available on `layout TAB to-tab`.
It uses the same integer 500–9500 range as split creation and defaults to 5000. The target's
share is independent of `--side`: left/up complement the tree's first-child ratio because the
moved pane comes first. Integer rounding can shift a cell between children. Ratios are rejected
for new-tab destinations, where no split is created. Invalid values fail before movement or
workspace allocation. In both APIs the existing-tab destination carries `ratio`, for example
`{"kind":"tab","tab":2,"generation":7,"target":3,"ratio":7000}`.

To create a workspace directly around the pane, use `--new-workspace` instead of `--stream`.
The name must be unused. This creates exactly one tab containing the existing pane; no shell
or replacement process is launched. Existing-tab destination options cannot be combined with
`--new-workspace`. Rejection leaves no reserved name, visible workspace or endpoint behind.

This uses the global manager socket. A workspace control socket cannot transfer into another
workspace. The manager request is `{"request":"transfer","transfer":{...}}`; its transfer
object contains `instance`, `source` (tab ID), `generation`, `pane`, `workspace`,
`destination` (the same tab/new-tab shape as the layout action) and `side`. `workspace` is
`{"kind":"existing","name":"NAME","stream":STREAM}` or `{"kind":"new","name":"NAME"}`.
The latter requires a new-tab destination and is opened only after a successful transfer.
An optional `follow` viewer ID asks the manager to switch that attached viewer to the moved
pane; it must still be attached to the source workspace/tab when the operation runs. CLI
transfers omit `follow`. Viewer frames publish their own `viewer` ID and `server_instance`,
so interactive transfers use their observed connection identity, not a later manager lookup. The manager returns
`{"reply":"layout","result":...}` wrapping the normal completed/failed control reply.
The CLI returns failure status for rejected transfers.

The server checks both revisions, pending viewer size changes, workspace lifetime, open state,
creation barriers and destination pane/tab limits before changing membership. Empty source tabs
close; empty source workspaces retire through normal cleanup. Current ownership is separate
from immutable launch attribution, so source cleanup cannot terminate a moved process. Final
records continue to identify the launch workspace/stream. Both affected workspaces publish
`workspace.changed`; consumers must relist using their own stream cursors.

An old workspace endpoint cannot send new input to the moved pane. Unsubmitted receipts remain
readable at their original route while it exists, but transfer marks them `failed` with zero
bytes written and a route-change error. They cannot deliver, including after moving back.
Rejected transfers leave reservations unchanged. Movement does not advance `input_sequence`;
already delivered receipts retain their sequence and delivery evidence. A retained queued input
operation blocks transfer until its completion is known. Same-workspace tab moves preserve receipts.

Manager-authorized consumers can read retained receipts after the original socket disappears:
`{"request":"input-status","instance":"INSTANCE","pane":1,"operation":42}` returns
`{"reply":"input-status","result":CONTROL_REPLY}` with the existing `input` result and request
ID zero. The server instance and receipt's pane must match; missing/mismatched/expired operations
return `expired`, while a different server returns `conflict`. Expiry is unchanged. The read works
without a live pane and never resubmits bytes, rebinds an operation or grants a workspace endpoint
access to another route. Zor uses it for receipt reconciliation.

New zor adoptions follow live panes across workspaces using manager location lookup. Task records
retain the original adoption request and immutable launch attribution, while requests resolve the
current route for the same server/pane/PID. Prompt receipt reconciliation and final exit evidence
survive movement; lookup never grants permission to replace a process. Workspace routing is no
longer part of zor's shared-writer identity.

Managed launches request `fixed_workspace: true` during creation so lost creation replies can
still be recovered through the original workspace. After the journal durably records the exact
pane/PID, zor releases that creation pin. Attached managed tasks can then move, submit prompts,
reconcile and stop through their current route, including after the source workspace disappears.
A lost release request or reply leaves the recorded launch intact; `launch-reconcile` retries the
same release without creating a process. A pin release never clears an explicit `fix-workspace` pin.

The manager request is
`{"request":"release-pane-pin","instance":"INSTANCE","pane":1,"pid":123}`. It returns a
`release-pane-pin` reply wrapping a completed `unit` result with ID zero. Exact server/pane/PID
checks precede mutation. Releasing an already-unpinned live pane is a no-op; a changed process or
explicit pin returns `conflict`. No process, terminal geometry or input state changes.

`zor run` releases its creation pin after obtaining the exact pane identity. Final output and
exit status are read through the manager using immutable launch attribution. Timeout/error cleanup
locates and closes only its own pane on its current route, including a process that closed its terminal descriptors, then releases its original workspace
with the original lifetime guard. A verified replacement server releases old ownership without
granting authority over the replacement. It never closes the destination workspace. Unreconciled managed
creation is intentionally pinned until exact durable identity exists. Explicitly pinned panes still reject cross-workspace movement; same-workspace
layout/tab operations remain available.

## Control protocol and tree document

The generic request is `command: "layout"`, with `id`, optional `instance`, `tab`, optional
`generation` and `action`. An action has an `operation`: `export`, `inspect`, `zoom`, `swap`, `relocate`,
`swap-direction`, `move-direction`, `resize-toward`, `resize-border`, `set-ratio`, `transfer`
or `apply`. Mutations require a matching generation; `inspect` takes `pane` and is read-only. `zoom` takes
`pane` (an ID or null); `swap` takes `pane` and `target`; `relocate` also takes `side`;
`resize-toward` takes `pane`, `direction` and `delta`; `set-ratio` takes `split` and `ratio`;
`apply` takes `document` and an optional complete `remap` array of [source, destination] pairs.
Directional swap/move take `pane` and `direction`. `resize-border` takes zero-based `column`,
`row`, `to_column` and `to_row`. `transfer` takes `pane`, `side` and a `destination`: either
`{"kind":"tab","tab":2,"generation":7,"target":3}` or
`{"kind":"new-tab","label":null}`. The destination revision is required for an existing tab.

A document is a bounded flat node array with a root index:

```json
{
  "root": 0,
  "nodes": [
    {"kind": "split", "axis": "horizontal", "ratio": 5000, "first": 1, "second": 2},
    {"kind": "pane", "pane": 1},
    {"kind": "pane", "pane": 2}
  ]
}
```

Canonical exports use preorder indices independent of allocator history. Limits are 256
leaves, 511 nodes, 64 root-to-leaf edges and the control protocol's 1 MiB frame bound. Node
array deserialization is bounded. Import rejects cycles/shared children, unreachable nodes,
duplicate/unknown/missing panes, invalid indices and ratios, excessive depth and unknown fields.
The original layout is retained on failure. Applying unchanged geometry does not advance its
revision or send redundant PTY resize effects.

Viewer frames carry both their existing frame generation and the active tab's layout generation,
plus the zoomed pane. Frame generations continue to guard application mouse hit testing;
layout generations guard tree edits. Coalesced frames preserve the newest values of both.

## History, copy and input ownership

Wheel browsing is private to each pane and viewer. Scroll A, then B, then A without
closing an intermediate mode; both visible panes retain their positions. The wheel
does not change keyboard focus. Ordinary input restores only the focused pane to
live output before forwarding its original bytes. Other private histories survive.

One resolved Escape dismisses the most recently manipulated history, or finishes
explicit copy/selection in one press. It does not open commands or reach the pane.
Further Escapes dismiss remaining passive histories in recent-interaction order;
with none left, normal terminal/prefix behavior resumes. This precedence also
applies when Escape itself is configured as the prefix.

Prefix `[` enters keyboard copy for the focused pane. Space anchors a selection;
`c` clears selection while staying in copy; `q`, Escape and successful copy finish.
The configured prefix remains available from copy. Text fields retain printable
prefix characters as text. Alt and CSI/SS3 sequences are not lone Escape; the
35 ms escape disambiguation delay separates a lone press from a continued sequence.

Mouse-reporting applications own their wheel input unless Shift requests local
history. Exited panes remain browsable while retained. Primary/alternate-screen
changes invalidate incompatible local history, selections and pending reads.
Hidden-tab histories are discarded. The viewer bounds retained history storage;
see [the transition contract](control-flow-transitions.md) for limits and identity.

Menus, choosers, rename fields and confirmations dismiss to normal input. Failure
notices do not open commands. The command popup supports wheel scrolling, clicking
painted actions, and outside-click dismissal; its initiating gesture executes once.

History reads and local dismissal remain responsive during pending operations.
External effects stay ordered. A command that needs an updated frame waits in a
cancellable state; Escape discards that command and its buffered text, not a
previously sent mutation. Buffered manager input retains its original pane target.
An incompatible target change produces a notice instead of redirecting that input.
