# Bevy source patterns for the fux + zor rewrite

Companion to [`prompts/ecs-native-rewrite-prompt.md`](prompts/ecs-native-rewrite-prompt.md). It records, file by file, what the pinned Bevy checkout (`../many_rigs/inspirations/bevy`, revision `b56fc29d3016e641754765244b5ba3f9cc504671`, 0.19.1) already implements that the rewrite should drive, copy as an idiom, or deliberately avoid. Every claim cites `crates/<crate>/src/<file>.rs:<lines>` in that checkout; nothing here was verified by building. Section numbers in parentheses refer to the rewrite prompt.

Crates covered and why:

| Crate | Role for fux/zor | Status in the rewrite stack |
|---|---|---|
| `bevy_ui_widgets` | headless widgets: state as markers, behaviour as entity observers, keyboard + pointer dual handling; the model for the viewer's choosers, popups, prompts, hint panel and for pane/separator interaction | reference only (not a dependency) |
| `bevy_feathers` | theme tokens and focus indication; the model for the fux `Theme` asset | reference only; depends on `bevy_render` |
| `bevy_picking` | pointer state, backend contract, hover map, event pipeline with bubbling; the server's cell backend is a port of `window.rs` | dependency (3.4) |
| `bevy_input_focus` | single-focus resource, `FocusedInput` bubbling, tab/directional navigation; the viewer's focus and `h j k l` navigation | dependency, viewer App only (3.11) |
| `bevy_ui` core + `bevy_app::propagate` | the layout engine fux drives through per-viewer cameras; what fux writes and reads | dependency (3.4) |
| `bevy_remote` | method registry over `SystemId`, mailbox dispatch, HTTP/SSE transport | dependency (3.9) |
| `bevy_state` | `StateTransition` schedule, `OnEnter`/`OnExit`/`OnTransition`, sub/computed states | dependency (3.1, 3.11, 4.1) |
| `bevy_asset` `server/mod.rs` | the async-outside / ECS-inside boundary; the shape of the PTY and provider adapters | dependency (3.6, 3.7) |

## Cross-cutting conclusions

1. **State is presence, behaviour is an observer.** `bevy_ui_widgets` keeps every interactive state as a marker component (`Pressed`, `Checked`, `InteractionDisabled`, `Hovered`) and every reaction as an entity observer attached at spawn (`observe.rs`). fux's viewer chrome and the server's pane/separator interaction follow this exactly; policy that must be ordered (lifecycle, authority, persistence) stays in systems, per prompt 3.3.
2. **Pointer input is a pipeline you feed, not a plugin you enable.** `bevy_picking` ships no input backend usable without a window; fux writes `PointerInput` messages per viewer and a ~50-line backend that turns cell coordinates into `PointerHits` against `ComputedNode`s (`window.rs` is the template). `PointerInputPlugin` and `DefaultPickingPlugins` are never added.
3. **Focus is global in `bevy_input_focus`.** `InputFocus` is one resource per World, `FocusedInput` has a private `window` field and dispatch needs a `PrimaryWindow`. That fits the viewer App (one terminal, one focus) and not the server, which keeps a fux `FocusedPane` component per viewer (3.2). Tab and directional navigation give the viewer pane cycling and `h j k l` for free once pane mirrors carry `TabIndex` and a `DirectionalNavigationMap`.
4. **`bevy_ui` lays out against a camera's computed render-target info, never a window.** fux writes `Camera.computed.target_info` and `UiTargetCamera` per tab root; it reads `ComputedNode.size`/`unrounded_size` and `UiGlobalTransform`. With `scale_factor` and `UiScale` at 1.0 all units are cells. Nothing in the UI tree may carry `Disabled`.
5. **`bevy_remote` is a `SystemId` table plus a mailbox.** Handlers see `params` only; the table has no `remove`; watches re-run every tick; `world.observe+watch` buffers unbounded. The prompt's token-wrapper design follows from those facts.
6. **`bevy_state` transitions are a schedule, not a flag.** `NextState` is consumed in `StateTransition` at the start of `Main`, `OnExit`/`OnTransition`/`OnEnter` run in that order, sub-states exist only while their parent matches. Server modes, viewer modes and zor lifecycles map onto this; per-entity lifecycles do not (they stay typed enum components).
7. **`bevy_asset` shows the adapter shape.** Work runs on `IoTaskPool`, results cross a channel, one system drains the channel each update and emits typed messages/events. The PTY reader, spawn completion, provider adapters and koh helpers use that shape and nothing else touches OS handles from inside the World.

## bevy_ui_widgets

Crate facts that bound every subsection: no `bevy_render` edge; deps are bevy_app, bevy_a11y, bevy_camera, bevy_ecs, bevy_input, bevy_input_focus, bevy_log, bevy_math, bevy_picking, bevy_reflect, bevy_ui, bevy_text, bevy_window; `[features] default = []` (`crates/bevy_ui_widgets/Cargo.toml`). The crate is **not** in the section-2 dependency stack, so it is a pattern source only; its state components live in `bevy_ui`, which *is* in the stack: `InteractionDisabled` `interaction_states.rs:23`, `Pressed` `:46`, `Checkable` `:51`, `Checked` `:56`, `Selectable` `:94`, `Selected` `:98`, with add-hooks `on_add_disabled`/`on_add_checkable`/`on_add_selectable` immediately below each. Two hard couplings constrain reuse: every observer takes `On<FocusedInput<..>>` from `bevy_input_focus` (viewer-only per §2), and every pointer observer takes `On<Pointer<..>>` from `bevy_picking`.

### lib.rs
**(a) Mechanism.** Declares the crate's whole output vocabulary and the plugin group. `Activate { entity }` is a bare `EntityEvent + Reflect + #[reflect(Event)]` (`lib.rs:81-85`); `ValueChange<T>` carries `#[event_target] source: Entity`, `value: T` and `is_final: bool` (`lib.rs:90-98`), where `is_final` distinguishes drag-in-progress from committed edits. `UiWidgetsPlugins` (`lib.rs:60-77`) is a `PluginGroupBuilder` chaining nine widget plugins plus `PopoverPlugin`; the doc comment states widgets keep no internal state and rely on the app to mutate state in response (`lib.rs:11-17`).

**(b) fux/zor use.** This is the event contract for §3.3: `Activate` becomes the payload of the fux command-column / confirmation / chooser activation path (`fux/tab.select`, `fux/workspace.kill` confirmation, `fux/layout.apply` confirm), and `ValueChange<T>` becomes the payload for workspace-chooser selection, tab-chooser rows, and `input.{reserve,submit}` draft edits. `is_final` maps directly onto fux's requirement that BRP mutations be idempotent under replay: only `is_final: true` should produce a mutating request, while `is_final: false` is a local preview. If the widget plugins are vendored into fux, this group is the registration list for the viewer App.

**(c) Pitfalls.** Neither event derives `#[entity_event(propagate)]`, so they do not bubble up `ChildOf` — fux's lifecycle events (§3.3) *do* need propagation, so they cannot reuse this derive shape verbatim. `ValueChange<T>` is generic but `#[reflect(Event)]` is on the generic type; §3.3 requires each event to be `register_type`d for `observe+watch`, so each concrete `ValueChange<X>` fux publishes needs separate registration. Also, `is_final` semantics are per-widget conventions, not enforced.

**(d) Excerpt.**
```rust
pub struct ValueChange<T> {
    /// The id of the widget that produced this value.
    #[event_target]
    pub source: Entity,
    /// The new value.
    pub value: T,
    /// If false, it means that we are in the middle of an interaction (slider being dragged,
    /// user typing), while if true it means that the user's interaction is finished
    pub is_final: bool,
}
```

### observe.rs
**(a) Mechanism.** `AddObserver<E, B, M, I>` is a zero-component `Bundle`/`DynamicBundle` whose only effect is `entity.observe(add_observer.observer)` in `apply_effect` (`observe.rs:76-84`), with a `is_despawned()` guard (`:66-68`). The `Bundle::component_ids`/`get_component_ids` impls return empty iterators and `get_components` is a `mem::forget` no-op, so the struct occupies no storage; the file carries `#![expect(unsafe_code, reason = "Unsafe code is used to improve performance.")]` (`:3`) and its own `TODO` that it may not belong in this crate (`:1`).

**(b) fux/zor use.** This is the mechanism for attaching entity observers at spawn time in §3.7 BSN templates: `pane(shell())` can bundle `observe(on_pane_press)` in one spawn instead of a follow-up `commands.entity(e).observe(..)` that races with the spawn. Directly relevant to the §3.4 note that pointer observers on pane/separator entities are the only place mouse policy lives, and to §3.3's observer entities.

**(c) Pitfalls.** Requires cloning/copying the *observer system value* into the bundle; closures capturing non-`Clone` state are awkward. It is `unsafe` and uses low-level `MovingPtr` plumbing (`:47-63`), so vendoring it into fux carries the unsafe impl. `apply_effect` silently skips despawned entities — an observer silently not attached is a *silent* bug, so invariant tests should assert observer presence where policy depends on it.

**(d) Excerpt.**
```rust
impl<E: EntityEvent, B: Bundle, M, I: IntoObserverSystem<E, B, M>> DynamicBundle
    for AddObserver<E, B, M, I>
{
    type Effect = Self;
    unsafe fn apply_effect(
        ptr: bevy_ecs::ptr::MovingPtr<'_, mem::MaybeUninit<Self>>,
        entity: &mut bevy_ecs::world::EntityWorldMut,
    ) {
        let add_observer = unsafe { ptr.assume_init() };
        if entity.is_despawned() { return; }
        let add_observer = add_observer.read();
        entity.observe(add_observer.observer);
    }
}
```

### button.rs
**(a) Mechanism.** `Button` is a bare marker requiring an a11y role (`button.rs:28-29`); `ActivateOnPress` (`:35`) switches the activation edge from release to press. Six observers implement one state machine over `Pressed`: down inserts `Pressed` and fires `Activate` immediately if `ActivateOnPress` (`:76-98`); up/drag-end/cancel remove `Pressed` when not disabled (`:100-137`); `Click` fires `Activate` only if still `Pressed`, not disabled and not `ActivateOnPress` (`:58-73`); `FocusedInput<KeyboardInput>` fires `Activate` on non-repeat Enter/Space (`:37-56`). Every handler calls `propagate(false)` before acting.

**(b) fux/zor use.** This is the reference for the viewer's clickable chrome: tab strip entries and command-column cells become `Button`-like entities whose release produces `fux/tab.select` / a command dispatch; `ActivateOnPress` maps to the menu-style rows in the tab chooser/workspace chooser (§3.4 pointer policy, §3.11 viewer). The `Pressed` marker is the model for press feedback in the cell painter, and the Enter/Space branch is the keyboard half of §3.11's dual keyboard+pointer handling.

**(c) Pitfalls.** `Click` fires only when `Pressed` is still present, so a backend that synthesises `Click` without a preceding `Press` (a plausible fux picking-backend shortcut for test fixtures) will never activate. All keyboard activation depends on `FocusedInput`, i.e. on the single global `InputFocus` resource; fux's per-viewer `FocusedPane` (§3.2) cannot express that, so these observers must be rewritten against the viewer's own focus component or run only in the viewer App. `propagate(false)` is called even when the entity is disabled/not a button, which differs subtly between handlers (e.g. `:59` fires before the state check, `:78` before the disabled check) — copy one ordering deliberately.

**(d) Excerpt.**
```rust
fn button_on_pointer_down(
    mut press: On<Pointer<Press>>, ...
) {
    if let Ok((button, disabled, pressed, activate_on_press)) = q_state.get_mut(press.entity) {
        press.propagate(false);
        if !disabled && !pressed {
            commands.entity(button).insert(Pressed);
            if activate_on_press {
                commands.trigger(Activate { entity: button });
            }
        }
    }
}
```

### checkbox.rs
**(a) Mechanism.** `Checkbox` requires `AccessibilityNode(CheckBox)` and `Checkable` (`checkbox.rs:37-38`); state is the external `Checked` marker. Click emits `ValueChange<bool>` with `!is_checked` (`:61-79`); Enter/Space does the same (`:40-59`); `Press` additionally takes focus and clears the focus ring visibility (`:81-121`); `SetChecked`/`ToggleChecked` (`:183`, `:211`) are the programmatic entry points and both refuse to fire when the `Query`'s `Without<InteractionDisabled>` filter fails; `checkbox_self_update` (`:274-280`) is the opt-in observer that binds `ValueChange<bool>` back to `Checked`.

**(b) fux/zor use.** The canonical fux control pattern for boolean config and the `[style]` section of fux.toml (§3.7): a checkable row that emits a typed change event and leaves the authoritative value in the config `Asset`. `SetChecked`/`ToggleChecked` are the model for the BRP `fux/*` mutation path — a client request triggers an event, the observer re-derives state, and the same code path serves test fixtures.

**(c) Pitfalls.** Two different disable mechanisms coexist and are easy to confuse: the query filter `Without<InteractionDisabled>` (keyboard/click handlers, `:42`, `:65`) versus a runtime `Has<InteractionDisabled>` check inside `Press`/`SetChecked` (`:96`, `:221`). fux's `Disabled` (§3.2) is a *presence* marker and must never be used on UI-tree entities, so mapping `InteractionDisabled` onto `Disabled` would break layout — it needs a distinct UI-only component. `checkbox_on_pointer_down` writes the global `InputFocus` and `InputFocusVisible` via `Option<ResMut<..>>` (`:88-99`), again viewer-only.

**(d) Excerpt.**
```rust
fn checkbox_on_key_input(...) {
    if let Ok(is_checked) = q_checkbox.get(ev.focused_entity) {
        if event.state == ButtonState::Pressed && !event.repeat
            && (event.key_code == KeyCode::Enter || event.key_code == KeyCode::Space) {
            ev.propagate(false);
            commands.trigger(ValueChange { source: ev.focused_entity,
                value: !is_checked, is_final: true });
        }
    }
}
```

### radio.rs
**(a) Mechanism.** `RadioGroup` is a stateless container requiring only an a11y role (`radio.rs:41-42`); `RadioButton` requires `Checkable` (`:59-60`). `radio_group_on_key_input` (`:62-159`) implements roving selection: it collects non-disabled `RadioButton` descendants via `Children::iter_descendants` (`:86-93`), finds the currently `Checked` index (defaulting to `usize::MAX`), maps `ArrowUp|Left`→prev-with-wrap, `ArrowDown|Right`→next-with-wrap, `Home`→0, `End`→len-1, then triggers **two** events: `ValueChange<bool>{true}` on the button and `ValueChange<Entity>{next_id}` on the group (`:141-155`). `trigger_radio_button_and_radio_group_value_change` (`:297-323`) does the same for clicks and walks `ChildOf` ancestors to find the group. `radio_self_update` (`:343-357`) enforces mutual exclusion by inserting `Checked` on the value and removing it from every other descendant.

**(b) fux/zor use.** The tab-chooser and workspace-chooser are radio groups: arrow keys move the highlighted row, one activation commits. The two-event shape (per-item bool + per-group `ValueChange<Entity>`) is exactly what fux needs to separate *visual row state* (`Checked`/`Selected` marker on the row entity) from the *semantic selection* (`fux/tab.select { tab }` request) without the row knowing the semantic id. `radio_self_update` is the template for the presenter-side `Selected` update driven purely from the event, keeping the request path authoritative.

**(c) Pitfalls.** `iter_descendants` + `collect::<Vec<_>>` allocates on every key press (`:86`) — unacceptable on the viewer hot path; a fux command column should keep an indexed row list component instead. The wrap logic is index-based over *enabled* items only, so a disabled row in the middle is skipped correctly but a disabled row at the ends changes wrap targets. `current_index >= len` is used as the sentinel for "nothing checked", which also matches the `unwrap_or(usize::MAX)` path. `radio_group_on_key_input` requires the *group* to be the focused entity while `radio_button_on_key_input` handles a focused standalone button — fux must pick one model per widget.

**(d) Excerpt.**
```rust
let next_index = match key_code {
    KeyCode::ArrowUp | KeyCode::ArrowLeft => {
        if current_index == 0 || current_index >= radio_buttons.len() { radio_buttons.len() - 1 }
        else { current_index - 1 }
    }
    KeyCode::ArrowDown | KeyCode::ArrowRight => {
        if current_index >= radio_buttons.len() - 1 { 0 } else { current_index + 1 }
    }
    KeyCode::Home => 0,
    KeyCode::End => radio_buttons.len() - 1,
    _ => return,
};
```

### list.rs
**(a) Mechanism.** `ListBox` requires `ActiveDescendant` (`list.rs:28-29`), a `#[component(immutable)] ActiveDescendant(pub Option<Entity>)` (`:52`) that is the single roving cursor for the whole list — rows themselves are not focusable. `listbox_on_key_input` (`:54-172`) mirrors the radio index walk over non-disabled items, prefers the current active descendant, falls back to the first `Selected` row, writes `ActiveDescendant`, sets `InputFocusVisible` to true and triggers `ScrollIntoView { entity }`; Space/Enter triggers `ValueChange<Entity>` on the listbox. `listbox_on_row_click` (`:174-254`) resolves `ev.original_event_target()` upward to the row (bailing if it hits the listbox first), sets the active descendant *even for disabled rows*, then emits `ValueChange<Entity>` unless the row is already selected. `listbox_focus_gained` (`:256-289`) seeds the active descendant on focus; `listbox_focus_lost` (`:291-300`) clears it. `listbox_update_selection` (`:315-364`) is the opt-in exclusion observer.

**(b) fux/zor use.** Direct model for the command column, the tab-chooser list and the copy-mode history row index: an `ActiveDescendant`-style component on the list container holds the cursor row so that only one entity owns focus, `ScrollIntoView` keeps it visible (composed with `ScrollArea`), and activation is a single `ValueChange<Entity>` naming the row — which is what `fux/tab.select`, `fux/pane.focus` and command dispatch consume. The focus-gained/lost seeding mirrors §3.11's `InputFocus` handoff when a viewer attaches.

**(c) Pitfalls.** `ActiveDescendant` is immutable (`:51`), so updates go through `commands.entity(..).insert(..)` — a full component replace per key press. `listbox_on_key_input` calls `q_listbox.get(listbox)` twice (contains + get) and re-collects descendants into a `Vec` on every press. The click handler's upward walk deliberately ignores the row's disabled state when setting the cursor but honours it for selection — subtle and worth keeping only if fux wants clickable-but-unselectable rows. `Selected` semantics (ARIA) differ from `Checked`; fux must not mix the two marker vocabularies in one widget.

**(d) Excerpt.**
```rust
// Clicking sets the active descendant, even if disabled
commands.entity(ev.entity).insert(ActiveDescendant(Some(row_id)));

// List row is disabled.
if let (_, disabled) = q_listitems.get(row_id).unwrap() && disabled { return; }
```

### menu.rs
**(a) Mechanism.** Three components: `MenuPopup` requires `TabGroup::modal()` and `MenuFocusState::Closed` (`menu.rs:124-128`), `MenuButton` requires `Button + ActivateOnPress` (`:416`), `MenuItem` (`:135`). `MenuEvent { source, action }` is `#[entity_event(propagate, auto_propagate)]` with `MenuAction::{Open(NavAction), Toggle, CloseAll, FocusRoot}` (`:62-88`) and bubbles from items through the portal relation to the owner. Dismissal is *focus-driven*, not click-driven: `menu_on_lose_focus` (`:177-209`) runs in `Update` and closes any popup that does not contain `focus.get()` (checking equality or `ChildOf` ancestors), triggering `CloseAll`; `menu_acquire_focus` (`:152-175`) turns `MenuFocusState::Opening(nav)` into `tab_navigation.initialize(menu, nav)` + `focus.set(next, FocusCause::Navigated)`. Keys: Escape on the popup and Enter/Space on an item both close the stack (`:211-328`); arrows move via `TabNavigation::navigate` guarded by `MenuLayout::{Column,Row}`. `MenuButton`'s arrows open with `NavAction::Last`/`First` (`:433-467`).

**(b) fux/zor use.** The dismissal mechanism is the transferable part: fux's tab chooser, workspace chooser, hint panel and confirmation popups should close on *focus leaving the popup subtree* rather than on a click-outside hit test, because the viewer's cell-based picking backend can miss the background cell. Mapping: `MenuEvent::CloseAll` ≙ cancel the pending `fux/*` request / dismiss the hint panel; `FocusRoot` ≙ return focus to the pane (restoring `FocusedPane`); `MenuFocusState::Opening` ≙ the §3.4/§3.11 deferral problem when a popup is a queued spawn and cannot take focus immediately; `ActivateOnPress` menu buttons ≙ tab-strip entries that open the chooser on press.

**(c) Pitfalls.** The module doc is explicit that the *menu button* has a race — clicking it causes focus loss, so the menu is normally already closed by the time the click is processed (`:130-136`) — fux must not copy the naive button/menu pairing without handling that. Menus must contain at least one focusable entity, and two menus cannot be open at once unless nested (`:118-122`). `menu_acquire_focus` and `menu_on_lose_focus` are plain systems, not observers, and must be `.chain()`ed in `Update` (`:474`); `menu_on_lose_focus` iterates and mutates `MenuFocusState` while querying `focus`, so it depends on `Update` ordering. `warn!` on no focusable items (`:166`) means an empty popup silently stays `Opening`. Uses `TabNavigation` from `bevy_input_focus::tab_navigation`, so again viewer-only.

**(d) Excerpt.**
```rust
pub enum MenuAction {
    Open(NavAction),
    Toggle,
    CloseAll,
    FocusRoot,
}
...
let contains_focus = match focus.get() {
    Some(focus_ent) => focus_ent == menu
        || q_parent.iter_ancestors(focus_ent).any(|ent| ent == menu),
    None => false,
};
if !contains_focus {
    *menu_focus = MenuFocusState::Closed;
    commands.trigger(MenuEvent { source: menu, action: MenuAction::CloseAll });
}
```

### popover.rs
**(a) Mechanism.** Pure geometry, no events: `Popover { positions: Vec<PopoverPlacement>, window_margin }` (`popover.rs:86-91`) lists candidate placements; `position_popover` (`:103-286`) runs in `UiSystems::Layout` after `ui_layout_system` and before `update_scrollbar_thumb`. It derives the window rect from `ComputedUiRenderTargetInfo::logical_size()` inflated by `-window_margin`, computes the parent rect from `ComputedNode` minus `border.min_inset/max_inset` and `UiGlobalTransform`, and for each candidate builds a `Rect`, intersects it with the window, and scores `occlusion = rect.area() - clipped.area()` picking the minimum (`:214-227`). It then writes `UiTransform.translation` and forces `PositionType::Absolute`, and if the physical translation is non-zero it manually re-translates `UiGlobalTransform` and recursively fixes descendants (`:288-303`) — the layout system has already run, so the subtree transform must be patched by hand.

**(b) fux/zor use.** The hint panel, the confirmation popup and the workspace-chooser overlay are popovers anchored to a pane or a status cell (§3.11 viewer). The occlusion-scoring loop is the right pattern for a cell-grid viewer: the "window" is the viewer's `ComputedNode` viewport, not a real window, and the same scoring keeps a popup inside an 80x24 cell area. Candidate ordering is the priority list — `[Below, Above]` for a command column, `[Top, Bottom]` for a status hint.

**(c) Pitfalls.** The component is read-only in the layout pass but the system mutates `Node.position_type`, `UiTransform` and every descendant's `UiGlobalTransform` — a second popover pass in the same frame would see stale transforms. `if parent_matrix.determinant() == 0.0 { continue; }` (`:246-248`) silently skips the popover under a degenerate parent. `best_occluded == f32::MAX` means no candidate was evaluated (empty `positions` vec → `Popover::default()` has an empty list, so a defaulted `Popover` does nothing). The `Clone` impl is hand-written (`:94-101`) because derive would be wrong for the nested vec. Requires `bevy_ui`'s absolute positioning to be meaningful with a `RenderTarget::None` camera — verify against §3.4's no-renderer setup before relying on it.

**(d) Excerpt.**
```rust
let clipped_rect = rect.intersect(window_rect);
let occlusion = rect.area() - clipped_rect.area();

// Find the position that has the least occlusion.
if occlusion < best_occluded {
    best_occluded = occlusion;
    best_rect = rect;
}
```

### scrollarea.rs
**(a) Mechanism.** `ScrollArea` requires `ScrollPosition` (`scrollarea.rs:18-19`). `scrollarea_on_scroll` (`:21-49`) converts `Pointer<Scroll>` into a `ScrollPosition` mutation: `MouseScrollUnit::Line` is scaled by `SCROLL_UNIT_CONVERSION_FACTOR`, the delta is subtracted, and each axis is clamped to `(content_size - visible_size).max(ZERO)` — but only if `Node.overflow.{x,y} == OverflowAxis::Scroll`. `on_scroll_into_view` (`:51-121`) implements minimal scrolling: it walks `ChildOf` ancestors to find the first `ScrollArea`, computes the target's top-left/bottom-right in scroll-area-local coordinates (`target_pos - scroll_area_pos + scroll_pos.0`), and adjusts `scroll_pos` only by the amount needed to bring the target fully inside, clamped per axis. Both call `propagate(false)`.

**(b) fux/zor use.** This is the copy-mode history viewport (§3.11 viewer, and the old `terminal.rs` bounded history): wheel events over the pane scroll the transcript, and any cursor movement (search-in-history, next/prev match, ending copy-mode) triggers `ScrollIntoView` so the row is visible. The two-tier clamp (`max_range` per axis, `can_scroll_*` gating) is exactly the arithm
etic needed when a pane is smaller than the scrollback.

**(c) Pitfalls.** `on_scroll_into_view` calls `q_node.get(scroll_area_id).unwrap()` after a `find` over ancestors (`:76`) — safe today only because `ScrollArea` requires a node, but it is an unreachable-panic in a fux fork with `Disabled` entities. Both handlers read `ComputedNode::content_size()` which is only meaningful after the layout pass; fux must order copy-mode scrolling after `UiSystems::Layout` or accept one frame of stale extents. `ScrollIntoView` is declared in `scrollbar.rs`, not here (`scrollbar.rs:41-44`), so a fux build that drops the scrollbar module still needs the event. Line-unit conversion factor is a constant chosen for mouse wheels and will be wrong for trackpad pixel deltas unless the viewer normalises them.

**(d) Excerpt.**
```rust
fn scrollarea_on_scroll(
    mut scroll: On<Pointer<Scroll>>,
    mut q_scroll_area: Query<(&Node, &ComputedNode, &mut ScrollPosition), With<ScrollArea>>,
) {
    if let Ok((node, computed_node, mut scroll_pos)) = q_scroll_area.get_mut(scroll.entity) {
        scroll.propagate(false);
        let visible_size = computed_node.size() * computed_node.inverse_scale_factor;
        let content_size = computed_node.content_size() * computed_node.inverse_scale_factor;
        let can_scroll_y = node.overflow.y == OverflowAxis::Scroll;
        ...
```

### scrollbar.rs
**(a) Mechanism.** `Scrollbar { target, orientation, min_thumb_length }` (`scrollbar.rs:69-75`) names a *separate* entity to scroll; `ScrollbarThumb` (`:102-108`) requires a hand-written set of components including `ComputedNode`, `UiGlobalTransform`, `UiTransform`, `BackgroundColor`, `BorderColor`, `FocusPolicy::Block`, `ZIndex` and deliberately **no `Node`** — the doc says its layout is handled after `ui_layout_system` so its size and position can be derived (`:88-92`). `update_scrollbar_thumb` (`:282-459`) runs in `UiSystems::Layout` after the layout system and computes `thumb_size = (track_length * visible/content).max(min_size).min(track_length)` and `thumb_pos = offset * (track_length - thumb_size)/(content - visible)` inside a local `size_and_pos` closure, then writes `ComputedNode.size/unrounded_size/border_radius/border`, uses `bypass_change_detection()` for border, and rebuilds `UiGlobalTransform` from `scrollbar_transform.affine() * thumb_transform.compute_affine(..) * Affine2::from_translation(..)` — all in physical units via `target_info.scale_factor()`. Track clicks page by one visible size (`:139-194`); thumb drags add `distance * content_size / scrollbar_size` to the drag origin (`:196-256`).

**(b) fux/zor use.** The copy-mode history viewport's scrollbar: fux's viewer paints cells, so the *derived-geometry* half is the reusable part — given a viewport length, a content length and a scroll offset, compute thumb size/position in cell units. If fux paints the scrollbar as UI nodes rather than glyphs, this file is the exact algorithm. The `target: Entity` indirection (scrollbar and scrolled content are different entities) matches fux's separation of the pane entity from its terminal view.

**(c) Pitfalls.** Depends on `bevy_camera::visibility::Visibility` and `bevy_ui::FocusPolicy/ZIndex` (`:2`, `:20-24`), so it assumes a render-shaped world even though fux has no renderer; the `ScrollbarThumb` `#[require(..)]` list would insert render-only components into fux entities. `update_scrollbar_thumb` mutates `ComputedNode` directly inside the layout system set — it is a *derived-geometry writer* that fux's §3.5 invariant "`ComputedNode` is the only geometry read by anyone" tolerates, but it means thumb geometry is not produced by `ui_layout_system` and must be re-derived after any owner change. It also writes `UiGlobalTransform` manually, so a second system moving the thumb would fight it.

**(d) Excerpt.**
```rust
fn size_and_pos(content_size: f32, visible_size: f32, track_length: f32,
                min_size: f32, mut offset: f32) -> (f32, f32) {
    let thumb_size = if content_size > visible_size {
        (track_length * visible_size / content_size).max(min_size).min(track_length)
    } else { track_length };
    ...
    let thumb_pos = if content_size > visible_size {
        offset * (track_length - thumb_size) / (content_size - visible_size)
    } else { 0. };
    (thumb_size, thumb_pos)
}
```

### slider.rs
**(a) Mechanism.** `Slider { track_click, orientation }` requires `SliderDragState, SliderValue, SliderRange, SliderStep` (`slider.rs:105-112`). `SliderValue`, `SliderRange` and `SliderStep` are `#[component(immutable)]` (`:121`, `:128`, `:215`), so all writes go through `insert`. Track click (`:255-369`) resolves orientation (`SliderOrientation::Auto` = `size().y > size().x`, `:51-55`), finds the thumb size by `iter_descendants` + `SliderThumb` (`:301-312`), converts the pointer via `ComputedNode::normalize_point(transform, position * target.scale_factor()/ui_scale.0)`, subtracts `thumb_size/2` from the travel, and applies `TrackClick::{Drag → return, Step → ±step, Snap → precision.round}`. Drag (`:371-428`, `:430-476`) keeps `SliderDragState { dragging, offset }` and recomputes value from `event.distance` un-rotated by `transform.transform_vector2` and divided by `ui_scale.0`; `is_final` is false mid-drag and true on `DragEnd`. Keyboard (`:566-598`) does Left/Right ±step and Home/End. Insert hooks (`:601-640`) push orientation/min/max/step/numeric value into `AccessibilityNode`. `SetSliderValue`/`SliderValueChange::{Absolute,Relative,RelativeStep}` (`:675-712`) is the programmatic path; `slider_self_update` (`:717-720`) binds `ValueChange<f32>` back to `SliderValue`.

**(b) fux/zor use.** Two candidate fux uses: (i) the split-ratio control in the layout inspector / layout archive UI (the same maths as `split_rect`'s ratio, so a slider can drive `flex_grow` directly), and (ii) any numeric config in fux.toml surfaced in the viewer. `TrackClick::Snap` + `SliderPrecision` is the pattern for a ratio that must land on a grid; `SetSliderValue::Relative` is the BRP-mutation shape (`fux/pane.resize` as a delta against a known base with an expected `LayoutGeneration`).

**(c) Pitfalls.** This is the most scale-factor-involved file in the crate: every length mixes `ComputedNode` physical size, `inverse_scale_factor`, `ComputedUiRenderTargetInfo::scale_factor()` and the global `UiScale` resource (`:315-317`, `:496-500`) — fux's viewer sets a `scale_factor_override(1.0)` (§3.11), so these become identity but the code path still runs. Immutable `SliderValue` means a drag emits a new component every frame. `emit_slider_drag_value_change` takes 13 arguments including three queries, which is the cost of not having a slider-local system. Track-click maths assumes the thumb does not overhang; the doc states the travel must be reduced by thumb width (`:88-94`), so a fux slider with a cell-drawn thumb must keep the same invariant.

**(d) Excerpt.**
```rust
let Some(normalized_pos) = node.normalize_point(
    *transform,
    press.pointer_location.position * node_target.scale_factor() / ui_scale.0,
) else { return; };
let track_size = if is_vertical { node.size().y - thumb_size } else { node.size().x - thumb_size };
let click_val = if track_size > 0. {
    let x_from_left = (normalized_pos.x + 0.5) * node.size().x;
    let adjusted_x = x_from_left - thumb_size / 2.0;
    adjusted_x * range.span() / track_size + range.start()
} else { range.center() };
```

### text_input.rs
**(a) Mechanism.** These are *input adapters*, not a widget: they translate events into queued `bevy_text::TextEdit` values on `EditableText`'s `pending_edits`. `on_focused_keyboard_input` (`text_input.rs:56-155`) builds a modifier bitfield (`SUPER/CTRL/ALT/SHIFT`, with `COMMAND`/`WORD` chosen per-OS at `:34-46`) and matches `(mod_flags, logical_key)` against ~25 arms, including `Key::Copy/Cut/Paste`, Cmd-A/C/X/V, word-wise Backspace/Delete, and platform-split Home/End semantics; it sets `keyboard_input.propagate(should_propagate)` so unhandled keys (Tab, Enter-without-newlines) bubble to tab navigation and submit actions. `on_pointer_press` (`:157-215`) maps click count 1/2/3 to `MoveToPoint`/`ShiftClickExtension`, `SelectWordAtPoint`, `SelectAll` after converting the pointer through `transform.try_inverse()` + `content_box().min` + `TextScroll.0`; `on_pointer_drag` (`:217-263`) extends selection. IME is three systems plus two observers: `on_ime_input` (`:266-314`) queues `ImeSetCompose`/`ImeCommit`/clear, `listen_for_ime_input_when_text_input_focused` (`:357-374`) toggles `window.ime_enabled`, `update_ime_position` (`:317-355`) writes `window.ime_position` from parley's `ime_cursor_area().y1`, and `on_focus_lost` (`:393-399`) clears compose and collapses selection. `SelectAllOnFocus` + `QueuedSelectAll` (`:408`, `:414`) defer select-all until pointer release (`:416-457`). The plugin also registers required components `Node`, `TextNodeFlags`, `ContentSize`, `TextScroll` for `EditableText` (`:489-505`), commented as impossible inside `bevy_text` due to a circular dep.

**(b) fux/zor use.** This is the reference for the rename/new-workspace prompts (§3.2 `WorkspaceName`, §3.9 `workspace.rename`, `tab.rename`): single-line text, Enter commits, Escape cancels, arrow/Home/End/word motions, bracketed paste from `Ime::Commit` (§3.11 already names `bevy_window::Ime::Commit` for paste). The transferable structure is *parse keys into an abstract edit enum, queue edits, apply later in one PostUpdate system* — fux should adopt exactly that split with its own edit enum and its own cell-grid text buffer, since it cannot use `EditableText`.

**(c) Pitfalls — this module is the one that does not transfer.** It imports `bevy_text::{EditableText, PreeditCursor, TextEdit}`, `bevy_ui::widget::{scroll_editable_text, update_editable_text_layout, TextNodeFlags, TextScroll}` and `bevy_window::{Ime, PrimaryWindow, Window}` (`:16-29`), i.e. the full text-layout pipeline plus a real window entity. §2 forbids using `bevy_text` types, so fux must re-implement the keymap table and the pointer-to-index conversion against its own buffer. Moreover `window.ime_position`/`ime_enabled` are window fields with no cell-grid analogue; `Window: With<PrimaryWindow>` is single-window by construction (`:339-340`, TODO acknowledges multi-window). The `propagate(should_propagate)` flag is the only thing that lets Tab reach tab-navigation, and the initial `is_composing()` early-return (`:65-69`) is load-bearing for IME/Tab interaction — both easy to lose in a rewrite. `EditableTextInputPlugin` is part of `UiWidgetsPlugins`, so adopting the group wholesale drags this in.

**(d) Excerpt.**
```rust
    // While the IME is composing, all keyboard input (including Tab) belongs to the IME.
    if editable_text.is_composing() {
        keyboard_input.propagate(false);
        return;
    }
    let allow_newlines = editable_text.allow_newlines;
    let mod_flags = (SUPER * u8::from(keys.pressed(Key::Super)))
        | (CTRL * u8::from(keys.pressed(Key::Control)))
        | (ALT * u8::from(keys.pressed(Key::Alt)))
        | (SHIFT * u8::from(keys.pressed(Key::Shift)));
```

## bevy_feathers

Crate facts that bound every subsection: `bevy_feathers` depends on `bevy_render`, `bevy_ui_render`, `bevy_shader`, `bevy_text`, `bevy_asset`, `bevy_scene`, `bevy_ui` (with `bevy_picking`) and embeds FiraSans/FiraMono TTFs plus `alpha_pattern.wgsl`/`color_plane.wgsl` via `embedded_asset!` (`lib.rs:70-85`); features are `custom_cursor`, `webgl`, `webgpu` (`Cargo.toml`). It is **not usable as a fux dependency** (render + text + asset-fetched fonts). Only the token/theme/focus/geometry patterns transfer; the module `containers`, `controls` and `display` are out of scope here.

### theme.rs
**(a) Mechanism.** `ThemeToken(SmolStr)` is the lookup key, displayable/debuggable and `Hash`able (`theme.rs:23-48`); `ThemeProps { color: HashMap<ThemeToken, Color> }` is the payload (`:52-55`) and `UiTheme(pub ThemeProps)` the resource (`:61`). `UiTheme::color` warns once and returns `palettes::basic::FUCHSIA` for a missing token (`:66-76`), and `set_color(&mut self, token: &str, color)` inserts by string (`:79-83`). Three immutable marker components bind entities to tokens: `ThemeBackgroundColor` requires `BackgroundColor` (`:92`), `ThemeBorderColor` requires `BorderColor` and supports only all-borders-same (`:101`), `ThemeTextColor`/`InheritableThemeTextColor` require `ThemedText` + `PropagateOver<TextColor>` (`:109`, `:121`). Resolution has two paths: `update_theme` (`:129-151`) early-returns unless `theme.is_changed()` and then rewrites every background/border/direct-text-span; four `on_changed_*` observers (`:153-188`, `:191-200`) handle per-entity token changes, with `on_changed_font_color` inserting `Propagate(TextColor(..))` for the inheritable variant. `ThemedText` is a bare opt-in marker (`:127`).

**(b) fux/zor use.** This is the direct blueprint for §3.7's `Theme` asset and the `[style]` config section: fux should model `[style]` as a `HashMap<token, color>` parsed into the same shape, hold it as an `Asset` handle (not a plain resource) so `file_watcher` + `AssetChanged<Theme>` drive reload, and keep the *entity-side* component as an immutable token name while one system resolves it. The dual path (bulk `is_changed()` sweep + per-entity Insert observer) is what makes hot reload work without touching every entity: the sweep covers a whole-theme swap, the observers cover a newly spawned widget picking up the current theme.

**(c) Pitfalls.** `update_theme` only reacts to `Res<UiTheme>::is_changed()`; an `Asset`-backed fux theme must explicitly write the resolved theme into a resource (or use `AssetChanged`) or reloads will be missed — and per §3.1, systems reacting to `AssetChanged<T>` must be ordered after `AssetEventSystems` in `PostUpdate` or run one update late. `color()` returns a loud magenta sentinel rather than an error, so a missing token is a *visible* bug, not a failure — fux should keep that behaviour but also make the missing token reachable from diagnostics. `ThemeToken` wraps `SmolStr` and therefore needs the `smol_str` dependency; fux can substitute `Arc<str>`/`&'static str` since its tokens come from config. The text-colour half of this file is unusable in fux (no `bevy_text::TextColor`, no `Propagate` hierarchy plugin — `lib.rs:87-92` installs `HierarchyPropagatePlugin::<TextColor, With<ThemedText>>`, which is `bevy_text`-typed).

**(d) Excerpt.**
```rust
pub(crate) fn update_theme(
    mut q_background: Query<(&mut BackgroundColor, &ThemeBackgroundColor)>,
    mut q_border: Query<(&mut BorderColor, &ThemeBorderColor)>,
    mut q_text_color: Query<(&mut TextColor, &ThemeTextColor)>,
    theme: Res<UiTheme>,
) {
    if theme.is_changed() {
        for (mut bg, theme_bg) in q_background.iter_mut() { bg.0 = theme.color(&theme_bg.0); }
        for (mut border, theme_border) in q_border.iter_mut() { border.set_all(theme.color(&theme_border.0)); }
        ...
    }
}
```

### tokens.rs
**(a) Mechanism.** 387 lines of `pub const X: ThemeToken = ThemeToken::new_static("feathers....")`, grouped by widget with `.hover/.pressed/.disabled/.checked` variants (`tokens.rs:9-10` for `WINDOW_BG`/`FOCUS_RING`/`TEXT_MAIN`/`TEXT_DIM`; menus at `:300-305`; text input at `:307-330`; **pane** at `:332-337`; subpane `:339-355`; group `:357-366`; listview `:368-387`).

**(b) fux/zor use.** Directly reusable naming scheme for the fux `[style]` config: fux should adopt a `fux.*`-prefixed flat token namespace with the same suffix grammar (`<widget>.<part>[.<state>]`), and the pane/listrow token groups (`PANE_HEADER_BG`, `PANE_HEADER_BORDER`, `PANE_HEADER_TEXT`, `PANE_HEADER_DIVIDER`, `PANE_BODY_BG`; `LISTROW_BG_HOVER`, `LISTROW_BG_SELECTED`, `LISTROW_TEXT_DISABLED`) map one-to-one onto fux's pane header, focused-pane emphasis and copy-mode selection row.

**(c) Pitfalls.** All tokens are `new_static`, so the token set is closed at compile time; a config-driven theme cannot invent tokens without a parallel dynamic path (fux should validate config token names against the known set and warn on unknown). Note the inconsistency visible in the source: `SWITCH_BORDER_PRESSED` is defined as `"feathers.switch.border.hover.pressed"` (`tokens.rs:283-285`) while its siblings use `.border.pressed` — a real string drift that a fux test comparing const names to their literals would catch.

**(d) Excerpt.**
```rust
/// Pane header background
pub const PANE_HEADER_BG: ThemeToken = ThemeToken::new_static("feathers.pane.header.bg");
/// Pane header border
pub const PANE_HEADER_BORDER: ThemeToken = ThemeToken::new_static("feathers.pane.header.border");
/// Pane header text color
pub const PANE_HEADER_TEXT: ThemeToken = ThemeToken::new_static("feathers.pane.header.text");
/// Pane body background
pub const PANE_BODY_BG: ThemeToken = ThemeToken::new_static("feathers.pane.body.bg");
```

### dark_theme.rs
**(a) Mechanism.** `create_dark_theme() -> ThemeProps` is a single `HashMap::from([..])` of ~150 `(token, color)` pairs built from `palette::GRAY_*`/`ACCENT` plus `lighter(n)`/`with_alpha(n)` derivations, e.g. `BUTTON_BG_HOVER = GRAY_3.lighter(0.05)`, `FOCUS_RING = ACCENT.with_alpha(0.5)`, `TEXT_INPUT_SELECTION_UNFOCUSED = TRANSPARENT` (`dark_theme.rs` throughout; focus ring at the top of the map).

**(b) fux/zor use.** The shipped default fux theme (`assets/theme/default.ron` or similar) and the fixture for the theme loader's round-trip test (§5: "config asset hot reload with an invalid edit"). The *derivation* idiom — hover = base.lighter(0.05), pressed = base.lighter(0.1), disabled = base.with_alpha(0.5) — is worth encoding in fux's theme defaults so a user config only needs to override base colors and the rest derive.

**(c) Pitfalls.** The derivation happens *at construction*, so the resolved map is flat and unrelated tokens can drift; a fux config that overrides only `BUTTON_BG` will not re-derive `BUTTON_BG_HOVER` unless fux keeps the derivation in the loader. `ThemeProps.color` is a `HashMap`, hence iteration order is nondeterministic — any serialization for fixture comparison must sort (relevant to §5's fixture round-trips). The module has no light theme; a light variant would need a second constructor plus a config-selected name.

**(d) Excerpt.**
```rust
(tokens::FOCUS_RING, palette::ACCENT.with_alpha(0.5)),
(tokens::TEXT_MAIN, palette::LIGHT_GRAY_1),
(tokens::TEXT_DIM, palette::LIGHT_GRAY_2),
(tokens::BUTTON_BG, palette::GRAY_3),
(tokens::BUTTON_BG_HOVER, palette::GRAY_3.lighter(0.05)),
(tokens::BUTTON_BG_PRESSED, palette::GRAY_3.lighter(0.1)),
(tokens::BUTTON_BG_DISABLED, palette::GRAY_2),
```

### focus.rs
**(a) Mechanism.** Two markers with opposite search directions: `FocusIndicator` matches when the entity *or an ancestor* is focused (`focus.rs:25`), `FocusWithinIndicator` when the entity *or a descendant* is focused (`:32`). `manage_focus_indicators` (`:34-88`) early-returns unless `InputFocus`, `InputFocusVisible` or `UiTheme` changed, then — only when `input_focus_visible.0` is true — walks `iter_descendants(focus).chain(once(focus))` inserting `Outline { color: theme.color(&tokens::FOCUS_RING), width: px(2), offset: px(2) }`, walks `iter_ancestors(focus).chain(once(focus))` for the within-variant, records both in a `HashSet` and finally removes `Outline` from every indicator not visited. Registered in `UiSystems::Content` (`:94-98`).

**(b) fux/zor use.** The focused-pane emphasis in the viewer (§3.2 `FocusedPane`, §3.11): a pane separator/header entity carries `FocusIndicator` so only the focused pane's chrome draws a ring, and the tab root carries `FocusWithinIndicator` so the active workspace/tab is emphasised when any descendant pane has focus. The insert/remove `Outline` pair is also the model for copy-mode's visible cursor block if fux paints chrome with `bevy_ui` nodes.

**(c) Pitfalls.** Depends entirely on the single global `InputFocus`/`InputFocusVisible` resources, which §3.2 explicitly rejects for fux ("`bevy_input_focus::InputFocus` is a single global resource and cannot express per-viewer focus") — fux must re-point this system at `FocusedPane(Entity)` and its own visibility bit, which means `InputFocusVisible`'s "hide the ring after mouse interaction" behaviour becomes per-viewer state. The `HashSet` is sized by `q_indicators.count()` but then filled from both indicator sets, so it may reallocate. `theme.is_changed()` in the guard means a theme reload re-walks every focus entity — fine at editor scale, a per-frame cost if fux ever marks the theme changed spuriously. Requires `bevy_ui::Outline`; confirm `Outline` is present without a renderer (it is a bevy_ui component) before relying on it for the viewer.

**(d) Excerpt.**
```rust
for entity in q_children.iter_descendants(focus).chain(core::iter::once(focus)) {
    if q_indicators.contains(entity) {
        commands.entity(entity).insert(Outline {
            color: theme.color(&tokens::FOCUS_RING),
            width: px(2),
            offset: px(2),
        });
        visited.insert(entity);
    }
}
```

### cursor.rs
**(a) Mechanism.** `DefaultCursor(EntityCursor)` (`cursor.rs:25`) and `OverrideCursor(Option<EntityCursor>)` (`:49`) are resources; `EntityCursor::{Custom(CustomCursor) (feature-gated), System(SystemCursorIcon)}` (`:34-41`) is a component. `update_cursor` (`:86-119`) runs in `PreUpdate` in `PickingSystems::Last`, reads `HoverMap` for `PointerId::Mouse`, finds the first hovered entity (or an ancestor) carrying `EntityCursor`, else falls back to `DefaultCursor`, and writes `CursorIcon` onto every `Window` only when `eq_cursor_icon` says it differs. `eq_cursor_icon` compares against `cursor_icon.as_system()` to stay feature-agnostic (`:60-73`).

**(b) fux/zor use.** **Not applicable to fux.** §3.11's viewer paints terminal cells and spawns an inert `Window` with a scale-factor override; there is no OS cursor to drive, and the constraint list forbids `bevy_winit`. The only transferable idea is the *pattern* of a `HoverMap`-driven derived state resolved by ancestor lookup with an override resource — which is the same lookup shape fux needs for separator-drag affordance (hover a separator → the pane edge is draggable). If fux ever wants a real cursor for the non-terminal chrome it would need a winit backend that §2 excludes.

**(c) Pitfalls.** Depends on `bevy_picking::hover::HoverMap`, whose contents for `PointerId::Mouse` are produced by an input plugin that §2 deliberately does not use (`DefaultPickingPlugins`' input plugin needs a `PrimaryWindow`); fux's own backend writes `PointerHits` for synthetic per-viewer pointer ids, so `PointerId::Mouse` would be empty and this system would always take the `DefaultCursor` branch. `OverrideCursor` gates *all* entity cursors while set, so a forgotten override is a global freeze of the affordance signal. `Without<Window>` on the cursor query (`:104`) prevents a window from being its own cursor source.

**(d) Excerpt.**
```rust
let cursor = r_override_cursor.0.as_ref().unwrap_or_else(|| {
    hover_map
        .and_then(|hover_map| match hover_map.get(&PointerId::Mouse) {
            Some(hover_set) => hover_set.keys().find_map(|entity| {
                cursor_query.get(*entity).ok().or_else(|| {
                    parent_query.iter_ancestors(*entity).find_map(|e| cursor_query.get(e).ok())
                })
            }),
            None => None,
        })
        .unwrap_or(&r_default_cursor)
});
```

### palette.rs
**(a) Mechanism.** Thirteen `pub const Color` values authored in OKLCH with an inline HTML swatch comment naming the hex and the intended role: `TRANSPARENT`, `BLACK`, `GRAY_0` (#1F1F24, window bg), `GRAY_1` (#2A2A2E, pane bg), `GRAY_2` (#36373B, item bg), `GRAY_3` (#46474D, item bg active), `WARM_GRAY_1` (#414142, border), `LIGHT_GRAY_1`/`LIGHT_GRAY_2` (label text bright/dim), `WHITE`, `ACCENT` (#206EC9, CTA/selection), `X_AXIS`/`Y_AXIS`/`Z_AXIS`.

**(b) fux/zor use.** Starting point for ``fux``'s default palette and for the `[style]` config default file. `ACCENT` is the natural selection/copy-mode-selection color, `LIGHT_GRAY_2` the dim/`TEXT_DIM` role for hint text, and `WARM_GRAY_1` the separator/border color for split boundaries.

**(c) Pitfalls.** OKLCH literals mean the values are only meaningful through `bevy_color::Color`'s conversion to the target space; fux's viewer emits *terminal cells*, so these must be converted to the cell color representation once at load, not per frame (the AXIS colors exist for numeric drag handles and are useless in a terminal). `bevy_color` is not in the §2 dependency stack — fux would add it or express the palette directly in `bevy_ui`'s color type / raw RGBA.

**(d) Excerpt.**
```rust
/// <div style="background-color: #2A2A2E; ..."></div> - pane background
pub const GRAY_1: Color = Color::oklcha(0.2866, 0.0072, 285.93, 1.0);
/// <div style="background-color: #206EC9; ..."></div> - call-to-action and selection color
pub const ACCENT: Color = Color::oklcha(0.542, 0.1594, 255.4, 1.0);
```

### font_styles.rs
**(a) Mechanism.** `InheritableFont { font: Handle<Font>, font_size: FontSize, weight: FontWeight }` requires `ThemedText` + `PropagateOver<TextFont>` (`font_styles.rs:21-30`); `on_changed_font` (`:33-43`) reacts to `Insert` and inserts `Propagate(TextFont { font: inheritable_font.font.clone().into(), font_size, weight, ..Default::default() })`, letting the hierarchy propagate the font to descendant text entities.

**(b) fux/zor use.** The analogous fux need is *cell attributes*, not fonts: a `InheritableCellStyle { fg, bg, bold, underline }` requiring a propagation marker, resolved at insert, so that a pane's title bar, the copy-mode hint panel and the status line inherit from the workspace theme without each entity naming every attribute. The structure (immutable-ish component + Insert observer + propagate) transfers; the payload does not.

**(c) Pitfalls.** Entirely `bevy_text`-typed (`Font`, `FontSize`, `FontWeight`, `TextFont`) and depends on `HierarchyPropagatePlugin` being installed in `PostUpdate` (`lib.rs:87-92`) with the matching `PropagateSet` configured into `UiSystems::Propagate` (`lib.rs:94-98`) — a subtle ordering requirement because the propagated value must be visible to the text measure system. fux has no such system, so the propagation set has no consumer and the whole module is n/a beyond the shape.

**(d) Excerpt.**
```rust
pub(crate) fn on_changed_font(
    insert: On<Insert, InheritableFont>,
    font_style: Query<&InheritableFont>,
    mut commands: Commands,
) {
    if let Ok(inheritable_font) = font_style.get(insert.entity) {
        commands.entity(insert.entity).insert(Propagate(TextFont {
            font: inheritable_font.font.clone().into(),
            font_size: inheritable_font.font_size,
            weight: inheritable_font.weight,
            ..Default::default()
        }));
    }
}
```

### constants.rs
**(a) Mechanism.** Non-themable literals in three submodules: `fonts::{REGULAR, ITALIC, BOLD, BOLD_ITALIC, MONO}` as `embedded://bevy_feathers/assets/fonts/...` strings (`constants.rs:4-14`), `icons::{CHEVRON_DOWN, CHEVRON_RIGHT, X}` as `embedded://.../icons/*.png` (`:18-25`), and `size::{ROW_HEIGHT=24px, CHECKBOX_SIZE=18, HEADER_HEIGHT=30, RADIO_SIZE=18, TOGGLE_WIDTH=32, TOGGLE_HEIGHT=18, MEDIUM_FONT=14, COMPACT_FONT=13, SMALL_FONT=12, EXTRA_SMALL_FONT=11}` (`:28-58`).

**(b) fux/zor use.** The `size::` block is the transferable half and answers a real fux question: what is the chrome height budget. `HEADER_HEIGHT = 30px` is the pane-header height and `ROW_HEIGHT = 24px` the list/button row height in the *viewer*, which must be reconciled with §3.4's cell grid — fux's viewer is cell-based, so these should become cell counts, not pixels, and the separation "theme has colors, constants has geometry" is the right split for fux's `[style]`/`[layout]` config boundary.

**(c) Pitfalls.** All `fonts`/`icons` constants are `embedded://` asset paths that assume `embedded_asset!` registration in `FeathersCorePlugin` (`lib.rs:70-85`) plus `AssetPlugin`; §2's `AssetPlugin` points at the config directory, so `embedded://` paths are a different root and fux should not copy this scheme. `size::` uses `bevy_text::FontSize` and `bevy_ui::Val::Px`; a cell-based fux wants `Val::Px` at most for chrome and cell counts for terminal content.

**(d) Excerpt.**
```rust
/// Common row size for buttons, sliders, spinners, etc.
pub const ROW_HEIGHT: Val = Val::Px(24.0);

/// Height for pane headers
pub const HEADER_HEIGHT: Val = Val::Px(30.0);
```

### rounded_corners.rs
**(a) Mechanism.** A plain (non-reflect, non-component) `RoundedCorners` enum with ten variants — `None, All, TopLeft, TopRight, BottomRight, BottomLeft, Top, Right, Bottom, Left` (`rounded_corners.rs:14-34`) — and one method `to_border_radius(&self, radius: f32) -> BorderRadius` (`:40-105`) that maps the variant to a `bevy_ui::BorderRadius` literal, using `Val::ZERO` for sharp corners.

**(b) fux/zor use.** This is a *styling helper for segmented chrome*, and fux's equivalent is the split-tree edge decoration: a pane header between two panes, or the outer corners of a tab's chrome, can be selected by which edges adjoin a separator. The value is the enum-to-struct mapping idiom (one variant → one structural literal) and the fact that a shared radius parameter keeps a segmented row visually uniform.

**(c) Pitfalls.** `rounded_corners` is private to `bevy_feathers` and the type is not `Reflect`/`Component`, so it is used by construction inside `containers`/`controls` only; a fux copy must make its own choice about reflectability if it wants the value in a BSN template or a persisted layout. In a cell-grid viewer this is moot for terminal content but still relevant for chrome if fux paints headers with `bevy_ui` nodes.

**(d) Excerpt.**
```rust
pub fn to_border_radius(&self, radius: f32) -> BorderRadius {
    let radius = px(radius);
    let zero = Val::ZERO;
    match self {
        RoundedCorners::None => BorderRadius::all(zero),
        RoundedCorners::All => BorderRadius::all(radius),
        RoundedCorners::Top => BorderRadius {
            top_left: radius, top_right: radius, bottom_right: zero, bottom_left: zero,
        },
        ...
    }
}
```

### alpha_pattern.rs
**(a) Mechanism.** Demonstrates the UiMaterial pattern: `AlphaPatternMaterial` derives `AsBindGroup, Asset, TypePath` and implements `UiMaterial::fragment_shader` returning `embedded://bevy_feathers/assets/shaders/alpha_pattern.wgsl` (`alpha_pattern.rs:20-23` — the struct is `pub(crate)`); `AlphaPatternResource(Handle<AlphaPatternMaterial>)` is built in `FromWorld` from `Assets<AlphaPatternMaterial>` (`:29-38`); `on_add_alpha_pattern` (`:45-53`) fills the `MaterialNode<AlphaPatternMaterial>` handle on `Add`, because templates cannot carry the asset handle at spawn time.

**(b) fux/zor use.** **n/a for fux** — it is the transparent-color-checkerboard material for a color picker, needs `bevy_render`/`bevy_ui_render`/wgsl, all of which §2 excludes. The single transferable idea is the `FromWorld`-created resource holding a handle, resolved by an `On<Add, Marker>` observer so a template only names the marker — the same deferral fux uses for `MenuFocusState::Opening` and for BSN templates that must attach observers or handles after spawn.

**(c) Pitfalls.** `pub(crate)` struct, `UiMaterialPlugin::<AlphaPatternMaterial>` registered in `FeathersCorePlugin` (`lib.rs:88`) and `AlphaPatternPlugin` is *not* part of `FeathersCorePlugin`'s plugin tuple — only `alpha_pattern::AlphaPatternMaterial` and `AlphaPatternResource` are imported there (`lib.rs:33-34`, `:112`), so the observer is registered only if the caller adds `AlphaPatternPlugin`. `FromWorld` calls `.unwrap()` on `get_resource_mut::<Assets<..>>()` (`:31-35`), which panics if the asset is not registered — a hard requirement on plugin ordering.

**(d) Excerpt.**
```rust
fn on_add_alpha_pattern(
    add: On<Add, AlphaPattern>,
    mut q_material_node: Query<&mut MaterialNode<AlphaPatternMaterial>>,
    r_material: Res<AlphaPatternResource>,
) {
    if let Ok(mut material) = q_material_node.get_mut(add.entity) {
        material.0 = r_material.0.clone();
    }
}
```

## bevy_ui_widgets and bevy_feathers: consequences for the prompt

1. **Dependency verdict.** Neither crate belongs in §2's stack. `bevy_ui_widgets` has no `bevy_render` edge and its dependencies (`bevy_input`, `bevy_input_focus`, `bevy_picking`, `bevy_ui`, `bevy_camera`, `bevy_a11y`) are already in the fux graph, so *vendoring the ten non-text modules* under `crates/fux/src/viewer/widgets/` is viable; `bevy_feathers` is not (bevy_render + bevy_ui_render + bevy_shader + embedded shader/font assets), so only its patterns are portable.
2. **The one blocker inside the widgets crate is `text_input.rs`.** It is the only module that imports `bevy_text` (`EditableText`, `TextEdit`, `PreeditCursor`), `bevy_ui::widget` text-layout systems and `bevy_window::{Ime, PrimaryWindow}` (`text_input.rs:16-29`), and §2 forbids `bevy_text` types. Its *architecture* (keymap → queued edit enum → one PostUpdate applier; IME lifecycle; click-count selection semantics; `propagate` for Tab) transfers; its code does not.
3. **Focus is the systemic mismatch.** Every keyboard observer in the crate keys off the global `bevy_input_focus::InputFocus`, and `feathers::focus` resolves outlines against the same resource; §3.2 requires per-viewer `FocusedPane(Entity)`. Expect the keyboard halves of `button`, `checkbox`, `radio`, `list`, `menu`, `slider` and all of `focus.rs` to be re-pointed at a per-viewer focus component; the pointer halves (`Pointer<Press|Release|Click|Drag|DragEnd|Cancel>`) transfer unchanged.
4. **`InteractionDisabled` ≠ fux `Disabled`.** §3.2 forbids `Disabled` on any UI-tree entity; `bevy_ui::InteractionDisabled` (`interaction_states.rs:23`) is a query-filter component that exists precisely for tree entities and must be the fux mapping for the widget layer.
5. **Geometry is always derived.** `ComputedNode`, `UiGlobalTransform`, `ComputedUiRenderTargetInfo` and `UiScale` are the only geometry inputs in `slider.rs`, `scrollbar.rs`, `popover.rs`, `scrollarea.rs` — consistent with §3.5, and all of it must be re-verified against the `RenderTarget::None` camera path from `crates/bevy_ui/src/update.rs:115-172`, which no file in this assignment exercises (no test in either crate constructs a target-less layout).
6. **Theme shape for §3.7.** `ThemeToken(SmolStr)` → flat `HashMap` → `UiTheme` resource plus per-entity immutable token components, resolved by (bulk system on change) + (Insert observer) (`theme.rs:129-200`), is exactly reproducible in fux with `bevy_asset` as the source of truth; the only changes needed are `AssetChanged<Theme>` in place of `Res::is_changed()`, a fux token namespace, and dropping the `bevy_text::TextColor` half.

Checkout `../many_rigs/inspirations/bevy` @ `b56fc29d`, crate version 0.19.1.
Crate-level facts: `crates/bevy_picking/Cargo.toml:11-34` — the only feature is `mesh_picking` (fux leaves it off, so `mesh_picking.rs` is not compiled); the crate has **no** `bevy_render`/`bevy_winit` edge, its camera dependency is `bevy_camera` (for `NormalizedRenderTarget`, `Camera`, `RenderTarget`), plus `bevy_window`, `bevy_transform` (ray module only), `bevy_time` (the time-set anchor), `bevy_input`, `bevy_platform`, and an unconditional `uuid` dep. `bevy_camera` arrives in fux anyway via `bevy_ui`, so this adds no new graph node.

## bevy_picking

### lib.rs

**Mechanism.** `lib.rs` is the crate's wiring, not its logic. `Pickable` (`lib.rs:197-255`) is the only opt-in component: `should_block_lower` (default `true`) stops the hover walk at this entity, `is_hoverable` (default `true`) decides whether the entity itself enters the hover map, and `Pickable::IGNORE` is the zero-cost way to make a backend-reported entity invisible to the pipeline. `PickingSystems` (`lib.rs:257-282`) names the five PreUpdate stages (`ProcessInput`, `Backend`, `Hover`, `PostHover`, `Last`) plus the two First stages (`Input`, `PostInput`). `PickingSettings` (`lib.rs:316-361`) exposes four booleans and `multi_click_interval` as `run_if` predicates. `PickingPlugin::build` (`lib.rs:368-413`) registers the message types, `PointerMap`, `RayMap`, the input-fold systems, the window backend, and the two `configure_sets` chains; `InteractionPlugin::build` (`lib.rs:417-447`) registers `HoverMap`/`PreviousHoverMap`/`PointerState`, all 17 `Pointer<E>` message types, and the four chained Hover systems.

**fux/zor use (3.4).** Register exactly `PickingPlugin` + `InteractionPlugin` (never `DefaultPickingPlugins`, `lib.rs:284-293`, which also pulls `input::PointerInputPlugin`). The fux cell backend and the viewer adapter are the app's own plugins:
- viewer adapter system: `First`, `in_set(PickingSystems::Input)` — writes `PointerInput` derived from attachment `Mouse` frames (3.10/3.11).
- cell backend system: `PreUpdate`, `in_set(PickingSystems::Backend)` — writes `PointerHits` with `HitData.camera` = the viewer's camera entity (3.4).
- mouse policy: `On<Pointer<Press|Release|Drag|DragEnd|Click>>` observers on pane/separator entities; no system ordering needed for the observers themselves (see `events.rs` sync point below). Fux systems that must read post-hover state rather than observe events go in `PickingSystems::Last`.
- `PickingSettings::is_enabled` is the kill switch for the `ShuttingDown` state (3.1); do **not** touch `is_input_enabled`.

**Pitfalls.**
- `PointerInput::receive` and therefore the whole pipeline is gated by `PickingSettings::input_should_run` (`is_input_enabled && is_enabled`, `lib.rs:331-333`); flipping `is_input_enabled` off silently discards every hand-written `PointerInput` from the attachment stream — the backend would then see stale/absent `PointerLocation`.
- `PickingPlugin` unconditionally registers `window::update_window_hits` behind `window_picking_should_run` (`lib.rs:390-392`). fux has no window entities in the server World, so it emits nothing, but fux should set `is_window_picking_enabled: false` to make that explicit and avoid a per-pointer branch each frame.
- `PickingPlugin` calls `allow_ambiguous_resource::<Messages<PointerHits>>` (`lib.rs:374-377`), so multiple backends never need ordering against each other; only fux-vs-fux ordering matters, and fux has one backend.
- The First chain is anchored `.after(TimeSystems).after(MessageUpdateSystems)` (`lib.rs:394-400`) — writes to `PointerInput` from `PickingSystems::Input` survive the frame's message maintenance and are seen in the same update by PreUpdate.
- Both plugins `init_resource::<PickingSettings>()`; that is idempotent but means a settings resource inserted *after* plugin build wins only if it exists at build time — insert before `add_plugins`.

**Excerpt** (`lib.rs:401-410`) — the PreUpdate ordering fux hooks into:

```rust
.configure_sets(
    PreUpdate,
    (
        PickingSystems::ProcessInput.run_if(PickingSettings::input_should_run),
        PickingSystems::Backend,
        PickingSystems::Hover.run_if(PickingSettings::hover_should_run),
        PickingSystems::PostHover,
        PickingSystems::Last,
    )
    .chain(),
)
```

### backend.rs

**Mechanism.** The backend contract is one sentence of prose and two types. A backend reads `PointerLocation` components and writes `PointerHits` messages; the `picks` vector is explicitly **unordered** and need not be filtered by `Pickable` (`backend.rs:16-27`). `PointerHits` (`backend.rs:93-126`) carries `pointer`, `picks: Vec<(Entity, HitData)>`, and `order: f32` — a *layer* key, not a depth key: hits are grouped by pointer, sorted by `order` descending, and only inside one `order` layer are they sorted by `HitData::depth`. The `f32` type exists so `bevy_ui` can sit half a layer above a 3D camera (`backend.rs:102-116`). `HitData` (`backend.rs:135-156`, impl `179-227`) is `{ camera: Entity, depth: f32, position: Option<Vec3>, normal: Option<Vec3>, extra: Option<Arc<dyn HitDataExtra>> }`; `HitDataExtra` is auto-implemented for every `Send + Sync + Debug + 'static` type (`backend.rs:78-80`) and read back with `extra_as::<T>()`. `ray::RayMap` (`backend.rs:231-320`) is the raycasting helper fux does not need.

**fux/zor use (3.4).** This is the API the fux cell backend implements:
```
fn update_cell_hits(
    pointers: Query<(&PointerId, &PointerLocation)>,
    viewers: Query<(&Camera, &RenderTarget /*::None{size}*/, &Viewport)>,   // fux,
    ui: Query<(&ComputedNode, &UiGlobalTransform, &ChildOf, ...)>,          // bevy_ui geometry
    mut hits: MessageWriter<PointerHits>,
)
```
`order` = the viewer camera's order (one pointer per viewer, so one layer per pointer normally; a viewer's panes must all be in the *same* `PointerHits` with distinct `depth`, because ordering between separate `PointerHits` for the same pointer is by layer only). `depth` = the pane's stack index / a monotonically decreasing value down the tree, self-consistent within that one message (`backend.rs:139-142`). `position` = the cell coordinate as `Vec3::new(col, row, 0.0)` so separator-drag observers can compute deltas. `normal` = `None` (window.rs makes the same choice). `extra` is the place for a fux-specific payload such as `CellHit { pane, col, row }` if observers need more than `HitData`; it must be `Send + Sync + Debug + 'static` and is excluded from `PartialEq` and reflection.

**Pitfalls.**
- `HitData::extra` is `#[reflect(ignore)]` (`backend.rs:153`) and `PartialEq` ignores it (`backend.rs:170-177`), so it can never reach BRP projections or the persisted snapshot (3.8/3.9) — keep authoritative data in components, not in `extra`.
- `RayMap::repopulate` (`backend.rs:299-320`) is registered by `PickingPlugin` and runs every frame in `ProcessInput`; it needs `(Camera, RenderTarget, GlobalTransform)` on the camera entity and fills nothing without `GlobalTransform`. fux viewer cameras deliberately have `Camera + RenderTarget::None` and no `GlobalTransform` (3.2), so the map stays empty — correct, but it also means fux must not try to reuse `Location::is_in_viewport`, which requires a `PrimaryWindow` (`pointer.rs:219-247`).
- Reading `PointerHits` from a fux system in PreUpdate introduces an ordering ambiguity the plugin does not report (`backend.rs:87-91`); put readers in `PickingSystems::Backend` (after the producers) or don't read them at all.
- `PointerHits` is a `Message`, added by `PickingPlugin` (`lib.rs:373`); a backend added without `PickingPlugin` panics on the missing writer.

**Excerpt** (`backend.rs:135-156`, field comments elided):

```rust
pub struct HitData {
    /// The camera entity used to detect this hit.
    pub camera: Entity,
    /// `depth` only needs to be self-consistent with other [`PointerHits`]s using the same
    /// [`RenderTarget`].
    pub depth: f32,
    pub position: Option<Vec3>,
    pub normal: Option<Vec3>,
    #[reflect(ignore)]
    pub extra: Option<Arc<dyn HitDataExtra>>,
}
```

### pointer.rs

**Mechanism.** `PointerId` (`pointer.rs:31-44`) is the stable identity of a virtual cursor and a `Component` that `#[require]`s `PointerLocation, PointerPress, PointerInteraction`; variants are `Mouse`, `Touch(u64)`, and `Custom(Uuid)` (the latter `#[reflect(ignore, clone)]`). `PointerLocation { location: Option<Location> }` (`pointer.rs:179-201`) is the current position; `Location { target: NormalizedRenderTarget, position: Vec2 }` (`pointer.rs:212-217`). `PointerMap` (`pointer.rs:93-103`) is a `PointerId -> Entity` resource rebuilt every frame by `update_pointer_map` (`pointer.rs:106-111`). `PointerInput { pointer_id, location, action }` (`pointer.rs:279-301`) is the message an input adapter writes; `PointerAction` (`pointer.rs:249-278`) is `Press | Release | Move{delta} | Scroll{unit,x,y,phase} | Cancel`. `PointerInput::receive` (`pointer.rs:322-365`) folds those messages into `PointerPress` and `PointerLocation`.

**fux/zor use (3.4 + 3.10/3.11).** One `PointerId::Custom(Uuid)` per viewer attachment. The viewer adapter spawns the pointer entity once per attachment — stable entity, stable `Uuid` — with `PointerLocation` seeded from the first `Mouse` frame, and then writes `PointerInput` per frame:
- mouse move inside the viewer grid → `PointerAction::Move { delta: cell - last_cell }` with `Location { target: NormalizedRenderTarget::None { width, height }, position: cell }`;
- click → `Press(Primary)` / `Release(Primary)` (Primary = left, Secondary = right for the right-click policy, Middle for paste);
- wheel → `Scroll { unit: Line, x, y, phase: TouchPhase::Moved }` for wheel-to-history;
- detach/`Ctrl-C`-style loss → `Cancel`, which the hover code treats specially (`hover.rs:152-162`).
The target must be built as `RenderTarget::None { size: UVec2::new(cols, rows) }.normalize(None)` → `NormalizedRenderTarget::None { width, height }` (`crates/bevy_camera/src/camera.rs:929-962`). **The prompt's `NormalizedRenderTarget::None { size }` is shorthand**: the real fields are `width: u32, height: u32` and they are *physical* pixels; with the viewer's `scale_factor_override(1.0)` (3.11) physical == cells, so no conversion is needed. `position` is likewise in cells.

**Pitfalls.**
- `receive` writes `PointerLocation` **only** for `Move` (`pointer.rs:350-357`); `Press`/`Release` update `PointerPress` only and `Scroll`/`Cancel` do nothing at all. Consequence: the position the backend and hover see is always the last `Move`'s. The adapter must emit a `Move` (even `delta: Vec2::ZERO`) whenever the cursor position changes, and in particular on the same frame as a `Press` at a new cell, or the press will be hover-tested at the previous cell.
- `PointerId::Custom(Uuid)` is `#[reflect(ignore, clone)]` (pointer.rs:42) — pointer entities can never appear in a reflected projection or a BRP-visible query.
- `PointerPress` fields are private (`pointer.rs:115-120`); reads go through `is_primary_pressed` etc. `PressDirection` (`pointer.rs:150-156`) is defined but not consumed by any system in the files read here.
- `update_pointer_map` **clears and rebuilds** every frame; if the viewer adapter despawns/re-spawns the pointer entity on reattach, `pointer_events`' `pointer_location(pointer_id)` lookup (events.rs:684-688) fails for one frame and `Over`/`Out` are skipped with only a `debug!` log.
- `PointerMap` is keyed by `PointerId`, so two viewers must never share a `Uuid`; the attachment `instance`/`token` is the natural seed, and reusing a `Uuid` across viewer reattach would silently retarget the old pointer.
- `PointerLocation.location` is `#[reflect(ignore)]` (pointer.rs:183): pointer state is deliberately outside reflection, consistent with fux's rule that only projection components are serialized.

**Excerpt** (`pointer.rs:31-44`):

```rust
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, Hash, Component, Reflect)]
#[require(PointerLocation, PointerPress, PointerInteraction)]
#[reflect(Component, Default, Debug, Hash, PartialEq, Clone)]
pub enum PointerId {
    /// The mouse pointer.
    #[default]
    Mouse,
    /// A touch input, usually numbered by window touch events from `winit`.
    Touch(u64),
    /// A custom, uniquely identified pointer. Useful for mocking inputs or implementing a software
    /// controlled cursor.
    #[reflect(ignore, clone)]
    Custom(Uuid),
}
```

### events.rs

**Mechanism.** Every pointer event is a `Pointer<E>` (`events.rs:74-92`): an `EntityEvent` with `#[entity_event(propagate = PointerTraversal, auto_propagate)]`, carrying `entity`, `pointer_id`, `pointer_location`, `event: E`, and a `pub(crate) propagate: bool`. `PointerTraversal` (`events.rs:94-122`) bubbles to `ChildOf::parent()`, then to the pointer's window entity if the target was a window, then stops. The 17 payloads (`events.rs:180-472`) split into hover (`Over`/`Enter`/`Move`/`Leave`/`Out`), button (`Press`/`Release`/`Click`), drag (`DragStart`/`Drag`/`DragEnd`/`DragEnter`/`DragOver`/`DragLeave`/`DragDrop`), plus `Scroll` and `Cancel`. All state lives in `PointerState` (`events.rs:550-590`) → `PointerButtonState` (`events.rs:475-493`: `pressing`, `clicking`, `dragging`, `dragging_over`) and an ancestor cache `HoveredEntityAncestors` (`events.rs:497-547`). `pointer_events` (`events.rs:667-1477`) is the single dispatcher: it diffs `PreviousHoverMap` vs `HoverMap` for `Out`/`Leave`/`Enter`/`Over` (plus `DragLeave`/`DragEnter`), then replays the frame's `PointerInput` actions for `Press`/`Click`/`Release`/`Drag*`/`Move`/`Scroll`/`Cancel`. Each event is dispatched twice by design: `commands.trigger(...)` for observers and `MessageWriter` for `MessageReader`s, via the `PickingMessageWriters` SystemParam (`events.rs:596-616`).

**fux/zor use (3.4).** Observers on `Pointer<Press|Release|Drag|DragEnd|Click>` on panes and separators are "the only place mouse policy lives" (prompt 3.3/3.4), and the bubbling rule is what makes tab-level behaviour cheap:
- `On<Pointer<Press>>` on a pane (or bubbled to the tab root) → set that viewer's `FocusedPane` (3.2) and write `Latched`/`Showing` as needed; the same event bubbling to the viewer entity is where the viewer's `InputFocus` mirror is set in the viewer App (3.11, see bevy_input_focus below).
- `On<Pointer<Drag>>` on a separator (1-cell `Node`) → recompute `flex_grow` on the two children of that split; `Drag.delta`/`distance` are in cells (screen px with scale 1.0).
- `On<Pointer<DragStart|DragEnd>>` on a pane → pane drag-to-move / drop-onto-target.
- `On<Pointer<Scroll>>` on a pane → wheel-to-history; `On<Pointer<Press>>` with `button: Secondary` → `RightClickPolicy`.
- `On<Pointer<Click>>` with bubbling and `count` gives double-click for free if fux ever needs it.

**Pitfalls.**
- The documented intra-frame order is a contract (`events.rs:619-665`); the observers fire at the sync point between `pointer_events` and `update_interactions` *inside* `PickingSystems::Hover` (`events.rs:26-28`), so an observer cannot rely on `PickingInteraction` already being updated for the current frame.
- `Click` and `Release` target the entity hovered in the **previous** frame (`events.rs:658-665`), and `Release` is sent to *every* previously-hovered entity, with `Click` only for entities actually pressed. fux's pane-close-on-click logic must therefore not assume the click target equals the current hover target.
- Only the pointer's **final** position in the frame determines hover (`events.rs:662-665`); a viewer frame batching several `Mouse` messages will have its intermediate positions effectively ignored.
- `Pointer<E>::propagate` is `pub(crate)`: a consumer can only stop bubbling from inside an observer (`event.propagate(false)`); a `MessageReader` cannot. A custom event type is possible but requires `E: Debug + Clone + Reflect`.
- `Enter`/`Leave` are computed manually and sent with `new_without_propagate` to the entity *and* its ancestors (`events.rs:120-121`, `922-940`, `1030-1048`); `Over`/`Out` always propagate. A pane-level observer can therefore see a `Press` bubbled from a child separator and a separate `Over` — check `event.entity` vs the observer's entity.
- `DragStart` is only emitted on a `Move` while pressed (`events.rs:1069-1090`); for the cell backend this means the viewer adapter must emit a `Move` with a non-zero delta before a `Drag` can ever fire.
- `pointer_events` reads `Instant::now()` directly (`events.rs:690`), not `bevy_time` — click timing is invisible to fux's clock-driven runner (3.1) and is not deterministic under batched updates; only `multi_click_interval` uses it, but a test asserting `Click.count` must account for it.

**Excerpt** (`events.rs:104-122`, the bubbling rule):

```rust
fn traverse(item: Self::Item<'_, '_>, pointer: &Pointer<E>) -> Option<Entity> {
    if !pointer.propagate {
        return None;
    }

    let PointerTraversalItem { child_of, window } = item;

    // Send event to parent, if it has one.
    if let Some(child_of) = child_of {
        return Some(child_of.parent());
    };

    // Otherwise, send it to the window entity (unless this is a window entity).
    if window.is_none()
        && let NormalizedRenderTarget::Window(window_ref) = pointer.pointer_location.target
    {
        return Some(window_ref.entity());
    }

    None
}
```

Note for fux: the second arm keys off `NormalizedRenderTarget::Window`, so with `None` targets the traversal stops at the top of the tab subtree (no window hop). That is the desired behaviour — a fux pane subtree terminates at the tab root — but it also means the viewer App's window entity will never receive a bubbled `Pointer<..>` event; viewer-level pointer handling must live on the tab-root entity (or in the App that owns the window, which is the viewer, not the server).

### hover.rs

**Mechanism.** `generate_hovermap` (`hover.rs:96-120`) is the whole pipeline's decision point: `reset_maps` (`hover.rs:123-145`) swaps `HoverMap` with `PreviousHoverMap` and clears in place (no allocation when keys are stable); `build_over_map` (`hover.rs:148-184`) collects `PointerHits` into `OverMap = HashMap<PointerId, BTreeMap<FloatOrd, Vec<(Entity, HitData)>>>`, dropping hits for pointers that saw `PointerAction::Cancel`, and sorts each layer by depth; `build_hover_map` (`hover.rs:187-222`) walks layers from highest to lowest and inserts entities into `HoverMap` until a `Pickable` with `should_block_lower` stops the walk — entities without `Pickable` block by default. `update_interactions` (`hover.rs:237-288`) copies the sorted hover list into each pointer's `PointerInteraction` and aggregates per-entity `PickingInteraction { Pressed=2, Hovered=1, None=0 }` with a precedence merge. `Hovered`/`DirectlyHovered` (`hover.rs:339-357`) are memoized `#[component(immutable)]` mirrors maintained by `update_is_hovered`/`update_is_directly_hovered` (`hover.rs:367-435`).

**fux/zor use (3.4).**
- `HoverMap`/`PreviousHoverMap` are plain resources a fux projection system can read to render viewer-visible hover state (separator highlight, pane hover border) without inventing its own hit test; `PreviousHoverMap` is the correct source for "what changed since the last projected frame".
- `PickingInteraction` is the component to expose in `PaneView`/`SeparatorView` (3.9 read-only projections) rather than shipping `HoverMap` itself.
- `Pickable::IGNORE` is what a fux overlay/zoom helper entity gets so it cannot swallow pane presses.
- Do **not** attach `Hovered`/`DirectlyHovered` to fux layout entities (see pitfalls).

**Pitfalls.**
- `update_is_hovered` and `update_is_directly_hovered` hardcode `PointerId::Mouse` (`hover.rs:385`, `hover.rs:420`): with fux's `PointerId::Custom` per viewer, those two components are always `false`. Any fux hover state must be derived from `HoverMap` directly.
- The number of hovered entities per pointer is normally one: the walk `break`s on the first blocking entity (`hover.rs:213-217`). fux gets one hovered pane/separator per viewer, which is what the separator-drag logic wants; a zoom overlay must therefore be explicit (`should_block_lower = false` to see through it, or its own entity above).
- `update_interactions` inserts `PickingInteraction` with `try_insert` via `Commands` and clears to `None` last "to preserve change detection" (`hover.rs:262-287`); a fux system reading `Changed<PickingInteraction>` in a later set sees it one command-flush later, not in the same schedule position.
- `reset_maps` retains only pointers present in the `Query<&PointerId>` (`hover.rs:137-143`); despawning a viewer's pointer entity therefore drops its hover state silently.
- `build_hover_map` iterates **every** `PointerId` entity, so fux's per-viewer pointers all get map entries — memory and cost scale with viewer count, not with a single cursor.

**Excerpt** (`hover.rs:205-218`, the blocking rule):

```rust
// Note we reverse here to start from the highest layer first.
for (entity, pick_data) in layer_map.values().rev().flatten() {
    if let Ok(pickable) = pickable.get(*entity) {
        if pickable.is_hoverable {
            pointer_entity_set.insert(*entity, pick_data.clone());
        }
        if pickable.should_block_lower {
            break;
        }
    } else {
        pointer_entity_set.insert(*entity, pick_data.clone()); // Emit events by default
        break; // Entities block by default so we break out of the loop
    }
}
```

### input.rs

**Mechanism.** `PointerInputPlugin` (`input.rs:93-113`) is the reference *input adapter*: `spawn_mouse_pointer` creates the `PointerId::Mouse` entity at Startup; `mouse_pick_events` (`input.rs:121-202`) and `touch_pick_events` (`input.rs:204-278`) translate `bevy_window::WindowEvent`s into `PointerInput` messages in `First` / `PickingSystems::Input`; `deactivate_touch_pointers` despawns ended touches in `Last`. `PointerInputSettings` (`input.rs:58-83`) gates mouse and touch independently via `run_if`.

**fux/zor use (3.4/3.11).** fux does **not** add this plugin. It is the structural template for the viewer adapter: same schedule position (`First`, `PickingSystems::Input`, so the writes land after `MessageUpdateSystems` and before PreUpdate `ProcessInput`), same message shape, and the same modifier→`PointerButton` mapping table (`input.rs:137-147`). The `WindowEvent` reader becomes the attachment `Mouse { cell, button, action }` reader; the touch machinery is dropped (`is_touch_enabled: false` equivalent) because the viewer has one cursor.

**Pitfalls.**
- The plugin's systems depend on `PrimaryWindow`; without one, `RenderTarget::Window(..).normalize(primary_window.single().ok())` yields `None` and the loop `continue`s (`input.rs:129-136`) — input is dropped silently, not rejected.
- Enabling it in fux would spawn an extra `PointerId::Mouse` entity that no backend can ever hit (fux pointers target `NormalizedRenderTarget::None`), adding a permanent empty entry to `HoverMap` and `PointerMap` each frame.
- The touch path spawns/despawns pointer **entities** from systems (`input.rs:222-232`, `input.rs:280-300`); fux's per-viewer pointer must instead be tied to attachment lifetime, which is entity lifecycle owned by the server, not by an input system.

**Excerpt** (`input.rs:137-147`, the mapping to reuse):

```rust
let button = match input.button {
    MouseButton::Left => PointerButton::Primary,
    MouseButton::Right => PointerButton::Secondary,
    MouseButton::Middle => PointerButton::Middle,
    MouseButton::Other(_) | MouseButton::Back | MouseButton::Forward => continue,
};
let action = match input.state {
    ButtonState::Pressed => PointerAction::Press(button),
    ButtonState::Released => PointerAction::Release(button),
};
```

### window.rs

**Mechanism.** The entire file is 46 lines and one system. `update_window_hits` (`window.rs:30-46`) iterates `Query<(&PointerId, &PointerLocation)>`; for every pointer whose location target is a window it writes a single-pick `PointerHits` for that window entity, with `depth 0.0`, `position = Some(position.extend(0.0))`, `normal = None`, and `order = f32::NEG_INFINITY` so the window sits behind every other backend's hits. It is registered by `PickingPlugin` in `PickingSystems::Backend` (`lib.rs:390-392`) and provides no `normal` (`window.rs:19-20`).

**fux/zor use (3.4).** This is the template for the fux cell backend, and the reason no other backend code needs porting:
1. same signature shape — `Query<(&PointerId, &PointerLocation)>` in, `MessageWriter<PointerHits>` out, registered by fux's own `PickingPlugin`-sibling plugin into `PickingSystems::Backend`;
2. the match arm changes from `NormalizedRenderTarget::Window(window_ref)` to `NormalizedRenderTarget::None { width, height }`, and `window_ref.entity()` is replaced by "resolve `(viewer, camera, tab)` from the viewport size", i.e. the viewer whose `Viewport { rows, cols }` equals `(height, width)`;
3. the hit list is no longer one entity: it is the panes and separators under that cell, obtained from `bevy_ui`'s `ComputedNode`/`UiGlobalTransform`/stack order (geometry reuse listed in prompt 3.4), with `order` = camera order and `depth` from the stack.
fux never reuses `update_window_hits` itself: it is registered unconditionally by `PickingPlugin`, and its semantics (window entity as the bottom-most hit) do not apply to a `None` target. Setting `is_window_picking_enabled: false` disables it explicitly.

**Pitfalls.**
- Trusting the location's target as the hit entity is only valid for the window backend; for fux the analogous trust is a `viewport → viewer` lookup that must fail closed (no hits) when no viewer matches, exactly as `if let Some(Location { .. }) = pointer_location.location` fails closed here.
- `f32::NEG_INFINITY` as `order` is a valid layer key because `build_over_map` uses `FloatOrd` in a `BTreeMap` (`hover.rs:158-166`); a fux backend that wants an underlay (e.g. tab root as a catch-all drop target) can copy the trick, but an underlay still needs `Pickable { should_block_lower: false, .. }` to let panes above it be hovered.
- The doc comment explicitly notes the missing `normal` (`window.rs:19-20`) — fux's cell backend should document the same, and rely on `position` alone for drag deltas.

**Excerpt** (`window.rs:33-45`, reflowed):

```rust
for (pointer_id, pointer_location) in pointers.iter() {
    if let Some(Location { target: NormalizedRenderTarget::Window(window_ref), position, .. }) =
        pointer_location.location
    {
        let entity = window_ref.entity();
        let hit_data = HitData::new(entity, 0.0, Some(position.extend(0.0)), None);
        pointer_hits_writer.write(PointerHits::new(
            *pointer_id,
            vec![(entity, hit_data)],
            f32::NEG_INFINITY,
        ));
    }
}
```

---

Crate-level facts (`crates/bevy_input_focus/Cargo.toml`): `#![no_std]`, deps `bevy_app`, `bevy_ecs`, `bevy_input`, `bevy_math`, `bevy_window`, optional `bevy_picking`, `thiserror`, `log`. Feature table: `default = ["std", "bevy_reflect", "bevy_ecs/async_executor", "gamepad", "keyboard", "mouse"]`; `bevy_picking` is an implicit optional-dep feature. The prompt's dependency line for fux is `default-features = false, features = ["std", "bevy_reflect", "keyboard"]`, which means: **only `KeyboardInput` is dispatched**, and `click_to_focus` (`tab_navigation.rs:397-427`, `#[cfg(feature = "bevy_picking")]`) is not compiled. fux must supply its own click-to-focus observer. `bevy_ecs/async_executor` is also off; nothing in this crate needs it.

## bevy_input_focus

### lib.rs

**Mechanism.** `InputFocus` (`lib.rs:103-114`) is a `Resource` with three **private** fields: `current_focus: Option<Entity>`, `recorded_changes: Vec<Option<(Entity, FocusCause)>>` (FIFO), and `original_focus: Option<Entity>`. The public API is `from_entity` (tests/shortcuts only — it clears buffered changes), `set(entity, cause)`, `get()`, `clear()` (`lib.rs:116-165`). `InputFocusVisible(pub bool)` (`lib.rs:176`) is a separate resource owned by whoever drives focus navigation. `FocusedInput<M: Message + Clone>` (`lib.rs:192-205`) is the bubble-able event: `#[entity_event(propagate = WindowTraversal, auto_propagate)]`, with `#[event_target] focused_entity`, `pub input: M`, and a **private** `window: Entity`. `AcquireFocus` (`lib.rs:211-219`) bubbles the same way to "the nearest focusable ancestor". `WindowTraversal` (`lib.rs:221-260`) bubbles `ChildOf` → window entity → stop, for both event types. `InputFocusPlugin` (`lib.rs:268-280`) does `PostStartup::set_initial_focus` + `PostUpdate::process_recorded_focus_changes` in `InputFocusSystems::FocusChangeEvents`. `InputDispatchPlugin` (`lib.rs:287-312`) adds one `dispatch_focused_input::<M>` per enabled input type in `PreUpdate` / `InputFocusSystems::Dispatch`, `.after(bevy_input::InputSystems)`. `dispatch_focused_input` (`lib.rs:341-386`) triggers one `FocusedInput` per message on the focused entity (or the window), and clears focus if the focused entity was despawned. `IsFocused`/`IsFocusedHelper` (`lib.rs:395-490`) are the read side.

**fux/zor use (3.11).** The viewer App adds `InputFocusPlugin` + `InputDispatchPlugin` + `TabNavigationPlugin` (+ `DirectionalNavigationPlugin` if h/j/k/l uses the graph API). The viewer spawns its `Window` entity marked `PrimaryWindow` — **required**: `set_initial_focus` takes `Single<Entity, With<PrimaryWindow>>` and is skipped when absent (`lib.rs:327-333`), and `dispatch_focused_input` does nothing at all when `windows.single()` fails (`lib.rs:345-348`). Focusable entities are the viewer's pane mirrors, parented under the window entity **through the tab/split hierarchy** (`ChildOf`), which is what makes `FocusedInput` bubble from pane → tab → workspace/command-mode handlers. `InputFocus` names the focused pane mirror; the viewer translates that into the attachment stream's focus and the server's per-viewer `FocusedPane(Entity)` (3.2). `InputFocusVisible` drives whether the viewer paints a focus border.

**Pitfalls.**
- `FocusedInput.window` is private (`lib.rs:196-205`); a fux system cannot construct a `FocusedInput` — it must go through `dispatch_focused_input::<M>`. For the prompt's 3.11 pipeline, fux must additionally register `dispatch_focused_input::<bevy_window::Ime>` (bracketed paste) itself, because only `KeyboardInput` is compiled under the chosen features and `Ime` is not dispatched by the plugin.
- The dispatch is per **message type**: `ButtonInput<KeyCode>` (chords) is a resource poll, not a message, and is never routed through focus — chord handling stays app-global, which is what prompt 3.11 says ("`ButtonInput<KeyCode>` answers chords").
- `InputFocus` is one resource per `World` (`lib.rs:103`), which is exactly why fux's server keeps `FocusedPane(Entity)` per viewer component (3.2, 3.5 invariant "a viewer's `FocusedPane` is inside its `Showing` tab"). The viewer App has one focus, so the resource is the right tool there and only there.
- Focus changes propagate through a buffered FIFO: `set`/`clear` merely append. Reading `InputFocus::get()` inside the same frame gives the latest value, but the corresponding `FocusGained`/`FocusLost` events only appear after PostUpdate's `process_recorded_focus_changes`.
- Both plugins init `InputFocus`/`InputFocusVisible`, but `InputDispatchPlugin` does not init `InputFocus` — adding it without `InputFocusPlugin` is a setup error, not a compile error.
- `set_initial_focus` runs in `PostStartup`, so a viewer that spawns its window in a later `OnEnter(Serving)` schedule will never get the automatic window focus; it must set focus itself (or spawn an `AutoFocus` entity).

**Excerpt** (`lib.rs:192-205`):

```rust
#[derive(EntityEvent, Clone, Debug, Component)]
#[entity_event(propagate = WindowTraversal, auto_propagate)]
#[cfg_attr(
    feature = "bevy_reflect",
    derive(Reflect),
    reflect(Event, Component, Clone)
)]
pub struct FocusedInput<M: Message + Clone> {
    /// The entity that has received focused input.
    #[event_target]
    pub focused_entity: Entity,
    /// The underlying input message.
    pub input: M,
    /// The primary window entity.
    window: Entity,
}
```

### tab_navigation.rs

**Mechanism.** `TabIndex(pub i32)` (`tab_navigation.rs:63`) marks participation; `>= 0` means sequentially tabbable in ascending order, `< 0` means directly focusable only. `TabGroup { order: i32, modal: bool }` (`tab_navigation.rs:72-96`) marks a subtree; non-modal groups are traversed in `order` then tree order, a modal group traps tabbing inside itself (`navigate_in_group`, `tab_navigation.rs:256-338`). `NavAction { Next, Previous, First, Last }` (`tab_navigation.rs:110-124`) with wrapping; `TabNavigationError` (`tab_navigation.rs:131-157`) distinguishes "no tab groups", "no focusable entities", "malformed groups", and the recoverable `NoTabGroupForCurrentFocus { previous_focus, new_focus }`. `TabNavigation` is a `SystemParam` (`tab_navigation.rs:159-171`) exposing `navigate(&InputFocus, NavAction)`, `initialize(parent, NavAction)`, and the internal `navigate_internal`. `gather_focusable` (`tab_navigation.rs:324-353`) walks children depth-first, **skipping nested tab groups** and non-modal group boundaries. `acquire_focus` (`tab_navigation.rs:355-378`) is the `AcquireFocus` observer: it stops at the first ancestor with `TabIndex`, and clears focus at the window. `TabNavigationPlugin` (`tab_navigation.rs:380-389`) installs `setup_tab_navigation` (Startup), the `acquire_focus` observer, and (feature-gated) `click_to_focus`. `setup_tab_navigation` (`tab_navigation.rs:391-395`) attaches `handle_tab_navigation` as an observer **on each primary window entity**. `handle_tab_navigation` (`tab_navigation.rs:429-468`) reacts to `FocusedInput<KeyboardInput>` with `KeyCode::Tab`, `Pressed`, `!repeat`, using `ButtonInput<KeyCode>` for Shift to select `Previous`, sets `InputFocusVisible.0 = true`, and unwraps the recoverable error case.

**fux/zor use (3.11).** The viewer's pane mirrors get `TabIndex` (`0, 1, 2, …` in projected pane order) and the tab root gets `TabGroup::new(0)`, so Tab/Shift+Tab cycle panes in the same order the viewer paints them — a natural mapping of the existing "cycle panes" control. `DirectionalNavigation`/`TabNavigation` both write `InputFocus` directly, so a fux viewer system that maps the new focus to `FocusedPane` just observes `FocusGained`/`FocusLost` (see `gained_and_lost.rs`) instead of polling. `AcquireFocus` is the right trigger for the viewer's click-to-focus path: the player's own `On<Pointer<Press>>` observer (from bevy_picking, compiled in the viewer because fux depends on `bevy_picking` directly) triggers `AcquireFocus { focused_entity: press.entity, window }`, and `acquire_focus` handles the "walk up to the nearest focusable ancestor, else clear at window" policy — this reproduces the feature-gated `click_to_focus` without the feature.

**Pitfalls.**
- `TabNavigationPlugin` attaches `handle_tab_navigation` to **primary window entities only** (`tab_navigation.rs:391-395`); with no `PrimaryWindow` the observer is never installed and Tab does nothing, silently. fux's viewer must spawn its window entity before Startup of a schedule that runs `setup_tab_navigation`.
- `handle_tab_navigation` only fires when the viewer's `InputFocus` entity is a descendant of the window entity (`WindowTraversal` terminates there). Same requirement as above.
- The Shift check uses `keys: Res<ButtonInput<KeyCode>>` pressed-state (`tab_navigation.rs:444-452`), not the message's modifiers, so a batched viewer frame that delivers `Tab` before the `Shift` press is misclassified as `Next`.
- `navigate` gathers **all** tab-indexed entities in the world (modulo tab-group filtering) into a `Vec` per call (`tab_navigation.rs:257-290`); fine for a viewer with tens of panes, not a per-frame primitive.
- `click_to_focus` is behind `feature = "bevy_picking"` (`tab_navigation.rs:397`), which the prompt's feature list leaves off — do not expect it to exist.
- `TabNavigationError::NoTabGroupForCurrentFocus` still returns a usable `new_focus`; the observer applies it and only logs (`tab_navigation.rs:455-467`). A fux viewer system calling `TabNavigation` directly must replicate that recovery or panes will become unreachable.
- `TabIndex`/`TabGroup` are plain components with no `Reflect`-gated registration helper here; if the viewer's pane mirrors are reflected/spawned from bsn templates, the components must be registered explicitly (they derive `Reflect` under `bevy_reflect`, but nothing in this crate registers them globally).

**Excerpt** (`tab_navigation.rs:444-467`, the observer body):

```rust
if key_event.key_code == KeyCode::Tab
    && key_event.state == ButtonState::Pressed
    && !key_event.repeat
{
    let maybe_next = nav.navigate(
        &focus,
        if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
            NavAction::Previous
        } else {
            NavAction::Next
        },
    );
    match maybe_next {
        Ok(next) => {
            event.propagate(false);
            focus.set(next, FocusCause::Navigated);
            visible.0 = true;
        }
```

### directional_navigation.rs

**Mechanism.** `DirectionalNavigationMap` (`directional_navigation.rs:254-392`) is a directed graph `EntityHashMap<NavNeighbors>` where each node holds exactly 8 neighbors, one per `CompassOctant` (index via `CompassOctant::to_index`, `directional_navigation.rs:190-201`). `NavNeighbor` is `Auto | Blocked | Set(Entity)` (`directional_navigation.rs:162-185`), so "no manual edge" and "explicitly no edge" are distinguishable. Builders: `add_edge`, `block_edge`, `add_symmetrical_edge`, `block_symmetrical_edge`, `add_edges`, `add_looping_edges`, `remove` (O(n), rewrites inbound edges to `Auto`), `remove_multiple`, `clear` (`directional_navigation.rs:262-392`). `DirectionalNavigation` is a `SystemParam` (`directional_navigation.rs:396-435`) exposing `navigate(CompassOctant) -> Result<Entity, DirectionalNavigationError>`; it **mutates `InputFocus` itself** on success, and errors are `NoFocus`, `NoNeighborInDirection`, `BlockedNavigation` (`directional_navigation.rs:438-466`). `FocusableArea { entity, position: Vec2, size: Vec2 }` (`directional_navigation.rs:472-481`) plus the `Navigable` trait (`directional_navigation.rs:485-488`) are the geometry inputs for `auto_generate_navigation_edges` (`directional_navigation.rs:518-560`), which fills every octant's best candidate while **preserving manual `Set`/`Blocked` edges**. `AutoNavigationConfig { min_alignment_factor, max_search_distance, prefer_aligned }` (`directional_navigation.rs:93-152`) tunes that search. `DirectionalNavigationPlugin` (`directional_navigation.rs:74-80`) only inserts the map and the config.

**fux/zor use (3.11 and 3.2).** The h/j/k/l pane-focus model maps onto this type directly, with one substitution: `DirectionalNavigation`'s SystemParam is bound to the singleton `InputFocus` resource (`directional_navigation.rs:397-400`), which cannot express per-viewer focus in the server World. Two usable shapes:
1. **Server-side, per-viewer (3.2/3.4):** fux owns a `DirectionalNavigationMap` (or a per-workspace one) whose nodes are **pane entities**, builds edges structurally from the split tree (a pane's `East` neighbour = the leftmost leaf of its parent split's right subtree; `add_symmetrical_edge` for each pair), and each viewer's `h/j/k/l` request reads `map.get_neighbor(viewer.FocusedPane, octant)` and writes `FocusedPane`. `remove_multiple` is exactly the cleanup a pane close needs (`directional_navigation.rs:288-305`).
2. **Viewer-side (3.11):** if the viewer wants geometry-derived navigation over its pane mirrors, feed `ComputedNode` centres/sizes as `FocusableArea`s to `auto_generate_navigation_edges` (or `navigator::find_best_candidate` directly) and let the viewer's own `DirectionalNavigation` mutate the viewer's `InputFocus`.
In both cases `CompassOctant` (`bevy_math`) is the vocabulary: `North` = up = k, `South` = j, `West` = h, `East` = l.

**Pitfalls.**
- `NavNeighbors` is a fixed 8-slot array (`directional_navigation.rs:190-201`); there is no arbitrary graph — a fux "cycle panes left regardless of geometry" behaviour cannot be expressed as a `CompassOctant` edge without precomputing it into the map, which is what `add_looping_edges` (`directional_navigation.rs:353-360`) is for.
- The map keys are raw `Entity` values. fux's `Ids` discipline (3.2) says panes keep their entity across move/swap, so the map survives those operations; but a *closed then restored* pane is a new entity and the map must be rebuilt or `remove_multiple`d, or it will hold stale edges that `add_edge` silently keeps (`add_edge` overwrites the slot from `A` but nothing prunes `B -> A`).
- `remove` is documented O(n) and rewrites **inbound** edges to `Auto` only; it does not make the graph symmetric again.
- `auto_generate_navigation_edges` uses UI coordinates (Y down) and flips Y internally in `score_candidate` (`navigator.rs:77-95`); the prompt's cell grid with `bevy_ui` `ComputedNode` rects is the same coordinate space, but a fux adapter must pass **global** centres (`UiGlobalTransform`), not local `Node` positions.
- Nothing in this module runs systems: the plugin only inserts two resources. A fux plugin must add the system that calls `auto_generate_navigation_edges` after layout changes, ordered against `bevy_ui`'s layout set.
- `DirectionalNavigation` mutates `InputFocus` and cannot be pointed at a component; using it per-viewer in the server is impossible without forking or reimplementing the three-line `navigate` body over `FocusedPane`.

**Excerpt** (`directional_navigation.rs:410-431`, the whole navigation step):

```rust
pub fn navigate(&mut self, direction: CompassOctant) -> Result<Entity, DirectionalNavigationError> {
    if let Some(current_focus) = self.focus.get() {
        // Respect manual edges first
        match self.map.get_neighbor(current_focus, direction) {
            NavNeighbor::Auto => Err(DirectionalNavigationError::NoNeighborInDirection {
                current_focus,
                direction,
            }),
            NavNeighbor::Blocked => Err(DirectionalNavigationError::BlockedNavigation {
                current_focus,
                direction,
            }),
            NavNeighbor::Set(new_focus) => {
                self.focus.set(new_focus, FocusCause::Navigated);
                Ok(new_focus)
            }
        }
    } else {
        Err(DirectionalNavigationError::NoFocus)
    }
}
```

### gained_and_lost.rs

**Mechanism.** `FocusCause { Navigated, Pressed }` (`gained_and_lost.rs:16-25`) distinguishes keyboard/gamepad navigation from primary-mouse press, so a widget can change behaviour (the doc example: select-all on navigate, not on press). `FocusGained { entity, cause }` and `FocusLost { entity }` (`gained_and_lost.rs:36-56`) are `EntityEvent`s with `#[entity_event(auto_propagate)]`, so they bubble up `ChildOf` — a tab-root observer sees its panes' focus changes. `process_recorded_focus_changes` (`gained_and_lost.rs:61-105`) drains the FIFO change buffer in PostUpdate, emitting `FocusLost` then `FocusGained` for each **transition**, skipping no-ops, and using `bypass_change_detection` so `InputFocus::is_changed()` keeps meaning "the focused entity changed".

**fux/zor use (3.11 + 3.2).** This is how the viewer turns focus into wire traffic without polling: an observer on the viewer's pane-mirror entities for `On<FocusGained>`/`On<FocusLost>` writes the attachment stream's focus (and, because the event bubbles, the tab root can react to "focus left the tab" for command-mode exit). The `cause` field is exactly the `FocusedPane` set-by-click vs set-by-key distinction the server needs, and the bubbling makes "focus within tab X" a tab-root observer rather than a query. On the server side the same pattern -- an event per focus transition -- is the shape fux should mirror for `FocusedPane` transitions so that BRP `+watch` (3.9) can stream them.

**Pitfalls.**
- Multiple focus changes in **one frame** emit multiple Gained/Lost pairs in the order they were recorded (`gained_and_lost.rs:254-277` test): a viewer frame that sets focus then clears it emits `Gained(a), Lost(a)`, and intermediate entities get fully-formed pairs. A fux consumer must tolerate (and ideally collapse) pairs it did not expect.
- Events are deferred `commands.trigger`s, so they are observed after the PostUpdate system, i.e. after the frame's other PostUpdate work; anything wanting the new focus in the same frame must read `InputFocus::get()`.
- `FocusGained`/`FocusLost` are not `Reflect`-registered by a plugin here (they only derive `Reflect` under the feature); a system that wants `observe+watch` on them must `register_type` them, exactly as fux's event-log policy (3.3) requires.
- `process_recorded_focus_changes` writes `original_focus = current_focus` at the end while bypassing change detection (`gained_and_lost.rs:104`): after a `set` followed by `clear` in the same frame, `original_focus` is `None` and the buffer is empty — the "previous focus" is not recoverable from the resource.
- `FocusCause::Pressed` is only ever produced by the feature-gated `click_to_focus` (`tab_navigation.rs:402-410`); with that feature off, fux's own click-to-focus path must pass `FocusCause::Pressed` explicitly.

**Excerpt** (`gained_and_lost.rs:74-93`, the replay loop):

```rust
let mut previous_focus = focus.original_focus;
for change in focus.bypass_change_detection().recorded_changes.drain(..) {
    let changed_ent = match change {
        Some((changed_ent, _cause)) => Some(changed_ent),
        None => None,
    };
    // Only send focus change events if the focused entity actually changed.
    if changed_ent == previous_focus {
        continue;
    }
    match change {
        Some((new_focus, cause)) => {
            if let Some(old_focus) = previous_focus {
                commands.trigger(FocusLost { entity: old_focus });
            }
            commands.trigger(FocusGained { entity: new_focus, cause });
            previous_focus = Some(new_focus);
        }
```

### autofocus.rs

**Mechanism.** Thirty-one lines: `AutoFocus` (`autofocus.rs:24`) is a marker component with `#[component(on_add = on_auto_focus_added)]`; the hook (`autofocus.rs:26-30`) sets `InputFocus` to the entity with `FocusCause::Navigated` on add, guarded by `get_resource_mut`.

**fux/zor use (3.11).** This is the correct mechanism for the viewer's pane mirrors, precisely because the mirrors are spawned by a scene/template rather than imperatively: when the viewer materializes the pane set for a tab (`Showing(tab)`), the mirror for the pane that should be focused carries `AutoFocus`, and the hook sets viewer focus at spawn time. The crate's own docs point at exactly this use for delayed spawning (`lib.rs:131-135`: "particularly useful when working with bsn! scenes, where spawning may be delayed"), which matches fux's restore-template path (3.7).

**Pitfalls.**
- Last writer wins: two `AutoFocus` entities added in one command flush leave focus on whichever the hook ran last; a restore that spawns N pane mirrors must put `AutoFocus` on exactly one.
- The hook does nothing if `InputFocus` is absent (no `InputFocusPlugin`), and it is a hook, not a system — it cannot observe anything, consistent with fux's rule that hooks only do index/bookkeeping work (3.3). It is a small, explicit exception to that rule and worth calling out in fux's own hook policy.
- The hook runs on `add` only; a mirror that is re-parented (3.4 move/swap) does not regain focus.

**Excerpt** (`autofocus.rs:23-31`, entire file):

```rust
#[component(on_add = on_auto_focus_added)]
pub struct AutoFocus;

fn on_auto_focus_added(mut world: DeferredWorld, HookContext { entity, .. }: HookContext) {
    if let Some(mut input_focus) = world.get_resource_mut::<InputFocus>() {
        input_focus.set(entity, FocusCause::Navigated);
    }
}
```

### navigator.rs

**Mechanism.** 302 lines, of which ~150 are tests. `find_best_candidate(origin, direction, candidates, config) -> Option<Entity>` (`navigator.rs:144-176`) is the only public function; it scores every candidate and keeps the minimum. `score_candidate` (`navigator.rs:70-140`) returns `f32::INFINITY` when the candidate is not in the octant's half-plane (`CompassOctant::is_in_direction` after flipping UI Y), when the perpendicular overlap ratio is below `min_alignment_factor`, or when the rect-edge distance exceeds `max_search_distance`. Otherwise the score is **rect-edge distance** (not centre distance) plus an optional misalignment penalty `(1 - alignment) * distance * 2.0` when `prefer_aligned` (`navigator.rs:111-138`). `calculate_overlap`/`calculate_1d_overlap` (`navigator.rs:12-67`) compute the overlap ratio against the *smaller* of the two sizes.

**fux/zor use (3.11).** This is the automatic-edge generator behind h/j/k/l when the structural split-tree edges are not wanted: give it the projected pane rects (`ComputedNode` global centre + size as `FocusableArea`) and it produces exactly the "nearest pane in that direction with overlap" behaviour that matches the existing pane-layout-controls semantics, with `max_search_distance` in cells and `min_alignment_factor` to stop diagonal leaks between columns. Because it only needs `(Entity, Vec2, Vec2)` it works identically against the server's `ComputedNode` rects and the viewer's mirror rects.

**Pitfalls.**
- `score_candidate` and the overlap helpers are private; the public surface is `find_best_candidate` (per candidate set, one call per direction) — a fux plugin that wants the whole map filled should call `auto_generate_navigation_edges` from the sibling module rather than reimplement the loop.
- Distance is measured between **rect edges**, deliberately, so a wide neighbour beats a nearer small one (`directional_navigation.rs` test `test_edge_distance_vs_center_distance`, `directional_navigation.rs:800-838`): fux's expected "nearest pane" semantics must be stated in those terms or the tests will disagree with the implementation.
- `prefer_aligned` is a heuristic penalty, not a constraint; with `min_alignment_factor = 0.0` (the default, `directional_navigation.rs:148`) a candidate barely touching in the perpendicular axis is still eligible.
- All arithmetic is `f32` in **logical** pixels; a fux viewer/world mixing cell counts and pixels must convert once, at the boundary.

**Excerpt** (`navigator.rs:150-176`, the search loop):

```rust
let mut best_candidate = None;
let mut best_score = f32::INFINITY;

for candidate in candidates {
    // Skip self
    if candidate.entity == origin.entity {
        continue;
    }

    // Score the candidate
    let score = score_candidate(
        origin.position,
        origin.size,
        candidate.position,
        candidate.size,
        direction,
        config,
    );

    if score < best_score {
        best_score = score;
        best_candidate = Some(candidate.entity);
    }
}

best_candidate
```

---

## bevy_picking and bevy_input_focus: consequences for the prompt

1. **`NormalizedRenderTarget::None` has `width`/`height: u32` fields, not `size`** (`crates/bevy_camera/src/camera.rs:942-962`); prompt 3.4's `None { size }` must be read as that. Build it as `RenderTarget::None { size }.normalize(None)` (`camera.rs:929-940`) — the `None` arm ignores the `primary_window` argument, so fux needs no window to construct a pointer target.
2. **`Location::is_in_viewport` is unusable in fux** (`pointer.rs:219-247`): it early-returns `false` when `primary_window.single()` fails and compares `render_target.normalize(Some(primary_window))` against the pointer's target. The fux cell backend must match viewports itself.
3. **The `Hovered`/`DirectlyHovered` components are dead for fux** (`hover.rs:385`, `420`): both hardcode `PointerId::Mouse`. Hover state must come from `HoverMap`.
4. **Both crates are `PrimaryWindow`-shaped**, which the server side of fux never satisfies and the viewer side always does: picking's `PointerInputPlugin` and `update_window_hits`, and input-focus's `set_initial_focus`, `dispatch_focused_input`, `setup_tab_navigation`. This is the concrete reason the prompt separates server picking (3.4, no window, `PointerId::Custom`) from viewer focus (3.11, one `PrimaryWindow`).
5. **`InputFocus` is one resource with a private `current_focus`** (`lib.rs:103-108`) — no per-viewer focus, confirming the prompt's `FocusedPane(Entity)` component on `Viewer` (3.2). The viewer App is the only place where the resource and its bubbling dispatch are correct.
6. **Two fux features are silently absent** under the prompt's dependency sets and must be written by hand or their absence documented: `bevy_input_focus`'s `click_to_focus` (needs `features = ["bevy_picking"]`) and `FocusedInput` dispatch for anything other than `KeyboardInput` (needs the `mouse`/`gamepad` features; `Ime::Commit` needs an fux-registered `dispatch_focused_input::<Ime>`).

Feature state that matters throughout `bevy_ui`: `crates/bevy_ui/Cargo.toml:61-72` shows `default = []`, `bevy_picking` opt-in (line 69) and `ghost_nodes` opt-in (line 72). fux's `bevy_ui = { version = "=0.19.1", default-features = false }` therefore compiles the **non-ghost** traversal and **no** picking backend.

## bevy_ui

### crates/bevy_ui/src/lib.rs

**Mechanism.** `UiSystems` (`lib.rs:95-135`) is the schedule vocabulary fux must order against: `Focus` (PreUpdate), then `Prepare`, `Propagate`, `Content`, `Layout`, `PostLayout`, `Stack`. `UiPlugin::build` (`lib.rs:142-241`) initialises `UiSurface`, `UiScale`, `UiStack` (144-147), chains `CameraUpdateSystems -> Prepare.after(AnimationSystems) -> Propagate -> Content -> Layout -> PostLayout` in `PostUpdate` (148-160), then adds `HierarchyPropagatePlugin::<ComputedUiTargetCamera>` and `::<ComputedUiRenderTargetInfo>` with each `PropagateSet` pinned inside `UiSystems::Propagate` (161-171). `ui_focus_system` runs in `PreUpdate` after `InputSystems` (172-175); `propagate_ui_target_cameras` is in `Prepare`, `ui_layout_system` in `Layout` before `TransformSystems::Propagate`, `ui_stack_system` in `Stack`, `update_clipping_system` in `PostLayout` (191-227). The plugin also unconditionally adds text/image widget systems into `Content`/`PostLayout` via `build_text_interop` (300-304).

**fux/zor use.** fux adds `UiPlugin` explicitly (prompt §2 plugin list); every fux layout-touching system belongs in a named `UiSystems` set so it lands in the right order — the `ResizePty` comparison system is `PostLayout` (after `Layout`, after `ComputedNode` is written), the `flex_grow`/`Display::None` mutation systems are `Prepare`-or-earlier (before `Layout`). zor does not depend on `bevy_ui` at all.

**Pitfalls.** (1) `UiSystems::Stack` is *not* part of the `.chain()` (148-160) — only `Prepare..PostLayout` chain; `ui_stack_system` is ordered only by its own `in_set`. (2) The plugin pulls `bevy_text` and `bevy_sprite` systems (`measure_text_system`, `text_system`, `update_image_content_size_system`, `update_text2d_layout` ambiguity config) that fux never uses; they are no-ops on empty queries but are in the schedule and add a `bevy_asset::AssetEventSystems` relation. (3) `picking_backend::UiPickingPlugin` and `widget::viewport_picking` are behind `feature = "bevy_picking"` (177-182) which fux leaves off — fux's own backend must not expect `UiPickingPlugin` to exist.

```rust
// lib.rs:161-171
.add_plugins(HierarchyPropagatePlugin::<ComputedUiTargetCamera>::new(
    PostUpdate,
))
.configure_sets(
    PostUpdate,
    PropagateSet::<ComputedUiRenderTargetInfo>::default().in_set(UiSystems::Propagate),
)
```

### crates/bevy_ui/src/ui_node.rs

**Mechanism.** `ComputedNode` (`ui_node.rs:29-130`) is the geometry read surface: `pub size: Vec2` (physical px, rounded), `pub unrounded_size`, `pub content_size`, `pub scrollbar_size`, `pub scroll_position`, `pub border`/`pub padding`/`pub border_radius`, `pub outline_width`/`offset`, `pub inverse_scale_factor`; `#[require(ComputedStackIndex)]`. `Node` (`ui_node.rs:492-800`, `DEFAULT` at 822-866) is the full CSS style with all-pub fields: `display`, `width/height/min_width/min_height/max_*: Val`, `flex_direction`, `flex_grow: f32` (default `0.0`), `flex_shrink` (default `1.0`), `flex_basis`, `overflow`, `aspect_ratio`, grid fields. `#[require(...)]` at 476-490 auto-inserts `ComputedNode`, `ComputedStackIndex`, `ContentSize`, `ComputedUiTargetCamera`, `ComputedUiRenderTargetInfo`, `UiTransform`, `Visibility`, `ZIndex`. `Display` (`ui_node.rs:1154-1180`) is `Flex | Grid | Block | None`, `DEFAULT = Flex`, `None` documented as "Use no layout, don't render this node and its children". `LayoutConfig { pub use_rounding: bool }` defaults `true` (`ui_node.rs:2906-2933`). The camera family: `UiTargetCamera(pub Entity)` + `entity()` (2938-2944), `IsDefaultUiCamera` (2980), `DefaultUiCamera` SystemParam whose `get()` (2990-3008) returns the single `IsDefaultUiCamera` camera else the highest-`(order, entity)` camera on the primary window, `ComputedUiTargetCamera { pub(crate) camera }` with only `pub fn get() -> Option<Entity>` (3016-3033), `ComputedUiRenderTargetInfo { pub(crate) scale_factor, pub(crate) physical_size }` with `scale_factor()`, `physical_size()`, `logical_size()` (3038-3070). `ZIndex(pub i32)` (2440), `GlobalZIndex(pub i32)` (2450), `OverrideClip` (2418) and `CalculatedClip { pub clip: Rect }` (2409) complete the set.

**fux/zor use (prompt §3.2, §3.4, §3.5).** fux writes `Node` on every tree entity (root, split, pane leaf, separator) and `UiTargetCamera(viewer_camera)` on each tab root; it never writes `ComputedNode`, `ComputedUiTargetCamera`, `ComputedUiRenderTargetInfo`, `CalculatedClip` or `ComputedStackIndex`. `ZIndex`/`Display::None` are the zoom and hidden-tab levers. `Display::None` on a hidden tab root satisfies the §3.5 invariant "every tab root has a `UiTargetCamera` naming a live viewer camera or is `Display::None`".

**Pitfalls.** (1) `ComputedUiRenderTargetInfo`'s two fields are `pub(crate)`: fux can read them only through `physical_size()`/`scale_factor()`/`logical_size()` and **cannot construct one**; the only writer is `propagate_ui_target_cameras`. (2) `DefaultUiCamera::get()` needs `PrimaryWindow`-targeting cameras; fux has no window, so with no `IsDefaultUiCamera` it returns `None`, `propagate_ui_target_cameras` falls back to `Entity::PLACEHOLDER`, and the root silently gets a 0x0 target (zero-size `ComputedNode`s, **no panic**). Every fux tab root must carry an explicit `UiTargetCamera`. (3) `Node` `require`s `Visibility`/`UiTransform`/`ZIndex`, so fux cannot use `Node` as a bare present-marker without those; and `Disabled` on any of these entities removes it from *every* unqualified query (see below).

```rust
// ui_node.rs:3016-3033
pub struct ComputedUiTargetCamera {
    pub(crate) camera: Entity,
}
impl ComputedUiTargetCamera {
    pub fn get(&self) -> Option<Entity> {
        Some(self.camera).filter(|&entity| entity != Entity::PLACEHOLDER)
    }
}
```

### crates/bevy_ui/src/update.rs

**Mechanism.** `propagate_ui_target_cameras` (`update.rs:115-171`) is the whole camera-target bridge. It first strips `Propagate<ComputedUiTargetCamera>`/`Propagate<ComputedUiRenderTargetInfo>` from any entity that has a UI parent (126-133), then for every root (`UiRootNodes`) resolves `camera = UiTargetCamera::entity().or(default_camera_entity).unwrap_or(Entity::PLACEHOLDER)` and `try_insert`s `Propagate(ComputedUiTargetCamera { camera })` plus `Propagate(ComputedUiRenderTargetInfo { scale_factor: camera.target_scaling_factor().unwrap_or(1.) * ui_scale.0, physical_size: camera.physical_viewport_size().unwrap_or(UVec2::ZERO) })` (135-170). `update_clipping_system`/`update_clipping` (`update.rs:21-113`) walk the same roots, compute each node's clip rect from `ComputedNode::resolve_clip_rect`, and (notably) set `maybe_inherited_clip = Some(Rect::default())` when `node.display == Display::None`, i.e. a hidden subtree gets an empty clip.

**fux/zor use (prompt §3.4).** This is the entire justification for `Camera + RenderTarget::None { size }` per viewer: fux writes `camera.computed.target_info = Some(RenderTargetInfo { physical_size: UVec2::new(cols, rows), scale_factor: 1.0 })` on attach/resize (both fields public: `crates/bevy_camera/src/camera.rs:197-200, 218-220, 393`) and `UiTargetCamera(viewer_camera)` on the tab root; nothing else in fux computes viewport size. `physical_viewport_size()` (`camera.rs:492-497`) falls back to `physical_target_size()` → `computed.target_info.physical_size`, so **no `Viewport` component is needed**. The multi-viewer smallest-viewport rule is a fux policy executed purely by re-pointing `UiTargetCamera` at the smallest viewer's camera; bevy has no smallest-viewport logic of its own. The `Display::None` empty-clip behaviour is what makes hidden tabs non-pickable for the fux backend's `clip_check_recursive`.

**Pitfalls.** (1) `physical_viewport_size().unwrap_or(UVec2::ZERO)` — a `UiTargetCamera` naming a despawned camera degrades to a 0x0 root, not an error; fux's `LayoutGeneration`/liveness validation must catch it first. (2) `scale_factor = target_scaling_factor * UiScale`: any non-1.0 `UiScale` or camera scale factor breaks the cell invariant. (3) `Propagate<..>` is re-`try_insert`ed every frame on every root, so the propagate pipeline re-runs each frame; do not put per-frame work in fux systems keyed off `Changed<ComputedUiRenderTargetInfo>`. (4) `update_clipping` recurses through `UiChildren` and needs `CalculatedClip` maintenance to be left alone by fux.

```rust
// update.rs:151-168
let (scale_factor, physical_size) = camera_query
    .get(camera)
    .ok()
    .map(|camera| {
        (
            camera.target_scaling_factor().unwrap_or(1.) * ui_scale.0,
            camera.physical_viewport_size().unwrap_or(UVec2::ZERO),
        )
    })
    .unwrap_or((1., UVec2::ZERO));
```

### crates/bevy_ui/src/layout/mod.rs

**Mechanism.** `LayoutContext { scale_factor, physical_size }` (`mod.rs:40-70`). `ui_layout_system` (`mod.rs:77-176`) does four things in order: (a) a sync pass over `Query<(Entity, Ref<Node>, &mut ContentSize, Ref<ComputedUiRenderTargetInfo>)>` that, when the target info **or** the `Node` **or** the `ContentSize` changed, builds a `LayoutContext` from `computed_target.scale_factor/physical_size` and calls `ui_surface.upsert_node(&ctx, entity, &node, content_size.bypass_change_detection().measure.take())` (84-102); (b) child-removal handling via `RemovedComponents<Children>` → `try_remove_children`, and `RemovedComponents<Node>` filtered by `!node_query.contains(entity)` → `remove_entities` (104-137); (c) for each root, `update_children_recursively` reconciles taffy children (using `ui_children.is_changed` or `Added<Node>` children), then `ui_surface.compute_layout(root, computed_target.physical_size, ..)` (139-176); (d) `update_uinode_geometry_recursive` (178-365) which per node reads `ui_surface.get_layout(entity, use_rounding)` and writes `ComputedNode`: `size`/`unrounded_size`/`inverse_scale_factor` only when they differ (change-tracked), `content_size`, `border`, `padding`, `border_radius`, `outline_width/offset`, `scrollbar_size`, `scroll_position` via `bypass_change_detection()`, plus `UiGlobalTransform = inherited * local` recomputed from `UiTransform::compute_affine` and the node's center offset. `use_rounding` comes from `Option<&LayoutConfig>` and is inherited downward; children of a `Display::None` node are still recursed into.

**fux/zor use (prompt §3.4, §3.5).** This system *is* the fux layout engine; fux writes only `Node`/`ChildOf`/`UiTargetCamera`/`Camera.computed.target_info` and reads `ComputedNode` afterwards. Zoom = `Display::None` on siblings + `flex_grow` on the zoomed path; split/resize = `flex_grow` on the two children of one split; hidden tabs keep their last `ComputedNode` because a `Display::None` root still gets a (zero-size) taffy layout and the recursion still writes. fux's `ResizePty` comparison runs after this in `PostLayout`.

**Pitfalls.** (1) `node_query.get(ui_root_entity).unwrap()` (149) panics if a root lacks `Node`+`ContentSize`+`ComputedUiRenderTargetInfo`; normally guaranteed by `Node::require`, but a `Disabled` component on a root drops it from this default-filtered query and makes the root invisible (never put `Disabled` in the fux layout tree). (2) The `Changed<ComputedNode>` signal fux uses for projection is only raised by the `size`/`unrounded_size`/`inverse_scale_factor` branch (change-tracked); every other field bypasses detection, so a system observing `Changed<ComputedNode>` sees size changes only. (3) `measure.take()` moves the measure out of `ContentSize` every sync, so fux must not hold onto a `NodeMeasure`. (4) Rounding is inherited globally from the root unless a node sets `LayoutConfig::use_rounding = false` — that is the knob for the §5 layout oracle.

```rust
// mod.rs:84-102 (sync pass)
if computed_target.is_changed() || node.is_changed() || content_size.is_changed() {
    let layout_context = LayoutContext::new(
        computed_target.scale_factor,
        computed_target.physical_size.as_vec2(),
    );
    if content_size.is_changed() && content_size.measure.is_none() {
        ui_surface.try_remove_node_context(entity);
    }
    let measure = content_size.bypass_change_detection().measure.take();
    ui_surface.upsert_node(&layout_context, entity, &node, measure);
}
```

### crates/bevy_ui/src/layout/ui_surface.rs

**Mechanism.** `UiSurface` (`ui_surface.rs:62-72`) is the taffy wrapper: `pub root_entity_to_viewport_node: EntityHashMap<taffy::NodeId>` (public), `pub(super) entity_to_taffy: EntityHashMap<LayoutNode>` and `pub(super) taffy: UiTree<NodeMeasure>` (**layout-module-private**). `LayoutNode { viewport_id: Option<taffy::NodeId>, id: taffy::NodeId }` records the implicit viewport node for roots. `upsert_node` (108-140) sets node context + style on an existing taffy node or creates a leaf (with context when a measure exists). `update_children` (155-176) re-points taffy children and, when a former root becomes a child, removes its stale viewport node. `get_or_insert_taffy_viewport_node` (184-210) lazily creates a per-root taffy parent: `Display::Grid`, `size = percent(1.0) x percent(1.0)`, `align_items/justify_items = Start`, then `add_child(implicit_root, root)`. `compute_layout` (212-260) runs `compute_layout_with_measure` on that viewport node with `AvailableSpace::Definite(render_target_resolution)` and a measure closure that only fetches a `ComputedTextBlock` when `TextMeasure::needs_buffer` says so. `get_layout` (283-310) toggles **global** taffy rounding (`enable_rounding()`/`disable_rounding()`) according to `use_rounding`, returns the rounded `taffy::Layout`, then re-reads the size with rounding off as `unrounded_size`, restoring `enable_rounding()` before returning.

**fux/zor use.** fux never touches `UiSurface` internals: no `min_width` handling here (that is `convert.rs` → taffy `min_size`), and `entity_to_taffy`/`taffy` are unreachable from outside the layout module. Two usable public entry points remain: the `Resource` itself (for `fux/…` diagnostics) and `layout::debug::print_ui_layout_tree`. The implicit 100%-viewport node is exactly what turns `RenderTargetInfo.physical_size` into the root's box — the §5 test "80x24 target yields an 80x24 `ComputedNode`" depends on it.

**Pitfalls.** (1) Rounding is a **global** taffy toggle mutated inside `get_layout`, which is called once per node during the geometry pass; `use_rounding` therefore has to be applied consistently per subtree or the pass produces mixed rounded/unrounded sizes. (2) `LayoutNode.viewport_id` means a root's taffy node has an extra parent; `remove_entities` (258-270) must also remove that viewport node, and `update_children` handles the demotion case. (3) `UiTree` carries hand-written `unsafe impl Send/Sync` justified by not using taffy's `calc` — if a fux fork ever enables `calc`, that invariant breaks. (4) A root's *first* taffy insertion happens via `upsert_node` before the viewport node exists, so `compute_layout` must be called after the sync pass (it is, same system).

```rust
// ui_surface.rs:190-207 (implicit viewport node)
let implicit_root = self.taffy
    .new_leaf(taffy::style::Style {
        display: taffy::style::Display::Grid,
        size: taffy::geometry::Size {
            width: taffy::style_helpers::percent(1.0_f32),
            height: taffy::style_helpers::percent(1.0_f32),
        },
        align_items: Some(taffy::style::AlignItems::Start),
        justify_items: Some(taffy::style::JustifyItems::Start),
        ..default()
    })
    .unwrap();
self.taffy.add_child(implicit_root, root_node.id).unwrap();
```

### crates/bevy_ui/src/layout/convert.rs

**Mechanism.** `from_node(node, context) -> taffy::style::Style` (`convert.rs:60-140`) is the pure translation layer: `Val::Px(v) -> style_helpers::length(context.scale_factor * v)` in both `into_length_percentage_auto` and `into_length_percentage` (17-57); `Val::Percent(v) -> percent(v / 100.)`; `Val::Vw/Vh/VMin/VMax` use `context.physical_size` (i.e. the camera target size, in the units set by `propagate_ui_target_cameras`); `flex_grow`/`flex_shrink`/`aspect_ratio` are copied verbatim (104-105, 133); `size` ← `width`/`height`, `min_size` ← `min_width`/`min_height`, `max_size` ← `max_width`/`max_height` (100-112); `gap` ← `column_gap`/`row_gap` (135-138); `border`/`padding`/`margin` use the matching `into_length_percentage(_auto)`, where `Val::Auto` becomes `length(0.)`. `Display` maps 1:1 including `None` (246-254), as do `OverflowAxis`, `FlexDirection`, `PositionType`, `FlexWrap`, align/justify enums.

**fux/zor use (prompt §3.4).** This is the mapping that makes `min_width`/`min_height` (the old `layout.rs` minimum cell sizes) and `flex_grow` (the ratio) do the work: fux writes `Node::min_width = Val::Px(min_cells)` / `min_height = Val::Px(min_cells)`, `flex_grow = ratio_weight`, `flex_direction = Row|Column`, root `width/height = Val::Percent(100.)`, separator `width|height = Val::Px(1.)`.

**Pitfalls.** (1) **`Val::Px` is scale-factor multiplied**: one `Px` is one cell only while `ComputedUiRenderTargetInfo.scale_factor == 1.0`, which requires `Camera.computed.target_info.scale_factor == 1.0` *and* `UiScale == 1.0` (`update.rs:158-162` multiplies them). Any change silently rescales the entire tree. (2) `Val::Auto` in `min_width`/`min_height` becomes taffy `auto()` and in `border`/`padding`/`gap` becomes `0.` — asymmetry worth knowing. (3) Percentages resolve against the parent (`Val::Percent` doc in `geometry.rs:33-49`), so the tab root's `100%` resolves against the implicit viewport node, i.e. against `physical_size`.

```rust
// convert.rs:103-112
flex_grow: node.flex_grow,
flex_shrink: node.flex_shrink,
flex_basis: node.flex_basis.into_dimension(context),
size: taffy::Size {
    width: node.width.into_dimension(context),
    height: node.height.into_dimension(context),
},
min_size: taffy::Size {
    width: node.min_width.into_dimension(context),
    height: node.min_height.into_dimension(context),
},
```

### crates/bevy_ui/src/layout/debug.rs

**Mechanism.** `print_ui_layout_tree(&UiSurface)` (`debug.rs:9-31`) builds a `NodeId -> Entity` map from `entity_to_taffy`, then for each `root_entity_to_viewport_node` entry recurses (`print_node`, 33-92) writing one `tracing::info!` line per camera entity with a tree of `x/y/width/height`, the entity id, the display variant (`NONE|LEAF|FLEX|GRID|BLOCK`) and whether the taffy node is `measured`.

**fux/zor use.** This is the only public way to inspect taffy's own view of the tree (since `taffy` is `pub(super)`); use it behind a fux diagnostic/BRP method rather than re-deriving layout. It is a `pub fn` in `bevy_ui::layout::debug`, re-exported through `pub use layout::*` (`lib.rs:54`).

**Pitfalls.** Logs at `info!` level through `tracing`, so with `LogPlugin` it lands in fux's daemon log; it is O(nodes) per call and must not run per-frame. It also `unwrap()`s `taffy.layout(node)`/`style(node)`/`children(node)` — calling it mid-update, before `compute_layout`, panics.

### crates/bevy_ui/src/experimental/ghost_hierarchy.rs

**Mechanism.** With `ghost_nodes` **off** (fux's configuration), `UiRootNodes` is a plain type alias `Query<'w, 's, Entity, (With<Node>, Without<ChildOf>)>` (`ghost_hierarchy.rs:39-40`) and `UiChildren` is a `SystemParam` wrapper over `Option<&Children>` and `&ChildOf` exposing `iter_ui_children`, `get_parent`, `is_changed`, `is_ui_node` (104-125). With the feature on, `GhostNode` is a marker that `require`s `Visibility, Transform, ComputedUiTargetCamera` and both params traverse past ghost nodes (`iter_ui_children` allocates a `SmallVec<[Entity; 8]>` beyond 8 children). The module is `pub mod experimental` but documented in `lib.rs:34-36` as "not re-exported, but is instead made public … to discourage accidental use".

**fux/zor use (prompt §3.4, §3.5).** These two types define what "a UI root" and "a UI child" mean to `ui_layout_system`, `update_clipping_system`, `propagate_ui_target_cameras` and `ui_stack_system`. A fux picking backend that must mirror bevy's traversal (ordering, ghost-skipping if the feature is ever enabled) should use `UiChildren`/`UiRootNodes` rather than raw `Children`, since these are the exact public definitions the layout systems use. fux's invariant "`bevy_ui`'s own `Children` order equals the projected tab/pane order" is checked against `UiChildren::iter_ui_children`.

**Pitfalls.** (1) The `experimental` path is public-but-discouraged API; importing it is a deliberate coupling choice to record. (2) `UiRootNodes` as a bare `Query` alias cannot be combined with other queries over the same data without an aliasing conflict in fux systems (queries are cloned, not shared). (3) In the non-ghost build `get_parent` is just `ChildOf::parent`, so `Propagate` propagation and layout agree on parenthood; if `ghost_nodes` is ever enabled, `Propagate` (which follows `ChildOf` directly) would disagree with layout traversal. Do not enable it.

### crates/bevy_ui/src/stack.rs

**Mechanism.** `ComputedStackIndex(pub u32)` (`stack.rs:24`) is `require`d by `ComputedNode`; `UiStack { pub partition: Vec<Range<usize>>, pub uinodes: Vec<Entity> }` (`stack.rs:31-40`) is the back-to-front list plus one contiguous partition per root. `ui_stack_system` (`stack.rs:54-116`) clears both, gathers roots (parentless `Node`s, then every node with `GlobalZIndex` not already visited), sorts them by `(global_zindex, zindex)`, recurses depth-first (`update_uistack_recursive`, 118-141) sorting siblings by `ZIndex`, then stamps each entity's `ComputedStackIndex` and records the partition range per root.

**fux/zor use.** The fux picking backend consumes `UiStack.uinodes` for hit ordering (the last entry receives interactions first) and `partition` to find one root's subtree, and it must filter candidates by `ComputedUiTargetCamera` because the stack itself is camera-agnostic. `ComputedStackIndex` is the per-entity order key fux can cache.

**Pitfalls.** (1) `UiStack` is fully rebuilt every frame, clearing and re-pushing (allocations are amortised by the resource's `Vec`s). (2) Roots have **no stable relative order** — the system's own test comment (`stack.rs:222-227`) says root ordering is untestable; fux must not derive tab order from `UiStack`, only within a partition. (3) With multiple viewers each tab root becomes its own partition, so the global `uinodes` list interleaves cameras; picking must use `partition` + `ComputedUiTargetCamera`, not raw indices.

```rust
// stack.rs:107-115
for (i, entity) in ui_stack.uinodes.iter().enumerate() {
    if let Ok(mut stack_index) = update_query.get_mut(*entity) {
        stack_index.set_if_neq(ComputedStackIndex(i as u32));
    }
}
```

### crates/bevy_ui/src/geometry.rs

**Mechanism.** `Val` (`geometry.rs:24-60`) is `Auto | Px(f32) | Percent(f32) | Vw | Vh | VMin | VMax` with `FromStr` (`"auto"`, `#.#px`, `%`, `vw`, `vh`, `vmin`, `vmax`) and a `PartialEq` that equates all zero-valued variants (94-130) so `Val::Px(0.) == Val::ZERO`. `Val::resolve(scale_factor, physical_base_value, viewport_size)` (450-470) is the layout-free resolver, and `UiRect` (632) with `UiRect::resolve` (1134) does the same for four sides; `Val::left/right/top/bottom/all/horizontal/vertical` build rects. Helpers `Val::ZERO`, `Val::DEFAULT = Auto` (132-133).

**fux/zor use (prompt §3.4).** fux writes `Val::Px` for minimum cell sizes and the 1-cell separator, `Val::Percent(100.)` for the tab root, and can read a `Val` back via `resolve` for BRP-side validation of `layout.rs`-derived constants (min size arithmetic from the old `layout.rs`) without running taffy.

**Pitfalls.** (1) `Val::resolve` takes a **scale factor** and multiplies `Px` by it — same invariant as `convert.rs` (keep it 1.0) — while `Percent` resolves against the physical base value. (2) The all-zero `PartialEq` means a `Val` comparison cannot distinguish units; do not use `Val` equality for change detection beyond zero-vs-nonzero. (3) `Val::Auto` is not a number; `resolve` returns `ValArithmeticError::NonEvaluable`-style failure and callers `.unwrap_or(0.)` (see `Val2::resolve`, `ui_transform.rs:60-72`).

### crates/bevy_ui/src/measurement.rs

**Mechanism.** `Measure` (`measurement.rs:97-100`) is `Send + Sync + 'static` with `measure(&mut self, MeasureArgs) -> Vec2`; `MeasureArgs` carries `known_width/height`, `available_width/height: AvailableSpace`, `font_system`, an optional text `buffer` and the resolved taffy `style`. `resolve_axis` (51-70) and `MeasureArgs::resolve_width/resolve_height` produce `ResolvedAxis { min, preferred, max, effective }` using taffy's `MaybeResolve`/`MaybeClamp`. `NodeMeasure` is `Fixed(FixedMeasure) | Text | Image | Custom(Box<dyn Measure>)`; `ContentSize` (`measurement.rs:141-160`) stores `pub(crate) measure: Option<NodeMeasure>` (`#[reflect(ignore)]`) with only `set`, `clear`, `fixed_size` public.

**fux/zor use.** fux leaves every `ContentSize.measure` as `None`: pane leaf sizes come from `flex_grow` under a definite camera target, not from content measurement (a terminal pane's content size is exactly its layout size). The §3.4 flow confirms it — the `ResizePty` comparison reads `ComputedNode`, not `ContentSize`. If a fux notification/status bar ever needs intrinsic size, the only supported route is `ContentSize::set(NodeMeasure::Custom(Box::new(my_measure)))`, and the measure must be re-`set` after every sync because `ui_layout_system` `take()`s it.

**Pitfalls.** (1) `ContentSize.measure` is `pub(crate)`: fux cannot read or construct it besides `set`/`clear`/`fixed_size`. (2) `NodeMeasure::Custom` boxes a `dyn Measure` — one allocation per node that uses it. (3) `#[reflect(ignore)]` on the measure means a reflected layout projection naturally excludes it (good for fux's `LayoutArchive`), but it also means a measure will never round-trip through `DynamicWorldBuilder`.

### crates/bevy_ui/src/ui_transform.rs (read because layout writes it)

**Mechanism.** `UiTransform { pub translation: Val2, pub scale: Vec2, pub rotation: Rot2 }` (`ui_transform.rs:130-141`) with `#[require(UiGlobalTransform)]` and `compute_affine(scale_factor, base_size, target_size) -> Affine2` (176-183). `UiGlobalTransform(Affine2)` (`ui_transform.rs:206`) is `Deref`+`Copy` with `try_inverse`, `affine()`, `to_scale_angle_translation`, and `Mul` both ways with `Affine2`. `ui_layout_system` computes it as `inherited * local_transform` where `local_transform.translation += local_center` and `local_center = layout_location - effective_parent_scroll + 0.5 * (layout_size - parent_size)` (`layout/mod.rs:260-290`).

**fux/zor use (prompt §3.4, §3.11).** `UiGlobalTransform` is the read surface for cell-level hit-testing and for turning a pane into a cell rect: `ComputedNode::contains_point(transform, point)` (`ui_node.rs:180-200`) and `normalize_point` are built on `try_inverse()`. The fux cell rect of a pane is `transform.affine().translation ± ComputedNode::size()/2`.

**Pitfalls.** (1) The transform's translation is the node **center** relative to the target center (nodes are laid out around the origin — `ComputedNode::border_box()` is `Rect::from_center_size(Vec2::ZERO, size)`), not a top-left corner; converting to cell rows/cols requires the half-size offset. (2) `computed_target.scale_factor.recip()` is passed as `inverse_target_scale_factor` and also feeds `UiTransform` scale resolution — again only 1.0 keeps cells exact. (3) `UiGlobalTransform` is written only when `inherited_transform != **global_transform` (change-tracked), so `Changed<UiGlobalTransform>` is a usable fux invalidation signal.

### crates/bevy_ui/Cargo.toml (feature facts for the dependency report)

`default = []` (line 62); `bevy_picking = ["dep:bevy_picking", "dep:uuid"]` (69); `ghost_nodes = []` (72, under an "Experimental features" comment); `serialize` (63-68) adds serde derives to `Node`/`Val`/`UiTransform`/`UiGlobalTransform` etc. fux's `default-features = false` therefore excludes the picking backend, ghost nodes and serde reflection of layout types — fux's own `LayoutArchive` must supply its own serializable projection.

## bevy_app

### crates/bevy_app/src/propagate.rs

**Mechanism.** `HierarchyPropagatePlugin<C, F = (), R = ChildOf>` (`propagate.rs:41-60`) is generic over the propagated component, an optional query filter and the relationship. `build` (140-153) adds four chained systems into `PropagateSet::<C>` in a chosen schedule — `update_source`, `update_removed_limit`, `propagate_inherited`, `propagate_output` — plus two observers (`on_r_inserted`, `on_r_removed`). `Propagate<C>(pub C)` (73) marks a source; `PropagateOver<C>` (79) suppresses the output component on one entity while still inheriting; `PropagateStop<C>` (84) halts the walk; `Inherited<C>(pub C)` (99) is the internal carrier. `update_source` (158-184) mirrors `Propagate<C>` into `Inherited<C>` on the source and, on removal, re-derives `Inherited` from the parent or drops it. `propagate_inherited` (241-300) walks `R::RelationshipTarget` from every changed/removed `Inherited`, inserting/removing `Inherited<C>` along the way unless `PropagateStop` or a closer `Propagate<C>` intervenes. `propagate_output` (311-334) turns `Changed<Inherited<C>>` into `try_insert(C)` (skipping when the value is already equal) and `RemovedComponents<Inherited<C>>` into `try_remove::<C>()`.

**fux/zor use (prompt §3.4, §3.5).** This is the machinery behind `ComputedUiRenderTargetInfo` reaching every descendant of a tab root: `propagate_ui_target_cameras` writes `Propagate<..>` only on roots (`update.rs:140-168`) and this plugin clones it down `ChildOf`. Two consequences for fux ordering: (a) any fux system reading `ComputedUiRenderTargetInfo`/`ComputedUiTargetCamera` on a non-root must run after `PropagateSet::<C>` (i.e. in `UiSystems::Content` or later) — reading it in `Prepare` gets last frame's value; (b) if fux ever propagates its own component down the layout tree (a per-tab `Theme`, `LayoutGeneration`, `TerminalViewport`), the same plugin/`PropagateSet` idiom applies and must be added *once* per `C`. `PropagateOver<C>` is the natural way to stop `ComputedUiRenderTargetInfo` from being the only source for a node fux wants to override (e.g. a future nested popup with its own target).

**Pitfalls.** (1) Adding `HierarchyPropagatePlugin::<C>` twice for the same `C` would register the four systems twice — `UiPlugin` already adds the two `ComputedUi*` ones, so fux must not. (2) The optional `QueryFilter F` is **not rechecked dynamically** (doc comment 24-26): toggling a component that the filter excludes will not re-propagate until the `Propagate<C>` value or the hierarchy changes. (3) Propagation follows `R::RelationshipTarget` — by default `ChildOf`/`Children`; it does not follow fux's `PaneIn`/`TabOf` relationships unless a separate plugin instance is created for that `R`. (4) Because `Disabled` is a global default query filter (`bevy_ecs/src/entity_disabling.rs:1-45`), a `Disabled` entity is invisible to these propagation queries too: propagation breaks at that node and its subtree keeps stale `Inherited`/`C` values. This is exactly the §3.2 rule "never put `Disabled` on any entity of the bevy_ui tree".

```rust
// propagate.rs:140-153
fn build(&self, app: &mut App) {
    app.add_systems(
        self.schedule,
        (
            update_source::<C, F, R>,
            update_removed_limit::<C, F, R>,
            propagate_inherited::<C, F, R>,
            propagate_output::<C, F>,
        )
            .chain()
            .in_set(PropagateSet::<C>::default()),
    );
```

## bevy_ui: what fux writes versus what fux reads (prompt 3.4)

| Direction | Item | Where the write/read happens | Evidence |
|---|---|---|---|
| **writes** | `Camera` per viewer + `RenderTarget::None { size: UVec2::new(cols, rows) }` | viewer entity on attach/resize | `crates/bevy_camera/src/camera.rs:197-200, 218-220, 384-393, 904-907` |
| **writes** | `camera.computed.target_info = Some(RenderTargetInfo { physical_size: (cols, rows), scale_factor: 1.0 })` | same; the *only* viewport source fux controls | `camera.rs:220, 520-524`; consumed at `bevy_ui/src/update.rs:158-162` |
| **writes** | `UiTargetCamera(viewer_camera)` on each tab root (smallest-viewport viewer for multi-viewer tabs) | layout-prep system, `Prepare` or earlier | `bevy_ui/src/ui_node.rs:2938-2944`; `update.rs:135-146` |
| **writes** | `Node { display, width/height, flex_direction, flex_grow, min_width/min_height, … }` on root / split / pane leaf / separator | layout-mutation systems (split/close/resize/move/swap/zoom/transfer) | `ui_node.rs:492-866`; consumed by `layout/convert.rs:60-140` |
| **writes** | root `width/height = Val::Percent(100.)`, `display = Flex` | tab root spawn | prompt §3.4; `convert.rs:100-103` |
| **writes** | children of a split: `flex_grow = ratio weight`, `min_width`/`min_height = Val::Px(min cells)` | resize/split | prompt §3.4; `convert.rs:103-112` |
| **writes** | separator `Node` 1 cell between siblings (`Val::Px(1.)`) | tree mutation | `geometry.rs:24-60`; `convert.rs:17-57` |
| **writes** | `Display::None` on siblings for zoom; `Display::None` on hidden tab roots | zoom/hide systems | `ui_node.rs:1154-1180`; `layout/mod.rs:316`; `update.rs:63-66` |
| **writes** | `ChildOf`/`Children` order = layout tree (reparent, reorder) | move/swap/reparent | `experimental/ghost_hierarchy.rs:39-40, 104-125` |
| **writes** | `LayoutConfig { use_rounding }` (optional, defaults true) | rarely: per-node rounding opt-out for the oracle | `ui_node.rs:2906-2933`; `layout/mod.rs:242-245` |
| **writes (avoid)** | `UiScale` — leave at 1.0 | never write | `lib.rs:118-128`; `update.rs:158-162` |
| **must not write** | `ComputedUiRenderTargetInfo`, `ComputedUiTargetCamera`, `CalculatedClip`, `ComputedStackIndex`, `UiStack`, `ContentSize.measure` | owned by `bevy_ui` | `ui_node.rs:3016-3070, 2409`; `stack.rs:24`; `measurement.rs:141-160` |
| **reads** | `ComputedNode.size` (rounded, == cells), `unrounded_size`, `content_size` | post-layout pane resize / projection / viewer paint | `ui_node.rs:29-130`; written at `layout/mod.rs:292-330` |
| **reads** | pane cell rect = `UiGlobalTransform.affine().translation ± ComputedNode.size()/2` | projection / attachment frames / picking | `ui_transform.rs:206`; `layout/mod.rs:260-290` |
| **reads** | `ComputedUiRenderTargetInfo::physical_size()` / `scale_factor()` / `logical_size()` | validation, viewer resize bookkeeping | `ui_node.rs:3038-3070` |
| **reads** | `ComputedUiTargetCamera::get()` | assert §3.5 invariant "tab root names a live viewer camera" | `ui_node.rs:3016-3033` |
| **reads** | `UiStack.uinodes` / `UiStack.partition` + `ComputedStackIndex` | fux picking backend hit order | `stack.rs:31-40, 54-116` |
| **reads** | `CalculatedClip` | fux picking backend `clip_check_recursive` | `ui_node.rs:2409-2412`; `update.rs:66-107` |
| **reads** | `ComputedNode::contains_point` / `normalize_point` / `resolve_clip_rect` / `border_box` | hit-testing, separator drag, cell→entity | `ui_node.rs:180-260` |
| **reads** | `UiChildren` / `UiRootNodes` (`bevy_ui::experimental`) | mirror bevy's traversal in fux systems | `ghost_hierarchy.rs:39-40, 104-125` |
| **reads (diagnostic)** | `layout::debug::print_ui_layout_tree(&UiSurface)` | fux diagnostic BRP method | `layout/debug.rs:9-31` |

### Rounding behaviour vs the old `layout.rs`

`ComputedNode.size` is taffy's layout with rounding enabled; `ComputedNode.unrounded_size` is the same layout with rounding disabled (`ui_surface.rs:283-310`). `UiSurface::get_layout` toggles rounding **globally** per call as a function of the node's inherited `LayoutConfig::use_rounding` (`ui_surface.rs:292-296`, `layout/mod.rs:242-245`). With `scale_factor = 1.0` and `UiScale = 1.0`, rounded means whole cells and unrounded is the raw f32 cell extent — the §5 "documented rounding rule" is exactly this pair, so the layout oracle should compare against `unrounded_size` when the old `layout.rs` produced fractional cell geometry, or against `size` when it floored to whole cells. `unrounded_size` is only written on the change-tracked branch alongside `size`, so both stay consistent.

### Multi-viewer smallest-viewport rule

bevy has **no** such rule: `propagate_ui_target_cameras` resolves exactly one camera per root, from that root's `UiTargetCamera` (`update.rs:135-146`), and `DefaultUiCamera::get()` only ever picks a single camera (`ui_node.rs:2990-3008`). A tab shown by several viewers is therefore laid out against whichever viewer camera its `UiTargetCamera` names; the "smallest viewport" choice is a pure fux policy (re-point the component when viewer set/viewport sizes change) enforced by the §3.5 invariant. Size comparison must use `ComputedUiRenderTargetInfo::physical_size` (or the viewer's `Viewport { rows, cols }`), and re-pointing is a plain component write — no tree mutation, no `LayoutGeneration` bump needed.

## bevy_remote

### src/lib.rs

**(a) Mechanism.** `RemotePlugin` is a builder that accumulates `(name, RemoteMethodHandler)` pairs in `RwLock<Vec<...>>` (`lib.rs:572-577`, `lib.rs:607-628`, `lib.rs:651-674`); at `build` it converts each handler into a `Box<dyn System<In = In<Option<Value>>, Out = BrpResult>>`, registers it with `app.main_mut().world_mut().register_boxed_system(system)` and stores the resulting `SystemId` in a `RemoteMethods(HashMap<String, RemoteMethodSystemId>)` resource (`lib.rs:805-830`, `lib.rs:954-993`). Two handler shapes exist: `Instant` (one reply) and `Watching` (returns `Ok(None)` to stay silent), distinguished at registration (`lib.rs:926-932`, `lib.rs:940-960`). Transport tasks never touch the World: they push `BrpMessage { method, params, sender: async_channel::Sender<BrpResult> }` (`lib.rs:1446-1457`) into a bounded mailbox (`CHANNEL_SIZE = 16`, `lib.rs:564`) created in `PreStartup` by `setup_mailbox_channel` as the `BrpSender`/`BrpReceiver` resources (`lib.rs:1460-1475`). The exclusive system `process_remote_requests(&mut World)` drains the mailbox and dispatches: instant handlers via `world.run_system_with(id, message.params)` with the result pushed straight back onto `message.sender` via `force_send`; watching handlers are parked in `RemoteWatchingRequests(Vec<(BrpMessage, RemoteWatchingMethodSystemId)>)` (`lib.rs:1482-1523`, `lib.rs:996-997`). `process_ongoing_watching_requests` re-runs every parked handler each tick with cloned params, uses `try_send` (never blocking the tick), closes the sender on any send failure, and `remove_closed_watching_requests` swap-removes the parked entries in the `Cleanup` set (`lib.rs:1527-1572`). All of this lives in a new `RemoteLast` schedule inserted immediately after `Last` (`lib.rs:832-835`, `lib.rs:910-911`), split into `RemoteSystems::ProcessRequests` then `RemoteSystems::Cleanup` (`lib.rs:842-854`, `lib.rs:916-923`).

**(b) fux/zor use.** This is the exact model for prompt 3.9/4.3: `fux/*` verbs ("one per today's `control::Request` variant") and zor's `zor/*` verbs become `RemoteMethodHandler::Instant`/`Watching` entries whose handlers are the token-checking wrappers; the SystemId table *is* the plugin action API of prompt 4.4 ("no restricted SDK: the BRP method table is the plugin API"). The resource-replacement trick in 3.9 ("because `RemoteMethods` has no `remove`, fux's plugin runs after `RemotePlugin` and replaces the resource") is confirmed by the API surface: only `new`/`insert`/`get`/`methods` exist (`lib.rs:964-993`) — the allowlist test in 3.9 can use `RemoteMethods::methods()` directly. The mailbox is fux's ingest seam: the runner's bounded batch (3.1) is the same shape as `process_remote_requests` draining `BrpReceiver` once per `app.update()`. Moving `RemoteLast` with `MainScheduleOrder::insert_after(First, RemoteLast)` (3.1) is a one-line variant of `lib.rs:832-835`.

**(c) Pitfalls.** (1) An unknown method makes the dispatcher `return` from the whole system, not `continue`, so one bad name stalls every message already queued behind it for that tick — 3.9's decision that the allowlist wrapper answers unknown names itself is required, not optional (`lib.rs:1490-1497`). (2) Instant replies use `force_send`, which evicts the oldest queued value on a full channel; watching replies use `try_send` and `close()` on failure, so a slow watch client silently loses its stream instead of applying backpressure (`lib.rs:1532-1540`). (3) `RemoteWatchingRequests` grows for every `+watch` request until its sender closes; parked handlers re-run unconditionally, so a handler that never returns `Ok(None)` burns a tick per update. (4) `RemotePlugin::build` moves the handler out of the `RwLock<Vec>` with `drain(..)`, so the plugin value cannot be reused across two `App`s. (5) `setup_mailbox_channel` is `PreStartup`; a transport plugin that starts in `Startup` depends on that ordering (see http.rs).

**(d) Excerpt** (`lib.rs:1487-1498`):

```rust
    while let Ok(message) = world.resource_mut::<BrpReceiver>().try_recv() {
        // Fetch the handler for the method. If there's no such handler
        // registered, return an error.
        let Some(&handler) = world.resource::<RemoteMethods>().get(&message.method) else {
            let _ = message.sender.force_send(Err(BrpError {
                code: error_codes::METHOD_NOT_FOUND,
                message: format!("Method `{}` not found", message.method),
                data: None,
            }));
            return;
        };
```

### src/http.rs

**(a) Mechanism.** `RemoteHttpPlugin` holds address/port/headers and in `build` inserts `HostAddress`/`HostPort`/private `HostHeaders` resources plus a `Startup` system, then starts nothing else; the port is never read back and `HostPort` is only a reflection of the requested value (`http.rs:113-160`, `http.rs:226-227`, doc comment on `HostPort`). `start_http_server` clones the `BrpSender` resource into `server_main` and `.detach()`es it on `IoTaskPool::get()` (`http.rs:235-249`). `listen` accepts in a loop and spawns one detached `IoTaskPool` task per connection (`http.rs:266-282`); `handle_client` runs a hyper `http1::Builder::new().timer(SmolTimer::new())` over `FuturesIo::new(client)` with a `service_fn` closing over a clone of the `BrpSender` (`http.rs:284-300`). `process_request_batch` buffers the entire body with `request.into_body().collect().await?.to_bytes()`, parses `BrpBatch::Single|Batch`, and for batches answers each request sequentially, rejecting any `+watch` member with `INVALID_REQUEST` (`http.rs:304-380`). `process_single_request` pulls the `id` out *before* parsing so parse failures still echo the id, then decides streaming by the literal substring `+watch` in the method name and sizes the per-request reply channel 8 (watch) or 1 (instant) before sending the `BrpMessage` (`http.rs:386-429`). Streaming replies are an SSE body: `BrpStream` wraps `Pin<Box<Receiver<BrpResult>>>` and `poll_frame` serializes each result as `data: {json}\n\n` with `Content-Type: text/event-stream`; non-stream replies are one JSON body (`http.rs:431-489`).

**(b) fux/zor use.** This is the transport under 3.9 verbatim: "it accepts any connection, buffers the whole body unbounded, dispatches batches of any size, spawns one task per connection without a cap, applies no request deadline ... and hands handlers `BrpMessage { method, params, sender }` — no headers, no body, no peer". fux's token check must therefore live *inside* each registered handler (3.9), and 3.9's pre-probe of a free port plus overwriting `HostPort` is the mitigation for the read-back gap visible at `http.rs:226-227`. The `+watch`-by-substring rule and the batch refusal are the two transport constraints fux's `fux/events+watch` design must respect (`http.rs:333-352`, `http.rs:407`). SSE framing (`data:` + blank line, `text/event-stream`) is what zor's event consumer must parse (4.2/4.4). The fallback in 3.9 ("a fux hyper service feeding the same `BrpSender` mailbox with limits") is feasible precisely because the mailbox is a resource, not a hyper detail.

**(c) Pitfalls.** (1) `listen` uses `listener.accept().await?`, so a single accept error tears down the whole server loop; per-connection errors are swallowed with `let _ =` (`http.rs:266-282`). (2) No body-size, batch-size, connection-count or request-deadline bound exists anywhere; `SmolTimer` only feeds hyper's default header/idle timers, which is why 3.9 records these as unmitigable from a handler (`http.rs:288-291`, `http.rs:305`). (3) A batch awaits each member's reply in sequence, so one slow handler delays every later member; and `+watch` inside a batch is rejected, not upgraded (`http.rs:317-352`). (4) The watch reply channel is bounded at 8 with `try_send`/`close` on the Bevy side, so a client that stops reading loses the stream (`http.rs:408`, `lib.rs:1532-1540`). (5) `RemoteHttpPlugin` requires `RemotePlugin` (the `BrpSender` resource) and works only with `feature = "http"` and non-wasm (`http.rs:1`, `lib.rs:560-561`); the render-subapp port is `#[cfg(feature = "bevy_render")]` and irrelevant to fux's no-render graph.

**(d) Excerpt** (`http.rs:407-418`):

```rust
    let watch = request.method.contains("+watch");
    let size = if watch { 8 } else { 1 };
    let (result_sender, result_receiver) = async_channel::bounded(size);

    let _ = request_sender
        .send(BrpMessage {
            method: request.method,
            params: request.params,
            sender: result_sender,
        })
        .await;
```

### src/builtin_methods.rs

**(a) Mechanism.** The file is a registry of public method-name constants (`builtin_methods.rs:45-111`) plus one free-function handler per method, each a `System` taking `In<Option<Value>>` and either `&World`/`&mut World`. Params are parsed with `parse`/`parse_some`, which map serde errors to `error_codes::INVALID_PARAMS` (`builtin_methods.rs:577-595`). Read paths resolve type paths through `AppTypeRegistry` into `ReflectComponent`/`ReflectResource` and serialize with `ReflectSerializer` (`builtin_methods.rs:598-660`, `builtin_methods.rs:761-840`); `world.query` builds required/optional/has component-id sets, applies `with`/`without` filters and returns per-entity rows (`builtin_methods.rs:857-1020`, `builtin_methods.rs:1022-1056`). The two `+watch` handlers are the interesting shape: they hold a `Local<HashMap<ComponentId, MessageCursor<RemovedComponentEntity>>>` so each request remembers where it stopped reading `RemovedComponents`, compare `entity_ref.get_change_ticks_by_id(component_id)` against the world change tick for `changed`, and return `Ok(None)` when nothing changed (`builtin_methods.rs:662-759`, `builtin_methods.rs:1435-1479`). `world.observe+watch` is different: it lazily creates a reflected observer per `event` (optionally `with_entity`), the observer callback serializes every trigger into an `Arc<Mutex<Vec<Value>>>` held in the `BrpEventObservers` resource keyed by `"{event}@{entity}"`, and each poll drains the buffer (`builtin_methods.rs:1557-1567`, `builtin_methods.rs:1570-1660`). `registry.schema` (`export_registry_types`) filters the type registry by crate/reflect-type limits and emits JSON schema per registered type (`builtin_methods.rs:1668-1720`); `schedule.list`/`schedule.graph` read `Schedules` and the cached build metadata (`builtin_methods.rs:1722-1800`); `rpc.discover` serializes `RemoteMethods` into an OpenRPC document and fills `servers` from `HostAddress`/`HostPort` when the `http` feature is on (`builtin_methods.rs:1079-1114`).

**(b) fux/zor use.** 3.9 names these functions directly: the token-checking wrappers "delegate to the public `bevy_remote::builtin_methods::process_remote_*_request` systems: the instant ones by direct call `(In(params), world)`, the two `+watch` ones through `world.run_system_cached_with` because their `Local` [state]" — the `Local` is exactly why a cached re-run is needed rather than a fresh `run_system_with` (the SystemId is registered once in `RemoteMethods`, so `run_system_with` already preserves `Local`; `run_system_cached_with` is the equivalent for a wrapper that calls them ad hoc). The fux-owned `fux/events+watch { cursor }` in 3.9 is a direct replacement for `process_remote_observe_watching_request`: both keep a per-stream buffer and drain on poll, but the builtin buffer is unbounded and its observers are never despawned, which is why 3.9 requires "the wrapper owns a bounded per-stream buffer and despawns its observer when the stream closes". `fux/schema`" per-method param/result schemas" (3.9) is the `export_registry_types` pattern applied to fux's own DTOs, and "`rpc.discover` advertises exactly the allowlist but carries names and the server URL only (its `MethodObject`s have empty params)" matches `builtin_methods.rs:1079-1114` exactly. zor's `zor/schema` (4.4) is the same call shape.

**(c) Pitfalls.** (1) `BrpEventObservers` is `HashMap<String, Arc<Mutex<Vec<Value>>>>`; the observer entity created by `world.spawn(observer[.with_entity(target)])` is never tracked or despawned and the `Vec` has no bound — a long-lived stream leaks both (`builtin_methods.rs:1557-1567`, `builtin_methods.rs:1620-1645`). (2) The watch handlers are driven by Bevy change ticks, so a client that polls late observes *current* component values, not a value log; removal detection depends on the `MessageCursor` remaining alive, which it only does because the handler lives in the SystemId table. (3) `process_remote_query_request` takes `&mut World` and `Option<"all">` iterates the whole registry, so it is a full-world scan cost per call. (4) Serialization failures in the observer callback are downgraded to a placeholder event plus `warn_once!`, so clients can silently receive shapeless payloads (`builtin_methods.rs:1626-1638`). (5) The write-capable handlers in this module (`spawn_entity`, `insert/remove/mutate_components`, `insert/remove/mutate_resources`, `despawn_entity`, `reparent_entities`, `trigger_event`, `write_message`) are precisely the 3.9 denylist; they exist and are registered by default, so the allowlist replacement is the only thing standing between a token holder and arbitrary World mutation.

**(d) Excerpt** (`builtin_methods.rs:662-670`, the `Local` cursor idiom):

```rust
pub fn process_remote_get_components_watching_request(
    In(params): In<Option<Value>>,
    world: &World,
    mut removal_cursors: Local<HashMap<ComponentId, MessageCursor<RemovedComponentEntity>>>,
) -> BrpResult<Option<Value>> {
    let BrpGetComponentsParams {
        entity,
        components,
        strict,
    } = parse_some(params)?;
```

## bevy_state

### src/lib.rs

**(a) Mechanism.** Crate-level documentation and the prelude only (`lib.rs:92-121` re-exports `condition::*`, the `state::` types and the `state_scoped` markers). It states the governing fact: states are "app-wide interdependent, finite state machines" with `Default` as the starting state, and there are exactly three flavors — `States` (changed only via `NextState<S>`), `SubStates` (exist only while a source state matches), `ComputedStates` (derived by a `compute` function) (`lib.rs:5-40`). The in-crate test `state_transition_runs_before_pre_startup` pins the policy that the first `OnEnter` runs before `PreStartup` (`lib.rs:145-190`).

**(b) fux/zor use.** Directly underwrites 3.1 ("Server-wide modes (`Starting`, `Serving`, `ShuttingDown`) are `bevy_state` `States` with `OnEnter`/`OnExit` schedules and `in_state`") and 3.11 for viewer modes. fux must add `StatesPlugin` explicitly (already listed in 2's "plugins that must be added explicitly"), because `init_state`/`add_sub_state`/`add_computed_state` warn-and-no-op (or panic) without it.

**(c) Pitfalls.** The crate is `#![no_std]` with `extern crate std` only under `feature = "std"`; fux enables `std`, `bevy_app`, `bevy_reflect` per section 2. `state_scoped` is a separate module not in scope here, but it is installed automatically for every initialized state (see `app.rs`).

**(d) Excerpt** (`lib.rs:11-14`):

```rust
//! - Standard [`States`](state::States) can only be changed by manually setting the [`NextState<S>`](state::NextState) resource.
//!   These states are the baseline on which the other state types are built, and can be used on
//!   their own for many simple patterns.
```

### src/app.rs

**(a) Mechanism.** `AppExtStates` (implemented for both `SubApp` and `App`, the latter delegating to `main_mut()`, `app.rs:288-326`) has four install methods. `init_state::<S>()` warns once if `StatesPlugin` is absent (`app.rs:87-92`), then, only if `State<S>` is absent, inserts `State<S>`, `NextState<S>`, the `StateTransitionEvent<S>` message, calls `S::register_state(schedule)` on the `StateTransition` schedule (which it fetches with `.expect(...)` and therefore panics without `StatesPlugin`), writes an initial `StateTransitionEvent { exited: None, entered: Some(default) }`, and calls `enable_state_scoped_entities::<S>`; re-calling warns "already initialized" (`app.rs:96-120`). `insert_state(state)` uses the same path but overwrites the existing `State<S>`, clears the pending transition messages, and re-emits the initial event (`app.rs:122-155`). `add_computed_state::<S>()` and `add_sub_state::<S>()` register only systems and a message, keyed off the presence of `Messages<StateTransitionEvent<S>>` as the idempotency guard (`app.rs:157-216`). `enable_state_scoped_entities::<S>` adds the `DespawnOn*/DisableOn*/EnableOn*` handling systems into the `ExitSchedules`/`EnterSchedules`/`TransitionSchedules` sets of `StateTransition` (`app.rs:244-286`). `StatesPlugin` inserts `StateTransition` after `PreUpdate`, before `PreStartup` in the startup order, and builds the schedule (`app.rs:330-339`).

**(b) fux/zor use.** 3.1's "use `bevy_app`'s `Main` with `First → PreUpdate → Update → PostUpdate → Last` mapped to `Ingest → Requests/Completions → Lifecycle/Layout → Projection → Effects`" interacts with `StatesPlugin` here: the plugin puts `StateTransition` immediately after `PreUpdate` and before startup, so with fux's own `MainScheduleOrder` edits the state transition lands between the Ingest phase and the Requests phase — a `NextState` set during Ingest is visible to the same update's `Lifecycle/Layout`. The `Retiring` workspace and `Starting` pane markers of 3.2 are the fux analogue of the state-scoped `DisableOnEnter`/`DespawnOnEnter` systems installed at `app.rs:244-286`; fux should implement its own equivalent sets rather than rely on this module, since 3.2 forbids `Disabled` on any `bevy_ui` tree entity. For zor (4.1), the app-level mode (`Idle`/`Running`/`Draining`) is a `States`; per-`Task`/`Attempt` lifecycle is per-entity and cannot use `State<S>` resources (they are singletons) — the reusable part is the `OnEnter`-style ordering guarantee, which zor reproduces as an ordered system set plus entity events.

**(c) Pitfalls.** (1) The "warn if no `StatesPlugin`" guard is only a warning; the actual failure is the `.expect("The `StateTransition` schedule is missing...")` panic in each install method (`app.rs:104-106`, `app.rs:276-280` is the equivalent comment for scoped entities). (2) `init_state` writes its initial transition event directly with `world_mut().write_message` before the schedule ever runs, so the first `OnEnter` fires from the startup-positioned `StateTransition` run (`lib.rs:145-190` test). (3) `insert_state` clears `Messages<StateTransitionEvent<S>>`, discarding any queued transitions. (4) `add_computed_state`/`add_sub_state` guard on the *message* resource, not the state resource, so manual mixing of `init_resource` and these methods can desynchronize.

**(d) Excerpt** (`app.rs:330-339`):

```rust
#[derive(Default)]
pub struct StatesPlugin;

impl Plugin for StatesPlugin {
    fn build(&self, app: &mut App) {
        let mut schedule = app.world_mut().resource_mut::<MainScheduleOrder>();
        schedule.insert_after(PreUpdate, StateTransition);
        schedule.insert_startup_before(PreStartup, StateTransition);
        setup_state_transitions_in_world(app.world_mut());
    }
}
```

### src/condition.rs

**(a) Mechanism.** Three `SystemCondition` systems. `state_exists::<S>` is true if the `State<S>` resource exists (`condition.rs:47-49`). `in_state(state)` returns a cloneable closure comparing `*current_state == state` and false when the resource is absent (`condition.rs:112-118`). `state_changed::<S>` returns `current_state.is_changed()` — i.e. true on the frame the state was written or the resource added (`condition.rs:218-224`). A test pins that all three compose with `distributive_run_if` (`condition.rs:245-258`).

**(b) fux/zor use.** 3.1's "systems ... `in_state`" and 3.11's viewer gating use these directly. The `None => false` behavior matters for fux: a `Disabled`/`Retiring` entity-level filter is separate, but any system gated on `in_state(ServerMode::Serving)` silently stops running while the mode resource is absent (e.g. before `OnEnter(Starting)` inserts it) — which is the desired fail-closed behavior for control surfaces that must not answer while `ShuttingDown`. For 3.11 the viewer's own `States` (e.g. `Attached`/`Detaching`) uses the same conditions.

**(c) Pitfalls.** `state_changed` is a change-detection condition, so it is true on the frame the resource was *added* as well as changed; it is not a replacement for `OnEnter`/`OnExit` and gives no "which state" information. Conditions take `Option<Res<..>>` deliberately, so a missing state never panics — it silently disables systems instead.

**(d) Excerpt** (`condition.rs:218-224`):

```rust
pub fn state_changed<S: States>(current_state: Option<Res<State<S>>>) -> bool {
    let Some(current_state) = current_state else {
        return false;
    };
    current_state.is_changed()
}
```

### src/state/mod.rs

**(a) Mechanism.** Pure module wiring: declares `computed_states`, `freely_mutable_state`, `resources`, `state_set`, `states`, `sub_states`, `transitions`, re-exports all of them and the derive macros (`state/mod.rs:1-13`). The remainder is the derivation test suite, which is the clearest specification of the interaction semantics: a computed state creates/removes `State<Computed>` as its source changes (`state/mod.rs:36-101`), a sub-state is *not* created by its own `NextState` while its source does not match and is removed when the source leaves (`state/mod.rs:103-166`), sub-states may derive from computed states (`state/mod.rs:168-210`), multi-source tuples require *all* sources before `compute` is called and drop the state otherwise (`state/mod.rs:212-290`), and there are explicit `OnEnter`/`OnExit`/`OnTransition` ordering tests (`state/mod.rs:890-970`).

**(b) fux/zor use.** The sub-state test at `state/mod.rs:103-166` is the exact shape for fux's viewer sub-states (a viewer's `Attached` sub-state exists only while the viewer entity/workspace is `Serving`, and `NextState` for it is ignored while absent) and for zor's attempt lifecycle derived from a task's state. The `struct OtherState { a_flexible_value: &'static str, .. }` example (`state/mod.rs:212-230`) demonstrates that states need not be plain enums.

**(c) Pitfalls.** `#[derive(SubStates)]`/`#[derive(ComputedStates)]` come from `bevy_state_macros` re-exported here, but the macro's generated `register_state`/`register_*_systems` is what `AppExtStates` calls; hand-implementing requires also implementing `States` (with `DEPENDENCY_DEPTH = SourceStates::SET_DEPENDENCY_DEPTH + 1`) and `FreelyMutableState`, as the `sub_states.rs` docs show. `DEPENDENCY_DEPTH` is the mechanism that orders dependent transitions and rejects cycles, so hand-written impls that get it wrong will mis-order.

**(d) Excerpt** (`state/mod.rs:37-49`):

```rust
    impl ComputedStates for TestComputedState {
        type SourceStates = Option<SimpleState>;

        fn compute(sources: Option<SimpleState>) -> Option<Self> {
            sources.and_then(|source| match source {
                SimpleState::A => None,
                SimpleState::B(value) => Some(if value { Self::BisTrue } else { Self::BisFalse }),
            })
        }
    }
```

### src/state/states.rs

**(a) Mechanism.** The `States` trait is a marker with one associated const: `const DEPENDENCY_DEPTH: usize = 1` (`states.rs:64-69`). Bounds are `'static + Send + Sync + Clone + PartialEq + Eq + Hash + Debug`; the docs state that `Default` supplies the starting value and that multiple independent states may coexist on one World. A `#[diagnostic::on_unimplemented]` note tells users to `#[derive(States)]` (`states.rs:58-63`).

**(b) fux/zor use.** Every fux mode enum (`ServerMode`, viewer mode, `Retiring` lifecycle) must derive `States`; `Eq + Hash + Debug` is what makes `OnEnter(S::Variant)` usable as a schedule label. `ComputedStates` types intentionally do *not* derive it — `computed_states.rs:95-97` provides the blanket impl instead, so a `ComputedStates` type must not be annotated with `#[derive(States)]`.

**(c) Pitfalls.** `DEPENDENCY_DEPTH` exists to order and de-duplicate computed-state execution and to prevent cycles; it is only correct if `ComputedStates::SourceStates::SET_DEPENDENCY_DEPTH + 1` is respected, so nested derived states (fux's likely shape: server mode → viewer mode → viewer sub-mode) must not be hand-tuned. `Send + Sync` means a state cannot hold a `Handle` to a non-send resource or any thread-local.

**(d) Excerpt** (`states.rs:64-69`):

```rust
pub trait States: 'static + Send + Sync + Clone + PartialEq + Eq + Hash + Debug {
    /// How many other states this state depends on.
    /// Used to help order transitions and de-duplicate [`ComputedStates`](crate::state::ComputedStates), as well as prevent cyclical
    /// `ComputedState` dependencies.
    const DEPENDENCY_DEPTH: usize = 1;
}
```

### src/state/resources.rs

**(a) Mechanism.** Three resources. `State<S>(pub(crate) S)` with `new`/`get`/`Deref`/`PartialEq<S>`, reflectable under `feature = "bevy_reflect"` (`resources.rs:58-80`). `PreviousState<S>`, inserted only after the first transition and useful inside `OnExit`/`OnTransition` (`resources.rs:100-141`). `NextState<S: FreelyMutableState>` is an enum `Unchanged | Pending(S) | PendingIfNeq(S)` where `set` always schedules the transition (including to the same value) and `set_if_neq` writes `PendingIfNeq`, which the transition machinery turns into "do not run the schedules if equal" (`resources.rs:181-215`). `take_next_state` uses `mem::take` through `bypass_change_detection`, calls `set_changed()` manually, and returns `(state, same_state_enforced)` so sub-states can interpret the flag (`resources.rs:218-233`).

**(b) fux/zor use.** This is the mechanism behind 3.2's "use `set_if_neq` for every derived write" and 3.1's mode transitions: a BRP `fux/server.shutdown` handler or an ingest system writes `NextState<ServerMode>`, and the transition is applied by the `StateTransition` schedule placed by `StatesPlugin` (3.1). `PreviousState` is the right source for "flush effects on exit" logic in `OnExit(Serving)`. zor's `Task`/`Attempt` lifecycle needs the same *semantics* (`PendingIfNeq`) but on components, since these are resources.

**(c) Pitfalls.** `State<S>` is `pub(crate)` in its tuple field, so only `get`/`new` are usable outside — nothing can mutate the current state in place; all changes must go through `NextState`. `PreviousState` does not exist before the first transition (`Option<Res<PreviousState<S>>>` is mandatory). `take_next_state` takes the value even when `NextState` is present but `Unchanged`, returning `None`; the change-tick bookkeeping is manual and easy to get wrong if reimplemented.

**(d) Excerpt** (`resources.rs:198-215`):

```rust
    pub fn set(&mut self, state: S) {
        *self = Self::Pending(state);
    }

    /// Like [`set`](Self::set), but will not run any state transition schedules if the target state is the same as the current one.
    /// If [`set`](Self::set) has already been called in the same frame with the same state, the transition schedules will be run anyways.
    pub fn set_if_neq(&mut self, state: S) {
        if !matches!(self, Self::Pending(s) if s == &state) {
            *self = Self::PendingIfNeq(state);
        }
    }

    /// Remove any pending changes to [`State<S>`]
    pub fn reset(&mut self) {
        *self = Self::Unchanged;
    }
```

### src/state/transitions.rs

**(a) Mechanism.** Defines the three schedule labels `OnEnter<S>(pub S)`, `OnExit<S>(pub S)`, and `OnTransition<S> { exited, entered }` (documented to run after `OnExit` and before `OnEnter`, and to run on identity transitions too) (`transitions.rs:19-40`), the `StateTransition` schedule label (`transitions.rs:61`), the `StateTransitionEvent<S> { exited: Option<S>, entered: Option<S>, allow_same_state_transitions: bool }` message (`transitions.rs:68-76`), and the four chained sets `DependentTransitions → ExitSchedules → TransitionSchedules → EnterSchedules` (`transitions.rs:81-92`). `internal_apply_state_transition` is the single write point: it replaces `State<S>`, writes the event (always, even for identity transitions), updates or inserts `PreviousState<S>` via `Commands`, and on removal takes `State<S>` out and emits `{exited: Some(old), entered: None}`, explicitly removing a stale `PreviousState` (`transitions.rs:138-210`). `setup_state_transitions_in_world` creates the schedule and chains the sets, idempotently (`transitions.rs:217-234`). `last_transition` reads only the **last** event for a state type (`transitions.rs:237-240`), and `run_enter`/`run_exit`/`run_transition` pipe it into `world.try_run_schedule(OnEnter(entered))` etc., skipping identity transitions for enter/exit when `!allow_same_state_transitions` but not for `OnTransition` (`transitions.rs:243-286`).

**(b) fux/zor use.** This is the direct answer to 3.1's "...are `bevy_state` `States` with `OnEnter`/`OnExit` schedules and `in_state`": fux adds systems to `OnEnter(ServerMode::Serving)` (start the attachment listener), `OnExit(Serving)` (stop accepting) and `OnTransition { exited: ShuttingDown, entered: Serving }` for restart paths. The strict ordering (`DependentTransitions` before exits, exits before transitions, transitions before enters) is the ordering fux's entity-graph transitions need when a mode change implies teardown: `OnExit` runs leaf-to-root because dependent states are ordered by `DEPENDENCY_DEPTH` (`state_set.rs`). zor's `Attempt` lifecycle (4.1) maps onto the same ordering but with entity events, and 4.1's "Receipts, uncertain states, verification seals" should be emit-in-`OnTransition`-style facts rather than state resources.

**(c) Pitfalls.** (1) `last_transition` discards earlier events, so if several systems queue different `NextState` values in one frame only the final one's `OnEnter`/`OnExit` runs — transitions are last-write-wins per tick. (2) `run_transition` has no identity check at all, so `OnTransition { exited: X, entered: X }` runs on identity transitions (documented, but a surprise for handler code that assumes a real change). (3) `internal_apply_state_transition` writes the event even when `exited == entered`, while enter/exit skip it — subscribers using the event see more transitions than schedule-runners do. (4) `State<S>` insertion/removal goes through `Commands`, i.e. deferred, so within the same tick the resource is only observably changed after the command application point. (5) `PreviousState` is inserted only when a transition actually happens, so `OnExit` code must treat it as optional (`Option<Res<PreviousState<S>>>`).

**(d) Excerpt** (`transitions.rs:243-258`):

```rust
pub(crate) fn run_enter<S: States>(
    transition: In<Option<StateTransitionEvent<S>>>,
    world: &mut World,
) {
    let Some(transition) = transition.0 else {
        return;
    };
    if transition.entered == transition.exited && !transition.allow_same_state_transitions {
        return;
    }
    let Some(entered) = transition.entered else {
        return;
    };

    let _ = world.try_run_schedule(OnEnter(entered));
}
```

### src/state/state_set.rs

**(a) Mechanism.** A sealed trait pair: `StateSetSealed`/`StateSet` (sealed, `state_set.rs:18-49`) and a private `InnerStateSet` implemented for both `S: States` and `Option<S>` with an associated `RawState` and `convert_to_usable_state` that "unwraps" the optional source (`state_set.rs:60-89`). `StateSet` is implemented for a single `InnerStateSet` (`state_set.rs:91-...`) and, via `impl_state_set_sealed_tuples!` (`state_set.rs:258-...`), for tuples. Each implementation's `register_computed_state_systems_in_schedule` / `register_sub_state_systems_in_schedule` adds one system in `ApplyStateTransition::<T>` and the three `last_transition::<T>.pipe(run_exit/run_transition/run_enter)` systems, and — critically — `configure_sets` the new state's sets **relative to its source**: `ApplyStateTransition::<T>` after the source's, `ExitSchedules::<T>` before the source's, `EnterSchedules::<T>` after the source's, `TransitionSchedules::<T>` unconstrained (`state_set.rs:94-156`, `state_set.rs:360-380`). The sub-state registration carries an explicit truth table for `(parent changed, next state, already exists, should exist) -> outcome` (`state_set.rs:161-176`) in which `NextState` is always consumed (via `take_next_state`) but only applied when the parent matches and/or the state should exist.

**(b) fux/zor use.** The `Option<Source>` wrapping is what lets fux derive a sub-state from a state that may not exist yet (e.g. a viewer sub-mode that only exists while `State<ViewerMode>` is present) — exactly the pattern 3.11 needs if the viewer has derived modes. The relative set ordering is the template for fux's own derived/tab-level state machines: exits cascade leaf→root, enters root→leaf, which is the order fux's layout/tab teardown and setup must follow to keep 3.5's invariants (e.g. a tab root's `UiTargetCamera` must exist before child panes enter). The truth table is the reference for zor's attempt lifecycle: a pending attempt-state must be *remembered* while its parent condition is false and applied when it becomes true (the `true/true/false/true -> None -> Some(next)` row), which is exactly the `CreationBarrier`/deferred-creation semantics fux needs for panes.

**(c) Pitfalls.** (1) In the **tuple** implementation of computed states the `allow_same_state_transitions` argument is hard-coded `false`, ignoring `T::ALLOW_SAME_STATE_TRANSITIONS` (`state_set.rs:290`), whereas the single-source implementation passes `T::ALLOW_SAME_STATE_TRANSITIONS` (`state_set.rs:122`); a computed state with tuple sources can therefore never run identity transitions. (2) `StateSet` is sealed, so the only sources are a `States` type, an `Option<States>`, or a tuple of those — a computed state cannot depend on a component or resource. (3) The generated systems are added to the `StateTransition` schedule, so all derived-state work happens in that schedule's `DependentTransitions` set, before any exit schedule; a system that reads the derived state in the same tick must run after `StateTransition`. (4) Tuple sizes are macro-generated (`variadics_please::all_tuples`), so very large source tuples are compile-time-costly but functionally unbounded.

**(d) Excerpt** (`state_set.rs:161-176`, the sub-state truth table header plus rules):

```rust
        // | parent changed | next state | already exists | should exist | what happens                     |
        // | -------------- | ---------- | -------------- | ------------ | -------------------------------- |
        // | false          | false      | false          | -            | -                                |
        // | false          | false      | true           | -            | -                                |
        // | true           | false      | true           | false        | Some(current) -> None            |
        // | true           | true       | true           | false        | Some(current) -> None            |
        // | true           | false      | false          | true         | None -> Some(default)            |
        // | true           | true       | false          | true         | None -> Some(next)               |
        // | true           | true       | true           | true         | Some(current) -> Some(next)      |
        // | false          | true       | true           | true         | Some(current) -> Some(next)      |
        // | true           | false      | true           | true         | Some(current) -> Some(current)   |
```

### src/state/sub_states.rs

**(a) Mechanism.** `SubStates: States + FreelyMutableState` with `type SourceStates: StateSet` and `fn should_exist(sources) -> Option<Self>` (`sub_states.rs:148-167`); the doc states that the value inside `Some` is used as the initial state only when `State<Self>` does not yet exist, and `NextState` may override it. `register_sub_state_systems` simply forwards to `SourceStates::register_sub_state_systems_in_schedule::<Self>` (`sub_states.rs:172-174`), and the derive macro is re-exported (`sub_states.rs:7`). The canonical example is `#[source(AppState = AppState::InGame)] enum GamePhase { Setup, Battle, Conclusion }` (`sub_states.rs:19-27`).

**(b) fux/zor use.** fux's `Retiring`/`Starting` transitions are `States`, but per-viewer or per-tab phases that must exist only while the containing mode is active are `SubStates` — e.g. a viewer's `Attaching → Live → Detaching` phase existing only under `WorkspaceMode::Serving`. zor's `Attempt` phase derived from a `Task` state is the same shape (`#[source(TaskState = TaskState::Running)]`), and `should_exist` returning `None` is the clean way to express 4.1's archived/`Disabled` attempts without a separate dirty flag.

**(c) Pitfalls.** `SubStates` requires `FreelyMutableState` in addition to `States`, so `NewState` must be settable; when the source condition stops matching, `State<Self>` is *removed* from the World (not set to a terminal value), which means `Res<State<Self>>` becomes non-optional-failure and any `in_state` condition on it silently goes false (`state/mod.rs:103-166` test). `should_exist` is called on every parent transition, so it must be pure and cheap; the value in `Some(..)` is ignored once the state exists, which is a common source of "my initial state was ignored" bugs.

**(d) Excerpt** (`sub_states.rs:148-167`):

```rust
pub trait SubStates: States + FreelyMutableState {
    /// The set of states from which the [`Self`] is derived.
    ///
    /// This can either be a single type that implements [`States`], or a tuple
    /// containing multiple types that implement [`States`], or any combination of
    /// types implementing [`States`] and Options of types implementing [`States`].
    type SourceStates: StateSet;

    /// This function gets called whenever one of the [`SourceStates`](Self::SourceStates) changes.
    /// ...
    /// Initial value can also be overwritten by [`NextState`](crate::state::NextState).
    fn should_exist(sources: Self::SourceStates) -> Option<Self>;
```

### src/state/computed_states.rs

**(a) Mechanism.** `ComputedStates` needs only `'static + Send + Sync + Clone + PartialEq + Eq + Hash + Debug` — no `States` derive — plus `type SourceStates: StateSet`, `const ALLOW_SAME_STATE_TRANSITIONS: bool = true`, and `fn compute(sources) -> Option<Self>`; `None` removes `State<Self>` from the world (`computed_states.rs:68-93`). A blanket `impl<S: ComputedStates> States for S` sets `DEPENDENCY_DEPTH = S::SourceStates::SET_DEPENDENCY_DEPTH + 1` (`computed_states.rs:95-97`). `register_computed_state_systems` forwards to the `StateSet` (`computed_states.rs:90-92`). A test pins that computed states are state-scoped by default: an entity spawned with `DespawnOnEnter(TestComputedState)` is despawned on the first `StateTransition` run (`computed_states.rs:127-146`).

**(b) fux/zor use.** fux's projection targets in 3.2 (`PaneView`, `TabView`, ...) are not state, but a *whole-server* derived mode is: e.g. `Serving` as `compute` over `(ServerMode, AssetLoadMode)` in the "multi-viewer" or "config-loading" cases, so systems can branch on one derived enum instead of matching two resources. The `ALLOW_SAME_STATE_TRANSITIONS` const is the exact analogue of fux's rule that a re-projection with an unchanged semantic revision must not re-run enter/exit work. For zor, a computed `MachineHealth` derived from `(MachineState, ServiceState)` is the intended use.

**(c) Pitfalls.** (1) `compute` is called only when a source fires a `StateTransitionEvent` (`state_set.rs:94-124`), so a computed state does not recompute on unrelated World changes; it is not a general-purpose derivation hook. (2) A computed state can be *removed* (`None`) even when the app is otherwise fine, so downstream systems must use `Option<Res<State<C>>>` or `in_state`. (3) The tuple-source identity-transition inconsistency noted under `state_set.rs` applies here because `ALLOW_SAME_STATE_TRANSITIONS` is ignored for tuples. (4) Because of the blanket impl, a type implementing `ComputedStates` must not also derive `States` (compile error on the `DEPENDENCY_DEPTH` associated const).

**(d) Excerpt** (`computed_states.rs:90-97`):

```rust
    fn register_computed_state_systems(schedule: &mut Schedule) {
        Self::SourceStates::register_computed_state_systems_in_schedule::<Self>(schedule);
    }
}

impl<S: ComputedStates> States for S {
    const DEPENDENCY_DEPTH: usize = S::SourceStates::SET_DEPENDENCY_DEPTH + 1;
}
```

## bevy_asset

### src/server/mod.rs

**(a) Mechanism.** `AssetServer` is a `Resource + Clone` wrapper over `Arc<AssetServerData>` (`server/mod.rs:65-79`), so it can be cloned into detached pool tasks and shared across threads. `AssetServerData` holds `infos: RwLock<AssetInfos>`, the loader table, the asset sources, the mode/meta-check policy, and — the key part — an **unbounded crossbeam channel** `asset_event_sender`/`asset_event_receiver` created in `new_with_loaders` (`server/mod.rs:72-88`, `server/mod.rs:140-153`); `watching_for_changes` is copied into `AssetInfos` there (`server/mod.rs:142`). Loading is a two-phase handoff: the caller gets a handle synchronously through `get_or_create_path_handle_erased`, and if a new load is needed `spawn_load_task` records `infos.stats.started_load_tasks += 1` and spawns `IoTaskPool::get().spawn(async move { server.load_internal(Some(owned_handle), path, false, None).await })` (`server/mod.rs:573-605`). On multi-threaded non-wasm targets the returned task is stored in `infos.pending_tasks` (keyed by asset index) instead of being detached — that is what keeps a load alive and lets `handle_internal_asset_events` reap finished ones; on wasm/single-threaded the task is `detach()`ed (`server/mod.rs:584-604`). The async path sends results back with `send_asset_event(InternalAssetEvent::Loaded { .. } | Failed { .. })`, and `send_asset_event` is `self.data.asset_event_sender.send(event).unwrap()` (`server/mod.rs:1216-1218`, `server/mod.rs:678-690`, `server/mod.rs:932-946`). `register_asset::<A>` additionally installs two per-type `fn(&mut World, ...)` callbacks that write into `Messages<AssetEvent<A>>` and `Messages<AssetLoadFailedEvent<A>>` (`server/mod.rs:204-233`). The single drain point is `pub fn handle_internal_asset_events(world: &mut World)`: it `resource_scope`s the server, takes `write_infos()`, and `for event in server.data.asset_event_receiver.try_iter()` dispatches `Loaded` → `infos.process_asset_load(index, loaded_asset, world, &sender)`, `LoadedWithDependencies` → the per-type `dependency_loaded_event_sender` (which is what emits `AssetEvent::LoadedWithDependencies`) plus waking waiters, and `Failed` → `process_asset_fail` plus batched `UntypedAssetLoadFailedEvent` and the typed failure sender (`server/mod.rs:2056-2108`). After that, still holding the info lock, it drains **file-watch** events only when `infos.watching_for_changes`, walking `AssetSourceEvent::{AddedAsset, ModifiedAsset, ModifiedMeta, RenamedFolder, RemovedAsset, RemovedFolder, AddedFolder}` into a set of paths to reload (including `queue_ancestors` over `loader_dependents`), then calls `load_folder_internal`/`reload_internal` — so hot reload is the same "spawn a task that later sends into the channel" loop (`server/mod.rs:2110-2200`). Finally it retains only unfinished `pending_tasks` (`server/mod.rs:2205-2211`). `ErasedLoadedAsset` is the type-erased payload: `Box<dyn AssetContainer>` plus dependency sets, loader dependencies and labeled sub-assets, with `From<LoadedAsset<A>>` and `take::<A>()`/`get::<A>()` downcasts (`loader.rs:220-232`, `loader.rs:246-290`); `process_asset_load` resolves labeled assets first, then `loaded_asset.value.insert(loaded_asset_index.index, world)` puts the value into the typed `Assets<A>` storage and queues the `Added`/`Modified` event, and only then computes dependency load states and re-sends `InternalAssetEvent::LoadedWithDependencies` through the same channel when the recursive dependencies are done (`server/info.rs:403-500`). On the ECS side `Assets::<A>::asset_events` moves `queued_events` into `Messages<AssetEvent<A>>` (and updates `AssetChanges<A>` with `SystemChangeTick`), gated by `asset_events_condition`, registered in `PostUpdate` in set `AssetEventSystems` (`assets.rs:595-622`, `lib.rs:660-666`, `lib.rs:708-709`); `Assets::<A>::track_assets` (handle-drop handling) runs in `PreUpdate` in set `AssetTrackingSystems`, explicitly ordered `after(handle_internal_asset_events)` (`lib.rs:418-421`, `assets.rs:576-593`).

**(b) fux/zor use.** This is the named model for 3.6: "`Effect` messages ... are drained by the runner and applied by the adapter, which converts OS activity into `Inbound` messages" — the PTY reader and the provider adapters in 4.1 are `IoTaskPool::get().spawn(...)` tasks that push typed messages into an `async_channel` held as a resource, with one *exclusive* draining system per phase, exactly like `handle_internal_asset_events`. The handle-plus-guard pattern (`spawn_load_task` keeps an owned handle and a `guard` alive until the task ends, `server/mod.rs:573-604`) is the shape for fux's pending-operation receipts and `CreationBarrier`: an `OwnedHandle`-style strong reference keeps the fux entity's work alive until the adapter reports completion, and the guard's `Drop` is the completion signal. `ErasedLoadedAsset` is the pattern for a type-erased adapter payload (a `Box<dyn ProcessOutput>` where fux does not know statically whether a reader produces PTY bytes or provider JSON), and `AssetServer`'s `Arc + clone-into-task` design is the rule for any fux resource handed to `IoTaskPool`. The hot-reload block is 3.7 literally: "Systems react through `AssetChanged<T>` queries and `AssetEvent`s ... a reload that fails validation keeps the previous asset" — `process_asset_load` only writes on `Loaded`, so a failed reload leaves the old value in storage and emits `Failed`/`AssetLoadFailedEvent` (`server/mod.rs:2086-2108`, `server/info.rs:433`). The ordering constraint from `asset_changed.rs:108-114` (react in `PostUpdate` after `AssetEventSystems`, or the next frame) is the constraint on fux's config/theme/keybinding reload systems. `AssetServerMode` (Processed vs Unprocessed, `server/mod.rs:83-89`) has no fux analogue and fux should stay `Unprocessed` (the `file_watcher` feature, per section 2).

**(c) Pitfalls.** (1) The channel is `crossbeam_channel::unbounded` and `send_asset_event` is `.unwrap()`: a burst of hot-reload events cannot exert backpressure and a dropped receiver **panics the loading task** (`server/mod.rs:140`, `server/mod.rs:1216-1218`). (2) `handle_internal_asset_events` holds the `AssetInfos` write lock for the entire drain, including the reload bookkeeping; the code has an explicit cfg-gated `drop(infos)` before spawning because of single-threaded deadlock risk, and fux must respect the same rule for its own adapters (never await while holding an info lock) (`server/mod.rs:2170-2178`). (3) Task retention is `#[cfg(not(any(target_arch = "wasm32", not(feature = "multi_threaded"))))]`; on the non-retained path the task is detached and `pending_tasks` silently stays empty, so "wait for pending loads" logic differs by target — fux pins `multi_threaded` (section 2). (4) `publish_asset_server_diagnostics` and the whole reload block are inside the same exclusive system, so a slow reload policy shows up as frame-time in the phase that hosts it; `lib.rs:422-427` explicitly marks the system `ambiguous_with_all()`, meaning any fux system in the same schedule must not assume an ordering relative to it unless it is added to `AssetTrackingSystems` (which is ordered after it). (5) `handle_event` has a catch-all `_ => {}` (`server/mod.rs:2156`), so not every `AssetSourceEvent` triggers a reload. (6) `AssetEvent::{Added, Modified}` are queued by `Assets` insertion and mutable access (`assets.rs:372-396`, `assets.rs:683-690`) — the drain reads a `Vec` that is only flushed by `asset_events`, so a system reading `Messages<AssetEvent<A>>` in `Update` sees the previous frame's events.

**(d) Excerpt** (`server/mod.rs:2056-2072`):

```rust
pub fn handle_internal_asset_events(world: &mut World) {
    world.resource_scope(|world, server: Mut<AssetServer>| {
        let mut infos = server.write_infos();
        let var_name = vec![];
        let mut untyped_failures = var_name;
        for event in server.data.asset_event_receiver.try_iter() {
            match event {
                InternalAssetEvent::Loaded {
                    index,
                    loaded_asset,
                } => {
                    infos.process_asset_load(
                        index,
                        loaded_asset,
                        world,
                        &server.data.asset_event_sender,
                    );
                }
```

