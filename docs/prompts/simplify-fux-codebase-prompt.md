# Simplify the fux codebase: minimal, idiomatic, ECS-native where it pays

Execute this in the fux workspace (`crates/fux`, `crates/local-ipc`; zor is untouched except
where a shared local-ipc API changes). The architecture is sound and the owner-loop / World
boundary is clean: this is **not** a rewrite. The work is removing duplication, deleting
vestigial abstractions, fixing a handful of real bugs found on the way, and cleaning the
repository so it reads as a codebase rather than a run log.

Every finding below was verified against commit `483d8ca` with `grep`; re-verify each line
reference before editing, since earlier batches move lines. Behavior must not change except
where a batch says "bug fix" and names the observable difference. Prove it with the full gate
after every batch. Do not weaken lints, add crate-level `allow`s, or touch `references/koh`.
This prompt does not authorize commits, pushes, PRs, releases, or version bumps; follow
explicit authorization in the conversation. Batches 8 and 9 are wire- or API-visible and are
staged for the next minor; implement them last and only if authorized.

Work batch by batch, in order. After each batch run the gate:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked -- --test-threads=1
cargo doc --workspace --no-deps --locked
cargo test --manifest-path crates/fux/tests/verify/fixture-child/Cargo.toml --locked
crates/fux/tests/verify/release-package.sh
```

Then record in `docs/codebase-simplification.md` (create it) a short table per batch: what
changed, lines removed, any behavior difference, and the gate result. Keep it factual.

## Decisions already made (do not relitigate)

- **The four `&mut World` systems stay exclusive.** `apply_requests`, `drain_viewer_queues`,
  `apply_spawn_completions` and `resolve_lifecycle` each mutate entities they must observe
  again within the same batch (a split's reservation is the next request's barrier; a
  completion's `TabOf` insert is split by the next completion; closing a tab retires the
  workspace in the same pass). Converting them to typed systems buys nothing under the
  `SingleThreadedExecutor`. `input::apply_completions` is already typed; `resolve_waits` does
  not exist.
- **`Ids` id→Entity maps stay.** bevy has no public-id→Entity index; a query scan per lookup is
  O(n) on the hot request path.
- **`Pane.dirty` stays a manual flag.** It persists across steps for hidden panes;
  `Changed<T>` is reset by `clear_trackers` every step.
- **Do not split the fat `Pane`/`Viewer`/`Tab` structs** into fine components. With four
  exclusive systems doing `world.get::<Pane>` at ~70 sites, it adds noise for no gain.
- **`LayoutTree<L>` stays generic.** It is what lets `layout.rs` tests run without a `World`.
- **The oneshot/token reply dance in `Inbound` is justified**: `Inbound` derives `Message +
  Clone + PartialEq`, so senders cannot ride inside it.

## Batch 1. Repository hygiene and stale documentation (clear win)

1. `docs/verification/` holds 2,351 tracked files (29 MB) of raw gate logs and JSON, while the
   same artifact class under `/.verification/` is already gitignored. `tools/archive/` holds
   697 tracked files (2.1 MB) of archived evidence pointing at the ignored `references/herdr`
   tree. Remove both from the tree (`git rm -r`), add `docs/verification/` to `.gitignore`,
   and leave one paragraph in `docs/release-readiness.md` saying where historical evidence
   lives (the git history before this change; name the commit).
2. Eighteen `*-prompt.md` agent task prompts sit at the repository root (2,687 lines). Move
   them to `docs/prompts/` (including this file once it has been executed). Their outcomes are
   in `CHANGELOG.md` and `docs/`.
3. `HANDOFF.md:3` says "Updated for the uncommitted main-based native integration" on a clean
   tree, and its "Six scheduled systems currently use `&mut World`" bullet is wrong. Rewrite
   `HANDOFF.md` to describe the current state in at most 40 lines, or fold it into
   `docs/design.md` and delete it.
4. `docs/design.md` is stale in three places: line 94 lists a **Waits** phase; lines 127 to
   134 say six systems take `&mut World` and describe `resolve_waits`; line 137 claims dirty
   flags exist because "change ticks would fire on `get_mut` reads". Correct all three: no
   Waits phase, four exclusive systems (name them and why), and dirty flags exist because
   `Pane.dirty` must survive across steps and the fat structs make `Changed<T>` too coarse.
   `crates/fux/src/ecs/systems/mod.rs:1-2` says "Every system is exclusive"; fix it.
5. `crates/fux/Cargo.toml:51-52` carries a comment about a linear-time regex dependency for
   "server-side `wait` patterns" above `unicode-segmentation`. There is no regex dependency
   and no `wait` command. Delete the comment.
6. Prune the dated acceptance reports in `docs/` (`*-2026-09-12.md`, `*-2026-09-13.md`,
   `control-flow-ux-*`, `pane-layout-manual-acceptance.md`, etc.) into one
   `docs/verification.md` index that links the surviving design and protocol documents and
   summarizes what was accepted when. Keep `design.md`, `security.md`,
   `local-control-protocol.md`, `local-attachment-protocol.md`, `service-ownership-contract.md`,
   `multi-machine-supervision.md`, `lint-baseline.md`, `diagnostics-and-failure-artifacts.md`
   and `release-readiness.md` as first-class documents.

## Batch 2. Correctness fixes (bug fixes; each names its observable change)

1. **Duplicate `CloseViewer` effect.** `ecs/systems/requests.rs:269-281`: on queue overflow
   the code emits `Effect::CloseViewer` and then calls `despawn_viewer` (`requests.rs:226-233`),
   which emits it again. Delete the explicit emit. Observable change: one `CloseViewer` per
   overflowed viewer instead of two. Add a test asserting exactly one.
2. **Admission count mismatch.** `requests_control.rs:503-507` (`check_viewer_admission`)
   counts viewers with `other.workspace == workspace` including detaching ones;
   `requests.rs:105-120` (`apply_attachments`) excludes `detaching`. Add
   `Viewer::attached_to(&self, workspace: Entity) -> bool` (attached and not detaching) and
   use it in both. Observable change: a detaching viewer no longer counts toward the limit on
   the admission path. Add a test.
3. **Unreachable arm.** `ecs/systems/layout_control.rs:66` matches `LayoutAction::Inspect`
   after line 27 already returned for it. Delete line 66's arm, then split `apply` (288 lines)
   into `read(world, tab, id, action)` for `Export`/`Inspect` and `edit(..)` for mutators, so
   the generation/instance conflict guard at lines 50 to 56 lives only in `edit` and
   `is_read_only()` disappears.
4. **`PaneId(0)` sentinel.** `requests_control.rs:151,366,452` return
   `CommandResult::Pane { pane: PaneId(0) }` to mean "a creation barrier was set; the
   completion phase replies", and `requests.rs:977` special-cases it. Replace with an explicit
   outcome: handlers return `Result<Outcome, Reply>` where
   `enum Outcome { Now(CommandResult), Deferred }`. No wire change.
5. **Inconsistent `#[serde(default)]`.** In `proto/control.rs:36-208`, thirteen `Request`
   variants have `#[serde(default)]` on `instance: Option<String>` and six do not
   (`FixWorkspace`, `RenamePane`, `PaneInput`, `InputReserve`, `InputSubmit`, `InputStatus`),
   so those six reject a frame that omits the key. Add `#[serde(default)]` to all six.
   Observable change: those requests accept an omitted `instance`, matching the documented
   protocol. Add a decode test per variant.
6. **`ManagerReply` wildcard lists.** `server/mod.rs:187-199` and `main.rs:482` enumerate nine
   `ManagerReply`/`ManagerOutcome` variants to say "unexpected". Add
   `ManagerReply::into_descriptor(self) -> Result<Descriptor>` in `daemon` and use `Ok(_) =>
   bail!(..)` after the matched arms.

## Batch 3. Server, proto and daemon deduplication (clear win)

1. **Delete the manager enum mirrors.** `server/connections.rs:512-553` maps fourteen
   `ManagerRequest` arms to identical `ManagerAction` arms; `:575-599` maps eleven
   `ManagerOutcome` arms back to `ManagerReply`; `ecs/messages.rs:34-98` defines the mirrors.
   The ECS already consumes `control::Request` directly in `Inbound::ControlRequest`, so the
   mirror isolates nothing. Change `Inbound::Manager` to carry `ManagerRequest` and have the
   ECS emit `ManagerReply` directly, with the one real difference (`Attach { name, created,
   stream }` versus `Attach { descriptor }`) handled by the connection building the descriptor.
   `created` is never read; drop it.
2. **Delete `DESCRIPTOR_HOOK`.** `connections.rs:494,603-618` install a process-wide
   `OnceLock<Arc<dyn Fn>>` that calls `adapter::descriptor(&paths, &identity, name, stream)`
   (`adapter.rs:348-361`), a pure function of two `Clone` values. Add `paths: DaemonPaths` and
   `identity: ManagerIdentity` to `Owner` (`connections.rs:24-32`) and call it directly.
   Delete `DescriptorHook`, `DescriptorLookup`, the install at `server/mod.rs:135-142`, and
   the one-line forwarder `Adapter::descriptor_for` (`adapter.rs:298-304`).
3. **One registration channel.** `Owner` carries three `mpsc::Sender`s
   (`connections.rs:28-30`), `ServerState` three receivers (`mod.rs:87-89`), `collect` three
   `while let` loops (`mod.rs:224-232`) and `wait_for_activity` three `select!` arms
   (`mod.rs:369-371`). Replace with `enum Register { ControlReply(u64, oneshot::Sender<Reply>),
   ManagerReply(..), Outbox(ViewerId, ViewerOutbox) }` on one channel and one
   `Adapter::register(Register)`.
4. **One stale-socket remover.** `proto/socket.rs:43-89` and `daemon/startup.rs:92-135` are
   the same algorithm; the manager version probes connect three times with 5 ms sleeps.
   Keep one `remove_stale_socket(path, owner_dir, probes: u8)` in `proto/socket.rs`.
5. **`same_user` is `local_ipc::peer_uid`.** `daemon/startup.rs:327-346` reimplements the
   platform split that `local-ipc/src/lib.rs:126-157` already owns. Replace the body with a
   call to `peer_uid` and delete the three `cfg` blocks. Likewise `proto/socket.rs`
   re-wraps nearly every local-ipc entry point (`authorize_peer`+`authorize_uid` equal
   `peer_is_current_user` with a message); call local-ipc directly where the wrapper adds
   only a string.
6. **One `encode_line`.** Serialize-then-check-`MAX_FRAME_BYTES` is written at
   `proto/control.rs:1184-1195`, `:1198-1212`, `connections.rs:443-444` and `:450-460`; the
   last one serializes the reply twice. Provide
   `pub fn encode_line<T: Serialize>(value: &T) -> io::Result<Vec<u8>>` and make all four
   callers one-liners.
7. **`PathError` round-trip.** `local_ipc::ensure_private_directory` returns a typed
   `DirectoryError`; `proto/socket.rs:33-41` flattens it to `io::Error`; `daemon/paths.rs:86-94`
   sniffs `PermissionDenied` to recover `PathError::UnsafeDirectory`. Match `DirectoryError`
   directly. Make `PathError::Io` carry `#[source] std::io::Error` instead of a `String`
   (drop the unneeded `Clone, PartialEq`). `paths.rs:79-84` `absolute()` duplicates the
   closure in `local-ipc/src/lib.rs:474-479`; expose one.
8. **`Box<dyn ChildKiller>` is `kill(pid, SIGHUP)`.** `os/pty.rs:46,127,469` store a trait
   object whose only use (`:455`, `:483`) is portable-pty's `libc::kill(pid, SIGHUP)`. Drop
   the field from `PaneProcess` and `ProcessGroup`, call `nix::sys::signal::kill` directly
   (`kill_group` at `:491` already does), and delete the `clone_killer()` call at `:420` and
   the `NoProcess` test double at `:549-558`.
9. **Extract `serve_subscription`.** `connections.rs:331-399` inlines the whole subscription
   state machine inside `serve_control_connection`. Extract it. Replace the hand-rolled
   `lock().unwrap_or_else(PoisonError::into_inner)` at `:334-336` and `os/pty.rs:181` with the
   existing `crate::os::lock`.
10. **Extract the pump threads.** `PaneProcess::spawn` (`os/pty.rs:190-351`, 161 lines) defines
    the reader body (`:262-313`) and writer body (`:316-338`) inline; each is self-contained
    via moved `Arc`s. Extract `reader_pump` and `writer_pump`. Leave `try_wait`-based reaping
    and the `filedescriptor` crate alone (a `nix::unistd::dup` would need `unsafe`).
11. **Dead protocol items.** `proto/control.rs:1094-1104` `Event::kind` and `:1120-1135`
    `EventKind` have no production caller in fux or zor; `ErrorCode::Timeout` (`:1040`) has
    zero uses. Remove them and their tests, and note in `docs/local-control-protocol.md` that
    `EventKind` was decode-only and never emitted.

## Batch 4. ECS request-handler ergonomics (clear win)

1. **`resource_scope` instead of cloning the batch.** `requests.rs:236-248` does
   `iter_current_update_messages().filter(..).cloned().collect::<Vec<Inbound>>()`, deep-copying
   every `ViewerRequest::Input(Vec<u8>)`, `control::Request` and `LayoutArchive` per step;
   `creation.rs:243-250` clones every `SpawnCompleted` result string. Use
   `world.resource_scope::<Messages<Inbound>, _>(|world, inbound| ..)`. Nothing under
   `apply_control`/`apply_manager` reads `Messages<Inbound>`. Handlers take `&Request` or
   clone only the small enum after matching. Then `drain_viewer_queues` stops cloning
   `required_process` at `requests.rs:353`.
2. **One `Failure` type.** Every handler takes `id: u64` solely to build `Reply::Failed`;
   there are 137 `failed(` call sites across `src/ecs`, 64 of them
   `ok_or_else(|| failed(id, ErrorCode::NotFound, ..))`. Introduce
   `struct Failure { code: ErrorCode, message: String }` with `Display` and constructors
   (`Failure::not_found(..)` etc.), make handlers return `Result<Outcome, Failure>`, and attach
   the request id in exactly one place in `apply_control`/`apply_manager`. This removes the
   `id` parameter from about fifteen signatures (`split`, `focus`, `kill`, `resize`,
   `tab_action`, `workspace_action`, `set_right_click`, `move_pane`, `within_workspace`,
   `layout_control::apply`, `ensure_settled`, `inspect`, `export_document`,
   `input::reserve/status/submit`), the `{error:?}` stringification at
   `layout_archive.rs:425,564`, the three identical `match result { Ok(..) => Reply::Completed
   { id: 0, .. }, Err(reply) => reply }` blocks at `requests.rs:1140-1173`, and the duplicated
   `"workspace creation failed".into()` at `:1227` and `:1331`.
3. **`resolve_pane` / `resolve_tab` helpers.** The three-step
   `pane_in_workspace(..) → ok_or_else(NotFound) → world.get::<Pane>(..).ok_or_else(NotFound)`
   pattern appears with four different messages ("pane not found" ×10, "pane no longer
   exists" ×4, "pane missing" ×2, "live pane not found" ×2; tabs likewise). Add on `Context`:
   `fn pane(&self, world: &World, id: PaneId) -> Result<(Entity, &Pane), Failure>`,
   `fn live_pane(..)` (also rejects `Starting`), and `fn tab(..)`. Make the messages uniform;
   `ErrorCode` is what koh and zor branch on, so check which tests assert on text and update
   them. Replace the twelve manual `world.get::<Pane>(x).map(|p| p.id)` sites (e.g.
   `creation.rs:365,494`, `layout_archive.rs:26`) with the existing `pane_id`/`tab_id` helpers
   at `support.rs:175-181`.
4. **Argument structs instead of `allow(too_many_arguments)`.** `requests_control.rs:4`
   disables the lint file-wide; `split` (`:72-88`) takes fifteen parameters copied one by one
   from `Request::Split` at `requests.rs:736-766`; `move_pane` (`transfer.rs:16-26`) takes
   nine, `within_workspace` eight. Introduce `SplitSpec` and `MoveSpec` built by destructuring
   the request once. Remove the `allow`s.
5. **Small ECS idiom items.**
   - `lifecycle.rs:152-159` computes `viewers_of_workspace` and then an `any` with the
     identical predicate; keep the `any`.
   - Move `requests::despawn_viewer` into `support` next to `ViewerExit::despawn`
     (`support.rs:36-40`), which does the same three things.
   - `requests.rs:136-143` and `:358-368` duplicate the required-process predicate; add
     `Pane::is_required_process(&self, want: &InitialTarget) -> bool`.
   - `Limits` (`resources.rs:12`) is all integers; derive `Copy` and stop the `.clone()`s at
     `creation.rs:44,208,587` and `lifecycle.rs:23`.
   - `viewers_of_workspace` (`support.rs:251`) returns a `Vec` that `requests.rs:1435` only
     `.len()`s; add a count function.
   - `RenamePane` (`requests.rs:784-815`) and `set_right_click` (`requests_control.rs:25-54`)
     are the same "compare field, bump `layout_generation`, mark dirty" routine; one generic
     `update_pane_field` covers both.
   - `ordered_workspaces` (`requests.rs:1347-1355`) ranks with `position` inside `sort_by`;
     build a `BTreeMap<Entity, usize>` first.
   - `list_workspaces` (`requests.rs:1383-1388`) fabricates a `Context` only so `summarize`
     can call `context.selection(world)`; change `summarize` to take `&Selection`.
   - `pub fn notice` at `requests.rs:1498` and `Session::workspace_names` (`mod.rs:154`) have
     zero callers. Delete them.

## Batch 5. ECS-native where it pays (trade-off, approved)

1. **Marker components for workspace state.** `workspace.retiring.is_some()/is_none()` is
   checked 23 times and `.open` 13 times across `src/ecs`, with the conjunction "open and not
   retiring" written out nine times. Keep `Workspace { name, label, selection, last_attached,
   tab_counter }`; make the existing `Retiring { since_ms, exit_code }` struct
   (`components.rs:155`) a `Component` inserted on retirement, and add a `#[derive(Component)]
   struct Open;` marker. Typed systems use `Query<&Workspace, (With<Open>, Without<Retiring>)>`
   behind one filter type alias; exclusive code uses `world.get::<Retiring>(ws)`. `retire()`
   at `support.rs:341` becomes an `insert`. This mirrors the existing `Creation` marker
   pattern, so it is consistent with the codebase.
2. **`on_remove` hooks for `Ids` index maintenance.** Five sites remove ids by hand
   (`support.rs:37,371,386,424`, `requests.rs:230`) and four `check_invariants` branches
   (`mod.rs:210,247,277,306`) exist to catch a miss. Add `#[component(on_remove = ..)]` hooks
   on `Pane`, `Tab`, `Viewer` and `Workspace` that only remove the entry from `Ids`. This does
   not violate the "no observers or component hooks drive core commands" rule in
   `design.md`, since an index hook drives nothing; add one sentence there saying so. Keep
   `check_invariants` as a test-only assertion that the maps and entities agree.
3. **`Mut::bypass_change_detection` on read-only `get_mut` sites** at `requests.rs:466`
   (history view), `:898` (capture) and `:1430` (listing refresh), so the doc's corrected
   reason in Batch 1 holds and future `Changed<T>` use is not poisoned by reads.

## Batch 6. Terminal, view, CLI and config (clear win)

1. **Replace `GridCell` with `vt100::Cell`.** `terminal.rs:253-309` defines
   `GridCell { text: [u8; 22], len, kind, style }` with `from_vt100`, `text()` and a hand
   optimised `matches_vt100`, plus a 57-line test (`:717-773`) proving `matches_vt100 == (self
   == from_vt100(cell))`. `vt100::Cell` (0.16.2) is `Clone + Eq` and its `PartialEq` already
   compares only `len`, attrs and `contents[..len]`. Store `Vec<vt100::Cell>`, compare
   `screen.cell(row, col) != self.cells.get(..)` in `refresh` (`:367-379`), clone in
   `copy_row`, and run `classify`/`CellStyle::from_vt100` only when emitting wire cells in
   `update` (`:462-492`) and `capture_lines` (`:416-442`). Delete `GridCell` and the
   equivalence test. Grapheme segmentation moves off the per-refresh comparison path.
2. **One row encoder.** `PaneView::from_screen` (`view.rs:304-346`) is called only from tests;
   production goes through `PaneUpdate::full_from_screen` (`view.rs:904-963`) then
   `PaneView::from_update`. Delete `from_screen`, have tests use the production path (the
   equivalence is what `view.rs:1407-1409` asserts), and factor a private `wire_row` used by
   `Grid::update`, `Grid::capture_lines` and `full_from_screen`. `PaneView` then no longer
   needs `vt100`.
3. **Finish the clap migration in `main.rs`.** Thirteen subcommands take `PassthroughArgs`
   (`main.rs:72-104`) and are parsed by hand in `alias_request` (`:731-917`),
   `parse_pane_options` (`:941-1041`), `parse_capture_options` (`:1050-1074`), `parse_target`
   (`:919-928`) and `workspace_command` (`:547-656`), while `layout_cli.rs` already uses clap
   derive. Help at `main.rs:71` and the fallback error at `:649` have already drifted.
   `PaneOptions` is an 8-tuple alias (`:930-939`) destructured twice. Make `Workspace`, `Tab`,
   `Split`, `New`, `Focus`, `Capture` and `SendKeys` clap `Subcommand`/`Args` structs
   (`#[arg(last = true)] argv: Vec<String>` for the command tail; `value_parser =
   clap::value_parser!(u16).range(500..=9500)` for `--ratio` as `layout_cli.rs:70` does).
   Replace the `&dyn Fn() -> Result<u64>` parameter at `:734` with an `Option<u64>` resolved
   by the caller. Keep `ctl` JSON as the one escape hatch. Verify every existing
   `tests/local_cli.rs` and fixture invocation still parses; add `--help` snapshot tests.
4. **`ValueEnum` on `RightClickPolicy` and `Direction`.** `main.rs:389-393` and `:976-981`
   both match `"auto" | "fux" | "pane"` strings although `view.rs:258-264` already has
   `RightClickPolicy::name()`; `layout_cli.rs:120-136` defines `enum Side` solely to derive
   `ValueEnum`. Derive `clap::ValueEnum` on both real types with `rename_all = "kebab-case"`,
   delete `Side` and both string matches, and make `PaneInputArgs.right_click`
   (`main.rs:110-111`) typed.
5. **Move daemon logic out of `main.rs`.** `CappedLog` (`main.rs:228-300`, plus its test at
   `:1128-1157`) is filesystem policy; `start_server` (`:510-545`) and `resolve` (`:470-490`)
   are the attach handshake. Move them to `daemon::log` and `daemon::bootstrap`. `main.rs`
   should end near 600 lines of clap and dispatch, as its header comment (`:2-3`) claims.
6. **`Key` newtype in config.** `Config { prefix: String, bindings: BTreeMap<String, Action> }`
   (`config.rs:27-29`) is validated by a hand loop (`:99-121`) and then re-validated and
   re-parsed in `commands::configured_bindings` (`commands.rs:370-383`). Add `pub struct
   Key(u8)` in `commands.rs` with serde via `key_byte`/`key_name`, so `Config { prefix: Key,
   bindings: BTreeMap<Key, Action> }`; `validate()` keeps only the prefix-clash and Shift-twin
   checks. Invalid notation is then rejected by serde with the offending path.
7. **Boolean parameters.** `status(failed: bool)` (`main.rs:502-508`) is called with a double
   negative at `:418`; `LayoutTree::cycle(.., forward: bool)` (`layout/edit.rs:188`);
   `Action::unavailable(.., workspaces: bool)` (`commands.rs:145`); `capture(.., attrs: bool,
   ..)` (`terminal.rs:656`). Replace with `reply.exit_code()`, a `Direction` argument, a
   `DispatchContext`, and the existing `proto::control::CaptureFormat`. Then enable
   `clippy::fn_params_excessive_bools` in the workspace lints.
8. **Split `tests/ecs.rs`.** It is 6,878 lines with `struct Harness` defined at line 815,
   after 800 lines of tests that use it. Move to `tests/ecs/main.rs` with `harness.rs` and
   topic modules (`final_records`, `input`, `capture`, `layout`, `transfer`, `workspace`,
   `randomized`). One integration binary, so no compile-time cost. Test names and counts must
   be unchanged; diff `cargo test -- --list` before and after.

## Batch 7. Client (safe collapses first, parser merge last)

Land 1 to 6 first; they change no behavior and shrink `interaction.rs`, `controller.rs` and
`mod.rs` by roughly 400 lines. Item 7 is the risky one and gets its own gate run and a review
of `docs/design.md` "Viewer" guarantees (unknown keys stay in command mode; paste and
fragmented sequences preserved byte-exact).

1. **One `Step` return from `Mode::key`.** `interaction.rs:220-236` defines `KeyTransition {
   completion: Completion, effect: Option<KeyEffect>, notice }`; `Completion` is a two-state
   enum used as a bool at 18 sites; `Mode::key` (`:242-651`) wraps its body in an IIFE
   returning `Option<Request>` re-wrapped at `:647-649`; `KeyEffect::Copy`/`ScrollCopy` are
   emitted from `Mode::Copy` and routed back into `Mode::Copy` via `controller_copy.rs`.
   Replace with `enum Step { Keep, Finish(Option<Request>), Send(Request),
   Manager(ManagerRequest), Action(Action, Target), Copied(String), Notice(&'static str) }`,
   handle copy keys inline in the `Mode::Copy` arm, and delete `controller_copy.rs`.
2. **Deduplicate chooser mouse handling.** `controller.rs:839-992` repeats the same
   reconcile/wheel/hit-test/select/confirm block four times (SwapPicker, Menu, Close dialogs,
   Destination/Tabs/Workspaces) with a six-line hit-test closure copied at `:855, :890, :920,
   :973`. Add `Mode::selection_mut() -> Option<(&mut usize, usize)>`, one `hit()` helper, and
   `MouseEvent::plain_wheel()` (the `code & !(4|8|16|64|1) == 0` test at `:967` duplicates
   `drag.rs:144`). `route_mouse` (337 lines) should drop under 200.
3. **Use `effects::Identity` everywhere.** `effects.rs:22-32` already defines
   `Identity(instance, workspace, stream, viewer)` with `Identity::of(frame)`, yet the four
   fields are hand-copied into `Mode::RenameWorkspace`/`CloseWorkspace`
   (`interaction.rs:30-42`), `SwapPicker` (`context.rs:33-37`), `Menu` (`context.rs:18-22`)
   and `Drag` (`drag.rs:18-20`), and compared four-way at seven sites. Store `identity:
   Identity` and compare with `==`. Delete the in-`key` re-validation at
   `interaction.rs:246-249, :262-265, :458-466, :558-566` (`reconcile` already runs on every
   frame at `controller.rs:341`); the two now-unreachable stale-target notices go with it.
4. **`Target` instead of whole-`Frame` clones.** `Menu::project` (`context.rs:187-210`) clones
   the entire `Frame` (every pane's `Vec<Cell>`) to override `focused`/`active_tab`; it is
   stored in `KeyEffect::Action`, `Controller::pending_action` (`controller.rs:27`),
   `WaitingCommand.target` (`mod.rs:118`) and cloned at `mod.rs:433` on every layout-mode
   keypress awaiting a reply. Replace with a `Copy` struct `Target { focused: Option<PaneId>,
   tab: Option<TabId>, generation: u64 }` and make `dispatch`/`Action::unavailable` take
   `(&Frame, Target)`.
5. **Shared text-entry and confirm modes.** Four text-entry variants (`RenameWorkspace`,
   `RenamePane`, `Rename`, `NewWorkspace`) each re-implement Enter-vs-`edit_text`
   (`interaction.rs:419-538`), the `text_entry()` list (`:94-102`) and a `HintPanel::text_input`
   arm (`controller.rs:1421-1449`); three close-confirm variants duplicate the panel
   (`:1451-1484`) and `y/n` handling (`:540-598`). Replace with `Mode::Text { kind: TextKind,
   text, identity }` and `Mode::Confirm { kind: CloseKind, identity }` where `kind.submit(text,
   frame)` and `kind.title()` carry the differences. `enter()` clears
   `entry_regions`/`panel_bounds` three times in one function (`:540-541, :554-555,
   :562-563`); once.
6. **`run` loop repetition and render allocations.** `mod.rs:425, 467, 563, 571, 620, 633,
   660` all read the same `effects.input(&mut pane_bytes, (manager_mutations > 0 ||
   effects.manager_pending()).then_some(current))?` line, and the readiness predicate appears
   at `:290-292, :399, :477-479`. Move `pane_bytes`, `manager_mutations`, `outstanding` into
   `effects::Queue` with `input()`, `push()` and `ready()`; split `run` (548 lines) into
   `handle_message`, `apply_events`, `drain_effects`, `paint`; bundle the three signal
   receivers into `Signals` so the `allow(too_many_arguments)` at `:209` goes. In render:
   merge `Composed` and `HitRegions` (`render.rs:92-104`, copied field by field at
   `screen.rs:76-98`); keep two buffers in `Screen` and swap instead of allocating
   `Buffer::empty` per paint at `render.rs:116` and `:648`; collapse the three colour/style
   types (`render.rs:631-637`, `:688-710`, `backend.rs:12-20`, `:83-107`) by emitting SGR
   directly from ratatui's `Color` and `Modifier`, deleting `rat_to_vt`, `backend_style` and
   `backend::CellStyle`.
7. **One byte parser.** `input.rs:159-218` (`PrefixFilter::feed_byte`) and
   `controller.rs:1154-1284` (`Controller::feed` + `plain_input`) both parse ESC/CSI/SS3/
   paste/SGR-mouse byte by byte with different CSI-completion rules (`controller.rs:1173-1178`
   versus `input.rs:173-182`) and duplicated arrow tables (`controller.rs:1195-1247`,
   `input.rs:237-243`); `mod.rs:419-448` decides per byte which parser gets it and
   `loading_input` buffers raw bytes for re-push. Make `PrefixFilter` the only parser with a
   modal flag so it emits `InputEvent::Key(char) | Nav(..) | Paste(bytes) | Mouse(..)` while a
   mode owns input; `Controller::feed(byte, frame)` becomes `Controller::event(InputEvent,
   frame)`; delete `escape`, `utf8`, `paste`, `escape_pending`, `resolve_escape` and the
   parser-state clauses of `owns_input` (`controller.rs:135-153, 159-166, 738-746`);
   `loading_input` becomes `Vec<InputEvent>`. Move the fragmented-sequence and paste-tail
   controller tests (`canceled_modes_keep_owning_unfinished_pastes`,
   `overlapping_paste_delimiters_release_a_cancelled_mode`, and their neighbours) to
   `input.rs` tests. The `control_traces.rs` generative test must still pass unchanged; it is
   the proof. Drop the unused raw bytes from `InputEvent::Mouse(MouseEvent, Vec<u8>)`
   (`input.rs:29`; production does `let _ = raw;` at `mod.rs:576`).

## Batch 8. Wire-visible: compact cell styles (next protocol bump only)

`view.rs:59-68` serializes `CellStyle` as a seven-field struct and `Color` (`:41-47`) externally
tagged, so every coloured cell carries about 110 bytes of JSON. Serialize `CellStyle` as
`[fg, bg, attrs]` with `attrs` a `u8` bitset mirroring `vt100::Attrs.mode`, and `Color` as
`"d"`, an integer, or `[r, g, b]`, via `#[serde(into, from)]` on a small wire tuple; keep the
typed struct in memory. Update `docs/local-attachment-protocol.md`, the client decoder at
`render.rs:613-635`, and the koh consumer fixtures in `tests/protocol_consumers.rs`. With
predictable per-cell size, `Grid::capture_lines` (`terminal.rs:432`) can bound rows by cell
count instead of `serde_json::to_vec` per row. This breaks mixed-version attach; it ships only
with the protocol bump and a CHANGELOG entry.

## Batch 9. API-visible: public surface (next minor only)

fux exposes 475 `pub fn` against one `pub(crate)`; zor does not depend on the fux crate, and
integration tests reach only a handful of items. Because `pub` items satisfy `dead_code = deny`,
the compiler cannot find leftovers (Batch 4 found two by hand). Keep `ids`, `layout`, `proto`,
`view`, `config`, `commands`, `client::{attach, attach_reported, AttachOptions}`, `daemon` and
`server::run` public; make `ecs::systems`, `ecs::support`, `os`, `server::adapter`,
`server::connections` and every `client` submodule `pub(crate)`; expose the ECS items tests
need (`Ids`, `ServerIdentity`, `EventLog`, `MAX_*`, `Session` accessors) under
`#[doc(hidden)]` or an `ecs::testing` module. Then fix every dead item the compiler reports.
In local-ipc, make `Connecting` `pub(crate)` and drop `BoundSocket::path()` (tests only).
Both are semver-minor removals; record them in both CHANGELOGs.

## Completion

The work is complete when every batch through 7 has landed with a green gate, the report in
`docs/codebase-simplification.md` lists lines removed per batch, `git ls-files | wc -l` has
dropped from 3,468 to under 900, `main.rs` is under 650 lines, `tests/ecs.rs` no longer
exists as a single file, and `grep -rn 'PaneId(0)\|DESCRIPTOR_HOOK\|ManagerAction\|GridCell\|KeyTransition\|resolve_waits' crates docs` returns nothing. Batches 8 and 9
are complete only if separately authorized. Report honestly anything left undone and why.
