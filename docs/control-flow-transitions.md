# Viewer input ownership and transitions

Target contract for `fix-control-flow-ux-prompt.md`. Implementation status is
tracked in the control-flow audit; this table alone is not completion evidence.

| State / entry | Keyboard owner | Pointer owner and focus | Escape | Completion / outside click | Target loss / late reply |
| --- | --- | --- | --- | --- | --- |
| Normal / attach or dismissal | Focused application via prefix parser | Normal server focus/application policy | Application, except configured Escape prefix | Normal routing | Apply new frame; never retarget buffered mutations silently |
| Passive history / wheel | Focused application; typing restores only its pane live | Wheel targets hovered pane without changing focus; application mouse reporting wins unless Shift | Restore most recently manipulated history pane live; consume Escape | Other panes remain independently browsable; first click uses normal focus policy | Remove invalid identity; stale reads ignored |
| Keyboard copy / prefix + `[` | Copy controller; prefix exits to commands | Selection capture owns its gesture; unrelated pane events retain normal routing | Clear selection, restore its pane live, return normal in one press | `q` and copy finish the same way; selection clear is a separate action | Dismiss with notice; no popup resurrection |
| Commands / explicit prefix | Command parser | Wheel scrolls commands; clicks use rendered hit targets; outside click dismisses and is consumed | Normal | Action enters its declared state; literal prefix forwards once | Refresh availability; failures are notices |
| Context menu / right click or command | Menu navigation | Menu until dismissal; initiating release consumed | Normal | Activate once; outside click dismisses and is consumed | Dismiss with notice |
| Chooser / explicit action | Chooser navigation | Chooser hit targets and wheel | Normal | Choose once; outside click dismisses | Dismiss if source invalid; ignore old results |
| Text field / rename or create | Field editor; printable prefix remains text | Field; outside click dismisses and is consumed | Normal, no mutation | Enter validates/submits; invalid input stays with explanation | Dismiss if target identity changed |
| Confirmation / close action | Confirmation only | Confirm/cancel hit targets | Normal, no mutation | `n` / outside click cancel; `y` submits once | Dismiss, never apply to replacement target |
| Layout editing / resize, swap, move | Layout controls | Existing layout interaction policy | Finish, retain already applied edits | Enter also finishes; hints say finish, not rollback | Dismiss with notice; stale mutation rejected |
| Pointer preview / press and drag | Escape can cancel | Captured gesture until release, including outside pane | Cancel preview; consume gesture tail | Release commits valid preview once; fresh next gesture works | Cancel preview; do not mutate |
| Selection gesture / local drag | Selection controls | Captured pane receives motion/release even outside its rectangle | Dismiss selection and consume gesture tail | Release ends capture; subsequent drag starts a new anchor | Clear capture and invalid selection |
| Loading / lookup or manager operation | Local dismissal remains available; application/mutation ordering preserved | Local history/focus interaction remains responsive | Dismiss UI; cannot undo a sent mutation | Only current operation can complete its UI; bounded buffered input | Ignore superseded lookup results; never replay into another owner |
| Waiting command / command depends on an earlier operation | Waiting parser retains the command's suffix; no application delivery | Wheel can browse visible panes; outside press dismisses | Cancel the waiting command and its buffered suffix; earlier sent effects remain committed | Resume against the acknowledged view; replay suffix only into an accepted command | Interaction epoch prevents canceled commands from resuming |
| Failure / rejected or timed-out operation | Previous safe owner, otherwise normal | Previous safe policy | Dismiss local interaction if any | Notice does not open commands or capture input | Fixed send-time deadline; correlate exact operation |

Histories are private to one viewer and scoped to the attached instance, workspace
stream and pane. Hidden-tab histories may be discarded rather than growing an
unbounded cache; this must not affect the daemon's history. Visible panes retain
independent positions. Reconnect starts a fresh cache. Escape dismisses retained
histories in most-recent-interaction order; when none remain, normal Escape rules
resume. Passive browsing never starts a keyboard copy cursor in an unfocused pane.

The cache retains at most 64 passive snapshots and 64 MiB of estimated retained
history storage across passive and explicit copy views. Oldest passive snapshots
are evicted first; an oversized explicit view is dismissed with a notice.
Primary/alternate-screen transitions invalidate that pane's local history and
copy selection, including pending read identity. Shift can start fresh local
history in the current buffer; its available depth is the server's reported
depth, not retained history from the previous buffer. Exited panes can be browsed
while retained; their former application's mouse reporting no longer owns input.

When Escape itself is configured as the prefix, a resolved lone Escape first
dismisses local history/copy. A subsequent Escape with no local history follows
the configured prefix policy. Text-entry modes retain their own printable prefix
characters. In copy mode, `c` clears selection without exiting; Escape exits.

External effects are ordered separately from local parsing, bounded by 256 entries
and 64 KiB of payload. Only one control or mutating manager operation proceeds at
a time. Input after an attachment navigation command follows its acknowledged
destination. Input held by a manager mutation is pinned to the original workspace
and focused pane; if that target changes, discard it with a notice rather than
send it to a replacement. Mouse effects retain their hit-tested generation.
Layout continuations wait for an updated revision and retain their layout hint.
Escape can dismiss the continuation and discard unsent buffered keys; it does
not undo earlier mutations. History reads have a separate bounded lane.

Unfinished paste/escape sequences remain owned by the parser that began them until
safely drained. CSI/SS3 and Alt input are not lone Escape. Fast Escape plus a key
and an identical Alt byte sequence cannot be distinguished: the documented escape
delay defines the boundary. Dismissal must not leak a paste tail, trigger a prefix
command from pasted text or send a gesture release to the wrong application.

Pending workspace and destination lookups share the waiting-mode pointer policy:
wheel routes immediately, an outside press dismisses, and consumed mouse reports
are never replayed into a populated chooser. A fresh left press supersedes an
active selection whose release was lost; normal hit testing decides the new
owner, including Shift selection in another pane.

A fresh left press also cancels an unreleased layout preview before ordinary hit
testing; it cannot commit the old gesture. After a context menu is dismissed, a
new right press supersedes its missing release. Matching history request IDs do
not override buffer identity: a reply from the other screen buffer dismisses the
old view even when its live state frame has not arrived yet.

Exited-pane history is available only while a frame still contains that pane.
The daemon normally removes exited panes from a nonempty layout; the final pane's
screen remains briefly during workspace retirement. This is not an indefinite
interactive retention feature. A controlled peer verifies history while a retained
exited frame exists and cleanup on its subsequent removal.

Command-popup middle/right presses are ignored as actions but still own their
motion and release. If a keyboard command opens another mode before release,
that capture transfers to the new controller owner. The eventual release must
not reach an application; a fresh subsequent press resumes normal hit testing.
