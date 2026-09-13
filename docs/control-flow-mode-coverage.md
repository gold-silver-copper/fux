# Mode-by-mode acceptance review

This maps the prompt's transition requirements to actual assertions. It is a
coverage review of the final verified implementation. Symbols below are in
`crates/fux/src/client/controller.rs` unless a different file is named. The
107-test client snapshot and 191-test library snapshot pass. The final v4 run
passes 18/18 local and 19/23 automation tests; four baseline zor fixtures fail. Do not infer coverage from a test's name alone.

## Shared checks and their limits

- `transient_modes_escape_to_normal_without_forwarding_or_mutating` enters all
  22 modal actions listed below, resolves one Escape, checks that controller
  ownership ends and no control/action/copy result is emitted. Its next-key
  assertion uses a fresh prefix filter; real next-input routing is additionally
  covered in the viewer, history-controls, manager-delay and mouse-app scenarios.
- `text_fields_ignore_inside_clicks_and_dismiss_outside_with_release_owned`
  covers RenamePane, RenameTab, RenameWorkspace, NewWorkspace and
  MoveToNewWorkspace. It checks retained ownership inside, no outside mutation,
  dismissal and consumed release.
- `mouse_close_dialogs_confirm_cancel_and_consume_gesture_tails` covers all three
  confirmation actions, including confirm/cancel hit regions and stale pane loss.
- `ServerMessage::Reply` handles an exact matching failed control reply as a
  notice. `viewer-history-delay` asserts rejected control followed by exact normal
  input and no popup. This is shared post-submission behavior, not a separate
  per-action failure injection. Manager and lookup failures use different paths
  and require their own assertions.
- Menu actions transition to their declared child interaction or submit a request.
  A menu has no independent RPC whose rejection needs a separate menu-specific
  handler. Child-mode and shared post-submission checks apply after activation.
- Outside-click dismissal is applicable to painted fields, menus, choosers and
  confirmations. Copy and immediate layout editing use their documented pane/
  gesture policy instead of treating every outside click as a dialog dismissal.

## Action matrix

All rows have the shared one-Escape check above. Shared handlers are explicitly
identified rather than claiming a separate failure injection for every action.

| Action(s) | Completion evidence | Pointer/outside evidence | Target-loss / failure evidence |
| --- | --- | --- | --- |
| RenamePane | `pane_rename_keeps_its_target_and_cancels_on_identity_changes`: Unicode, empty label and original pane despite focus change | Shared five-field test; history-controls real rename/outside/next-input sequence | Same test removes pane and replaces instance/workspace; shared control rejection |
| RenameTab | `rename_submits_fragmented_unicode_and_cancels_without_mutation`: exact Tab Rename request and mode exit | Shared five-field test | `captured_tab_and_layout_modes_cannot_submit_after_target_loss`; shared control rejection |
| RenameWorkspace | `workspace_rename_edits_labels_and_rejects_stale_lifetimes`: exact instance/stream rename and pasted-newline isolation | Shared five-field test | Same test changes stream before submit and asserts no request; shared control rejection |
| NewWorkspace | `workspace_chooser_replays_buffered_input_and_switches`: exact Workspace New request | Shared five-field test | No pane target is required; `workspace_creation_and_menu_discard_replaced_attachment_context` checks instance/workspace/stream/viewer replacement; shared control rejection |
| MoveToNewWorkspace | `workspace_move_keeps_observed_identity_and_follows_only_its_viewer`: exact transfer identity, generation and viewer; empty/pasted input cannot submit | Shared five-field test | Same test changes layout generation and rejects submission; manager-delay transfer failure/epoch handling |
| ClosePane | `confirmations_carry_the_original_target_and_ignore_paste`: exact Kill and pasted confirmation ignored | Shared confirmation test | Same test removes frame/target and checks dismissal/notice; shared control rejection |
| CloseTab | Same confirmation test: exact Tab Close | Shared confirmation test | `captured_tab_and_layout_modes_cannot_submit_after_target_loss`; shared control rejection |
| CloseWorkspace | `workspace_close_confirms_the_captured_lifetime_and_rejects_replacements` | Shared confirmation test | Exact captured lifetime and replacement rejection; Workspace Kill uses the shared control-reply rejection handler, not a manager reply |
| PaneMenu | `contextual_pane_actions_keep_clicked_target_and_release_ownership` | `contextual_menu_clicks_disabled_actions_and_application_mouse_override`; release and outside handling | Context menu validity test rejects replaced identity/geometry; child/shared failure policy |
| TabMenu | `hidden_tab_menu_targets_its_tab_and_cancels_when_catalog_changes` | Same menu hit-testing machinery; broad viewer tab-menu capture | Same test changes catalog; child/shared failure policy |
| WorkspaceMenu | Workspace-close/rename paths and broad viewer workspace menu | Shared context-menu machinery; broad viewer capture | `workspace_creation_and_menu_discard_replaced_attachment_context`; five menu actions delegate to ChooseWorkspace, NewWorkspace, ReorderWorkspace, CloseWorkspace and RenameWorkspace covered here |
| SwapPane | `swap_picker_selects_an_explicit_nonadjacent_target_and_cancels_stale_edits` | Same picker hit testing; stale edits and cancel | Same test rejects changed layout; shared control rejection |
| ChooseTab | `tab_chooser_mouse_selects_reorders_and_cancels_without_forwarding_release` | Same test verifies outside dismissal and release ownership | `vanished_tab_choice_reports_failure_and_escape_returns_normal` checks no request, retryable error and Escape; shared normal-routing tests cover the next input |
| ReorderTab | `reorder_chooser_places_before_or_last_and_escape_cancels` | Tab chooser mouse test | `captured_tab_and_layout_modes_cannot_submit_after_target_loss`; shared control rejection |
| MoveToTab | `transfer_chooser_pins_both_revisions_and_ignores_paste` | Tab chooser mouse test | Same test preserves destination revision for server rejection and invalidates changed source generation; shared control rejection |
| ChooseWorkspace | `workspace_chooser_replays_buffered_input_and_switches` | `pending_workspace_lookup_browses_without_replaying_mouse`; workspace chooser pointer tests | Same replay test injects lookup failure; `cancelled_lookup_cannot_populate_a_reopened_chooser`; identity-change lookup test |
| ReorderWorkspace | `workspace_order_uses_manager_and_cancels_stale_loading` | `workspace_chooser_mouse_reorders_through_manager` | Source-workspace checks in same order test; manager-delay injects ReorderFailed and checks normal input afterward |
| MoveToWorkspace | `destination_chooser_keeps_catalog_lifetime_and_discards_cancelled_input` | `pending_destination_browses_and_dismisses_without_replaying_mouse`; delayed-manager real fixture | Same test rejects replacement catalog/changed layout; stale completion and failure covered in manager fixture |
| ResizeMode | `resize_repeats_with_arrows_and_application_cursor_keys`: immediate directional requests, Enter exits | Pane/layout real fixtures; outside click is not dialog cancellation | `captured_tab_and_layout_modes_cannot_submit_after_target_loss` removes pane and tab separately before next arrow; waiting-command and control rejection paths |
| SwapMode, MoveMode | `move_and_swap_modes_use_latest_frame_but_keep_original_target` | Pane/layout real fixtures; outside click is not dialog cancellation | `captured_tab_and_layout_modes_cannot_submit_after_target_loss`; waiting-command and control rejection paths |
| CopyMode | Copy key/selection tests; mouse-app asserts q and successful OSC52 copy return to normal, exact clipboard payload, and exact next-input bytes | Copy parser application routing; outside release; Shift and fresh-press recovery; auxiliary popup handoff | History/buffer/identity tests and delayed-peer reply/timeout matrix; fresh resize and buffer galleries |

## Non-action owners

| Owner | Evidence / remaining review |
| --- | --- |
| Normal and passive history | A → B → A, independent viewer, focused typing and exact mouse bytes in history-controls and mouse-app. Nested unequal geometry, alternate-buffer app and retained exited-frame/removal variants pass targeted runs. Final v4 and supplemental copy/paste v3 scenarios pass. |
| Command popup | Painted actions/header/outside/wheel tests; parser byte-boundary tests; new middle/right dismissal and keyboard-Copy handoff regressions. Actual raw apps assert no unmatched releases and one sentinel. |
| Layout preview | Revision/identity, cancellation, outside release and lost-release recovery tests plus history-controls PTY. Fresh press cannot commit the old preview. |
| Selection capture | Selection-exit/clear, popup handoff, fresh-anchor and external-release tests. Mouse-app covers the next full application gesture after a lost release. |
| Waiting command | Buffered suffix, wheel without mouse replay and cancellation tests; delayed control fixture covers resume/cancel; manager fixture covers queued mutation targeting. |
| Pending reads | ReadWindow exact identity/fixed deadlines/capacity; Copy sent-versus-desired clamp/burst tests; actual B progress while A is withheld. Round-robin `take_read` covers a finite working set of at most 64 passive sessions plus explicit Copy, at most one read per session and eight outstanding reads; capacity is checked before taking work. This is finite-set progress, not a proof against arbitrary unbounded arrivals. |
| Paste / incomplete escape | Every-boundary prefix/terminal sequence matrix, overlapping paste delimiters and canceled-mode paste ownership tests. Mouse-app additionally checks byte-exact UTF-8/prefix-containing bracketed paste through passive history, then buffer invalidation during Copy-owned paste: discarded tail cannot reach the app or split a pane; following sentinel arrives exactly once. |

Final source review covered the client ownership/parser/scheduler changes and
related real-PTY fixtures. No unresolved confirmed in-scope defect remains from
that review. The four baseline routing failures and native-input validation limit
are recorded in [final verification](control-flow-ux-final-verification.md).
