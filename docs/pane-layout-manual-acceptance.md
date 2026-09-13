# Manual visual acceptance: pane and layout controls

The [Betamax harness](betamax-harness.md) now automates the interaction scenarios
and renders headless PNG evidence. Use its run report to avoid repeating passed
functional checks. Native terminal font/rendering differences, actual modifier
and mouse delivery, clipboard integration, and usability still need a real terminal.

Run this in a real terminal on macOS. Allow approximately 30–45 minutes and use three terminal windows: **A** for control commands, **B** for the main viewer, **C** for a second viewer. Commands below assume zsh or bash and Python 3. `Prefix key` means press **Ctrl-A**, release both keys, then press the named key. Type shell commands normally inside a pane; do not prefix them.

This checklist is not a record of a completed visual pass. Record PASS, FAIL, or BLOCKED for each numbered section. The implementation and automated evidence are in [the acceptance ledger](pane-layout-implementation.md); all bindings are documented in [the control guide](pane-layout-controls.md). Passing this walkthrough establishes the exercised fux behavior; a claim of complete Herdr parity also requires the ledger's baseline comparison to have no material gaps.

## 1. Start an isolated server

In **A**, paste this entire block. It leaves your normal fux configuration and sessions untouched.

```sh
cd /Users/kisaczka/Desktop/code/fux
export FUX_BIN="$PWD/target/release/fux"
cargo build --release -p fux
export PANE_TEST="$(mktemp -d /tmp/fux-visual.XXXXXX)"
mkdir -p "$PANE_TEST/config/fux" "$PANE_TEST/state"

cat > "$PANE_TEST/pane-shell.sh" <<'SH'
#!/bin/sh
printf 'Shell PID=%s PTY=%s\n' "$$" "$(tty)"
exec /bin/sh
SH

python3 - <<'PY'
import json, os, pathlib
p = pathlib.Path(os.environ['PANE_TEST'])
(p / 'config/fux/config.toml').write_text(
    'default-command = { argv = ' +
    json.dumps(['/bin/sh', str(p / 'pane-shell.sh')]) + ' }\n')
PY

printf 'export FUX_BIN=%q\nexport PANE_TEST=%q\n' \
  "$FUX_BIN" "$PANE_TEST" > "$PANE_TEST/env.sh"
cat >> "$PANE_TEST/env.sh" <<'SH'
fx() {
  env XDG_RUNTIME_DIR="$PANE_TEST" \
      XDG_STATE_HOME="$PANE_TEST/state" \
      XDG_CONFIG_HOME="$PANE_TEST/config" "$FUX_BIN" "$@"
}
# Guard a tab edit with a freshly observed instance and revision.
le() {
  local ws="$1" tab="$2" snapshot instance generation
  shift 2
  snapshot="$(fx "$ws" layout "$tab" export)" || return
  instance="$(printf '%s' "$snapshot" | python3 -c \
    'import json,sys; print(json.load(sys.stdin)["result"]["value"]["instance"])')" || return
  generation="$(printf '%s' "$snapshot" | python3 -c \
    'import json,sys; print(json.load(sys.stdin)["result"]["value"]["generation"])')" || return
  fx "$ws" layout "$tab" --instance "$instance" --generation "$generation" "$@"
}
SH
source "$PANE_TEST/env.sh"
fx serve > "$PANE_TEST/server.log" 2>&1 &
PANE_SERVER_PID=$!
printf 'Paste in B and C: source %q\n' "$PANE_TEST/env.sh"
```

In **A**, run `fx list`. If startup is still in progress, retry after a moment. If it continues failing, inspect `cat "$PANE_TEST/server.log"` and stop the walkthrough.

In **B**, paste the `source …` command printed above, then run:

```sh
fx
```

**Pass:** one shell is visible with its PID and PTY, plus the fux status/tab bar. This fresh server should have pane 1 in tab 1; confirm with `fx list` in A before using those IDs below. If IDs differ, substitute the actual IDs throughout sections 2–7.

## 2. Create and identify three nested panes

In **A**:

```sh
fx split horizontal --target 1 --ratio 5000
fx split vertical --target 2 --ratio 5000
fx layout 1 export > "$PANE_TEST/initial.json"
export PANE_INSTANCE="$(python3 -c \
  'import json,sys; print(json.load(open(sys.argv[1]))["result"]["value"]["instance"])' \
  "$PANE_TEST/initial.json")"
fx rename-pane 1 ALPHA --instance "$PANE_INSTANCE"
fx rename-pane 2 BETA --instance "$PANE_INSTANCE"
fx rename-pane 3 GAMMA --instance "$PANE_INSTANCE"
fx list > "$PANE_TEST/before.json"
for pane in 1 2 3; do
  fx locate-pane "$pane" --instance "$PANE_INSTANCE" > "$PANE_TEST/pane-$pane-before.json"
done
```

In **B**, select each pane with Prefix `o`. In each, type:

```sh
printf 'PID=%s PTY=%s | ABC 123 | wide: 界界 | accent: café\n' "$$" "$(tty)"
```

**Pass:** ALPHA occupies the left half; BETA and GAMMA divide the right half. Borders join cleanly, text stays within its pane, and labels identify the panes. Record the three PID/PTY pairs. Selection may remain on ALPHA after CLI splits because existing viewers keep their own focus.

## 3. Keyboard focus, zoom, resize, swap and move

In **B**, execute these in order:

1. Prefix `o` three times, then Prefix `u` three times. Check traversal wraps through every pane.
2. Prefix `h`, `j`, `k`, `l` individually. Check selection moves toward the requested neighbor where one exists. At an outer edge, no movement is expected.
3. Prefix `!` twice. Check it toggles between the last two distinct selections.
4. Select ALPHA. Prefix `z`, then Prefix `z` again. Check full-area zoom and exact layout restoration. Zoom again, then Prefix `o`: another pane becomes selected and the split layout returns.
5. Select ALPHA. Prefix `r`, press `l` four times, then Enter. Check ALPHA grows toward its right border. Prefix `r`, uppercase `L` four times, then Enter should shrink it back.
6. Prefix `v`, press `l`, then Enter. Check ALPHA exchanges positions with a right-hand neighbor.
7. Prefix `m`, choose a direction with an existing neighbor using h/j/k/l, then Enter. Check the selected pane relocates beside that neighbor.
8. Prefix `.`, choose another pane with arrows, then Enter. Check the two chosen panes exchange positions. Repeat, but press Escape: no swap occurs.
9. Prefix `;`, Ctrl-U, type `ALPHA edited`, then Enter. Check the selected pane label changes. Reopen, type another label, then Escape: the saved label remains.

**Pass:** focus is visibly identifiable; every edit keeps the original shell PID/PTY and its content. Enter exits modes. Escape dismisses modes/choosers; do not assume it rolls back directional edits already committed by earlier key presses.

## 4. Mouse layout controls and cancellation

In **B**, unzoom if necessary:

1. Left-drag an internal separator and release at a different position. Check the final border follows the drop position within the allowed limits.
2. Alt-left-drag pane content toward another pane's left, right, top and bottom edges. Check a cyan destination preview and a hint naming the target and side. Release on one edge: check the pane moves there.
3. Start another Alt-left drag. While holding the mouse button, press Escape, then release. Check the preview disappears and no move occurs.
4. Right-click an unfocused shell pane. Check its menu targets the clicked pane. Cancel with an outside left-click. Repeat and use arrows/Enter to choose a non-destructive action such as rename.
5. Repeat menu opening via Prefix `?`. Check keyboard access reaches the equivalent actions.

**Pass:** no overlapping text, broken wide characters in the preview, stuck highlight, or stray mouse escape sequences in shell input. PTY resizing commits on release; continuous resizing during the drag is not required. If your terminal intercepts Alt-mouse, record that environment limitation rather than marking that gesture passed.

## 5. CLI edits, export/restore, and stale-edit rejection

Keep all three original panes in tab 1 for this section. In **A**:

```sh
le default 1 zoom
fx layout 1 export > "$PANE_TEST/saved-layout.json"
le default 1 swap 1 2
le default 1 move 3 1 down
le default 1 ratio 0 6500
le default 1 zoom 2
le default 1 apply "$PANE_TEST/saved-layout.json"
fx layout 1 inspect 1 > "$PANE_TEST/inspection.json"
```

Watch **B** after each command. **Pass:** edits visibly match their intent; apply restores saved geometry, labels and zoom without restarting shells. Inspection changes neither focus nor layout.

Now deliberately send an outdated revision in **A**:

```sh
fx layout 1 export > "$PANE_TEST/stale-layout.json"
PANE_OLD_GENERATION="$(python3 -c \
  'import json,sys; print(json.load(open(sys.argv[1]))["result"]["value"]["generation"])' \
  "$PANE_TEST/stale-layout.json")"
le default 1 swap 1 2
fx layout 1 --instance "$PANE_INSTANCE" --generation "$PANE_OLD_GENERATION" \
  apply "$PANE_TEST/stale-layout.json"
```

**Pass:** the last command reports a conflict/stale revision and leaves the newly swapped layout unchanged. This rejection is expected. Recover with:

```sh
le default 1 apply "$PANE_TEST/saved-layout.json"
```

## 6. Two simultaneous viewers and tiny terminals

In **C**, paste the setup's printed `source …` command, then run `fx`.

1. In B select ALPHA; in C select BETA. Type `echo FROM_B` in B and `echo FROM_C` in C. Check each reaches its selected shell.
2. Resize or swap panes in B. Check C sees the same arrangement while retaining its valid private selection.
3. Zoom in B. Check both viewers show the shared zoom target. Unzoom before checking independent input again.
4. In A run `fx focus 3`. Check it does not forcibly change either attached viewer's private focus.
5. Make C substantially smaller. Check the shared tab area fits the smallest viewer. Enlarge C and check recovery.
6. Make B as small as the terminal application permits, including a one-content-row or zero-content-row case if possible. Use Prefix `o` and Prefix `z`, then enlarge B and unzoom.

**Pass:** no crash, stuck input, missing live pane, or corrupted borders after recovery. Tiny panes may have no visible content; a one-row terminal has only the status bar. Restoring the window recovers the layout, although applications may reflow their text. Record the smallest size actually tested; do not claim sizes the terminal would not allow.

Detach **C** using Prefix `d` before continuing. Check its ordinary shell works normally.

## 7. Application mouse forwarding

In **A**, create this disposable probe:

```sh
cat > "$PANE_TEST/mouse-probe.py" <<'PY'
import os, termios, tty
fd = 0
saved = termios.tcgetattr(fd)
try:
    tty.setraw(fd)
    os.write(1, b'\x1b[?1000h\x1b[?1006hMouse probe: click; Ctrl-C exits.\r\n')
    while True:
        data = os.read(fd, 4096)
        if not data or b'\x03' in data:
            break
        os.write(1, repr(data).encode() + b'\r\n')
finally:
    os.write(1, b'\x1b[?1000l\x1b[?1006l\r\n')
    termios.tcsetattr(fd, termios.TCSADRAIN, saved)
PY
fx pane-input 1 --right-click auto --instance "$PANE_INSTANCE"
```

In **B**, focus pane 1 and run:

```sh
python3 "$PANE_TEST/mouse-probe.py"
```

1. Ordinary right-click its content: the probe should print mouse reports instead of opening a menu.
2. Alt-right-click: the fux menu should open. Dismiss it with Escape. Its captured click/release should not appear in the probe.
3. In A run `fx pane-input 1 --right-click fux --instance "$PANE_INSTANCE"`. Ordinary right-click now opens the menu. Dismiss with an outside click; check captured mouse events do not leak.
4. In A run `fx pane-input 1 --right-click pane --instance "$PANE_INSTANCE"`. Ordinary right-click reaches the probe again.
5. In B press Ctrl-C to exit the probe. In A restore `fx pane-input 1 --right-click auto --instance "$PANE_INSTANCE"`.

**Pass:** policy changes affect routing immediately without restarting the shell; the terminal returns to normal after the probe exits.

## 8. Live moves between tabs and workspaces

From here onward, tab membership changes: do not reuse the earlier tab-1 layout commands blindly.

In **B**:

1. Focus ALPHA and press Prefix `b`. Open Prefix `w` and select its new tab. Check ALPHA has the same PID/PTY and content. The original tab should retain BETA/GAMMA; the viewer may initially stay there.
2. Prefix `,`, type a tab label, Enter. Prefix `g`, choose the other tab, Enter. Check tab order changes while the selected tab remains selected.
3. Prefix `e`, choose the original tab, Enter. Check ALPHA moves back beside an existing pane; its empty source tab disappears.
4. Prefix `t` creates a scratch tab. Return to the original tab with Prefix `w`. Alt-left-drag one original pane onto the scratch tab label. Check cyan destination feedback and a live transfer on release.
5. Right-click an inactive tab label and rename/reorder it through the menu. Check the action targets that tab without unexpectedly selecting it. Use wheel navigation and a left-click in the reorder chooser.
6. Select an original pane. Prefix `y`, type `visual-other`, Enter. Check the viewer follows the same live pane into the new workspace.
7. Prefix `=`, type `Visual other`, Enter. Prefix `f`, select `default`, Enter. Prefix `s`: check the display label, routing name and changed workspace order.
8. Prefix `i`, choose `default`, Enter. Check the viewer follows the pane back, preserving its shell. Prefix `!` should return to the most recent still-live alternate selection; a deleted historical target must fail safely.

For the workspace mouse route, create a scratch workspace using Prefix `a`, naming it `mouse-destination` if prompted. Return to `default` via Prefix `s`. Alt-left-drag an original pane onto the workspace-name area, choose `mouse-destination` with the mouse, and check the same live pane arrives there. Use the workspace context menu to exercise choose/reorder and cancellation.

For overflow tabs, create enough scratch tabs with Prefix `t` that some labels are hidden. Return to a tab containing an original pane. Alt-left-drag that pane over a visible tab label, keep holding, scroll to an initially hidden destination, then release. **Pass:** the hint identifies the chosen destination and the pane arrives there with unchanged identity.

In **A**, capture the original panes' current locations:

```sh
for pane in 1 2 3; do
  fx locate-pane "$pane" --instance "$PANE_INSTANCE" > "$PANE_TEST/pane-$pane-after.json"
  python3 - "$PANE_TEST/pane-$pane-before.json" "$PANE_TEST/pane-$pane-after.json" <<'PY'
import json, sys
def location(path):
    return json.load(open(path))['result']['result']['value']['location']
before, after = map(location, sys.argv[1:])
assert before['pane'] == after['pane'] and before['pid'] == after['pid'], (before, after)
print('Same pane/PID:', after)
PY
done
```

**Pass:** all three original identities/PIDs match. Workspace and tab fields may differ. Scratch tabs/workspaces legitimately create additional shells; transfers of original panes must not.

## 9. Restore a workspace archive

In **A**:

```sh
fx workspace export-layout > "$PANE_TEST/desired-archive.json"
```

In **B**, rename/reorder a tab and rename a pane or workspace. Do not create/delete containers or transfer across workspaces during this check. In **A**:

```sh
fx workspace export-layout > "$PANE_TEST/expected-archive.json"
fx workspace apply-layout "$PANE_TEST/desired-archive.json" \
  --against "$PANE_TEST/expected-archive.json"
```

**Pass:** saved labels/order/layout return together; shells retain their identities and valid viewer-private selections remain valid. Archives restore existing containers, not deleted sessions or processes.

## 10. Close dialogs and detach/reattach

Perform destructive checks only on the disposable panes/workspaces created above.

1. Right-click a pane → Close. Click the warning text: nothing should close. Click Cancel: the pane survives and accepts input.
2. Reopen Close and click outside: it cancels. Reopen and click Confirm close: only the captured pane closes.
3. Right-click a scratch tab → Close. Exercise Cancel, then Confirm close. Check only that tab and its panes disappear.
4. Switch to `mouse-destination` with Prefix `s` (create it again with Prefix `a` if earlier close checks emptied it). Right-click its name → Close. Exercise Cancel, then Confirm close. Check its viewer detaches and `default` remains usable. Keep `default` open for the next checks.
5. Reattach in B with `fx default`. Exercise Prefix `x`, Prefix `c`, and Prefix `q`, cancelling each with `n` or Escape. Check no target closes.
6. Prefix `d`, then run `fx default` again. Check surviving layout, labels, shell PIDs and content persist across viewer detachment. Detach again.

**Pass:** mouse-only confirmation/cancellation works, no dialog click/release reaches the underlying shell, and the terminal is usable after detachment. Viewer reattachment does not promise restoration of private last-focus history or a daemon restart.

## 11. Independent history and consistent exits

In **A**, create a fresh workspace:

```sh
fx workspace new ux-scroll
```

Detach **B** with Prefix `d`, then run `fx ux-scroll`. In its first pane, type:

```sh
i=1; while [ "$i" -le 120 ]; do printf 'A%03d\n' "$i"; i=$((i+1)); done
```

Press Prefix `\` to split side by side. In the new right pane, type:

```sh
i=1; while [ "$i" -le 120 ]; do printf 'B%03d\n' "$i"; i=$((i+1)); done
```

Detach **C** from its previous viewer and run `fx ux-scroll` there too.

1. Keep keyboard focus on B. In **B**, wheel up over A, then B, then A again.
   Record the first visible numbered line in each pane. Both positions must be
   retained independently; pointer movement must not change keyboard focus.
   **C** must remain live rather than inheriting the history positions.
2. Press Escape once. Only the most recently browsed pane returns live. No command
   popup appears. Press Escape again to dismiss the other retained history.
3. Browse A and B again. Type `printf 'SCROLL_SENTINEL\n'` and Enter without a
   prefix. The focused B returns live and executes the command once; A remains
   scrolled. No Escape text or stray mouse bytes should enter either shell.
4. Press Prefix `[`, start a selection with Space, then press Escape once.
   Selection and copy mode both disappear. Type `printf 'AFTER_COPY\n'` and Enter.
   Re-enter copy and check `c` clears only the selection, while `q` exits copy.
5. Re-enter copy, then press Ctrl-A. Commands open once. Escape dismisses them to
   normal input. Open rename with Prefix `;`, type a temporary name, then Escape.
   The label remains unchanged; the next ordinary shell command works.
6. Open the command popup. Scroll it with the wheel, click an available command,
   and verify it executes once. Open it again and click outside; the popup closes
   and that click/release does not reach the shell. A fresh subsequent click works.
7. Start a Shift-left selection drag, move outside the pane, and release. Start a
   second drag; it must get a fresh anchor. Repeat with Escape while held, then
   release and start a fresh drag. No selection or gesture capture remains stuck.
8. Browse history while resizing **B** smaller, then restore it. Check refreshed
   geometry, borders, hint placement and selection cleanup. Shared pane geometry
   follows the smallest viewer; do not expect **C** to keep the shared PTY large.
   Try the smallest dimensions your terminal supports and record that size.
9. Repeat the mouse-reporting probe in section 7: ordinary wheel input belongs to
   that application, while Shift-wheel browses fux history. Browsing another pane
   must not intercept the probe's mouse events. Switching primary/alternate
   screens invalidates an incompatible local history/selection.

**Pass:** one Escape exits the current local interaction without opening commands;
other passive histories survive; application focus, input and mouse ownership
remain clear. With Escape configured as the prefix, the first resolved Escape
still dismisses local history; the next follows normal prefix rules.

Controlled manager/control delay checks are reproducible through the headless
harness rather than attempting to time a real server manually. After the build
steps in [the harness guide](betamax-harness.md), run:

```sh
target/rust-harness/debug/fux-xtask scenario viewer-history-delay target/debug/fux
target/rust-harness/debug/fux-xtask scenario viewer-manager-delay target/debug/fux
```

## 12. Record results and stop the disposable server

In **A**, before stopping:

```sh
fx workspace catalog > "$PANE_TEST/final-catalog.json"
fx workspace export-layout > "$PANE_TEST/final-archive.json"
printf 'Evidence directory: %s\n' "$PANE_TEST"
```

Copy and fill this report. Include the exact failure command/key sequence and visible result for each FAIL; record environmental restrictions as BLOCKED.

```text
Terminal application/version:
macOS version:
Normal terminal dimensions / smallest tested dimensions:
Binary tested: /Users/kisaczka/Desktop/code/fux/target/release/fux
Evidence directory:
1 isolated startup: PASS / FAIL / BLOCKED
2 nested panes and rendering:
3 keyboard controls:
4 mouse layout controls:
5 export/apply and stale rejection:
6 simultaneous viewers and tiny terminal:
7 application mouse routing:
8 live tab/workspace transfers and ordering:
9 workspace archive restoration:
10 close dialogs and detach/reattach:
11 independent history and consistent exits:
Failures or untested substeps:
```

Detach any remaining viewers, then in the original **A** shell:

```sh
kill "$PANE_SERVER_PID"
wait "$PANE_SERVER_PID" 2>/dev/null
```

This stops only the server started by this walkthrough. Keep the evidence directory until findings are resolved. Report results before marking the manual gate passed in the acceptance ledger.

## Popup release ownership and visible exits

1. With two panes running mouse-reporting applications, press Ctrl-A to show the
   command popup. Hold the right mouse button over the other pane, press Escape,
   then release. The popup should dismiss without an application action.
2. Repeat with the middle button. Then repeat each button while pressing `[` in
   the popup to enter Copy before releasing. Escape should leave Copy in one press;
   the next ordinary key should go to the focused application once.
3. At 80 and 40 columns, enter Copy and then begin a selection with Space. Both
   hints should show `Esc finish`; Escape returns to live output. For passive wheel
   history, check `Esc live` appears before secondary scrolling instructions.
4. For exact byte-delivery checks, run the native `viewer-mouse-app` scenario using
   the build/font setup in `betamax-harness.md`. Its raw applications assert that
   these ignored press/release events reach neither pane and the sentinel arrives
   exactly once. Native terminal/OS button behavior still needs steps 1–3.
