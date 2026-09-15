//! Viewer acceptance (milestone 2): input translation, key encoding, scene replication into a
//! headless viewer App, the cell painter's composition and diff, and directional navigation over
//! a replicated 2x2 layout. The real terminal and socket are not involved: frames and terminal
//! events go through `viewer::Inbox`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration-test helpers outside #[test] fns; clippy.toml only relaxes test fns"
)]

use bevy_app::App;
use bevy_ecs::prelude::*;
use bevy_ecs::reflect::AppTypeRegistry;
use bevy_input::ButtonState;
use bevy_input::keyboard::{Key, KeyCode, KeyboardInput};
use bevy_input_focus::InputFocus;
use bevy_input_focus::tab_navigation::TabIndex;
use bevy_math::UVec2;
use bevy_picking::pointer::{PointerAction, PointerButton, PointerId};
use bevy_ui::prelude::*;
use bevy_ui::{ComputedNode, UiTargetCamera};
use bevy_world_serialization::DynamicWorldBuilder;
use termina::WindowSize;
use termina::event::{
    Event, KeyCode as TKey, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseEventKind,
};

use fux::model::{Ids, InstanceNode, NodeId, PaneId, PointerKind, Shows, ViewerRequest};
use fux::viewer::input::{Input, Translator};
use fux::viewer::keys::{self, KeyChord};
use fux::viewer::replicate::EntityMap;
use fux::viewer::{self, Inbox, Outbox, Painter};
use fux::wire::{
    Cell, ClientFrame, Cursor, Line, Modes, ProcessSummary, RootEntry, SceneFrame, ServerFrame,
    Style, TerminalDelta,
};

fn window() -> Entity {
    Entity::from_bits(7)
}

fn key(code: TKey, modifiers: Modifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

fn translate(event: Event) -> Option<Input> {
    Translator::new(window(), 80, 24).translate(&event)
}

fn key_input(event: Event) -> KeyboardInput {
    match translate(event) {
        Some(Input::Key(input)) => input,
        other => panic!("expected a key, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// (1) input translation
// ---------------------------------------------------------------------------------------------

#[test]
fn keys_translate_to_keyboard_input_with_terminal_bytes() {
    let ctrl_c = key_input(key(TKey::Char('c'), Modifiers::CONTROL));
    assert_eq!(ctrl_c.key_code, KeyCode::KeyC);
    assert_eq!(ctrl_c.logical_key, Key::Character("c".into()));
    assert_eq!(ctrl_c.text.as_deref(), Some("\x03"));
    assert_eq!(ctrl_c.state, ButtonState::Pressed);
    assert_eq!(ctrl_c.window, window());

    let up = key_input(key(TKey::Up, Modifiers::NONE));
    assert_eq!(up.key_code, KeyCode::ArrowUp);
    assert_eq!(up.logical_key, Key::ArrowUp);
    assert_eq!(up.text.as_deref(), Some("\x1b[A"));

    let shift_up = key_input(key(TKey::Up, Modifiers::SHIFT));
    assert_eq!(shift_up.text.as_deref(), Some("\x1b[1;2A"));

    let alt_x = key_input(key(TKey::Char('x'), Modifiers::ALT));
    assert_eq!(alt_x.text.as_deref(), Some("\x1bx"));

    let f5 = key_input(key(TKey::Function(5), Modifiers::NONE));
    assert_eq!(f5.key_code, KeyCode::F5);
    assert_eq!(f5.text.as_deref(), Some("\x1b[15~"));

    assert!(
        translate(Event::Key(KeyEvent {
            kind: termina::event::KeyEventKind::Release,
            ..KeyEvent::from(TKey::Char('a'))
        }))
        .is_none(),
        "releases carry nothing"
    );
}

#[test]
fn mouse_translates_to_pointer_input_and_pointer_event() {
    let mut translator = Translator::new(window(), 80, 24);
    let press = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 3,
        row: 4,
        modifiers: Modifiers::NONE,
    });
    let Some(Input::Mouse(mouse)) = translator.translate(&press) else {
        panic!("expected mouse input");
    };
    let button = mouse.button.expect("button input");
    assert_eq!(button.button, bevy_input::mouse::MouseButton::Left);
    assert_eq!(button.state, ButtonState::Pressed);
    let action = mouse.pointer_action.expect("pointer action");
    assert_eq!(action.pointer_id, PointerId::Mouse);
    assert_eq!(action.location.position, bevy_math::Vec2::new(3.5, 4.5));
    assert!(matches!(
        action.action,
        PointerAction::Press(PointerButton::Primary)
    ));
    assert!(
        mouse.pointer_move.is_some(),
        "first event positions the pointer"
    );
    assert_eq!(mouse.event.col, 3);
    assert_eq!(mouse.event.row, 4);
    assert_eq!(mouse.event.kind, PointerKind::Press);
    assert_eq!(mouse.event.button, fux::model::PointerButton::Left);

    let scroll = Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 3,
        row: 4,
        modifiers: Modifiers::NONE,
    });
    let Some(Input::Mouse(mouse)) = translator.translate(&scroll) else {
        panic!("expected mouse input");
    };
    assert!(mouse.pointer_move.is_none(), "same cell: no move");
    assert_eq!(mouse.wheel.map(|w| w.y), Some(1.0));
    assert_eq!(mouse.event.kind, PointerKind::ScrollUp);

    let resized = translator.translate(&Event::WindowResized(WindowSize {
        cols: 100,
        rows: 40,
        pixel_width: None,
        pixel_height: None,
    }));
    assert!(matches!(
        resized,
        Some(Input::Resize {
            cols: 100,
            rows: 40
        })
    ));
}

// ---------------------------------------------------------------------------------------------
// (2) key encoding
// ---------------------------------------------------------------------------------------------

fn encoded(event: Event, modes: Modes) -> Vec<u8> {
    let mut out = Vec::new();
    keys::encode(&key_input(event), modes, &mut out);
    out
}

#[test]
fn arrows_follow_application_cursor_mode() {
    let normal = Modes::default();
    let app = Modes {
        application_cursor: true,
        ..Modes::default()
    };
    assert_eq!(encoded(key(TKey::Up, Modifiers::NONE), normal), b"\x1b[A");
    assert_eq!(encoded(key(TKey::Up, Modifiers::NONE), app), b"\x1bOA");
    assert_eq!(encoded(key(TKey::Left, Modifiers::NONE), app), b"\x1bOD");
    assert_eq!(encoded(key(TKey::Home, Modifiers::NONE), app), b"\x1bOH");
    // Modified arrows keep the CSI form in either mode.
    assert_eq!(
        encoded(key(TKey::Up, Modifiers::CONTROL), app),
        b"\x1b[1;5A"
    );
    assert_eq!(
        encoded(key(TKey::PageDown, Modifiers::NONE), app),
        b"\x1b[6~"
    );
}

#[test]
fn control_letters_and_alt_encode_as_bytes() {
    let modes = Modes::default();
    assert_eq!(
        encoded(key(TKey::Char('a'), Modifiers::CONTROL), modes),
        b"\x01"
    );
    assert_eq!(
        encoded(key(TKey::Char('['), Modifiers::CONTROL), modes),
        b"\x1b"
    );
    assert_eq!(
        encoded(key(TKey::Char(' '), Modifiers::CONTROL), modes),
        b"\0"
    );
    assert_eq!(
        encoded(key(TKey::Char('x'), Modifiers::ALT), modes),
        b"\x1bx"
    );
    assert_eq!(encoded(key(TKey::Enter, Modifiers::NONE), modes), b"\r");
    assert_eq!(
        encoded(key(TKey::BackTab, Modifiers::NONE), modes),
        b"\x1b[Z"
    );
    assert_eq!(
        encoded(key(TKey::Char('é'), Modifiers::NONE), modes),
        "é".as_bytes()
    );
}

#[test]
fn paste_is_bracketed_only_when_requested() {
    let mut out = Vec::new();
    keys::encode_paste("a\nb", Modes::default(), &mut out);
    assert_eq!(out, b"a\nb");
    out.clear();
    keys::encode_paste(
        "a\nb",
        Modes {
            bracketed_paste: true,
            ..Modes::default()
        },
        &mut out,
    );
    assert_eq!(out, b"\x1b[200~a\nb\x1b[201~");
}

#[test]
fn chords_derive_modifiers_from_terminal_bytes() {
    assert_eq!(
        KeyChord::of(&key_input(key(TKey::Char('b'), Modifiers::CONTROL))),
        KeyChord::ctrl('b')
    );
    assert_eq!(
        KeyChord::of(&key_input(key(TKey::Char('x'), Modifiers::ALT))),
        KeyChord {
            alt: true,
            ..KeyChord::character('x')
        }
    );
    assert_eq!(
        KeyChord::of(&key_input(key(TKey::Char('%'), Modifiers::SHIFT))),
        KeyChord::character('%')
    );
    // Named keys never read as Ctrl even though their bytes are control characters.
    assert_eq!(
        KeyChord::of(&key_input(key(TKey::Enter, Modifiers::NONE))),
        KeyChord::plain(Key::Enter)
    );
    // Shift on a named key is what the terminal encoded: `CSI Z` or the `;2` parameter.
    assert_eq!(
        KeyChord::of(&key_input(key(TKey::BackTab, Modifiers::NONE))),
        KeyChord::shift(Key::Tab)
    );
    assert_eq!(
        KeyChord::of(&key_input(key(TKey::Up, Modifiers::SHIFT))),
        KeyChord::shift(Key::ArrowUp)
    );
    assert_eq!(
        KeyChord::of(&key_input(key(TKey::Up, Modifiers::ALT))),
        KeyChord::plain(Key::ArrowUp)
    );
}

// ---------------------------------------------------------------------------------------------
// Scene fixtures: a server-side World serialized the way the attachment stream does it
// ---------------------------------------------------------------------------------------------

struct ServerScene {
    world: World,
    registry: AppTypeRegistry,
    root: Entity,
    leaves: Vec<Entity>,
    panes: Vec<Entity>,
}

impl ServerScene {
    fn registry() -> AppTypeRegistry {
        let registry = AppTypeRegistry::default();
        {
            let mut r = registry.write();
            r.register::<Node>();
            r.register::<ComputedNode>();
            r.register::<Name>();
            r.register::<PaneId>();
            r.register::<NodeId>();
            r.register::<ChildOf>();
            r.register::<Children>();
            r.register::<Shows>();
            r.register::<InstanceNode>();
        }
        registry
    }

    fn leaf(world: &mut World, container: Entity, node: u64, pane: u64) -> (Entity, Entity) {
        let pane = world.spawn(PaneId(pane)).id();
        let leaf = world
            .spawn((
                InstanceNode,
                NodeId(node),
                ChildOf(container),
                Shows(pane),
                Node {
                    flex_grow: 1.0,
                    flex_basis: Val::Px(0.0),
                    ..Default::default()
                },
            ))
            .id();
        (leaf, pane)
    }

    /// `rows` x `cols` grid of panes: root column of row containers, each row a flex row of
    /// leaves. Pane ids count from 1 in reading order.
    fn grid(rows: usize, cols: usize) -> Self {
        let mut world = World::new();
        world.init_resource::<Ids>();
        let mut leaves = Vec::new();
        let mut panes = Vec::new();
        let root = world
            .spawn((
                InstanceNode,
                NodeId(1),
                Name::new("main"),
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    ..Default::default()
                },
            ))
            .id();
        let mut next_node = 2;
        for r in 0..rows {
            let container = if rows > 1 {
                let c = world
                    .spawn((
                        InstanceNode,
                        NodeId(next_node),
                        ChildOf(root),
                        Node {
                            flex_direction: FlexDirection::Row,
                            flex_grow: 1.0,
                            flex_basis: Val::Px(0.0),
                            ..Default::default()
                        },
                    ))
                    .id();
                next_node += 1;
                c
            } else {
                world.entity_mut(root).insert(Node {
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Row,
                    ..Default::default()
                });
                root
            };
            for c in 0..cols {
                let (leaf, pane) =
                    Self::leaf(&mut world, container, next_node, (r * cols + c + 1) as u64);
                next_node += 1;
                leaves.push(leaf);
                panes.push(pane);
            }
        }
        Self {
            world,
            registry: Self::registry(),
            root,
            leaves,
            panes,
        }
    }

    /// `[a | [b / c]]`: pane 1 fills the left half, panes 2 and 3 stack in the right half.
    fn beside_stack() -> Self {
        let mut world = World::new();
        world.init_resource::<Ids>();
        let root = world
            .spawn((
                InstanceNode,
                NodeId(1),
                Name::new("main"),
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Row,
                    ..Default::default()
                },
            ))
            .id();
        let (a, pane_a) = Self::leaf(&mut world, root, 2, 1);
        let column = world
            .spawn((
                InstanceNode,
                NodeId(3),
                ChildOf(root),
                Node {
                    flex_direction: FlexDirection::Column,
                    flex_grow: 1.0,
                    flex_basis: Val::Px(0.0),
                    ..Default::default()
                },
            ))
            .id();
        let (b, pane_b) = Self::leaf(&mut world, column, 4, 2);
        let (c, pane_c) = Self::leaf(&mut world, column, 5, 3);
        Self {
            world,
            registry: Self::registry(),
            root,
            leaves: vec![a, b, c],
            panes: vec![pane_a, pane_b, pane_c],
        }
    }

    /// A 40-column root that clips its overflow, holding a 20-column pane 1 and a 60-column
    /// pane 2: pane 2's cells past column 40 are laid out but never painted.
    fn clipped_overflow() -> Self {
        let mut world = World::new();
        world.init_resource::<Ids>();
        let root = world
            .spawn((
                InstanceNode,
                NodeId(1),
                Name::new("main"),
                Node {
                    width: Val::Percent(50.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Row,
                    overflow: Overflow::clip(),
                    ..Default::default()
                },
            ))
            .id();
        let (a, pane_a) = Self::leaf(&mut world, root, 2, 1);
        let (b, pane_b) = Self::leaf(&mut world, root, 3, 2);
        for (leaf, width) in [(a, 20.0), (b, 60.0)] {
            world.entity_mut(leaf).insert(Node {
                width: Val::Px(width),
                flex_shrink: 0.0,
                ..Default::default()
            });
        }
        Self {
            world,
            registry: Self::registry(),
            root,
            leaves: vec![a, b],
            panes: vec![pane_a, pane_b],
        }
    }

    fn ron(&self) -> String {
        let registry = self.registry.read();
        let entities: Vec<Entity> = self.world.iter_entities().map(|e| e.id()).collect();
        DynamicWorldBuilder::from_world(&self.world, &registry)
            .deny_all()
            .allow_component::<Node>()
            .allow_component::<Name>()
            .allow_component::<PaneId>()
            .allow_component::<NodeId>()
            .allow_component::<ChildOf>()
            .allow_component::<Children>()
            .allow_component::<Shows>()
            .allow_component::<InstanceNode>()
            .extract_entities(entities.into_iter())
            .build()
            .serialize(&registry)
            .expect("serialize scene")
    }

    fn frame(&self, revision: u64) -> SceneFrame {
        SceneFrame {
            revision,
            full: true,
            scene: self.ron(),
            despawned: Vec::new(),
            roots: Some(vec![RootEntry {
                node: NodeId(1),
                name: "main".into(),
            }]),
            target: Some(PaneId(1)),
            showing: Some(NodeId(1)),
            terminals: Vec::new(),
            notice: None,
        }
    }
}

fn delta(pane: u64, cols: u16, rows: u16, first_row: &str) -> TerminalDelta {
    TerminalDelta {
        pane: PaneId(pane),
        seq: 1,
        rows,
        cols,
        full: true,
        lines: vec![Line {
            row: 0,
            cells: first_row
                .chars()
                .map(|c| Cell {
                    text: String::from(c),
                    width: 1,
                    style: Style::default(),
                })
                .collect(),
        }],
        cursor: Cursor::default(),
        modes: Modes::default(),
        title: None,
        clipboard: None,
        process: ProcessSummary::Live,
    }
}

fn push_frame(app: &mut App, frame: SceneFrame) {
    app.world_mut()
        .resource_mut::<Inbox>()
        .frames
        .push(ServerFrame::Scene(frame));
}

fn push_keys(app: &mut App, events: impl IntoIterator<Item = Event>) {
    app.world_mut()
        .resource_mut::<Inbox>()
        .events
        .extend(events);
}

fn take_requests(app: &mut App) -> Vec<ViewerRequest> {
    core::mem::take(&mut app.world_mut().resource_mut::<Outbox>().0)
        .into_iter()
        .filter_map(|f| match f {
            ClientFrame::Request { request } => Some(request),
            ClientFrame::Ack { .. } => None,
        })
        .collect()
}

fn local(app: &App, server: Entity) -> Entity {
    *app.world()
        .resource::<EntityMap>()
        .0
        .get(&server)
        .unwrap_or_else(|| panic!("{server:?} not replicated"))
}

// ---------------------------------------------------------------------------------------------
// (3) replication
// ---------------------------------------------------------------------------------------------

#[test]
fn scene_frames_replicate_with_mapped_ids_and_local_camera() {
    let scene = ServerScene::grid(1, 2);
    let mut app = viewer::build(80, 24);
    push_frame(&mut app, scene.frame(1));
    app.update();

    let world = app.world();
    let root = local(&app, scene.root);
    let camera = world.resource::<viewer::LocalCamera>().0;
    let area = world.resource::<viewer::ChromeRoots>().pane_area;
    assert_eq!(world.get::<UiTargetCamera>(root).map(|c| c.0), Some(camera));
    assert_eq!(world.get::<ChildOf>(root).map(|c| c.parent()), Some(area));
    assert!(world.get::<viewer::Replicated>(root).is_some());
    assert_eq!(world.get::<NodeId>(root), Some(&NodeId(1)));

    let leaves: Vec<Entity> = scene.leaves.iter().map(|&l| local(&app, l)).collect();
    let children: Vec<Entity> = world.get::<Children>(root).unwrap().iter().collect();
    assert_eq!(
        children, leaves,
        "child order follows the server's Children"
    );
    for (leaf, pane) in leaves.iter().zip(&scene.panes) {
        assert!(
            world.get::<TabIndex>(*leaf).is_some(),
            "leaves are tabbable"
        );
        assert_eq!(
            world.get::<Shows>(*leaf).map(|s| s.0),
            Some(local(&app, *pane))
        );
    }
    assert_eq!(
        world.resource::<Ids>().pane(PaneId(2)),
        Some(local(&app, scene.panes[1]))
    );

    // Laid out locally against the pane area (one status row).
    let size = world.get::<ComputedNode>(root).unwrap().size.as_uvec2();
    assert_eq!(size, UVec2::new(80, 23));
    let left = world
        .get::<ComputedNode>(leaves[0])
        .unwrap()
        .size
        .as_uvec2();
    assert_eq!(left, UVec2::new(40, 23));
    assert_eq!(world.resource::<viewer::TargetPane>().0, Some(PaneId(1)));
    assert_eq!(world.resource::<viewer::Roots>().0.len(), 1);
    let acks: Vec<u64> = world
        .resource::<Outbox>()
        .0
        .iter()
        .filter_map(|f| match f {
            ClientFrame::Ack { revision } => Some(*revision),
            _ => None,
        })
        .collect();
    assert_eq!(acks, vec![1]);
}

#[test]
fn despawned_ids_remove_entities_and_unshown_panes() {
    let scene = ServerScene::grid(1, 2);
    let mut app = viewer::build(80, 24);
    push_frame(&mut app, scene.frame(1));
    app.update();
    let gone_leaf = local(&app, scene.leaves[1]);
    let gone_pane = local(&app, scene.panes[1]);
    let kept_leaf = local(&app, scene.leaves[0]);

    push_frame(
        &mut app,
        SceneFrame {
            revision: 2,
            full: false,
            despawned: vec![scene.leaves[1].to_bits()],
            roots: None,
            ..scene.frame(2)
        },
    );
    // A delta also carries the root's new `Children`; emulate with the server world updated.
    app.update();

    let world = app.world();
    assert!(
        world.get_entity(gone_leaf).is_err(),
        "despawned leaf is gone"
    );
    assert!(
        world.get_entity(gone_pane).is_err(),
        "pane no leaf shows is gone"
    );
    assert!(world.get_entity(kept_leaf).is_ok());
    assert!(
        !world
            .resource::<EntityMap>()
            .0
            .contains_key(&scene.leaves[1])
    );
    assert_eq!(world.resource::<Ids>().pane(PaneId(2)), None);
    assert_eq!(
        world.resource::<Ids>().pane(PaneId(1)),
        Some(local(&app, scene.panes[0]))
    );
}

// ---------------------------------------------------------------------------------------------
// (4) painter
// ---------------------------------------------------------------------------------------------

#[test]
fn painter_composes_panes_and_chrome_and_diffs() {
    let scene = ServerScene::grid(1, 2);
    let mut app = viewer::build(80, 24);
    let mut frame = scene.frame(1);
    frame.terminals = vec![delta(1, 40, 23, "hello"), delta(2, 40, 23, "world")];
    frame.roots = Some(vec![
        RootEntry {
            node: NodeId(1),
            name: "main".into(),
        },
        RootEntry {
            node: NodeId(99),
            name: "logs".into(),
        },
    ]);
    push_frame(&mut app, frame);
    app.update();

    let text = |app: &App, col: u16, row: u16| -> String {
        app.world()
            .resource::<Painter>()
            .screen()
            .get(col, row)
            .map(|c| String::from(c.text.as_str()))
            .unwrap_or_default()
    };
    assert_eq!(text(&app, 0, 0), "h");
    assert_eq!(text(&app, 4, 0), "o");
    assert_eq!(text(&app, 5, 0), " ");
    assert_eq!(text(&app, 40, 0), "w");
    assert_eq!(text(&app, 44, 0), "d");
    // Status bar: the tab strip starts at column 0 of the last row.
    let bar: String = (0..13).map(|c| text(&app, c, 23)).collect();
    assert_eq!(bar, "1:main 2:logs");
    let bg_at = |app: &App, col: u16| {
        app.world()
            .resource::<Painter>()
            .screen()
            .get(col, 23)
            .map(|c| c.style.bg)
    };
    assert_eq!(
        bg_at(&app, 7),
        bg_at(&app, 79),
        "inactive labels keep the bar's fill"
    );
    assert_ne!(bg_at(&app, 7), Some(fux::wire::Color::Default));
    assert_ne!(
        bg_at(&app, 0),
        bg_at(&app, 7),
        "the active tab is highlighted"
    );
    let first = core::mem::take(&mut app.world_mut().resource_mut::<Painter>().out);
    assert!(!first.is_empty(), "first paint writes the screen");
    assert!(
        first.windows(5).any(|w| w == b"hello"),
        "pane content reaches the terminal"
    );
    // The runner hands the written buffer back for its capacity; the next paint must not
    // resend what it still contains.
    app.world_mut().resource_mut::<Painter>().out = first;

    app.update();
    let second = core::mem::take(&mut app.world_mut().resource_mut::<Painter>().out);
    assert!(
        second.is_empty(),
        "an identical frame emits nothing, got {:?}",
        String::from_utf8_lossy(&second)
    );

    // A single changed cell repaints only that cell.
    let mut update = delta(2, 40, 23, "wOrld");
    update.full = false;
    update.seq = 2;
    push_frame(
        &mut app,
        SceneFrame {
            revision: 2,
            full: false,
            scene: String::new(),
            roots: None,
            terminals: vec![update],
            ..scene.frame(2)
        },
    );
    app.update();
    let third = core::mem::take(&mut app.world_mut().resource_mut::<Painter>().out);
    let printable: String = third
        .iter()
        .filter(|b| b.is_ascii_alphanumeric())
        .map(|&b| b as char)
        .collect();
    assert!(
        third.windows(1).filter(|w| w == b"O").count() == 1,
        "{printable}"
    );
    assert!(
        !third.windows(5).any(|w| w == b"hello"),
        "unchanged cells are not resent"
    );
    assert_eq!(text(&app, 41, 0), "O");
}

// ---------------------------------------------------------------------------------------------
// (5) directional navigation
// ---------------------------------------------------------------------------------------------

fn focused(app: &App) -> Option<Entity> {
    app.world().resource::<InputFocus>().get()
}

#[test]
fn prefix_hjkl_reaches_every_leaf_of_a_2x2_layout() {
    let scene = ServerScene::grid(2, 2);
    let mut app = viewer::build(80, 24);
    push_frame(&mut app, scene.frame(1));
    app.update();
    let leaf = |app: &App, i: usize| local(app, scene.leaves[i]);
    assert_eq!(
        focused(&app),
        Some(leaf(&app, 0)),
        "focus follows the server target"
    );
    assert!(
        take_requests(&mut app).is_empty(),
        "server-driven focus sends no Target"
    );

    let prefix = || key(TKey::Char('b'), Modifiers::CONTROL);
    let step = |app: &mut App, c: char| {
        push_keys(app, [prefix(), key(TKey::Char(c), Modifiers::NONE)]);
        app.update();
    };
    step(&mut app, 'l');
    assert_eq!(focused(&app), Some(leaf(&app, 1)), "l moves east");
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::Target(PaneId(2))]
    );
    step(&mut app, 'j');
    assert_eq!(focused(&app), Some(leaf(&app, 3)), "j moves south");
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::Target(PaneId(4))]
    );
    step(&mut app, 'h');
    assert_eq!(focused(&app), Some(leaf(&app, 2)), "h moves west");
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::Target(PaneId(3))]
    );
    step(&mut app, 'k');
    assert_eq!(focused(&app), Some(leaf(&app, 0)), "k moves north");
    assert!(
        take_requests(&mut app).is_empty(),
        "back on the server's target: nothing to request"
    );
    step(&mut app, 'k');
    assert_eq!(
        focused(&app),
        Some(leaf(&app, 0)),
        "no neighbour keeps focus"
    );

    // A plain key in Normal mode reaches the focused pane as bytes, not a binding.
    push_keys(&mut app, [key(TKey::Char('l'), Modifiers::NONE)]);
    app.update();
    assert_eq!(focused(&app), Some(leaf(&app, 0)));
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::Input(b"l".to_vec())]
    );

    // Prefix bindings that talk to the server.
    step(&mut app, '%');
    assert!(matches!(
        take_requests(&mut app).as_slice(),
        [ViewerRequest::Split {
            direction: fux::model::SplitDirection::Right,
            template: None
        }]
    ));
    step(&mut app, 'c');
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::NewRoot { template: None }]
    );
    step(&mut app, 'x');
    assert!(
        take_requests(&mut app).is_empty(),
        "close waits for confirmation"
    );
    push_keys(&mut app, [key(TKey::Char('y'), Modifiers::NONE)]);
    app.update();
    assert_eq!(take_requests(&mut app), vec![ViewerRequest::ClosePane]);
}

#[test]
fn navigation_beside_a_stack_matches_the_server_rule() {
    let scene = ServerScene::beside_stack();
    let mut app = viewer::build(80, 24);
    push_frame(&mut app, scene.frame(1));
    app.update();
    let leaf = |app: &App, i: usize| local(app, scene.leaves[i]);
    let step = |app: &mut App, c: char| {
        push_keys(
            app,
            [
                key(TKey::Char('b'), Modifiers::CONTROL),
                key(TKey::Char(c), Modifiers::NONE),
            ],
        );
        app.update();
    };
    assert_eq!(focused(&app), Some(leaf(&app, 0)));
    step(&mut app, 'l');
    assert_eq!(
        focused(&app),
        Some(leaf(&app, 1)),
        "l from a reaches the top-aligned b"
    );
    step(&mut app, 'j');
    assert_eq!(focused(&app), Some(leaf(&app, 2)), "j from b reaches c");
    step(&mut app, 'h');
    assert_eq!(focused(&app), Some(leaf(&app, 0)), "h from c reaches a");
    step(&mut app, 'l');
    step(&mut app, 'j');
    step(&mut app, 'k');
    assert_eq!(focused(&app), Some(leaf(&app, 1)), "k from c reaches b");
    step(&mut app, 'h');
    assert_eq!(focused(&app), Some(leaf(&app, 0)), "h from b reaches a");
}

#[test]
fn prefix_tab_and_shift_tab_walk_tab_order_both_ways() {
    let scene = ServerScene::grid(2, 2);
    let mut app = viewer::build(80, 24);
    push_frame(&mut app, scene.frame(1));
    app.update();
    let leaf = |app: &App, i: usize| local(app, scene.leaves[i]);
    let step = |app: &mut App, code: TKey| {
        push_keys(
            app,
            [
                key(TKey::Char('b'), Modifiers::CONTROL),
                key(code, Modifiers::NONE),
            ],
        );
        app.update();
    };
    assert_eq!(focused(&app), Some(leaf(&app, 0)));
    step(&mut app, TKey::Tab);
    assert_eq!(
        focused(&app),
        Some(leaf(&app, 1)),
        "Tab moves to the next leaf"
    );
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::Target(PaneId(2))]
    );
    step(&mut app, TKey::BackTab);
    assert_eq!(
        focused(&app),
        Some(leaf(&app, 0)),
        "Shift-Tab moves to the previous leaf"
    );
    step(&mut app, TKey::BackTab);
    assert_eq!(
        focused(&app),
        Some(leaf(&app, 3)),
        "Shift-Tab wraps to the last leaf"
    );
    step(&mut app, TKey::Tab);
    assert_eq!(
        focused(&app),
        Some(leaf(&app, 0)),
        "Tab wraps to the first leaf"
    );
    take_requests(&mut app);

    // A focus outside every tab group (a leaf re-instanced away, here the status bar) is not a
    // dead end: the walk resumes from the group's edge.
    let status_bar = app.world().resource::<viewer::ChromeRoots>().status_bar;
    app.world_mut()
        .resource_mut::<InputFocus>()
        .set(status_bar, bevy_input_focus::FocusCause::Navigated);
    step(&mut app, TKey::BackTab);
    assert_eq!(
        focused(&app),
        Some(leaf(&app, 3)),
        "Shift-Tab from a stranded focus reaches the last leaf"
    );
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::Target(PaneId(4))]
    );

    // Plain Tab in Normal mode is pane input, not navigation.
    push_keys(&mut app, [key(TKey::Tab, Modifiers::NONE)]);
    app.update();
    assert_eq!(focused(&app), Some(leaf(&app, 3)));
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::Input(b"\t".to_vec())]
    );
}

// ---------------------------------------------------------------------------------------------
// (6) click-to-focus through the cell picking backend
// ---------------------------------------------------------------------------------------------

fn click(app: &mut App, col: u16, row: u16) -> Vec<ViewerRequest> {
    let at = |kind| {
        Event::Mouse(MouseEvent {
            kind,
            column: col,
            row,
            modifiers: Modifiers::NONE,
        })
    };
    push_keys(
        app,
        [
            at(MouseEventKind::Down(MouseButton::Left)),
            at(MouseEventKind::Up(MouseButton::Left)),
        ],
    );
    app.update();
    take_requests(app)
        .into_iter()
        .filter(|r| !matches!(r, ViewerRequest::Pointer(_)))
        .collect()
}

#[test]
fn clicks_focus_the_visible_pane_and_ignore_clipped_cells() {
    let scene = ServerScene::clipped_overflow();
    let mut app = viewer::build(80, 24);
    push_frame(&mut app, scene.frame(1));
    app.update();
    let leaf = |app: &App, i: usize| local(app, scene.leaves[i]);
    assert_eq!(focused(&app), Some(leaf(&app, 0)));

    assert_eq!(
        click(&mut app, 30, 5),
        vec![ViewerRequest::Target(PaneId(2))],
        "a click on pane 2's visible cells focuses it"
    );
    assert_eq!(focused(&app), Some(leaf(&app, 1)));

    assert_eq!(
        click(&mut app, 10, 5),
        vec![],
        "back on the server's target: nothing to request"
    );
    assert_eq!(focused(&app), Some(leaf(&app, 0)));

    // Column 60 is inside pane 2's rect but clipped by the root: nothing to pick there.
    assert_eq!(
        click(&mut app, 60, 5),
        vec![],
        "a clipped cell is not pickable"
    );
    assert_eq!(focused(&app), Some(leaf(&app, 0)));
}

// ---------------------------------------------------------------------------------------------
// (7) clipboard forwarding
// ---------------------------------------------------------------------------------------------

#[test]
fn pane_clipboard_writes_reach_the_terminal_as_osc_52_once() {
    let scene = ServerScene::grid(1, 2);
    let mut app = viewer::build(80, 24);
    let mut frame = scene.frame(1);
    frame.terminals = vec![delta(1, 40, 23, "hello"), delta(2, 40, 23, "world")];
    push_frame(&mut app, frame);
    app.update();
    let out = |app: &mut App| core::mem::take(&mut app.world_mut().resource_mut::<Painter>().out);
    let osc = b"\x1b]52;c;aGVsbG8=\x07";
    assert!(
        !out(&mut app).windows(osc.len()).any(|w| w == osc),
        "no clipboard write without a payload"
    );

    let mut update = delta(2, 40, 23, "world");
    update.full = false;
    update.seq = 2;
    update.clipboard = Some("aGVsbG8=".into());
    push_frame(
        &mut app,
        SceneFrame {
            revision: 2,
            full: false,
            scene: String::new(),
            roots: None,
            terminals: vec![update],
            ..scene.frame(2)
        },
    );
    app.update();
    let bytes = out(&mut app);
    assert_eq!(
        bytes.windows(osc.len()).filter(|w| *w == osc).count(),
        1,
        "one OSC 52 write per delta that carries a payload: {:?}",
        String::from_utf8_lossy(&bytes)
    );
    assert!(
        bytes.ends_with(osc),
        "the clipboard write follows the frame's cell diff"
    );

    app.update();
    assert!(
        !out(&mut app).windows(osc.len()).any(|w| w == osc),
        "a payload is not replayed on later frames"
    );

    // An unknown pane's delta is dropped whole, clipboard included.
    let mut stray = delta(9, 40, 23, "x");
    stray.clipboard = Some("c3RyYXk=".into());
    push_frame(
        &mut app,
        SceneFrame {
            revision: 3,
            full: false,
            scene: String::new(),
            roots: None,
            terminals: vec![stray],
            ..scene.frame(3)
        },
    );
    app.update();
    assert!(
        !out(&mut app).windows(4).any(|w| w == b"]52;"),
        "no clipboard write for a pane the viewer does not show"
    );
}

// ---------------------------------------------------------------------------------------------
// Connection handshake and framing against a local listener
// ---------------------------------------------------------------------------------------------

#[test]
fn connection_sends_hello_then_streams_frames_to_the_wake_channel() {
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::PermissionsExt as _;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let dir = tempfile::tempdir().expect("tempdir");
    let brp = dir.path().join("test.brp.json");
    let descriptor = fux::remote::descriptor::Descriptor {
        instance: "nonce".into(),
        pid: std::process::id(),
        http: fux::remote::descriptor::Endpoint {
            host: "127.0.0.1".into(),
            port: 1,
        },
        attach: Some(fux::remote::descriptor::AttachDescriptor {
            host: "127.0.0.1".into(),
            port,
            token: "secret".into(),
        }),
        token: "brp-token".into(),
    };
    std::fs::write(&brp, serde_json::to_vec(&descriptor).expect("json")).expect("write");
    std::fs::set_permissions(&brp, std::fs::Permissions::from_mode(0o600)).expect("chmod");

    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept");
        let mut prefix = [0u8; 4];
        socket.read_exact(&mut prefix).expect("prefix");
        let len = fux::wire::payload_len(&prefix).expect("len").expect("len");
        let mut payload = vec![0u8; len];
        socket.read_exact(&mut payload).expect("payload");
        let hello: fux::wire::Hello = serde_json::from_slice(&payload).expect("hello");
        let mut out = Vec::new();
        fux::wire::encode(
            &ServerFrame::Welcome(fux::wire::Welcome {
                viewer: fux::model::ViewerId(1),
                instance: hello.instance.clone(),
                workspace: hello.workspace.clone(),
            }),
            &mut out,
        )
        .expect("encode");
        socket.write_all(&out).expect("welcome");
        // The client's first request must arrive intact too.
        socket.read_exact(&mut prefix).expect("prefix");
        let len = fux::wire::payload_len(&prefix).expect("len").expect("len");
        let mut payload = vec![0u8; len];
        socket.read_exact(&mut payload).expect("payload");
        let frame: ClientFrame = serde_json::from_slice(&payload).expect("client frame");
        fux::wire::encode(
            &ServerFrame::Bye {
                reason: fux::wire::ByeReason::Detached,
                message: "bye".into(),
            },
            &mut out,
        )
        .expect("encode");
        socket.write_all(&out).expect("bye");
        (hello, frame)
    });

    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    let mut connection = viewer::connection::Connection::connect(
        &brp,
        "default",
        fux::model::Viewport { rows: 23, cols: 80 },
        None,
        wake_tx,
    )
    .expect("connect");
    connection
        .send(&ClientFrame::Request {
            request: ViewerRequest::Detach,
        })
        .expect("send");
    let (hello, frame) = server.join().expect("server thread");
    assert_eq!(hello.token, "secret");
    assert_eq!(hello.instance, "nonce");
    assert_eq!(hello.workspace, "default");
    assert_eq!(hello.viewport, fux::model::Viewport { rows: 23, cols: 80 });
    assert!(matches!(
        frame,
        ClientFrame::Request {
            request: ViewerRequest::Detach
        }
    ));
    let timeout = std::time::Duration::from_secs(5);
    assert!(matches!(
        wake_rx.recv_timeout(timeout).expect("welcome wake"),
        viewer::Wake::Frame(ServerFrame::Welcome(_))
    ));
    assert!(matches!(
        wake_rx.recv_timeout(timeout).expect("bye wake"),
        viewer::Wake::Frame(ServerFrame::Bye { .. })
    ));
    assert!(matches!(
        wake_rx.recv_timeout(timeout).expect("disconnect wake"),
        viewer::Wake::Disconnected(_)
    ));
    connection.close();
}
